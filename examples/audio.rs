//! Example demonstrating folder-based loading with multiple file extensions.
//!
//! This example shows how to use `with_extension()` to load a folder containing
//! assets with different file formats (e.g., mixed `.ogg` and `.wav` audio files).
//!
//! The `with_extension()` builder method allows chaining additional extensions
//! onto any `FolderLoaderPlugin`, so the same folder can contain assets in
//! multiple formats while all being indexed by a single ID type.
//!
//! Run with: `cargo run --example audio`

use bevy::{asset::LoadState, log::LogPlugin, prelude::*};
use bevy_common_assets::ron::RonAssetPlugin;
use msg_load_folder::prelude::*;
use serde::Deserialize;

// =============================================================================
// Asset Definition
// =============================================================================

/// A sound effect descriptor loaded from RON files.
/// In a real project you would use Bevy's AudioSource directly with .ogg/.wav,
/// but this example uses RON to stay self-contained without binary assets.
#[derive(Asset, Clone, Reflect, Deserialize, Debug)]
pub struct SoundEffect {
    pub name: String,
    pub volume: f32,
}

// =============================================================================
// ID Type
// =============================================================================

/// A unique identifier for sounds, derived from filenames.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct SoundId(&'static str);

impl From<String> for SoundId {
    fn from(s: String) -> Self {
        SoundId(Box::leak(s.into_boxed_str()))
    }
}

impl std::fmt::Display for SoundId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// =============================================================================
// Application State
// =============================================================================

#[derive(Resource, Default)]
struct DisplayedSounds(bool);

// =============================================================================
// Main Application
// =============================================================================

fn main() {
    App::new()
        .add_plugins(MinimalPlugins)
        .add_plugins(LogPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: "assets".to_string(),
            // This example is a one-shot that exits after loading; no watching.
            watch_for_changes_override: Some(false),
            ..default()
        })
        // Register loaders for both extensions
        .add_plugins(RonAssetPlugin::<SoundEffect>::new(&["sfx.ron", "sound.ron"]))
        // Load the sounds folder, accepting both .sfx.ron and .sound.ron files
        // This demonstrates the with_extension() builder pattern:
        .add_plugins(
            FolderLoaderPlugin::<SoundId, SoundEffect>::new("sounds", ".sfx.ron")
                .with_extension(".sound.ron"),
        )
        .init_resource::<DisplayedSounds>()
        .add_systems(Update, display_sounds)
        .run();
}

/// System that displays loaded sounds once loading is complete, then exits.
fn display_sounds(
    asset_server: Res<AssetServer>,
    folder_handle: Res<AssetFolderHandle<SoundId, SoundEffect>>,
    sound_library: Res<AssetFolder<SoundId, SoundEffect>>,
    sound_assets: Res<Assets<SoundEffect>>,
    mut displayed: ResMut<DisplayedSounds>,
    mut app_exit: MessageWriter<AppExit>,
) {
    if displayed.0 || !folder_handle.is_loaded() {
        return;
    }
    // `is_loaded()` only signals that the folder was scanned; wait for the
    // sound assets themselves to finish loading before reporting.
    let all_settled = sound_library.iter().all(|(_, handle)| {
        matches!(
            asset_server.load_state(handle.id()),
            LoadState::Loaded | LoadState::Failed(_)
        )
    });
    if !all_settled {
        return;
    }
    displayed.0 = true;

    info!("=== Loaded Sounds ===");
    info!("Total sounds loaded: {}", sound_library.len());

    for (id, handle) in sound_library.iter() {
        if let Some(sound) = sound_assets.get(handle) {
            info!("Sound: {} | name: {} | volume: {}", id, sound.name, sound.volume);
        }
    }

    info!("=====================");
    app_exit.write(AppExit::Success);
}
