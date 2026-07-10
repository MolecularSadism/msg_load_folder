//! Basic example demonstrating folder-based asset loading with msg_load_folder.
//!
//! This example shows how to:
//! 1. Define a custom asset type
//! 2. Define an ID type for asset lookup
//! 3. Configure the `FolderLoaderPlugin`
//! 4. Access loaded assets in systems
//!
//! The `assets/spells/` folder deliberately includes a malformed file
//! (`broken.spell.ron`). Notice that it does not prevent the other spells from
//! loading — that is the resilient loading behavior in action.
//!
//! Runs headless (no window) and exits after loading completes.
//!
//! Run with: `cargo run --example ron`

use bevy::{asset::LoadState, log::LogPlugin, prelude::*};
use bevy_common_assets::ron::RonAssetPlugin;
use msg_load_folder::prelude::*;
use serde::Deserialize;

// =============================================================================
// Asset Definition
// =============================================================================

/// A spell asset loaded from RON files.
#[derive(Asset, Clone, Reflect, Deserialize, Debug)]
pub struct Spell {
    pub name: String,
    pub damage: f32,
    pub mana_cost: u32,
    #[serde(default)]
    pub description: String,
}

// =============================================================================
// ID Type
// =============================================================================

/// A unique identifier for spells, derived from filenames.
///
/// For example, `fireball.spell.ron` becomes `SpellId("fireball")`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct SpellId(&'static str);

impl From<String> for SpellId {
    fn from(s: String) -> Self {
        SpellId(Box::leak(s.into_boxed_str()))
    }
}

impl std::fmt::Display for SpellId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// =============================================================================
// Application State
// =============================================================================

/// Tracks whether we've displayed the loaded spells.
#[derive(Resource, Default)]
struct DisplayedSpells(bool);

// =============================================================================
// Main Application
// =============================================================================

fn main() {
    App::new()
        // Headless: use MinimalPlugins + LogPlugin + AssetPlugin instead of DefaultPlugins
        .add_plugins(MinimalPlugins)
        .add_plugins(LogPlugin::default())
        .add_plugins(AssetPlugin {
            file_path: "assets".to_string(),
            // This example is a one-shot that exits after loading; no watching.
            watch_for_changes_override: Some(false),
            ..default()
        })
        // Register the RON asset loader for .spell.ron files
        .add_plugins(RonAssetPlugin::<Spell>::new(&["spell.ron"]))
        // Add the folder loader plugin for spells
        .add_plugins(FolderLoaderPlugin::<SpellId, Spell>::new(
            "spells",
            ".spell.ron",
        ))
        .init_resource::<DisplayedSpells>()
        .add_systems(Startup, setup)
        .add_systems(Update, (check_loading_status, display_spells).chain())
        .run();
}

/// Setup system - runs once at startup.
fn setup() {
    info!("Starting spell loading example...");
    info!("Looking for .spell.ron files in assets/spells/");
}

/// System that checks and reports loading status.
fn check_loading_status(folder_handle: Res<AssetFolderHandle<SpellId, Spell>>) {
    if folder_handle.is_changed() && folder_handle.is_loaded() {
        info!("Spell folder processed!");
    }
}

/// System that displays loaded spells once loading is complete, then exits.
fn display_spells(
    asset_server: Res<AssetServer>,
    folder_handle: Res<AssetFolderHandle<SpellId, Spell>>,
    spell_library: Res<AssetFolder<SpellId, Spell>>,
    spell_assets: Res<Assets<Spell>>,
    mut displayed: ResMut<DisplayedSpells>,
    mut app_exit: MessageWriter<AppExit>,
) {
    if displayed.0 || !folder_handle.is_loaded() {
        return;
    }
    // `is_loaded()` means the folder has been scanned and its handles
    // registered; the asset data itself still loads asynchronously. Wait until
    // every discovered spell has settled (loaded — or failed, like the
    // deliberately broken file) so we report a complete picture.
    let all_settled = spell_library.iter().all(|(_, handle)| {
        matches!(
            asset_server.load_state(handle.id()),
            LoadState::Loaded | LoadState::Failed(_)
        )
    });
    if !all_settled {
        return;
    }
    displayed.0 = true;

    info!("=== Loaded Spells ===");
    info!("Registered spell entries: {}", spell_library.len());

    let mut failed = Vec::new();
    for (id, handle) in spell_library.iter() {
        if let Some(spell) = spell_assets.get(handle) {
            info!("---");
            info!("ID: {}", id);
            info!("Name: {}", spell.name);
            info!("Damage: {}", spell.damage);
            info!("Mana Cost: {}", spell.mana_cost);
            if !spell.description.is_empty() {
                info!("Description: {}", spell.description);
            }
        } else {
            // Registered, but the asset data isn't available — e.g. a malformed
            // file. The folder still loaded everything else just fine.
            failed.push(id);
        }
    }

    info!("=====================");
    if !failed.is_empty() {
        warn!(
            "{} spell(s) failed to load and were skipped gracefully: {:?}",
            failed.len(),
            failed
        );
    }

    // Example: Access a specific spell by ID
    for (id, handle) in spell_library.iter() {
        if let Some(spell) = spell_assets.get(handle)
            && spell.name == "Fireball"
        {
            info!("Found Fireball spell with ID: {}", id);
        }
    }

    // Exit after displaying results
    app_exit.write(AppExit::Success);
}
