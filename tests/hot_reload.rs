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

/// The stable handle id currently registered for `name`, if any. Used to prove
/// that structural changes to the folder never re-key an untouched entry.
fn handle_id(app: &App, name: &str) -> Option<bevy::asset::AssetId<Thing>> {
    app.world()
        .resource::<AssetFolder<ThingId, Thing>>()
        .get(ThingId::from(name.to_string()))
        .map(Handle::id)
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

/// Several files added between scans are all discovered on a single signal.
#[test]
fn adding_many_files_at_once_is_discovered() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "alpha") == Some(1)),
        "alpha should load initially"
    );

    // Drop three new files in one go, then fire a single folder-change signal.
    write(&root, "things/beta.thing.ron", "(value: 2)");
    write(&root, "things/gamma.thing.ron", "(value: 3)");
    write(&root, "things/delta.thing.ron", "(value: 4)");
    signal_folder_changed(&mut app);

    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "beta") == Some(2)
            && loaded_value(a, "gamma") == Some(3)
            && loaded_value(a, "delta") == Some(4)),
        "all newly added files should be discovered from a single signal"
    );
    // The original entry is untouched.
    assert_eq!(loaded_value(&app, "alpha"), Some(1));
    assert_eq!(
        app.world().resource::<AssetFolder<ThingId, Thing>>().len(),
        4
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Removing every file empties the library without panicking, and the loader
/// still reports itself loaded (it is a "ready", not a terminal, signal).
#[test]
fn removing_all_files_empties_the_library() {
    let root = unique_asset_root();
    write(&root, "things/one.thing.ron", "(value: 1)");
    write(&root, "things/two.thing.ron", "(value: 2)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "one") == Some(1)
            && loaded_value(a, "two") == Some(2)),
        "both files should load initially"
    );

    std::fs::remove_file(root.join("things/one.thing.ron")).unwrap();
    std::fs::remove_file(root.join("things/two.thing.ron")).unwrap();
    signal_folder_changed(&mut app);

    assert!(
        run_until(&mut app, 1000, |a| a
            .world()
            .resource::<AssetFolder<ThingId, Thing>>()
            .is_empty()),
        "library should be empty once every file is removed"
    );
    // Still "loaded": the folder was scanned; it simply has nothing in it now.
    assert!(
        app.world()
            .resource::<AssetFolderHandle<ThingId, Thing>>()
            .is_loaded()
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Rapid add → remove → re-add churn of the same file keeps the library in sync
/// every step of the way, and the re-added file loads its (possibly new) value.
#[test]
fn add_remove_readd_churn_stays_in_sync() {
    let root = unique_asset_root();
    write(&root, "things/keep.thing.ron", "(value: 0)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "keep") == Some(0)),
        "keep should load initially"
    );

    // Add.
    write(&root, "things/churn.thing.ron", "(value: 1)");
    signal_folder_changed(&mut app);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "churn") == Some(1)),
        "churn should be discovered after being added"
    );

    // Remove.
    std::fs::remove_file(root.join("things/churn.thing.ron")).unwrap();
    signal_folder_changed(&mut app);
    assert!(
        run_until(&mut app, 1000, |a| !in_library(a, "churn")),
        "churn should be dropped after being removed"
    );

    // Re-add with a different value.
    write(&root, "things/churn.thing.ron", "(value: 2)");
    signal_folder_changed(&mut app);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "churn") == Some(2)),
        "re-added churn should be rediscovered and load its new value"
    );

    // The untouched neighbour survived every round.
    assert_eq!(loaded_value(&app, "keep"), Some(0));

    let _ = std::fs::remove_dir_all(&root);
}

/// A single rescan that sees one file gone and a different one appeared applies
/// both the removal and the addition together.
#[test]
fn interleaved_add_and_remove_in_one_scan() {
    let root = unique_asset_root();
    write(&root, "things/stays.thing.ron", "(value: 1)");
    write(&root, "things/goes.thing.ron", "(value: 2)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "stays") == Some(1)
            && loaded_value(a, "goes") == Some(2)),
        "both initial files should load"
    );

    // Remove one and add another before any rescan runs.
    std::fs::remove_file(root.join("things/goes.thing.ron")).unwrap();
    write(&root, "things/arrives.thing.ron", "(value: 3)");
    signal_folder_changed(&mut app);

    assert!(
        run_until(&mut app, 1000, |a| !in_library(a, "goes")
            && loaded_value(a, "arrives") == Some(3)),
        "one scan should apply both the removal and the addition"
    );
    assert_eq!(loaded_value(&app, "stays"), Some(1));

    let _ = std::fs::remove_dir_all(&root);
}

/// Adding and removing files around an existing entry must never re-key it: its
/// handle id has to stay identical so references held elsewhere keep working.
#[test]
fn structural_changes_keep_existing_handles_stable() {
    let root = unique_asset_root();
    write(&root, "things/anchor.thing.ron", "(value: 1)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "anchor") == Some(1)),
        "anchor should load initially"
    );
    let anchor_before = handle_id(&app, "anchor").expect("anchor registered");

    // Add a sibling.
    write(&root, "things/added.thing.ron", "(value: 2)");
    signal_folder_changed(&mut app);
    assert!(
        run_until(&mut app, 1000, |a| in_library(a, "added")),
        "sibling should be discovered"
    );
    assert_eq!(
        handle_id(&app, "anchor"),
        Some(anchor_before),
        "adding a file must not re-key the anchor's handle"
    );

    // Remove the sibling again.
    std::fs::remove_file(root.join("things/added.thing.ron")).unwrap();
    signal_folder_changed(&mut app);
    assert!(
        run_until(&mut app, 1000, |a| !in_library(a, "added")),
        "sibling should be dropped"
    );
    assert_eq!(
        handle_id(&app, "anchor"),
        Some(anchor_before),
        "removing a file must not re-key the anchor's handle"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Files added in a nested subdirectory are found by the recursive scan.
#[test]
fn files_added_in_nested_subfolders_are_discovered() {
    let root = unique_asset_root();
    write(&root, "things/top.thing.ron", "(value: 1)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "top") == Some(1)),
        "top-level file should load"
    );

    // Add a file two directories deep.
    write(&root, "things/nested/deep/buried.thing.ron", "(value: 7)");
    signal_folder_changed(&mut app);

    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "buried") == Some(7)),
        "a file added deep in a subfolder should be discovered by the recursive scan"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A file that never parsed (registered but with no usable asset) is still
/// dropped from the library once it is removed from disk.
#[test]
fn removing_a_broken_file_drops_it() {
    let root = unique_asset_root();
    write(&root, "things/good.thing.ron", "(value: 1)");
    write(&root, "things/broken.thing.ron", "definitely not ron");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "good") == Some(1)
            && in_library(a, "broken")),
        "good should load and broken should still be registered"
    );
    // Broken parsed to nothing but occupies a slot.
    assert_eq!(loaded_value(&app, "broken"), None);

    std::fs::remove_file(root.join("things/broken.thing.ron")).unwrap();
    signal_folder_changed(&mut app);

    assert!(
        run_until(&mut app, 1000, |a| !in_library(a, "broken")),
        "a removed broken file should be dropped just like a valid one"
    );
    assert_eq!(loaded_value(&app, "good"), Some(1));

    let _ = std::fs::remove_dir_all(&root);
}

/// A redundant folder-change signal that reports no structural change is a
/// no-op: nothing is dropped, re-keyed, or duplicated.
#[test]
fn signal_without_changes_is_a_noop() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");
    write(&root, "things/beta.thing.ron", "(value: 2)");

    let mut app = build_app(&root);
    assert!(
        run_until(&mut app, 1000, |a| loaded_value(a, "alpha") == Some(1)
            && loaded_value(a, "beta") == Some(2)),
        "both files should load initially"
    );
    let alpha_before = handle_id(&app, "alpha");
    let beta_before = handle_id(&app, "beta");

    // Fire a signal without touching the folder at all.
    signal_folder_changed(&mut app);
    // Let the (no-op) rescan run to completion.
    run_until(&mut app, 200, |_| false);

    assert_eq!(
        app.world().resource::<AssetFolder<ThingId, Thing>>().len(),
        2,
        "a no-op signal must not change the entry count"
    );
    assert_eq!(handle_id(&app, "alpha"), alpha_before, "alpha must not be re-keyed");
    assert_eq!(handle_id(&app, "beta"), beta_before, "beta must not be re-keyed");

    let _ = std::fs::remove_dir_all(&root);
}
