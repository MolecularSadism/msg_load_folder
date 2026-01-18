//! # msg_load_folder
//!
//! Generic plugin-based folder loading infrastructure for Bevy games.
//!
//! This crate provides a plugin that automatically discovers and loads assets from folders,
//! creating a library resource indexed by IDs derived from filenames.
//!
//! ## Quick Start
//!
//! ```rust
//! use msg_load_folder::prelude::*;
//! use bevy::prelude::*;
//! use serde::Deserialize;
//!
//! // 1. Define your asset type
//! #[derive(Asset, Clone, Reflect, Deserialize)]
//! struct Spell {
//!     name: String,
//!     damage: f32,
//! }
//!
//! // 2. Define your ID type (implement required traits)
//! #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
//! struct SpellId(u64);
//!
//! impl From<String> for SpellId {
//!     fn from(s: String) -> Self {
//!         // In practice, use interned strings for efficiency
//!         SpellId(s.len() as u64)
//!     }
//! }
//!
//! // 3. Add the plugin
//! fn build_app(app: &mut App) {
//!     app.add_plugins(FolderLoaderPlugin::<SpellId, Spell>::new(
//!         "prefabs/spells",
//!         ".spell.ron",
//!     ));
//! }
//!
//! // 4. Use the library
//! fn use_spells(
//!     library: Res<AssetFolder<SpellId, Spell>>,
//!     assets: Res<Assets<Spell>>,
//! ) {
//!     for (id, handle) in library.iter() {
//!         if let Some(spell) = assets.get(handle) {
//!             info!("Spell: {}", spell.name);
//!         }
//!     }
//! }
//! ```

use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::path::Path;

use bevy::asset::LoadedFolder;
use bevy::prelude::*;

pub mod prelude {
    pub use crate::{
        AssetFolder, AssetFolderHandle, AtlasIcon, FolderLoaderPlugin, deserialize_optional_string,
        id_from_filename, is_hidden_file,
    };
}

// =============================================================================
// FolderLoaderPlugin
// =============================================================================

/// Plugin that sets up automatic folder-based asset loading.
///
/// Creates the necessary resources and systems to:
/// 1. Load all assets from a folder matching the specified extension
/// 2. Derive IDs from filenames
/// 3. Store handles in a `AssetFolder<Id, Asset>` resource
///
/// # Type Parameters
///
/// * `Id` - The ID type (must implement required traits including `From<String>`)
/// * `A` - The asset type (must implement `Asset + Clone`)
///
/// # Example
///
/// ```rust
/// # use msg_load_folder::prelude::*;
/// # use bevy::prelude::*;
/// # use serde::Deserialize;
/// # #[derive(Asset, Clone, Reflect, Deserialize)]
/// # struct Spell { name: String }
/// # #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
/// # struct SpellId(u64);
/// # impl From<String> for SpellId { fn from(s: String) -> Self { SpellId(s.len() as u64) } }
/// # fn example(app: &mut App) {
/// app.add_plugins(FolderLoaderPlugin::<SpellId, Spell>::new(
///     "prefabs/spells",
///     ".spell.ron",
/// ));
/// # }
///
/// fn my_system(library: Res<AssetFolder<SpellId, Spell>>) {
///     for id in library.keys() {
///         // ...
///     }
/// }
/// ```
pub struct FolderLoaderPlugin<Id, A>
where
    Id: Clone + Copy + Eq + Hash + Send + Sync + Default + From<String> + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    folder_path: &'static str,
    file_extension: &'static str,
    _marker: PhantomData<(Id, A)>,
}

impl<Id, A> FolderLoaderPlugin<Id, A>
where
    Id: Clone + Copy + Eq + Hash + Send + Sync + Default + From<String> + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    /// Creates a new folder loader plugin.
    ///
    /// # Arguments
    ///
    /// * `folder_path` - Path to the assets folder relative to assets directory
    ///   (e.g., "prefabs/spells")
    /// * `file_extension` - File extension to filter, including the dot
    ///   (e.g., ".spell.ron")
    #[must_use]
    pub fn new(folder_path: &'static str, file_extension: &'static str) -> Self {
        Self {
            folder_path,
            file_extension,
            _marker: PhantomData,
        }
    }
}

impl<Id, A> Plugin for FolderLoaderPlugin<Id, A>
where
    Id: Clone + Copy + Eq + Hash + Send + Sync + Default + From<String> + std::fmt::Debug + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    fn build(&self, app: &mut App) {
        // Store config in a resource
        app.insert_resource(FolderLoaderConfig::<Id, A> {
            folder_path: self.folder_path,
            file_extension: self.file_extension,
            _marker: PhantomData,
        });

        // Initialize resources
        app.init_resource::<AssetFolderHandle<A>>();
        app.init_resource::<AssetFolder<Id, A>>();

        // Add the loading system
        app.add_systems(Update, load_assets_from_folder::<Id, A>);
    }
}

/// Configuration resource for folder loading.
#[derive(Resource)]
struct FolderLoaderConfig<Id, A>
where
    Id: Clone + Copy + Eq + Hash + Send + Sync + Default + From<String> + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    folder_path: &'static str,
    file_extension: &'static str,
    _marker: PhantomData<(Id, A)>,
}

// =============================================================================
// AssetFolderHandle Resource
// =============================================================================

/// Resource tracking folder load state for an asset type.
///
/// Generic over a marker type `A` to allow multiple folder handles
/// for different asset types (spells, perks, actors, etc.).
#[derive(Resource, Reflect)]
#[reflect(Resource)]
pub struct AssetFolderHandle<A: Send + Sync + 'static> {
    /// Handle to the loaded folder.
    pub handle: Option<Handle<LoadedFolder>>,
    /// Whether the folder has been fully processed.
    pub loaded: bool,
    /// Paths of assets that failed to load (to avoid retrying).
    #[reflect(ignore)]
    pub failed_paths: Vec<String>,
    #[reflect(ignore)]
    _marker: PhantomData<A>,
}

impl<A: Send + Sync + 'static> Default for AssetFolderHandle<A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<A: Send + Sync + 'static> AssetFolderHandle<A> {
    /// Create a new folder handle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            handle: None,
            loaded: false,
            failed_paths: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Check if the folder has started loading.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.handle.is_some() && !self.loaded
    }

    /// Check if the folder has finished loading.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }
}

// =============================================================================
// AssetFolder Resource
// =============================================================================

/// Generic library resource for assets loaded from folders.
///
/// Maps asset IDs to their handles, providing convenient access methods.
/// This is the main resource created by `FolderLoaderPlugin`.
///
/// # Type Parameters
///
/// * `Id` - The ID type (e.g., SpellId, PerkId)
/// * `A` - The asset type (e.g., Spell, PerkData)
///
/// # Example
///
/// ```rust
/// # use msg_load_folder::prelude::*;
/// # use bevy::prelude::*;
/// # use serde::Deserialize;
/// # #[derive(Asset, Clone, Reflect, Deserialize)]
/// # struct Spell { name: String }
/// # #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
/// # struct SpellId(u64);
/// # impl From<String> for SpellId { fn from(s: String) -> Self { SpellId(s.len() as u64) } }
/// fn my_system(
///     library: Res<AssetFolder<SpellId, Spell>>,
///     assets: Res<Assets<Spell>>,
/// ) {
///     let spell_id = SpellId::default();
///     if let Some(handle) = library.get(spell_id) {
///         if let Some(spell) = assets.get(handle) {
///             info!("Found spell: {}", spell.name);
///         }
///     }
/// }
/// ```
#[derive(Resource, Clone, Reflect, Deref, DerefMut)]
pub struct AssetFolder<Id, A>
where
    Id: Clone + Copy + Eq + Hash + Send + Sync + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    /// Asset handles indexed by ID.
    #[reflect(ignore)]
    assets: HashMap<Id, Handle<A>>,
}

// Manual Default implementation that doesn't require A: Default
impl<Id, A> Default for AssetFolder<Id, A>
where
    Id: Clone + Copy + Eq + Hash + Send + Sync + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<Id, A> AssetFolder<Id, A>
where
    Id: Clone + Copy + Eq + Hash + Send + Sync + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    /// Create a new empty library.
    #[must_use]
    pub fn new() -> Self {
        Self {
            assets: HashMap::new(),
        }
    }

    /// Get handle for an ID.
    #[must_use]
    pub fn get(&self, id: Id) -> Option<&Handle<A>> {
        self.assets.get(&id)
    }

    /// Get mutable handle for an ID.
    #[must_use]
    pub fn get_mut(&mut self, id: Id) -> Option<&mut Handle<A>> {
        self.assets.get_mut(&id)
    }

    /// Insert a handle for an ID.
    pub fn insert(&mut self, id: Id, handle: Handle<A>) -> Option<Handle<A>> {
        self.assets.insert(id, handle)
    }

    /// Check if the library contains an ID.
    #[must_use]
    pub fn contains(&self, id: Id) -> bool {
        self.assets.contains_key(&id)
    }

    /// Check if any assets have been loaded.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        !self.assets.is_empty()
    }

    /// Get all known IDs.
    pub fn keys(&self) -> impl Iterator<Item = Id> + '_ {
        self.assets.keys().copied()
    }

    /// Returns the number of loaded assets.
    #[must_use]
    pub fn len(&self) -> usize {
        self.assets.len()
    }

    /// Returns `true` if no assets are loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// Returns an iterator over all IDs and their handles.
    pub fn iter(&self) -> impl Iterator<Item = (Id, &Handle<A>)> + '_ {
        self.assets.iter().map(|(id, h)| (*id, h))
    }

    /// Returns a mutable iterator over all IDs and their handles.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Id, &mut Handle<A>)> + '_ {
        self.assets.iter_mut().map(|(id, h)| (*id, h))
    }

    /// Direct access to underlying HashMap.
    #[must_use]
    pub fn assets(&self) -> &HashMap<Id, Handle<A>> {
        &self.assets
    }

    /// Mutable access to underlying HashMap.
    #[must_use]
    pub fn assets_mut(&mut self) -> &mut HashMap<Id, Handle<A>> {
        &mut self.assets
    }
}

// =============================================================================
// Loading System
// =============================================================================

/// Generic system that loads assets from folders.
fn load_assets_from_folder<Id, A>(
    asset_server: Res<AssetServer>,
    config: Res<FolderLoaderConfig<Id, A>>,
    mut folder_handle: ResMut<AssetFolderHandle<A>>,
    loaded_folders: Res<Assets<LoadedFolder>>,
    mut library: ResMut<AssetFolder<Id, A>>,
    data_assets: Res<Assets<A>>,
) where
    Id: Clone + Copy + Eq + Hash + Send + Sync + Default + From<String> + std::fmt::Debug + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    // Start loading the folder if we haven't yet
    if folder_handle.handle.is_none() {
        folder_handle.handle = Some(asset_server.load_folder(config.folder_path));
        return;
    }

    // Skip if already processed
    if folder_handle.loaded {
        return;
    }

    // Wait for folder to be loaded
    let Some(folder_handle_ref) = &folder_handle.handle else {
        return;
    };
    let Some(folder) = loaded_folders.get(folder_handle_ref) else {
        return;
    };

    let mut pending_assets = 0;
    let mut loaded_count = 0;

    // Process each file in the folder
    for handle in &folder.handles {
        let Some(path) = handle.path() else {
            continue;
        };

        let path_str = path.path().to_string_lossy().to_string();

        // Extract ID from filename
        let Some(id) = id_from_filename_with_extension::<Id>(path.path(), config.file_extension)
        else {
            continue;
        };

        // Skip if already registered
        if library.contains(id) {
            loaded_count += 1;
            continue;
        }

        // Skip if already marked as failed
        if folder_handle.failed_paths.contains(&path_str) {
            continue;
        }

        // Get typed handle
        let typed_handle: Handle<A> = handle.clone().typed();

        // Check asset loading state
        let load_state = asset_server.get_load_state(&typed_handle);

        use bevy::asset::LoadState;
        match load_state {
            Some(LoadState::Loaded) => {
                // Check if data is actually available
                if data_assets.get(&typed_handle).is_some() {
                    // Register in library
                    library.insert(id, typed_handle);
                    loaded_count += 1;

                    debug!(
                        "Loaded asset from folder: {:?} ({})",
                        id,
                        path.path().display()
                    );
                } else {
                    // Data not available yet, wait
                    pending_assets += 1;
                }
            }
            Some(LoadState::Failed(_)) => {
                // Mark as failed to avoid retrying
                folder_handle.failed_paths.push(path_str);
                warn!(
                    "Asset failed to load and will be skipped: {} (ID: {:?})",
                    path.path().display(),
                    id
                );
            }
            Some(LoadState::Loading) | None => {
                // Still loading
                pending_assets += 1;
            }
            Some(LoadState::NotLoaded) => {
                // Not started loading yet
                pending_assets += 1;
            }
        }
    }

    // Mark as loaded only if no assets are still pending
    if pending_assets == 0 {
        folder_handle.loaded = true;

        let total_discovered = loaded_count + folder_handle.failed_paths.len();
        if folder_handle.failed_paths.is_empty() {
            info!(
                "Loaded {} assets from folder '{}'",
                loaded_count, config.folder_path
            );
        } else {
            warn!(
                "Loaded {} of {} assets from folder '{}' ({} failed)",
                loaded_count,
                total_discovered,
                config.folder_path,
                folder_handle.failed_paths.len()
            );
        }
    }
}

// =============================================================================
// ID Extraction Utilities
// =============================================================================

/// Extracts an ID from a filename by stripping the extension.
///
/// # Arguments
///
/// * `path` - The full path to the asset file
/// * `extension` - The extension to strip (e.g., ".spell.ron")
///
/// # Returns
///
/// The ID if the filename matches the extension and is valid,
/// or `None` if:
/// - The file doesn't have the expected extension
/// - The filename starts with `.` (hidden file)
/// - The filename starts with `_` (disabled file)
pub fn id_from_filename_with_extension<Id>(path: &Path, extension: &str) -> Option<Id>
where
    Id: From<String>,
{
    let filename = path.file_name()?.to_string_lossy();

    // Check if filename has the expected extension
    if !filename.ends_with(extension) {
        return None;
    }

    // Strip extension to get the ID string
    let id_str = filename.strip_suffix(extension)?;

    // Skip hidden files (starting with .)
    if id_str.starts_with('.') {
        return None;
    }

    // Skip disabled files (starting with _)
    if id_str.starts_with('_') {
        return None;
    }

    // Skip empty IDs
    if id_str.is_empty() {
        return None;
    }

    Some(Id::from(id_str.to_string()))
}

/// Legacy function for backwards compatibility.
/// Extracts an ID from a filename using extension from path itself.
pub fn id_from_filename<Id>(path: &Path, extension: &str) -> Option<Id>
where
    Id: From<String>,
{
    id_from_filename_with_extension(path, extension)
}

/// Check if a path represents a hidden or disabled file.
#[must_use]
pub fn is_hidden_file(path: &Path) -> bool {
    path.file_name()
        .map(|name| {
            let name_str = name.to_string_lossy();
            name_str.starts_with('.') || name_str.starts_with('_')
        })
        .unwrap_or(false)
}

// =============================================================================
// AtlasIcon
// =============================================================================

/// Icon rendering data from a texture atlas slice.
///
/// Contains all the handles and indices needed to render an icon from
/// an atlas-based spritesheet.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AtlasIcon {
    /// The atlas image handle.
    pub image: Handle<Image>,
    /// The texture atlas layout handle.
    pub layout: Handle<TextureAtlasLayout>,
    /// The atlas index for this icon's slice.
    pub atlas_index: usize,
}

impl AtlasIcon {
    /// Creates a new AtlasIcon.
    #[must_use]
    pub fn new(
        image: Handle<Image>,
        layout: Handle<TextureAtlasLayout>,
        atlas_index: usize,
    ) -> Self {
        Self {
            image,
            layout,
            atlas_index,
        }
    }

    /// Returns a clone of the underlying image handle for UI usage.
    #[must_use]
    pub fn get_image(&self) -> Handle<Image> {
        self.image.clone()
    }

    /// Returns the texture atlas configuration for this icon.
    #[must_use]
    pub fn texture_atlas(&self) -> TextureAtlas {
        TextureAtlas {
            layout: self.layout.clone(),
            index: self.atlas_index,
        }
    }

    /// Creates an ImageNode from this icon.
    #[must_use]
    pub fn image_node(&self) -> ImageNode {
        ImageNode::from_atlas_image(self.image.clone(), self.texture_atlas())
    }
}

// =============================================================================
// Parsing Utilities
// =============================================================================

/// Deserializes a string field to `Option<String>`.
/// Accepts a bare string and converts empty strings to `None`.
///
/// # Example
///
/// ```rust
/// use serde::Deserialize;
/// use msg_load_folder::deserialize_optional_string;
///
/// #[derive(Deserialize)]
/// struct MyData {
///     #[serde(default, deserialize_with = "deserialize_optional_string")]
///     atlas_slice: Option<String>,
/// }
/// ```
pub fn deserialize_optional_string<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    let s = String::deserialize(deserializer)?;
    Ok(if s.is_empty() { None } else { Some(s) })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Mock ID type for testing
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
    struct MockId(u64);

    impl From<String> for MockId {
        fn from(s: String) -> Self {
            MockId(s.len() as u64)
        }
    }

    #[test]
    fn test_id_from_filename_valid() {
        let path = Path::new("test_item.mock.ron");
        let id: Option<MockId> = id_from_filename_with_extension(path, ".mock.ron");
        assert!(id.is_some());
    }

    #[test]
    fn test_id_from_filename_hidden() {
        let path = Path::new(".hidden.mock.ron");
        let id: Option<MockId> = id_from_filename_with_extension(path, ".mock.ron");
        assert!(id.is_none());
    }

    #[test]
    fn test_id_from_filename_disabled() {
        let path = Path::new("_disabled.mock.ron");
        let id: Option<MockId> = id_from_filename_with_extension(path, ".mock.ron");
        assert!(id.is_none());
    }

    #[test]
    fn test_id_from_filename_wrong_extension() {
        let path = Path::new("test_item.other.ron");
        let id: Option<MockId> = id_from_filename_with_extension(path, ".mock.ron");
        assert!(id.is_none());
    }

    #[test]
    fn test_is_hidden_file() {
        assert!(is_hidden_file(Path::new(".hidden.ron")));
        assert!(is_hidden_file(Path::new("_disabled.ron")));
        assert!(!is_hidden_file(Path::new("normal.ron")));
    }

    #[test]
    fn test_asset_folder_handle_states() {
        // Mock asset type for testing
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut handle: AssetFolderHandle<MockAsset> = AssetFolderHandle::new();

        // Initial state
        assert!(!handle.is_loading());
        assert!(!handle.is_loaded());

        // After starting load
        handle.handle = Some(Handle::default());
        assert!(handle.is_loading());
        assert!(!handle.is_loaded());

        // After load complete
        handle.loaded = true;
        assert!(!handle.is_loading());
        assert!(handle.is_loaded());
    }

    #[test]
    fn test_folder_asset_library() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut library: AssetFolder<MockId, MockAsset> = AssetFolder::new();

        assert!(library.is_empty());
        assert_eq!(library.len(), 0);
        assert!(!library.is_ready());

        let id = MockId(1);
        library.insert(id, Handle::default());

        assert!(!library.is_empty());
        assert_eq!(library.len(), 1);
        assert!(library.is_ready());
        assert!(library.contains(id));
        assert!(library.get(id).is_some());

        let keys: Vec<_> = library.keys().collect();
        assert_eq!(keys.len(), 1);

        let iter_count = library.iter().count();
        assert_eq!(iter_count, 1);
    }

    #[test]
    fn test_atlas_icon() {
        let icon = AtlasIcon::new(Handle::default(), Handle::default(), 5);

        assert_eq!(icon.atlas_index, 5);

        let atlas = icon.texture_atlas();
        assert_eq!(atlas.index, 5);
    }
}
