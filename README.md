# msg_load_folder

Generic plugin-based folder loading infrastructure for Bevy games.

This crate provides a plugin that automatically discovers and loads assets from folders, creating a library resource indexed by IDs derived from filenames. It enables data-driven game design where content is defined in asset files rather than code.

## Features

- **Automatic discovery**: Loads all assets from a folder matching specified extensions
- **Multiple extensions**: Load folders with mixed formats (e.g., `.ogg` + `.wav`) via `with_extension()`
- **ID derivation**: Automatically derives IDs from filenames (e.g., `fireball.spell.ron` -> `SpellId("fireball")`)
- **Generic design**: Works with any asset type and ID type — config files, audio, textures, etc.
- **Loading state tracking**: Provides resources to check loading progress
- **Resilient loading**: Files are loaded individually, so a single malformed file (e.g. a `.ron` with a syntax error) never blocks the rest of the folder
- **Hot reloading**: When asset watching is on, edited, added and removed files are picked up automatically
- **File filtering**: Skips hidden files (`.`) and disabled files (`_`)

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
msg_load_folder = { git = "https://github.com/MolecularSadism/msg_load_folder", tag = "v0.4.0" }
bevy = "0.19"
serde = { version = "1.0", features = ["derive"] }
```

## Quick Start

```rust
use msg_load_folder::prelude::*;
use bevy::prelude::*;
use serde::Deserialize;

// 1. Define your asset type
#[derive(Asset, Clone, Reflect, Deserialize)]
struct Spell {
    name: String,
    damage: f32,
}

// 2. Define your ID type (must implement From<String>)
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
struct SpellId(u64);

impl From<String> for SpellId {
    fn from(s: String) -> Self {
        // In practice, use interned strings for efficiency
        SpellId(s.len() as u64)
    }
}

// 3. Add the plugin
fn build_app(app: &mut App) {
    app.add_plugins(FolderLoaderPlugin::<SpellId, Spell>::new(
        "prefabs/spells",   // Folder path relative to assets/
        ".spell.ron",       // File extension to match
    ));
}

// 4. Use the library
fn use_spells(
    library: Res<AssetFolder<SpellId, Spell>>,
    assets: Res<Assets<Spell>>,
) {
    for (id, handle) in library.iter() {
        if let Some(spell) = assets.get(handle) {
            info!("Loaded spell: {}", spell.name);
        }
    }
}
```

## File Organization

Assets are organized in folders with a consistent naming convention:

```text
assets/
  prefabs/
    spells/
      fireball.spell.ron      -> SpellId("fireball")
      ice_bolt.spell.ron      -> SpellId("ice_bolt")
      _disabled.spell.ron     -> Skipped (starts with _)
      .hidden.spell.ron       -> Skipped (starts with .)
    items/
      health_potion.item.ron  -> ItemId("health_potion")
```

## Resilience & Hot Reloading

Files are discovered by scanning the folder and then loaded **individually**.
This makes loading resilient: if one file is malformed — for example a `.ron`
file with a syntax or semantic error — only that single entry is affected. Every
other asset in the folder still loads, and the broken file's ID is simply absent
from the library until the file is fixed. (This is a deliberate improvement over
Bevy's `load_folder`, which fails the *entire* folder if any one file fails to
load.)

When Bevy's asset watching is enabled, the library also **hot reloads**:

- **Editing** a file reloads its asset in place. Existing handles stay valid, so
  references keep working. This is also how a previously-broken file recovers —
  fix it and save, and it loads on the next watcher tick.
- **Adding** or **removing** a file is detected automatically and the library is
  updated to match.
- **Re-adding** a file at a path that was previously removed loads its *fresh*
  contents rather than a stale cached copy — so a remove-then-re-add cycle always
  reflects what is currently on disk.

Untouched entries keep their exact handle across any add/remove churn, so a
structural change to the folder never invalidates references you hold to other
assets.

Hot reloading requires the `AssetServer` to be watching for changes. Opt in via
`AssetPlugin` (and enable Bevy's `file_watcher` feature):

```rust,no_run
# use bevy::prelude::*;
# use bevy::asset::AssetPlugin;
# let mut app = App::new();
# app.add_plugins(MinimalPlugins);
// In a full app this is usually `DefaultPlugins.set(AssetPlugin { .. })`;
// the watch setting lives on `AssetPlugin` either way.
app.add_plugins(AssetPlugin {
    watch_for_changes_override: Some(true),
    ..default()
});
```

Without watching, the folder is still scanned and loaded once (and remains
resilient to malformed files); it just won't react to later changes.

## API Reference

### `FolderLoaderPlugin<Id, A>`

Plugin that sets up automatic folder-based asset loading.

```rust
# use msg_load_folder::prelude::*;
# use bevy::prelude::*;
# use bevy::asset::AssetPlugin;
# #[derive(Asset, Clone, Reflect)]
# struct Spell { name: String }
# #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
# struct SpellId(u64);
# impl From<String> for SpellId { fn from(s: String) -> Self { SpellId(s.len() as u64) } }
# // Stand-in for a Bevy audio asset — any `Asset` type works the same way.
# #[derive(Asset, Clone, Reflect)]
# struct AudioSource;
# #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
# struct SoundId(u64);
# impl From<String> for SoundId { fn from(s: String) -> Self { SoundId(s.len() as u64) } }
# let mut app = App::new();
# app.add_plugins(MinimalPlugins).add_plugins(AssetPlugin::default());
// Single extension
app.add_plugins(FolderLoaderPlugin::<SpellId, Spell>::new(
    "prefabs/spells",  // folder_path
    ".spell.ron",      // file_extension
));

// Multiple extensions — chain with_extension()
app.add_plugins(
    FolderLoaderPlugin::<SoundId, AudioSource>::new("sounds", ".ogg")
        .with_extension(".wav")
        .with_extension(".mp3"),
);
```

### `AssetFolder<Id, A>`

Resource containing loaded assets indexed by ID.

```rust
# use msg_load_folder::prelude::*;
# use bevy::prelude::*;
# #[derive(Asset, Clone, Reflect)]
# struct Spell { name: String }
# #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
# struct SpellId(u64);
# let mut library: AssetFolder<SpellId, Spell> = AssetFolder::new();
# let spell_id = SpellId(1);
# library.insert(spell_id, Handle::default());
// Inside a system you'd take `library: Res<AssetFolder<SpellId, Spell>>`.

// Get by ID
if let Some(handle) = library.get(spell_id) {
    let _ = handle;
}

// Check if ID exists
if library.contains(spell_id) {
    // ...
}

// Iterate all
for (id, handle) in library.iter() {
    let _ = (id, handle);
}

// Check loading state
if library.is_ready() {
    // ...
}

// Get count
let count = library.len();
# assert_eq!(count, 1);
```

### `AssetFolderHandle<Id, A>`

Resource tracking folder loading state.

```rust
# use msg_load_folder::prelude::*;
# use bevy::prelude::*;
# #[derive(Asset, Clone, Reflect)]
# struct Spell;
# #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
# struct SpellId(u64);
# let handle: AssetFolderHandle<SpellId, Spell> = AssetFolderHandle::new();
// Inside a system you'd take `handle: Res<AssetFolderHandle<SpellId, Spell>>`.
if handle.is_loaded() {
    info!("Spell folder has been scanned and registered!");
}
# assert!(!handle.is_loaded());
```

`is_loaded()` becomes `true` once the folder has been scanned and its assets
registered at least once. The library keeps reacting to changes afterwards, so
this is a "ready" signal rather than a terminal state.

### `AtlasIcon`

Helper struct for icon rendering from texture atlases.

```rust
# use msg_load_folder::prelude::*;
# use bevy::prelude::*;
# let image_handle = Handle::default();
# let layout_handle = Handle::default();
# let atlas_index = 0;
let icon = AtlasIcon::new(image_handle, layout_handle, atlas_index);
let image_node = icon.image_node();
let texture_atlas = icon.texture_atlas();
# let _ = (image_node, texture_atlas);
```

## Multiple File Extensions

For folders containing assets in multiple formats (e.g., mixed audio files), use `with_extension()`:

```rust
# use msg_load_folder::prelude::*;
# use bevy::prelude::*;
# use bevy::asset::AssetPlugin;
# // Stand-in for a Bevy audio asset — any `Asset` type works the same way.
# #[derive(Asset, Clone, Reflect)]
# struct AudioSource;
# #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
# struct SoundId(u64);
# impl From<String> for SoundId { fn from(s: String) -> Self { SoundId(s.len() as u64) } }
# let mut app = App::new();
# app.add_plugins(MinimalPlugins).add_plugins(AssetPlugin::default());
// Loads .ogg, .wav, and .mp3 files from the sounds/ folder
app.add_plugins(
    FolderLoaderPlugin::<SoundId, AudioSource>::new("sounds", ".ogg")
        .with_extension(".wav")
        .with_extension(".mp3"),
);
```

Files are matched against extensions in the order they were added. The ID is derived by stripping the matching extension from the filename:

```text
assets/
  sounds/
    explosion.ogg       -> SoundId("explosion")
    ambient.wav         -> SoundId("ambient")
    click.mp3           -> SoundId("click")
    _disabled.ogg       -> Skipped (starts with _)
```

## Utility Functions

### `id_from_filename`

Extract an ID from a filename path.

```rust
use std::path::Path;
use msg_load_folder::id_from_filename;
# #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
# struct SpellId(u64);
# impl From<String> for SpellId { fn from(s: String) -> Self { SpellId(s.len() as u64) } }

let path = Path::new("spells/fireball.spell.ron");
let id: Option<SpellId> = id_from_filename(path, ".spell.ron");
// Returns Some(SpellId("fireball"))
# assert!(id.is_some());
```

### `id_from_filename_with_extensions`

Extract an ID by trying multiple extensions in order.

```rust
use std::path::Path;
use msg_load_folder::id_from_filename_with_extensions;
# #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
# struct SoundId(u64);
# impl From<String> for SoundId { fn from(s: String) -> Self { SoundId(s.len() as u64) } }

let path = Path::new("explosion.ogg");
let id: Option<SoundId> = id_from_filename_with_extensions(path, &[".ogg", ".wav", ".mp3"]);
// Returns Some(SoundId("explosion"))
# assert!(id.is_some());
```

### `is_hidden_file`

Check if a path represents a hidden or disabled file.

```rust
use std::path::Path;
use msg_load_folder::is_hidden_file;

assert!(is_hidden_file(Path::new(".hidden.ron")));
assert!(is_hidden_file(Path::new("_disabled.ron")));
assert!(!is_hidden_file(Path::new("normal.ron")));
```

### `deserialize_optional_string`

Serde helper for optional string fields.

```rust
use serde::Deserialize;
use msg_load_folder::deserialize_optional_string;

#[derive(Deserialize)]
struct MyData {
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    atlas_slice: Option<String>,
}
# let _ = MyData { atlas_slice: None };
```

## Integration with `msg_interned_id`

This crate works well with `msg_interned_id` for efficient ID types:

```rust,ignore
use msg_interned_id::InternedId;
use msg_load_folder::prelude::*;

#[derive(InternedId, Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct SpellId(bevy::ecs::intern::Interned<str>);

app.add_plugins(FolderLoaderPlugin::<SpellId, SpellData>::new(
    "prefabs/spells",
    ".spell.ron",
));
```

## Bevy Version Compatibility

| `msg_load_folder` | Bevy |
|-------------------|------|
| 0.4               | 0.19 |
| 0.3               | 0.18 |
| 0.2               | 0.17 |
| 0.1               | 0.16 |

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

Contributions are welcome! This crate is part of the [MolecularSadism](https://github.com/MolecularSadism) game development libraries.
