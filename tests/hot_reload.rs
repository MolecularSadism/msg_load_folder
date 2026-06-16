//! Hot-reload tests.
//!
//! Bevy's real filesystem watcher (the `file_watcher` feature) turns OS file
//! events into two kinds of triggers:
//!
//! * a changed file → `AssetServer::reload(path)` (reloads the asset in place);
//! * an added/removed file → a reload of the parent folder, surfaced as an
//!   `AssetEvent<LoadedFolder>`.
//!
//! These tests drive those exact triggers directly, so they verify the loader's
//! hot-reload behavior deterministically without depending on OS-level file
//! watching (which is slow and flaky in CI).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bevy::asset::LoadedFolder;
use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use msg_load_folder::prelude::*;
use serde::Deserialize;

#[derive(Asset, Clone, Reflect, Deserialize, Debug)]
struct Thing {
    value: i32,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
struct ThingId(&'static str);

impl From<String> for ThingId {
    fn from(s: String) -> Self {
        ThingId(Box::leak(s.into_boxed_str()))
    }
}

fn unique_asset_root() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("msg_load_folder_hr_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp asset root");
    dir
}

fn write(root: &std::path::Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

fn build_app(root: &std::path::Path) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin {
            file_path: root.to_string_lossy().into_owned(),
            ..default()
        })
        .add_plugins(RonAssetPlugin::<Thing>::new(&["thing.ron"]))
        .add_plugins(FolderLoaderPlugin::<ThingId, Thing>::new(
            "things",
            ".thing.ron",
        ));
    app
}

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

fn loaded_value(app: &App, name: &str) -> Option<i32> {
    let lib = app.world().resource::<AssetFolder<ThingId, Thing>>();
    let assets = app.world().resource::<Assets<Thing>>();
    lib.get(ThingId::from(name.to_string()))
        .and_then(|h| assets.get(h))
        .map(|t| t.value)
}

fn in_library(app: &App, name: &str) -> bool {
    app.world()
        .resource::<AssetFolder<ThingId, Thing>>()
        .contains(ThingId::from(name.to_string()))
}

/// Reloads the asset at `rel` exactly as Bevy's watcher does on a file change.
fn reload(app: &App, rel: &str) {
    app.world()
        .resource::<AssetServer>()
        .reload(PathBuf::from(rel));
}

/// Simulates the folder-reload signal Bevy emits when a file is added/removed,
/// after pointing the loader at a real `LoadedFolder` handle (which the watcher
/// would otherwise keep alive).
fn signal_folder_changed(app: &mut App) {
    let folder = app
        .world()
        .resource::<AssetServer>()
        .load_folder("things");
    let id = folder.id();
    app.world_mut()
        .resource_mut::<AssetFolderHandle<ThingId, Thing>>()
        .handle = Some(folder);
    app.world_mut()
        .write_message(AssetEvent::<LoadedFolder>::Modified { id });
}

/// A file edited on disk is reloaded in place behind its stable handle — and a
/// file that was previously broken recovers the same way once it parses.
#[test]
fn editing_and_fixing_files_reloads_in_place() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");
    write(&root, "things/broken.thing.ron", "not valid ron");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "alpha") == Some(1)),
        "alpha should load initially"
    );
    // The broken file is registered but has no usable asset.
    assert!(in_library(&app, "broken"));
    assert_eq!(loaded_value(&app, "broken"), None);

    // Edit alpha: the change is reflected without re-registering anything.
    write(&root, "things/alpha.thing.ron", "(value: 100)");
    reload(&app, "things/alpha.thing.ron");
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "alpha") == Some(100)),
        "edited file should reload in place"
    );

    // Fix the broken file: its existing handle reloads and becomes available.
    write(&root, "things/broken.thing.ron", "(value: 42)");
    reload(&app, "things/broken.thing.ron");
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "broken") == Some(42)),
        "previously broken file should load once fixed"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A file added to the folder is discovered when the folder-change signal fires.
#[test]
fn adding_a_file_is_discovered() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "alpha") == Some(1)),
        "alpha should load initially"
    );
    assert!(!in_library(&app, "beta"));

    // Create a new file, then signal that the folder changed.
    write(&root, "things/beta.thing.ron", "(value: 2)");
    signal_folder_changed(&mut app);

    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "beta") == Some(2)),
        "newly added file should be discovered and loaded"
    );
    // The pre-existing file is untouched.
    assert_eq!(loaded_value(&app, "alpha"), Some(1));

    let _ = std::fs::remove_dir_all(&root);
}

/// A file removed from the folder is dropped from the library on the next
/// folder-change signal.
#[test]
fn removing_a_file_drops_it() {
    let root = unique_asset_root();
    write(&root, "things/keep.thing.ron", "(value: 1)");
    write(&root, "things/remove_me.thing.ron", "(value: 2)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "keep") == Some(1)
            && loaded_value(a, "remove_me") == Some(2)),
        "both files should load initially"
    );

    std::fs::remove_file(root.join("things/remove_me.thing.ron")).unwrap();
    signal_folder_changed(&mut app);

    assert!(
        run_until(&mut app, 1000, |a| !in_library(a, "remove_me")),
        "removed file should be dropped from the library"
    );
    assert_eq!(loaded_value(&app, "keep"), Some(1));

    let _ = std::fs::remove_dir_all(&root);
}
