# msg_load_folder

Generic plugin-based folder loading infrastructure for Bevy games.

This crate provides a plugin that automatically discovers and loads assets from folders, creating a library resource indexed by IDs derived from filenames. It enables data-driven game design where content is defined in asset files rather than code.

## Features

- **Automatic discovery**: Loads all assets from a folder matching a specified extension
- **ID derivation**: Automatically derives IDs from filenames (e.g., `fireball.spell.ron` -> `SpellId("fireball")`)
- **Generic design**: Works with any asset type and ID type
- **Loading state tracking**: Provides resources to check loading progress
- **Error handling**: Gracefully handles failed assets without crashing
- **File filtering**: Skips hidden files (`.`) and disabled files (`_`)

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
msg_load_folder = { git = "https://github.com/MolecularSadism/msg_load_folder", tag = "v0.2.0" }
bevy = "0.17"
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

```
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

## API Reference

### `FolderLoaderPlugin<Id, A>`

Plugin that sets up automatic folder-based asset loading.

```rust
app.add_plugins(FolderLoaderPlugin::<SpellId, Spell>::new(
    "prefabs/spells",  // folder_path
    ".spell.ron",      // file_extension
));
```

### `AssetFolder<Id, A>`

Resource containing loaded assets indexed by ID.

```rust
fn my_system(library: Res<AssetFolder<SpellId, Spell>>) {
    // Get by ID
    if let Some(handle) = library.get(spell_id) { ... }

    // Check if ID exists
    if library.contains(spell_id) { ... }

    // Iterate all
    for (id, handle) in library.iter() { ... }

    // Check loading state
    if library.is_ready() { ... }

    // Get count
    let count = library.len();
}
```

### `AssetFolderHandle<A>`

Resource tracking folder loading state.

```rust
fn check_loading(handle: Res<AssetFolderHandle<Spell>>) {
    if handle.is_loading() {
        info!("Still loading spells...");
    }
    if handle.is_loaded() {
        info!("All spells loaded!");
    }
}
```

### `AtlasIcon`

Helper struct for icon rendering from texture atlases.

```rust
let icon = AtlasIcon::new(image_handle, layout_handle, atlas_index);
let image_node = icon.image_node();
let texture_atlas = icon.texture_atlas();
```

## Utility Functions

### `id_from_filename`

Extract an ID from a filename path.

```rust
let path = Path::new("spells/fireball.spell.ron");
let id: Option<SpellId> = id_from_filename(path, ".spell.ron");
// Returns Some(SpellId("fireball"))
```

### `is_hidden_file`

Check if a path represents a hidden or disabled file.

```rust
assert!(is_hidden_file(Path::new(".hidden.ron")));
assert!(is_hidden_file(Path::new("_disabled.ron")));
assert!(!is_hidden_file(Path::new("normal.ron")));
```

### `deserialize_optional_string`

Serde helper for optional string fields.

```rust
#[derive(Deserialize)]
struct MyData {
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    atlas_slice: Option<String>,
}
```

## Integration with `msg_interned_id`

This crate works well with `msg_interned_id` for efficient ID types:

```rust
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
| 0.2               | 0.17 |
| 0.1               | 0.16 |

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Contributing

Contributions are welcome! This crate is part of the [MolecularSadism](https://github.com/MolecularSadism) game development libraries.
