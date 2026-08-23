//! Integration tests for the [`LoadedFolders`] readiness gate and the
//! [`LoadResource`] asset-backed-resource pattern, run against a real
//! (temporary) asset folder and a real `AssetServer`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bevy::asset::LoadedFolder;
use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use msg_load_folder::prelude::*;
use serde::Deserialize;

// =============================================================================
// Test asset types
// =============================================================================

#[derive(Asset, Clone, Reflect, Deserialize, Debug)]
struct Thing {
    value: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
struct ThingId(u64);

impl From<String> for ThingId {
    fn from(s: String) -> Self {
        ThingId(s.len() as u64)
    }
}

/// An asset-backed resource: `FromWorld` is where its handles would be
/// requested in real use.
#[derive(Resource, Asset, Clone, Reflect)]
struct TestConfig {
    value: i32,
}

impl FromWorld for TestConfig {
    fn from_world(_world: &mut World) -> Self {
        Self { value: 7 }
    }
}

/// An asset-backed resource whose dependency tree can never load: its
/// `FromWorld` requests a path that does not exist.
#[derive(Resource, Asset, Clone, Reflect)]
struct BrokenConfig {
    #[dependency]
    missing: Handle<Thing>,
}

impl FromWorld for BrokenConfig {
    fn from_world(world: &mut World) -> Self {
        Self {
            missing: world
                .resource::<AssetServer>()
                .load("missing/nowhere.thing.ron"),
        }
    }
}

/// Keeps a folder load alive for the duration of a test.
#[derive(Resource)]
struct KeepFolder(#[allow(dead_code)] Handle<LoadedFolder>);

// =============================================================================
// Helpers
// =============================================================================

/// Creates a unique temporary asset root for a test and returns its path.
fn unique_asset_root() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("msg_load_folder_ready_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp asset root");
    dir
}

fn write(root: &std::path::Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, contents).expect("write asset file");
}

/// Builds a headless app with a real asset server rooted at `root`.
fn build_app(root: &std::path::Path) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin {
            file_path: root.to_string_lossy().into_owned(),
            watch_for_changes_override: Some(false),
            ..default()
        })
        .add_plugins(RonAssetPlugin::<Thing>::new(&["thing.ron"]))
        .add_plugins(LoadedFoldersPlugin);
    app
}

/// Pumps the app until `cond` holds or `max` updates elapse. Returns whether
/// the condition was met. Sleeps briefly between updates so background IO tasks
/// can make progress.
fn run_until(app: &mut App, max: usize, cond: impl Fn(&App) -> bool) -> bool {
    for _ in 0..max {
        app.update();
        if cond(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    cond(app)
}

fn all_ready(app: &App) -> bool {
    app.world()
        .resource::<LoadedFolders>()
        .all_ready(app.world().resource::<AssetServer>())
}

// =============================================================================
// LoadedFolders gate
// =============================================================================

/// With no folder loads started, the gate must hold closed rather than report
/// a vacuous "everything is loaded".
#[test]
fn gate_is_closed_before_any_folder_is_seen() {
    let root = unique_asset_root();
    let mut app = build_app(&root);

    app.update();
    assert!(!all_ready(&app), "no folders seen must mean not ready");
}

/// A loaded folder opens the gate once all of its files are in.
#[test]
fn gate_opens_once_folders_finish_loading() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");
    write(&root, "things/beta.thing.ron", "(value: 2)");
    let mut app = build_app(&root);

    let handle = app.world().resource::<AssetServer>().load_folder("things");
    app.insert_resource(KeepFolder(handle));

    // The folder is discovered from asset events, then settles.
    assert!(
        run_until(&mut app, 500, all_ready),
        "gate should open once the folder and all of its files have loaded"
    );
}

/// A folder whose load fails wholesale must still be discovered and settle
/// the gate.
///
/// Bevy fails a `LoadedFolder` as a unit: a single broken file means the
/// folder asset is never inserted, so no `Added` event ever fires — the
/// failure must be discovered from `AssetLoadFailedEvent<LoadedFolder>`.
#[test]
fn failed_folder_load_is_discovered_and_counts_as_ready() {
    let root = unique_asset_root();
    write(&root, "things/broken.thing.ron", "this is { not valid ron");
    let mut app = build_app(&root);

    let handle = app.world().resource::<AssetServer>().load_folder("things");
    app.insert_resource(KeepFolder(handle));

    assert!(
        run_until(&mut app, 500, all_ready),
        "a folder whose load failed must still be discovered and settle the gate"
    );
}

/// Watching folders at load start closes the gate immediately and holds it
/// closed until *every* watched folder settles — the first folder finishing
/// early must not open the gate while a larger one is still loading.
#[test]
fn gate_stays_closed_until_every_watched_folder_settles() {
    let root = unique_asset_root();
    write(&root, "small/only.thing.ron", "(value: 1)");
    for i in 0..40 {
        write(
            &root,
            &format!("large/file_{i}.thing.ron"),
            &format!("(value: {i})"),
        );
    }
    let mut app = build_app(&root);

    let small = app.world().resource::<AssetServer>().load_folder("small");
    let large = app.world().resource::<AssetServer>().load_folder("large");
    let (small_id, large_id) = (small.id(), large.id());
    {
        let mut folders = app.world_mut().resource_mut::<LoadedFolders>();
        folders.watch(small);
        folders.watch(large);
    }
    assert_eq!(app.world().resource::<LoadedFolders>().seen_count(), 2);

    // Watched folders are known before anything finishes loading.
    assert!(
        !all_ready(&app),
        "watched folders must hold the gate closed from load start"
    );

    // At every point in time the gate must be open exactly when both folders
    // have settled — never after just the first (smaller) one.
    for _ in 0..500 {
        app.update();
        let server = app.world().resource::<AssetServer>();
        let both_settled = server.is_loaded_with_dependencies(small_id)
            && server.is_loaded_with_dependencies(large_id);
        assert_eq!(
            all_ready(&app),
            both_settled,
            "gate must open exactly when every watched folder has settled"
        );
        if both_settled {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("watched folders never finished loading");
}

/// Dropping every handle to a passively-discovered folder releases the asset;
/// the gate must treat the released folder as settled instead of wedging.
#[test]
fn dropping_a_discovered_folder_handle_does_not_wedge_the_gate() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");
    let mut app = build_app(&root);

    let handle = app.world().resource::<AssetServer>().load_folder("things");
    app.insert_resource(KeepFolder(handle));
    assert!(run_until(&mut app, 500, all_ready), "folder should load");

    // Drop the only strong handle; the asset server releases the folder.
    app.world_mut().remove_resource::<KeepFolder>();
    for _ in 0..50 {
        app.update();
        assert!(
            all_ready(&app),
            "a released folder must count as settled, not wedge the gate"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// With asset watching disabled (the release-build default),
/// `FolderLoaderPlugin` must still register its folder with the gate so
/// `all_ready` becomes meaningful instead of holding closed forever.
#[test]
fn folder_loader_plugin_registers_with_the_gate_without_watching() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");
    let mut app = build_app(&root);
    app.add_plugins(FolderLoaderPlugin::<ThingId, Thing>::new(
        "things",
        ".thing.ron",
    ));

    assert!(
        run_until(&mut app, 500, |app| {
            all_ready(app)
                && app
                    .world()
                    .resource::<AssetFolderHandle<ThingId, Thing>>()
                    .is_loaded()
        }),
        "the loader's folder must register with and open the gate even without watching"
    );
    assert_eq!(
        app.world().resource::<AssetFolder<ThingId, Thing>>().len(),
        1,
        "per-file loading must keep working alongside the gate registration"
    );
}

// =============================================================================
// LoadResource / ResourceHandles
// =============================================================================

/// The default registry has nothing waiting, so it reports done — the
/// documented reason population must happen at plugin-build time.
#[test]
fn empty_registry_is_vacuously_done() {
    assert!(ResourceHandles::default().is_all_done());
}

/// `load_resource` parks the resource until its assets load, then inserts it.
#[test]
fn load_resource_inserts_once_assets_are_ready() {
    let root = unique_asset_root();
    let mut app = build_app(&root);
    app.load_resource::<TestConfig>();

    assert!(
        !app.world().resource::<ResourceHandles>().is_all_done(),
        "queued at build time, the registry must not be done before loading"
    );
    assert!(
        !app.world().contains_resource::<TestConfig>(),
        "the resource must not exist before its assets load"
    );

    assert!(
        run_until(&mut app, 500, |app| {
            app.world().contains_resource::<TestConfig>()
        }),
        "resource should be inserted once its dependency tree loads"
    );
    assert_eq!(app.world().resource::<TestConfig>().value, 7);
    assert!(app.world().resource::<ResourceHandles>().is_all_done());
}

/// The guarded internal `init_asset` means a repeated `load_resource` for one
/// type — still a logic error, but a real hazard — no longer wipes `Assets<T>`
/// and dangles existing handles.
#[test]
fn repeated_load_resource_does_not_wipe_asset_storage() {
    let root = unique_asset_root();
    let mut app = build_app(&root);
    app.load_resource::<TestConfig>();

    // An asset loaded between the two registrations must survive the second.
    let handle = app
        .world_mut()
        .resource_mut::<Assets<TestConfig>>()
        .add(TestConfig { value: 42 });

    app.load_resource::<TestConfig>();

    let assets = app.world().resource::<Assets<TestConfig>>();
    assert!(
        assets.get(&handle).is_some(),
        "a second load_resource for the same type wiped Assets<T>"
    );
    assert_eq!(assets.get(&handle).unwrap().value, 42);
}

/// A resource whose dependency tree fails must be written off (with an error
/// log) rather than requeued forever: `is_all_done` still settles, and the
/// resource is never inserted.
#[test]
fn failed_resource_dependency_counts_as_done_without_inserting() {
    let root = unique_asset_root();
    let mut app = build_app(&root);
    app.load_resource::<BrokenConfig>();

    assert!(
        !app.world().resource::<ResourceHandles>().is_all_done(),
        "the registry must be pending while the dependency is in flight"
    );

    assert!(
        run_until(&mut app, 500, |app| {
            app.world().resource::<ResourceHandles>().is_all_done()
        }),
        "a failed dependency tree must count as done instead of requeueing forever"
    );
    assert!(
        !app.world().contains_resource::<BrokenConfig>(),
        "a resource whose dependencies failed must not be inserted"
    );
    assert_eq!(app.world().resource::<ResourceHandles>().pending_count(), 0);
    assert_eq!(
        app.world().resource::<ResourceHandles>().finished_count(),
        1
    );
}
