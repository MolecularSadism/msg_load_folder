//! # msg_load_folder
//!
//! Generic plugin-based folder loading infrastructure for Bevy games.
//!
//! This crate provides a plugin that automatically discovers and loads assets from folders,
//! creating a library resource indexed by IDs derived from filenames. It works with any
//! asset type — config files (RON, JSON), audio (OGG, WAV, MP3), textures, and more.
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
//!
//! ## Multiple File Extensions
//!
//! For folders with mixed formats, chain [`FolderLoaderPlugin::with_extension`]:
//!
//! ```rust,ignore
//! app.add_plugins(
//!     FolderLoaderPlugin::<SoundId, AudioSource>::new("sounds", ".ogg")
//!         .with_extension(".wav")
//!         .with_extension(".mp3"),
//! );
//! ```
//!
//! ## Resilience & Hot Reloading
//!
//! Files are discovered by scanning the folder and then loaded *individually*,
//! so the loader degrades gracefully: if a single file is malformed (for
//! example a `.ron` file with a syntax or semantic error), only that one entry
//! is affected — every other asset in the folder still loads. The broken file's
//! ID is simply absent from the library until the file is fixed.
//!
//! When Bevy's asset watching is enabled the library also hot reloads:
//!
//! * **Editing** a file reloads its asset in place (the handle is stable, so
//!   existing references keep working). This is also how a previously-broken
//!   file recovers — fix it and it loads on the next save.
//! * **Adding or removing** a file is picked up automatically and the library
//!   is updated to match. Untouched entries keep their exact handle, so a
//!   structural change never invalidates references you hold to other assets.
//! * **Re-adding** a file at a previously-removed path loads its *fresh*
//!   contents, never a stale cached copy — a remove-then-re-add cycle always
//!   reflects what is currently on disk.
//!
//! Hot reloading requires the [`AssetServer`] to be watching for changes, which
//! you opt into when adding Bevy's `AssetPlugin`:
//!
//! ```rust,ignore
//! app.add_plugins(DefaultPlugins.set(AssetPlugin {
//!     watch_for_changes_override: Some(true),
//!     ..default()
//! }));
//! ```
//!
//! Without watching, the folder is still scanned and loaded once (and remains
//! resilient to malformed files); it simply won't react to later changes.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use bevy::asset::io::ErasedAssetReader;
use bevy::asset::{AssetPath, LoadState, LoadedFolder};
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task, block_on, futures_lite::StreamExt, poll_once};

pub mod prelude {
    pub use crate::{
        AssetFolder, AssetFolderHandle, AtlasIcon, FolderLoaderPlugin, deserialize_optional_string,
        id_from_filename, id_from_filename_with_extensions, is_hidden_file,
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
    file_extensions: Vec<&'static str>,
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
            file_extensions: vec![file_extension],
            _marker: PhantomData,
        }
    }

    /// Adds an additional file extension to match.
    ///
    /// Use this to load folders containing assets with multiple file formats.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use msg_load_folder::prelude::*;
    /// # use bevy::prelude::*;
    /// # use serde::Deserialize;
    /// # #[derive(Asset, Clone, Reflect, Deserialize)]
    /// # struct Sound { name: String }
    /// # #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
    /// # struct SoundId(u64);
    /// # impl From<String> for SoundId { fn from(s: String) -> Self { SoundId(s.len() as u64) } }
    /// # fn example(app: &mut App) {
    /// app.add_plugins(
    ///     FolderLoaderPlugin::<SoundId, Sound>::new("sounds", ".sfx.ron")
    ///         .with_extension(".sound.ron"),
    /// );
    /// # }
    /// ```
    #[must_use]
    pub fn with_extension(mut self, extension: &'static str) -> Self {
        self.file_extensions.push(extension);
        self
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
            file_extensions: self.file_extensions.clone(),
            _marker: PhantomData,
        });

        // Initialize the asset storage, but only once per asset type `A`.
        //
        // `init_asset::<A>()` is NOT idempotent: internally it calls
        // `insert_resource(Assets::<A>::default())`, which *replaces* any
        // existing `Assets<A>` collection and drops every handle already loaded
        // into it. When multiple `FolderLoaderPlugin`s share the same asset
        // type `A` (e.g. two folders of `Image`s keyed by different `Id`s), or
        // when another plugin has already loaded assets of type `A`, a second
        // unconditional `init_asset::<A>()` would wipe the previously loaded
        // assets — leaving lookups silently returning `None`.
        //
        // Guarding on `Assets<A>` existence makes the call idempotent: the
        // first plugin to use asset type `A` sets up its storage (and the
        // associated events/systems), and every subsequent `FolderLoaderPlugin`
        // reuses that same collection instead of clobbering it.
        if !app.world().contains_resource::<Assets<A>>() {
            app.init_asset::<A>();
        }
        app.init_resource::<AssetFolderHandle<Id, A>>();
        app.init_resource::<AssetFolder<Id, A>>();
        app.init_resource::<FolderScanState<Id, A>>();

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
    file_extensions: Vec<&'static str>,
    _marker: PhantomData<(Id, A)>,
}

// =============================================================================
// AssetFolderHandle Resource
// =============================================================================

/// Resource tracking folder load state for an asset type.
///
/// Generic over `Id` and `A` so that multiple [`FolderLoaderPlugin`]s using
/// the same asset type `A` but different ID types get independent folder
/// handles, avoiding collisions.
#[derive(Resource, Reflect)]
#[reflect(Resource)]
pub struct AssetFolderHandle<Id: Send + Sync + 'static, A: Send + Sync + 'static> {
    /// Handle to the watched [`LoadedFolder`].
    ///
    /// Only populated when the [`AssetServer`] is watching for changes; it is
    /// kept alive purely so Bevy reloads the folder (and emits an
    /// [`AssetEvent<LoadedFolder>`]) when files are added, removed or moved,
    /// which drives hot reloading.
    pub handle: Option<Handle<LoadedFolder>>,
    /// Whether the folder has been scanned and its assets registered at least once.
    initial_load_complete: bool,
    #[reflect(ignore)]
    _marker: PhantomData<(Id, A)>,
}

impl<Id: Send + Sync + 'static, A: Send + Sync + 'static> Default for AssetFolderHandle<Id, A> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Id: Send + Sync + 'static, A: Send + Sync + 'static> AssetFolderHandle<Id, A> {
    /// Create a new folder handle.
    #[must_use]
    pub fn new() -> Self {
        Self {
            handle: None,
            initial_load_complete: false,
            _marker: PhantomData,
        }
    }

    /// Returns `true` once the folder has been scanned and its assets
    /// registered at least once.
    ///
    /// Note that the library keeps reacting to changes after this returns
    /// `true`: files that are edited, added or removed are picked up on the
    /// fly when asset watching is enabled.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.initial_load_complete
    }
}

// =============================================================================
// FolderScanState (internal)
// =============================================================================

/// Internal, per-loader bookkeeping for the resilient folder scan.
///
/// Kept separate from [`AssetFolderHandle`] so the public, reflected resource
/// stays simple and free of non-reflectable fields (the in-flight task).
#[derive(Resource)]
struct FolderScanState<Id, A>
where
    Id: Send + Sync + 'static,
    A: Send + Sync + 'static,
{
    /// Whether the loader has performed its one-time setup yet.
    initialized: bool,
    /// Whether a (re)scan of the folder has been requested but not yet started.
    rescan_requested: bool,
    /// The in-flight directory scan, if one is running.
    scan_task: Option<Task<Vec<PathBuf>>>,
    _marker: PhantomData<(Id, A)>,
}

impl<Id, A> Default for FolderScanState<Id, A>
where
    Id: Send + Sync + 'static,
    A: Send + Sync + 'static,
{
    fn default() -> Self {
        Self {
            initialized: false,
            rescan_requested: false,
            scan_task: None,
            _marker: PhantomData,
        }
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

/// Generic system that loads assets from folders, gracefully and with hot reloading.
///
/// Rather than relying on a single [`LoadedFolder`] (which Bevy fails *as a
/// whole* if even one contained file fails to load, e.g. a RON file with a
/// syntax/semantic error), this system enumerates the folder itself and loads
/// each file *individually*. A single broken file therefore only affects its
/// own entry — every other asset in the folder still loads.
///
/// Each frame it:
/// 1. Performs one-time setup: requests an initial scan and, when asset
///    watching is enabled, starts a [`LoadedFolder`] load whose change events
///    drive hot reloading.
/// 2. Reacts to [`AssetEvent<LoadedFolder>`]s (files added/removed/moved) by
///    requesting a rescan.
/// 3. Spawns an asynchronous directory scan when one is requested.
/// 4. Applies finished scans: newly seen files are loaded and registered,
///    files that disappeared are dropped.
///
/// Edits to the *contents* of an already-registered file are handled
/// automatically by Bevy: the handle is stable, so the asset behind it is
/// reloaded in place (this is also how a previously-broken file recovers once
/// it is fixed).
fn load_assets_from_folder<Id, A>(
    asset_server: Res<AssetServer>,
    config: Res<FolderLoaderConfig<Id, A>>,
    mut folder_handle: ResMut<AssetFolderHandle<Id, A>>,
    mut scan_state: ResMut<FolderScanState<Id, A>>,
    mut folder_events: MessageReader<AssetEvent<LoadedFolder>>,
    mut library: ResMut<AssetFolder<Id, A>>,
) where
    Id: Clone + Copy + Eq + Hash + Send + Sync + Default + From<String> + std::fmt::Debug + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    // 1. One-time setup: request the initial scan and, when watching is on,
    //    keep a folder handle alive so Bevy notifies us of structural changes.
    if !scan_state.initialized {
        scan_state.initialized = true;
        scan_state.rescan_requested = true;
        if asset_server.watching_for_changes() {
            folder_handle.handle = Some(asset_server.load_folder(config.folder_path));
        }
    }

    // 2. A change to the folder's structure (file added/removed/moved) reloads
    //    the LoadedFolder; treat that as a signal to rescan. Content-only edits
    //    are reloaded in place by Bevy and need no rescan.
    if let Some(watched_folder) = folder_handle.handle.as_ref().map(Handle::id) {
        for event in folder_events.read() {
            if matches!(
                event,
                AssetEvent::Added { id } | AssetEvent::Modified { id } if *id == watched_folder
            ) {
                scan_state.rescan_requested = true;
            }
        }
    }

    // 3. Kick off a scan if one is pending and none is already running.
    if scan_state.rescan_requested && scan_state.scan_task.is_none() {
        scan_state.scan_task = Some(spawn_directory_scan(
            &asset_server,
            config.folder_path,
            config.file_extensions.clone(),
        ));
        scan_state.rescan_requested = false;
    }

    // 4. Apply a finished scan.
    let finished = scan_state
        .scan_task
        .as_mut()
        .and_then(|task| block_on(poll_once(task)));
    if let Some(paths) = finished {
        scan_state.scan_task = None;
        apply_scan_results(&asset_server, &config, &mut library, paths);
        if !folder_handle.initial_load_complete {
            folder_handle.initial_load_complete = true;
            info!(
                "Loaded {} asset(s) from folder '{}'",
                library.len(),
                config.folder_path
            );
        }
    }
}

/// Spawns an asynchronous, recursive scan of `folder_path` on the IO task pool.
///
/// The scan goes through the folder's [`AssetSource`](bevy::asset::io::AssetSource)
/// reader (rather than `std::fs`) so it works with any asset source — the
/// default filesystem, custom sources, processed assets, etc. It returns the
/// paths of all files whose name ends with one of `file_extensions`.
fn spawn_directory_scan(
    asset_server: &AssetServer,
    folder_path: &'static str,
    file_extensions: Vec<&'static str>,
) -> Task<Vec<PathBuf>> {
    // `AssetServer` is cheap (Arc-backed) to clone and is needed inside the task.
    let server = asset_server.clone();
    let folder = AssetPath::parse(folder_path).into_owned();
    IoTaskPool::get().spawn(async move {
        let mut paths = Vec::new();
        let source = match server.get_source(folder.source().clone_owned()) {
            Ok(source) => source,
            Err(err) => {
                error!("FolderLoader: cannot scan '{folder}': {err}");
                return paths;
            }
        };
        scan_directory(source.reader(), folder.path(), &file_extensions, &mut paths).await;
        paths
    })
}

/// Recursively collects files under `path` whose name ends with one of
/// `file_extensions`. Errors reading individual directories are logged and
/// skipped so the rest of the tree is still scanned.
async fn scan_directory(
    reader: &dyn ErasedAssetReader,
    path: &Path,
    file_extensions: &[&str],
    out: &mut Vec<PathBuf>,
) {
    let mut entries = match reader.read_directory(path).await {
        Ok(entries) => entries,
        Err(err) => {
            warn!(
                "FolderLoader: failed to read directory '{}': {err}",
                path.display()
            );
            return;
        }
    };

    while let Some(child) = entries.next().await {
        match reader.is_directory(&child).await {
            Ok(true) => Box::pin(scan_directory(reader, &child, file_extensions, out)).await,
            Ok(false) => {
                if filename_has_extension(&child, file_extensions) {
                    out.push(child);
                }
            }
            Err(err) => warn!(
                "FolderLoader: failed to inspect '{}': {err}",
                child.display()
            ),
        }
    }
}

/// Registers the assets found by a scan, loading each file individually so a
/// single broken file cannot block the others, and dropping entries whose
/// backing files have disappeared.
fn apply_scan_results<Id, A>(
    asset_server: &AssetServer,
    config: &FolderLoaderConfig<Id, A>,
    library: &mut AssetFolder<Id, A>,
    paths: Vec<PathBuf>,
) where
    Id: Clone + Copy + Eq + Hash + Send + Sync + Default + From<String> + std::fmt::Debug + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    let source = AssetPath::parse(config.folder_path)
        .source()
        .clone_owned();

    let mut present: HashSet<Id> = HashSet::with_capacity(paths.len());
    for path in paths {
        // Authoritative filtering: skips hidden (`.`) / disabled (`_`) / empty
        // names and yields the ID only for files matching a configured extension.
        let Some(id) = id_from_filename_with_extensions::<Id>(&path, &config.file_extensions)
        else {
            continue;
        };
        present.insert(id);

        // Loading is idempotent: an already-loaded path returns its existing
        // (stable) handle, so we only need to register newly discovered files.
        if !library.contains(id) {
            let asset_path = AssetPath::from(path.clone()).with_source(source.clone());
            let handle = asset_server.load::<A>(asset_path.clone());

            // If the asset server reports this path as already settled the very
            // instant we ask to load it, the path was loaded before and `load`
            // handed back a *cached* asset rather than reading the file. This is
            // exactly the remove-then-re-add case: dropping our handle when the
            // file disappeared did not evict the cached asset, so without a
            // reload the library would keep serving the previous, now-stale
            // contents of a file that has since been rewritten on disk. Force a
            // reload so the freshly written file is picked up. A genuinely new
            // path is still `Loading` here, so this never fires on first load.
            if matches!(
                asset_server.load_state(handle.id()),
                LoadState::Loaded | LoadState::Failed(_)
            ) {
                asset_server.reload(asset_path);
                debug!("FolderLoader reloaded re-added {:?} ({})", id, path.display());
            }

            library.insert(id, handle);
            debug!("FolderLoader registered {:?} ({})", id, path.display());
        }
    }

    // Forget assets whose files were removed; dropping the handle lets Bevy
    // unload the underlying asset.
    let removed: Vec<Id> = library
        .keys()
        .filter(|id| !present.contains(id))
        .collect();
    for id in removed {
        library.assets_mut().remove(&id);
        debug!("FolderLoader removed {id:?} (file no longer present)");
    }
}

/// Cheap pre-filter: does `path`'s file name end with any of `file_extensions`?
///
/// The full hidden/disabled/empty-name filtering is applied later by
/// [`id_from_filename_with_extensions`]; this only avoids collecting obviously
/// unrelated files during the scan.
fn filename_has_extension(path: &Path, file_extensions: &[&str]) -> bool {
    path.file_name().is_some_and(|name| {
        let name = name.to_string_lossy();
        file_extensions.iter().any(|ext| name.ends_with(ext))
    })
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

/// Extracts an ID from a filename by trying multiple extensions.
///
/// Tries each extension in order and returns the first match.
/// Returns `None` if no extension matches or the file is hidden/disabled.
pub fn id_from_filename_with_extensions<Id>(path: &Path, extensions: &[&str]) -> Option<Id>
where
    Id: From<String>,
{
    for ext in extensions {
        if let Some(id) = id_from_filename_with_extension(path, ext) {
            return Some(id);
        }
    }
    None
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

        let mut handle: AssetFolderHandle<MockId, MockAsset> = AssetFolderHandle::new();

        // Initial state
        assert!(!handle.is_loaded());

        // After starting load
        handle.handle = Some(Handle::default());
        assert!(!handle.is_loaded());

        // After the initial scan has registered the folder's assets
        handle.initial_load_complete = true;
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

    // ==========================================================================
    // Additional tests for Bevy 0.17 migration validation
    // ==========================================================================

    #[test]
    fn test_id_from_filename_extracts_correct_id() {
        let path = Path::new("fireball.spell.ron");
        let id: Option<MockId> = id_from_filename_with_extension(path, ".spell.ron");
        assert!(id.is_some());
        // "fireball" has 8 characters
        assert_eq!(id.unwrap(), MockId(8));
    }

    #[test]
    fn test_id_from_filename_with_nested_path() {
        let path = Path::new("prefabs/spells/fireball.spell.ron");
        let id: Option<MockId> = id_from_filename_with_extension(path, ".spell.ron");
        assert!(id.is_some());
        assert_eq!(id.unwrap(), MockId(8)); // "fireball"
    }

    #[test]
    fn test_id_from_filename_empty_id() {
        // Extension only - should return None
        let path = Path::new(".spell.ron");
        let id: Option<MockId> = id_from_filename_with_extension(path, ".spell.ron");
        assert!(id.is_none());
    }

    #[test]
    fn test_legacy_id_from_filename() {
        let path = Path::new("test_item.mock.ron");
        let id: Option<MockId> = id_from_filename(path, ".mock.ron");
        assert!(id.is_some());
        assert_eq!(id.unwrap(), MockId(9)); // "test_item"
    }

    #[test]
    fn test_is_hidden_file_with_nested_paths() {
        assert!(is_hidden_file(Path::new("some/path/.hidden.ron")));
        assert!(is_hidden_file(Path::new("some/path/_disabled.ron")));
        assert!(!is_hidden_file(Path::new("some/path/normal.ron")));
    }

    #[test]
    fn test_asset_folder_multiple_assets() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut library: AssetFolder<MockId, MockAsset> = AssetFolder::new();

        // Insert multiple assets
        for i in 0..10 {
            library.insert(MockId(i), Handle::default());
        }

        assert_eq!(library.len(), 10);
        assert!(library.is_ready());

        // Verify all are accessible
        for i in 0..10 {
            assert!(library.contains(MockId(i)));
            assert!(library.get(MockId(i)).is_some());
        }

        // Test keys count
        let keys: Vec<_> = library.keys().collect();
        assert_eq!(keys.len(), 10);

        // Test iteration
        let iter_count = library.iter().count();
        assert_eq!(iter_count, 10);
    }

    #[test]
    fn test_asset_folder_get_mut() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut library: AssetFolder<MockId, MockAsset> = AssetFolder::new();
        let id = MockId(1);
        library.insert(id, Handle::default());

        // Test mutable access
        assert!(library.get_mut(id).is_some());
        assert!(library.get_mut(MockId(999)).is_none());
    }

    #[test]
    fn test_asset_folder_insert_returns_old_value() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut library: AssetFolder<MockId, MockAsset> = AssetFolder::new();
        let id = MockId(1);

        // First insert returns None
        let old = library.insert(id, Handle::default());
        assert!(old.is_none());

        // Second insert returns the old handle
        let old = library.insert(id, Handle::default());
        assert!(old.is_some());
    }

    #[test]
    fn test_asset_folder_deref() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut library: AssetFolder<MockId, MockAsset> = AssetFolder::new();
        library.insert(MockId(1), Handle::default());

        // Test Deref access to HashMap methods
        assert!(library.contains_key(&MockId(1)));
        assert!(!library.contains_key(&MockId(2)));
    }


    #[test]
    fn test_atlas_icon_image_node_creation() {
        let icon = AtlasIcon::new(Handle::default(), Handle::default(), 3);

        // Test that image_node() creates a valid ImageNode
        let _image_node = icon.image_node();

        // Test get_image returns a handle
        let _image = icon.get_image();
    }

    #[test]
    fn test_atlas_icon_default() {
        let icon = AtlasIcon::default();

        assert_eq!(icon.atlas_index, 0);
    }

    #[test]
    fn test_atlas_icon_equality() {
        let icon1 = AtlasIcon::new(Handle::default(), Handle::default(), 5);
        let _icon2 = AtlasIcon::new(Handle::default(), Handle::default(), 5);
        let icon3 = AtlasIcon::new(Handle::default(), Handle::default(), 3);

        // Note: Handle::default() creates different handles each time,
        // so icon1 == icon2 may be false depending on implementation
        // But icon should not equal one with different index
        assert_ne!(icon1.atlas_index, icon3.atlas_index);
    }

    #[test]
    fn test_asset_folder_handle_default() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let handle: AssetFolderHandle<MockId, MockAsset> = AssetFolderHandle::default();

        assert!(!handle.is_loaded());
        assert!(handle.handle.is_none());
    }

    #[test]
    fn test_asset_folder_default() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let library: AssetFolder<MockId, MockAsset> = AssetFolder::default();

        assert!(library.is_empty());
        assert!(!library.is_ready());
    }

    #[test]
    fn test_asset_folder_assets_access() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut library: AssetFolder<MockId, MockAsset> = AssetFolder::new();
        library.insert(MockId(1), Handle::default());

        // Test direct HashMap access
        let assets = library.assets();
        assert_eq!(assets.len(), 1);

        let assets_mut = library.assets_mut();
        assets_mut.insert(MockId(2), Handle::default());
        assert_eq!(library.len(), 2);
    }

    // ==========================================================================
    // Multi-extension tests
    // ==========================================================================

    #[test]
    fn test_id_from_filename_with_extensions_first_match() {
        let path = Path::new("explosion.ogg");
        let id: Option<MockId> =
            id_from_filename_with_extensions(path, &[".ogg", ".wav", ".mp3"]);
        assert!(id.is_some());
        assert_eq!(id.unwrap(), MockId(9)); // "explosion"
    }

    #[test]
    fn test_id_from_filename_with_extensions_second_match() {
        let path = Path::new("ambient.wav");
        let id: Option<MockId> =
            id_from_filename_with_extensions(path, &[".ogg", ".wav", ".mp3"]);
        assert!(id.is_some());
        assert_eq!(id.unwrap(), MockId(7)); // "ambient"
    }

    #[test]
    fn test_id_from_filename_with_extensions_no_match() {
        let path = Path::new("music.flac");
        let id: Option<MockId> =
            id_from_filename_with_extensions(path, &[".ogg", ".wav", ".mp3"]);
        assert!(id.is_none());
    }

    #[test]
    fn test_id_from_filename_with_extensions_hidden() {
        let path = Path::new(".hidden.ogg");
        let id: Option<MockId> =
            id_from_filename_with_extensions(path, &[".ogg", ".wav"]);
        assert!(id.is_none());
    }

    #[test]
    fn test_id_from_filename_with_extensions_disabled() {
        let path = Path::new("_disabled.wav");
        let id: Option<MockId> =
            id_from_filename_with_extensions(path, &[".ogg", ".wav"]);
        assert!(id.is_none());
    }

    #[test]
    fn test_id_from_filename_with_extensions_single() {
        // Single extension behaves like id_from_filename_with_extension
        let path = Path::new("fireball.spell.ron");
        let id: Option<MockId> =
            id_from_filename_with_extensions(path, &[".spell.ron"]);
        assert!(id.is_some());
        assert_eq!(id.unwrap(), MockId(8)); // "fireball"
    }

    #[test]
    fn test_id_from_filename_with_extensions_empty_list() {
        let path = Path::new("something.ogg");
        let id: Option<MockId> = id_from_filename_with_extensions(path, &[]);
        assert!(id.is_none());
    }

    #[test]
    fn test_filename_has_extension_matches() {
        assert!(filename_has_extension(
            Path::new("spells/fireball.spell.ron"),
            &[".spell.ron"]
        ));
        assert!(filename_has_extension(
            Path::new("ambient.wav"),
            &[".ogg", ".wav", ".mp3"]
        ));
    }

    #[test]
    fn test_filename_has_extension_no_match() {
        assert!(!filename_has_extension(
            Path::new("notes.txt"),
            &[".spell.ron"]
        ));
        assert!(!filename_has_extension(Path::new("music.flac"), &[".ogg"]));
        assert!(!filename_has_extension(Path::new("no_extension"), &[".ron"]));
    }

    #[test]
    fn test_filename_has_extension_is_lenient_about_prefixes() {
        // This pre-filter is intentionally lenient: hidden/disabled files still
        // match here and are rejected later by `id_from_filename_with_extensions`.
        assert!(filename_has_extension(
            Path::new("_disabled.spell.ron"),
            &[".spell.ron"]
        ));
        assert!(filename_has_extension(
            Path::new(".hidden.spell.ron"),
            &[".spell.ron"]
        ));

        // ...and the authoritative filter rejects them.
        let disabled: Option<MockId> = id_from_filename_with_extensions(
            Path::new("_disabled.spell.ron"),
            &[".spell.ron"],
        );
        assert!(disabled.is_none());
    }

    #[test]
    fn test_asset_folder_iter_mut() {
        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut library: AssetFolder<MockId, MockAsset> = AssetFolder::new();
        library.insert(MockId(1), Handle::default());
        library.insert(MockId(2), Handle::default());

        // Test mutable iteration
        let count = library.iter_mut().count();
        assert_eq!(count, 2);
    }

    // ==========================================================================
    // Shared-asset-type regression tests
    // ==========================================================================

    /// Two `FolderLoaderPlugin`s that share the same asset type `A` but use
    /// different `Id`s must not clobber each other's `Assets<A>` collection.
    ///
    /// Before the `init_asset` guard, the second plugin's `build()` called
    /// `init_asset::<A>()` again, which `insert_resource`s a fresh
    /// `Assets::<A>::default()` and silently drops every handle already loaded
    /// into the collection. This reproduces that scenario: an asset is loaded
    /// into the shared collection between the two plugin builds, and must
    /// survive the second build.
    #[test]
    fn test_second_loader_does_not_wipe_shared_asset_storage() {
        use bevy::asset::AssetPlugin;

        #[derive(Asset, Clone, Reflect, Default)]
        struct SharedAsset {
            value: u32,
        }

        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
        struct IdA(u64);
        impl From<String> for IdA {
            fn from(s: String) -> Self {
                IdA(s.len() as u64)
            }
        }

        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
        struct IdB(u64);
        impl From<String> for IdB {
            fn from(s: String) -> Self {
                IdB(s.len() as u64)
            }
        }

        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .add_plugins(AssetPlugin::default());

        // First folder loader for `SharedAsset`, keyed by `IdA`.
        app.add_plugins(FolderLoaderPlugin::<IdA, SharedAsset>::new(
            "folder_a", ".a.ron",
        ));

        // Simulate an asset being loaded into the shared collection (this is
        // what e.g. a `from_world`/`FromWorld` loader or an earlier folder load
        // would have done).
        let handle = app
            .world_mut()
            .resource_mut::<Assets<SharedAsset>>()
            .add(SharedAsset { value: 42 });
        assert!(
            app.world()
                .resource::<Assets<SharedAsset>>()
                .get(&handle)
                .is_some(),
            "asset should be present right after insertion"
        );

        // Second folder loader for the SAME asset type, keyed by `IdB`.
        // This previously wiped `Assets<SharedAsset>`.
        app.add_plugins(FolderLoaderPlugin::<IdB, SharedAsset>::new(
            "folder_b", ".b.ron",
        ));

        // The previously loaded asset must survive the second plugin's build.
        let assets = app.world().resource::<Assets<SharedAsset>>();
        assert!(
            assets.get(&handle).is_some(),
            "a second FolderLoaderPlugin sharing the asset type wiped Assets<SharedAsset>"
        );
        assert_eq!(assets.get(&handle).unwrap().value, 42);

        // Both loaders' per-Id resources must still be present and independent.
        assert!(app.world().contains_resource::<AssetFolder<IdA, SharedAsset>>());
        assert!(app.world().contains_resource::<AssetFolder<IdB, SharedAsset>>());
    }

    // ==========================================================================
    // Bevy 0.19 resources-as-components regression tests
    // ==========================================================================

    /// Bevy 0.19 makes `Resource` a subtrait of `Component`, and
    /// `#[reflect(Resource)]` now reflects the `Component` trait (via
    /// `ReflectComponent`) rather than a standalone `ReflectResource`. This locks
    /// that in: the reflected `AssetFolderHandle` must register and expose
    /// `ReflectComponent` in the type registry.
    #[test]
    fn reflected_resource_registers_as_component_in_0_19() {
        use bevy::ecs::reflect::ReflectComponent;

        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug, Reflect)]
        struct ReflectableId(u64);
        impl From<String> for ReflectableId {
            fn from(s: String) -> Self {
                ReflectableId(s.len() as u64)
            }
        }

        #[derive(Asset, Clone, Reflect, Default)]
        struct ReflectableAsset;

        let mut app = App::new();
        app.register_type::<AssetFolderHandle<ReflectableId, ReflectableAsset>>();

        let registry = app.world().resource::<AppTypeRegistry>().read();
        let registration = registry
            .get(std::any::TypeId::of::<
                AssetFolderHandle<ReflectableId, ReflectableAsset>,
            >())
            .expect("AssetFolderHandle should be registered");

        assert!(
            registration.data::<ReflectComponent>().is_some(),
            "in Bevy 0.19 a #[reflect(Resource)] type must also reflect the Component trait"
        );
    }

    /// A `#[derive(Resource)]` type must still behave as a plain resource under
    /// the 0.19 component-backed model: inserted once, fetched by type, and
    /// mutated in place — none of which should route through the ECS component
    /// storage from the user's perspective.
    #[test]
    fn derive_resource_still_behaves_as_a_resource_in_0_19() {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
        struct Id(u64);
        impl From<String> for Id {
            fn from(s: String) -> Self {
                Id(s.len() as u64)
            }
        }

        #[derive(Asset, Clone, Reflect, Default)]
        struct MockAsset;

        let mut app = App::new();
        assert!(!app.world().contains_resource::<AssetFolder<Id, MockAsset>>());

        app.init_resource::<AssetFolder<Id, MockAsset>>();
        assert!(app.world().contains_resource::<AssetFolder<Id, MockAsset>>());

        // Mutable resource access works.
        app.world_mut()
            .resource_mut::<AssetFolder<Id, MockAsset>>()
            .insert(Id(3), Handle::default());
        assert_eq!(
            app.world().resource::<AssetFolder<Id, MockAsset>>().len(),
            1
        );
    }
}
