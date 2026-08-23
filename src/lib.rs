#![doc = include_str!("../README.md")]

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use bevy::asset::io::ErasedAssetReader;
use bevy::asset::{
    AssetLoadFailedEvent, AssetPath, LoadState, LoadedFolder, RecursiveDependencyLoadState,
    UntypedHandle,
};
use bevy::prelude::*;
use bevy::tasks::{IoTaskPool, Task, block_on, futures_lite::StreamExt, poll_once};

pub mod prelude {
    pub use crate::{
        AssetFile, AssetFolder, AssetFolderHandle, FolderLoaderPlugin, LoadResource, LoadedFolders,
        LoadedFoldersPlugin, ResourceHandles, all_folders_ready, all_resources_loaded,
        deserialize_optional_string, id_from_filename, id_from_filename_with_extensions,
        is_hidden_file,
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
/// * `Id` - The ID type (e.g., `SpellId`, `PerkId`)
/// * `A` - The asset type (e.g., `Spell`, `PerkData`)
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

    /// Direct access to underlying `HashMap`.
    #[must_use]
    pub fn assets(&self) -> &HashMap<Id, Handle<A>> {
        &self.assets
    }

    /// Mutable access to underlying `HashMap`.
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
///    drive hot reloading. When the [`LoadedFolders`] gate is present, the
///    folder load is started in all builds — watching or not — and registered
///    via [`LoadedFolders::watch`], so the gate composes with this loader
///    everywhere (a gate that only saw folders in dev builds would hold a
///    release build's loading screen closed forever).
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
    mut gate: Option<ResMut<LoadedFolders>>,
) where
    Id: Clone + Copy + Eq + Hash + Send + Sync + Default + From<String> + std::fmt::Debug + 'static,
    A: Asset + Clone + Send + Sync + 'static,
{
    // 1. One-time setup: request the initial scan and, when watching is on,
    //    keep a folder handle alive so Bevy notifies us of structural changes.
    //    When the readiness gate is in use the folder is loaded and watched in
    //    all builds, so the gate stays meaningful with watching disabled.
    if !scan_state.initialized {
        scan_state.initialized = true;
        scan_state.rescan_requested = true;
        let watching = asset_server.watching_for_changes();
        if watching || gate.is_some() {
            let handle = asset_server.load_folder(config.folder_path);
            if let Some(gate) = gate.as_mut() {
                gate.watch(handle.clone());
            }
            if watching {
                folder_handle.handle = Some(handle);
            }
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
    let source = AssetPath::parse(config.folder_path).source().clone_owned();

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
                debug!(
                    "FolderLoader reloaded re-added {:?} ({})",
                    id,
                    path.display()
                );
            }

            library.insert(id, handle);
            debug!("FolderLoader registered {:?} ({})", id, path.display());
        }
    }

    // Forget assets whose files were removed; dropping the handle lets Bevy
    // unload the underlying asset.
    let removed: Vec<Id> = library.keys().filter(|id| !present.contains(id)).collect();
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
// Folder Readiness Gate
// =============================================================================

/// Plugin registering the [`LoadedFolders`] readiness gate.
///
/// Folders enter the gate in two ways:
///
/// - **Explicitly**, via [`LoadedFolders::watch`]: the folder is registered the
///   moment its load starts, so the gate holds closed until it settles. This is
///   the deterministic way to gate a loading screen.
/// - **Passively**, from [`AssetEvent<LoadedFolder>`] and
///   [`AssetLoadFailedEvent<LoadedFolder>`]: folders loaded elsewhere are
///   discovered with no wiring — but Bevy only emits these events once a
///   folder *finishes* (or fails) loading, so with several passively-discovered
///   folders in flight [`LoadedFolders::all_ready`] can report `true` after
///   the first completes and before the rest surface. Prefer
///   [`LoadedFolders::watch`] whenever more than one folder must hold the gate.
///
/// Every [`FolderLoaderPlugin`] registers its folder with the gate
/// automatically when this plugin is present, in all builds — with or without
/// asset watching.
///
/// Idempotent: the plugin may be added from several places (it is not unique),
/// and only the first add registers the resource and system.
pub struct LoadedFoldersPlugin;

impl Plugin for LoadedFoldersPlugin {
    fn build(&self, app: &mut App) {
        if app.world().contains_resource::<LoadedFolders>() {
            return;
        }
        app.init_resource::<LoadedFolders>();
        app.add_systems(PreUpdate, track_loaded_folders);
    }

    fn is_unique(&self) -> bool {
        false
    }
}

/// Every [`LoadedFolder`] the app is known to load — registered explicitly via
/// [`Self::watch`] or discovered passively from asset events. Folders stay
/// tracked for the lifetime of the run.
///
/// Registered by [`LoadedFoldersPlugin`].
#[derive(Resource, Default)]
pub struct LoadedFolders {
    seen: HashSet<AssetId<LoadedFolder>>,
    /// Strong handles for watched folders, kept alive so a watched folder can
    /// never be released out from under the gate.
    watched: Vec<Handle<LoadedFolder>>,
}

impl LoadedFolders {
    /// Registers a folder with the gate the moment its load starts.
    ///
    /// The stored handle is strong, so a watched folder stays alive (and its
    /// load state observable) even if every other handle to it is dropped.
    /// Watching the same folder twice is a no-op.
    ///
    /// Prefer this over relying on passive discovery: passively-discovered
    /// folders only surface once they finish loading, so with several folders
    /// in flight [`Self::all_ready`] can open early. Watched folders hold the
    /// gate closed from the frame their load is requested.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use msg_load_folder::prelude::*;
    /// # use bevy::prelude::*;
    /// fn start_loading(asset_server: Res<AssetServer>, mut folders: ResMut<LoadedFolders>) {
    ///     folders.watch(asset_server.load_folder("prefabs/spells"));
    ///     folders.watch(asset_server.load_folder("sounds"));
    /// }
    /// ```
    pub fn watch(&mut self, handle: Handle<LoadedFolder>) {
        if !self.watched.iter().any(|h| h.id() == handle.id()) {
            self.seen.insert(handle.id());
            self.watched.push(handle);
        }
    }

    /// Returns `true` once at least one folder is known and every known folder
    /// has settled: loaded with all of its files, failed, or been released.
    ///
    /// A folder whose load failed counts as settled so a single broken file
    /// can't wedge the gate, and a folder whose handles were all dropped
    /// counts as settled too — the asset is released, so nothing is waiting on
    /// it (this can only happen to passively-discovered folders; watched ones
    /// are kept alive by the gate).
    ///
    /// The answer is only meaningful once every `load_folder` call has been
    /// issued: folders the gate does not yet know about cannot hold it closed.
    /// Register folders with [`Self::watch`] at load start for deterministic
    /// gating, or use the [`all_folders_ready`] run condition.
    #[must_use]
    pub fn all_ready(&self, asset_server: &AssetServer) -> bool {
        !self.seen.is_empty() && self.seen.iter().all(|id| folder_settled(asset_server, *id))
    }

    /// Number of folders known to the gate (watched or discovered).
    #[must_use]
    pub fn seen_count(&self) -> usize {
        self.seen.len()
    }

    /// Number of known folders that have settled (loaded, failed or released),
    /// for loading-progress displays alongside [`Self::seen_count`].
    #[must_use]
    pub fn settled_count(&self, asset_server: &AssetServer) -> usize {
        self.seen
            .iter()
            .filter(|id| folder_settled(asset_server, **id))
            .count()
    }
}

/// Run condition for [`LoadedFolders::all_ready`].
///
/// # Example
///
/// ```rust
/// # use msg_load_folder::prelude::*;
/// # use bevy::prelude::*;
/// # let mut app = App::new();
/// # fn enter_game() {}
/// app.add_systems(Update, enter_game.run_if(all_folders_ready));
/// ```
#[must_use]
pub fn all_folders_ready(folders: Res<LoadedFolders>, asset_server: Res<AssetServer>) -> bool {
    folders.all_ready(&asset_server)
}

/// Whether the folder has settled: loaded with all of its files, failed, or
/// been released (no load state left means every handle was dropped and the
/// asset is gone — nothing can be waiting on it).
fn folder_settled(asset_server: &AssetServer, id: AssetId<LoadedFolder>) -> bool {
    match asset_server.get_recursive_dependency_load_state(id) {
        None | Some(RecursiveDependencyLoadState::Failed(_)) => true,
        Some(_) => asset_server.is_loaded_with_dependencies(id),
    }
}

/// Records every folder the app loads into [`LoadedFolders`].
///
/// A folder that fails wholesale never emits an [`AssetEvent::Added`] (Bevy
/// fails the [`LoadedFolder`] as a unit and never inserts the asset), so
/// failures are discovered from [`AssetLoadFailedEvent`] instead.
fn track_loaded_folders(
    mut events: MessageReader<AssetEvent<LoadedFolder>>,
    mut failures: MessageReader<AssetLoadFailedEvent<LoadedFolder>>,
    mut folders: ResMut<LoadedFolders>,
) {
    for event in events.read() {
        match event {
            AssetEvent::Added { id }
            | AssetEvent::Modified { id }
            | AssetEvent::LoadedWithDependencies { id } => {
                folders.seen.insert(*id);
            }
            _ => {}
        }
    }
    for failure in failures.read() {
        folders.seen.insert(failure.id);
    }
}

// =============================================================================
// Asset-Backed Resources (LoadResource)
// =============================================================================

/// Loading a resource through the asset pipeline, so it exists only once its
/// assets do.
///
/// [`LoadResource::load_resource`] takes a type that is both `Resource` and
/// `Asset`, builds it via `FromWorld` (which is where its handles are
/// requested), and parks it in [`ResourceHandles`]. A `PreUpdate` system
/// inserts it as a resource once the asset server reports the whole dependency
/// tree loaded — so any system that can see the resource can also use its
/// handles. If the dependency tree fails to load instead, the failure is
/// logged and the entry counts as done without the resource being inserted
/// (see [`ResourceHandles::is_all_done`]).
pub trait LoadResource {
    /// Queue `T` to be inserted as a resource once all of its asset
    /// dependencies have loaded. This ensures the resource only exists when
    /// its assets are ready.
    ///
    /// Call this **at plugin-build time**, once per `T`:
    ///
    /// - Deferring the call to a startup system leaves [`ResourceHandles`]
    ///   empty on early frames, making [`ResourceHandles::is_all_done`]
    ///   vacuously `true` before loading has even been requested — screens
    ///   gating on it would let the app through unloaded.
    /// - A second call for the same `T` queues a second `FromWorld` value that
    ///   overwrites the first on insert; the internal `init_asset` is guarded,
    ///   so the duplicate no longer dangles existing handles of `T`, but the
    ///   double registration is still a logic error.
    ///
    /// # Panics
    ///
    /// Panics at plugin-build time when the [`AssetServer`] is missing (add
    /// `AssetPlugin` first), aborting startup rather than degrading.
    fn load_resource<T: Resource + Asset + Clone + FromWorld>(&mut self) -> &mut Self;
}

impl LoadResource for App {
    fn load_resource<T: Resource + Asset + Clone + FromWorld>(&mut self) -> &mut Self {
        // One-time infrastructure setup, guarded so any number of
        // `load_resource` calls share one registry and one drain system.
        if !self.world().contains_resource::<ResourceHandles>() {
            self.init_resource::<ResourceHandles>();
            self.add_systems(PreUpdate, load_resource_assets);
        }

        // Initialize the asset storage only once per `T`: `init_asset` is NOT
        // idempotent (see the identical guard in `FolderLoaderPlugin::build`) —
        // an unconditional second call would replace `Assets<T>` and silently
        // dangle every existing handle of `T`.
        if !self.world().contains_resource::<Assets<T>>() {
            self.init_asset::<T>();
        }

        let world = self.world_mut();
        let value = T::from_world(world);
        let assets = world.resource::<AssetServer>();
        let handle = assets.add(value);
        let mut handles = world.resource_mut::<ResourceHandles>();
        handles.waiting.push_back(QueuedResource {
            type_name: core::any::type_name::<T>(),
            handle: handle.untyped(),
            insert: |world, handle| {
                let value = world
                    .get_resource::<Assets<T>>()
                    .and_then(|assets| assets.get(handle.id().typed::<T>()))
                    .cloned();
                if let Some(value) = value {
                    world.insert_resource(value);
                } else {
                    error!(
                        "LoadResource: asset backing resource `{}` was gone at insert time; \
                         the resource was not inserted",
                        core::any::type_name::<T>()
                    );
                }
            },
        });
        self
    }
}

/// A function that inserts a loaded resource.
type InsertLoadedResource = fn(&mut World, &UntypedHandle);

/// A resource queued by [`LoadResource::load_resource`], waiting on its asset
/// dependencies.
struct QueuedResource {
    /// The resource's type name, for error reporting.
    type_name: &'static str,
    handle: UntypedHandle,
    insert: InsertLoadedResource,
}

/// Registry of asset-backed resources queued by
/// [`LoadResource::load_resource`], tracking which are still waiting on their
/// asset dependencies.
///
/// Loading screens gate on [`ResourceHandles::is_all_done`] or the
/// [`all_resources_loaded`] run condition, and can display progress from
/// [`ResourceHandles::pending_count`] / [`ResourceHandles::finished_count`].
#[derive(Resource, Default)]
pub struct ResourceHandles {
    // Use a queue for waiting assets so they can be cycled through and moved to
    // `finished` one at a time.
    waiting: VecDeque<QueuedResource>,
    finished: Vec<UntypedHandle>,
}

impl ResourceHandles {
    /// Returns `true` once every queued resource has settled: inserted after
    /// its assets loaded, or written off because its dependency tree failed.
    ///
    /// A resource whose dependency tree fails to load is logged as an error
    /// and counted as done — mirroring the [`LoadedFolders`] gate's failure
    /// policy — so a single broken asset degrades loudly instead of hanging
    /// the loading screen forever. Such a resource is never inserted.
    ///
    /// Note this is vacuously `true` while nothing has been queued — which is
    /// why [`LoadResource::load_resource`] must be called at plugin-build time
    /// rather than from a startup system.
    #[must_use]
    pub fn is_all_done(&self) -> bool {
        self.waiting.is_empty()
    }

    /// Number of queued resources still waiting on their asset dependencies.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.waiting.len()
    }

    /// Number of queued resources that have settled (inserted, or written off
    /// after their dependency tree failed), for loading-progress displays
    /// alongside [`Self::pending_count`].
    #[must_use]
    pub fn finished_count(&self) -> usize {
        self.finished.len()
    }
}

/// Run condition for [`ResourceHandles::is_all_done`].
///
/// # Example
///
/// ```rust
/// # use msg_load_folder::prelude::*;
/// # use bevy::prelude::*;
/// # let mut app = App::new();
/// # fn enter_game() {}
/// app.add_systems(Update, enter_game.run_if(all_resources_loaded));
/// ```
#[must_use]
pub fn all_resources_loaded(handles: Res<ResourceHandles>) -> bool {
    handles.is_all_done()
}

/// Moves queued resources whose dependency trees have loaded into the world.
///
/// A queued resource whose dependency tree fails is logged and counted as
/// finished (without being inserted), so [`ResourceHandles::is_all_done`] can
/// still settle — see its documentation for the failure policy.
fn load_resource_assets(world: &mut World) {
    world.resource_scope(|world, mut resource_handles: Mut<ResourceHandles>| {
        world.resource_scope(|world, assets: Mut<AssetServer>| {
            for _ in 0..resource_handles.waiting.len() {
                let queued = resource_handles.waiting.pop_front().unwrap();
                if assets.is_loaded_with_dependencies(&queued.handle) {
                    (queued.insert)(world, &queued.handle);
                    resource_handles.finished.push(queued.handle);
                } else if let Some(RecursiveDependencyLoadState::Failed(error)) =
                    assets.get_recursive_dependency_load_state(&queued.handle)
                {
                    error!(
                        "LoadResource: assets for resource `{}` failed to load: {error}; \
                         the resource will not be inserted",
                        queued.type_name
                    );
                    resource_handles.finished.push(queued.handle);
                } else {
                    resource_handles.waiting.push_back(queued);
                }
            }
        });
    });
}

/// A config type that knows the asset path it was loaded from, so hot-reload
/// and error messages can name the file without a lookup table.
///
/// For types that also derive [`Reflect`], calling `value.path()` with
/// `bevy::prelude::*` in scope collides with `bevy_reflect::GetPath::path`;
/// disambiguate with `AssetFile::path(&value)` there.
///
/// # Example
///
/// ```rust
/// # use bevy::prelude::*;
/// use msg_load_folder::AssetFile;
///
/// #[derive(Asset, Clone, TypePath)]
/// struct Config {
///     path: String,
///     volume: f32,
/// }
///
/// impl AssetFile for Config {
///     fn path(&self) -> &str {
///         &self.path
///     }
/// }
///
/// fn validate(config: &Config) {
///     if !(0.0..=1.0).contains(&config.volume) {
///         warn!("volume out of range in '{}'", config.path());
///     }
/// }
/// ```
pub trait AssetFile {
    /// The asset path this value was loaded from.
    fn path(&self) -> &str;
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
#[must_use]
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
#[must_use]
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
#[must_use]
pub fn id_from_filename<Id>(path: &Path, extension: &str) -> Option<Id>
where
    Id: From<String>,
{
    id_from_filename_with_extension(path, extension)
}

/// Check if a path represents a hidden or disabled file.
#[must_use]
pub fn is_hidden_file(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        let name_str = name.to_string_lossy();
        name_str.starts_with('.') || name_str.starts_with('_')
    })
}

// =============================================================================
// Parsing Utilities
// =============================================================================

/// Deserializes a string field to `Option<String>`.
/// Accepts a bare string and converts empty strings to `None`.
///
/// # Errors
///
/// Returns a deserialization error if the underlying value is not a string.
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
        let id: Option<MockId> = id_from_filename_with_extensions(path, &[".ogg", ".wav", ".mp3"]);
        assert!(id.is_some());
        assert_eq!(id.unwrap(), MockId(9)); // "explosion"
    }

    #[test]
    fn test_id_from_filename_with_extensions_second_match() {
        let path = Path::new("ambient.wav");
        let id: Option<MockId> = id_from_filename_with_extensions(path, &[".ogg", ".wav", ".mp3"]);
        assert!(id.is_some());
        assert_eq!(id.unwrap(), MockId(7)); // "ambient"
    }

    #[test]
    fn test_id_from_filename_with_extensions_no_match() {
        let path = Path::new("music.flac");
        let id: Option<MockId> = id_from_filename_with_extensions(path, &[".ogg", ".wav", ".mp3"]);
        assert!(id.is_none());
    }

    #[test]
    fn test_id_from_filename_with_extensions_hidden() {
        let path = Path::new(".hidden.ogg");
        let id: Option<MockId> = id_from_filename_with_extensions(path, &[".ogg", ".wav"]);
        assert!(id.is_none());
    }

    #[test]
    fn test_id_from_filename_with_extensions_disabled() {
        let path = Path::new("_disabled.wav");
        let id: Option<MockId> = id_from_filename_with_extensions(path, &[".ogg", ".wav"]);
        assert!(id.is_none());
    }

    #[test]
    fn test_id_from_filename_with_extensions_single() {
        // Single extension behaves like id_from_filename_with_extension
        let path = Path::new("fireball.spell.ron");
        let id: Option<MockId> = id_from_filename_with_extensions(path, &[".spell.ron"]);
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
        assert!(!filename_has_extension(
            Path::new("no_extension"),
            &[".ron"]
        ));
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
        let disabled: Option<MockId> =
            id_from_filename_with_extensions(Path::new("_disabled.spell.ron"), &[".spell.ron"]);
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
        assert!(
            app.world()
                .contains_resource::<AssetFolder<IdA, SharedAsset>>()
        );
        assert!(
            app.world()
                .contains_resource::<AssetFolder<IdB, SharedAsset>>()
        );
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
        assert!(
            !app.world()
                .contains_resource::<AssetFolder<Id, MockAsset>>()
        );

        app.init_resource::<AssetFolder<Id, MockAsset>>();
        assert!(
            app.world()
                .contains_resource::<AssetFolder<Id, MockAsset>>()
        );

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
