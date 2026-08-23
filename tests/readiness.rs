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

/// A seen folder whose load failed counts as settled so a broken folder can't
/// wedge the gate forever.
///
/// A folder that fails wholesale never emits an `Added` event (Bevy fails the
/// `LoadedFolder` as a unit), so the discovery event is written manually here
/// to put the folder into the gate's seen set — the state a folder that was
/// discovered and later failed ends up in.
#[test]
fn failed_seen_folder_counts_as_ready() {
    let root = unique_asset_root();
    write(&root, "things/broken.thing.ron", "this is { not valid ron");
    let mut app = build_app(&root);

    let handle = app.world().resource::<AssetServer>().load_folder("things");
    let id = handle.id();
    app.insert_resource(KeepFolder(handle));
    app.world_mut()
        .resource_mut::<Messages<AssetEvent<LoadedFolder>>>()
        .write(AssetEvent::Added { id });

    assert!(
        run_until(&mut app, 500, all_ready),
        "a seen folder whose load failed must still settle the gate"
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
