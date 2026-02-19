//! Basic example demonstrating folder-based asset loading with msg_load_folder.
//!
//! This example shows how to:
//! 1. Define a custom asset type
//! 2. Define an ID type for asset lookup
//! 3. Configure the FolderLoaderPlugin
//! 4. Access loaded assets in systems
//!
//! Run with: `cargo run --example basic`

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use msg_load_folder::prelude::*;
use serde::Deserialize;

// =============================================================================
// Asset Definition
// =============================================================================

/// A spell asset loaded from RON files.
///
/// Each spell is defined in a `.spell.ron` file with this structure:
/// ```ron
/// (
///     name: "Fireball",
///     damage: 50.0,
///     mana_cost: 25,
///     description: "Hurls a ball of fire at the target",
/// )
/// ```
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
        // In a real application, you would use string interning (e.g., bevy's intern crate)
        // For this example, we leak the string to get a static reference
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
        // Add default Bevy plugins (including asset loading)
        .add_plugins(DefaultPlugins.set(AssetPlugin {
            // Configure asset folder relative to the project root
            file_path: "assets".to_string(),
            ..default()
        }))
        // Register the RON asset loader for .spell.ron files
        .add_plugins(RonAssetPlugin::<Spell>::new(&["spell.ron"]))
        // Add the folder loader plugin for spells
        // This will automatically load all `.spell.ron` files from `assets/spells/`
        .add_plugins(FolderLoaderPlugin::<SpellId, Spell>::new(
            "spells",
            ".spell.ron",
        ))
        // Initialize our display tracking resource
        .init_resource::<DisplayedSpells>()
        // Add systems
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
fn check_loading_status(folder_handle: Res<AssetFolderHandle<Spell>>) {
    if folder_handle.is_changed() {
        if folder_handle.is_loading() {
            info!("Loading spells from folder...");
        } else if folder_handle.is_loaded() {
            info!("Spell loading complete!");

            if !folder_handle.failed_paths.is_empty() {
                warn!(
                    "Some spells failed to load: {:?}",
                    folder_handle.failed_paths
                );
            }
        }
    }
}

/// System that displays loaded spells once loading is complete.
fn display_spells(
    folder_handle: Res<AssetFolderHandle<Spell>>,
    spell_library: Res<AssetFolder<SpellId, Spell>>,
    spell_assets: Res<Assets<Spell>>,
    mut displayed: ResMut<DisplayedSpells>,
) {
    // Only display once after loading completes
    if !folder_handle.is_loaded() || displayed.0 {
        return;
    }
    displayed.0 = true;

    info!("=== Loaded Spells ===");
    info!("Total spells loaded: {}", spell_library.len());

    // Iterate through all loaded spells
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
        }
    }

    info!("=====================");

    // Example: Access a specific spell by ID
    // In a real game, you might look up spells when casting them
    for (id, handle) in spell_library.iter() {
        if let Some(spell) = spell_assets.get(handle) {
            if spell.name == "Fireball" {
                info!("Found Fireball spell with ID: {}", id);
            }
        }
    }
}
