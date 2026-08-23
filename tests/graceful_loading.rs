//! Integration tests for resilient folder loading.
//!
//! These exercise the real plugin against a real (temporary) asset folder and a
//! real `AssetServer`, verifying that a single malformed file does not prevent
//! the remaining files in the folder from loading.

// Bind `bevy` / `bevy_common_assets` to whichever major the active cargo
// feature selects (see Cargo.toml); under the default `bevy_0_19` feature the
// names already exist, so no alias is needed.
#[cfg(feature = "bevy_0_18")]
extern crate bevy018 as bevy;
#[cfg(feature = "bevy_0_18")]
extern crate bevy_common_assets018 as bevy_common_assets;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use msg_load_folder::prelude::*;
use serde::Deserialize;

// =============================================================================
// Test asset + id types
// =============================================================================

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

// =============================================================================
// Helpers
// =============================================================================

/// Creates a unique temporary asset root for a test and returns its path.
fn unique_asset_root() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("msg_load_folder_it_{}_{n}", std::process::id()));
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

/// Builds a headless app wired up to load `Thing`s from `<root>/things`.
fn build_app(root: &std::path::Path) -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin {
            file_path: root.to_string_lossy().into_owned(),
            // These tests cover the non-watching path deterministically.
            watch_for_changes_override: Some(false),
            ..default()
        })
        .add_plugins(RonAssetPlugin::<Thing>::new(&["thing.ron"]))
        .add_plugins(FolderLoaderPlugin::<ThingId, Thing>::new(
            "things",
            ".thing.ron",
        ));
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

// =============================================================================
// Tests
// =============================================================================

/// The headline behavior: one file with a RON syntax/semantic error must not
/// stop the other files in the folder from loading.
#[test]
fn broken_file_does_not_block_the_rest() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");
    write(&root, "things/beta.thing.ron", "(value: 2)");
    write(&root, "things/gamma.thing.ron", "(value: 3)");
    // Malformed: not valid RON at all.
    write(&root, "things/broken.thing.ron", "this is { not valid ron");
    // Disabled file (leading underscore) should be ignored entirely.
    write(&root, "things/_disabled.thing.ron", "(value: 99)");

    let mut app = build_app(&root);

    // Wait until all three valid assets have actually loaded.
    let ok = run_until(&mut app, 1000, |app| {
        ["alpha", "beta", "gamma"]
            .iter()
            .all(|n| loaded_value(app, n).is_some())
    });
    assert!(ok, "valid assets should load despite a broken sibling");

    // Valid files loaded with correct data.
    assert_eq!(loaded_value(&app, "alpha"), Some(1));
    assert_eq!(loaded_value(&app, "beta"), Some(2));
    assert_eq!(loaded_value(&app, "gamma"), Some(3));

    // The broken file is registered (so it can recover if fixed) but its asset
    // is not available.
    assert!(
        in_library(&app, "broken"),
        "broken file should still get a handle"
    );
    assert_eq!(
        loaded_value(&app, "broken"),
        None,
        "broken file must not produce a usable asset"
    );

    // Disabled file is skipped completely.
    assert!(
        !in_library(&app, "_disabled"),
        "disabled (_) files must be skipped"
    );

    // The loader reports completion even though one file failed.
    assert!(
        app.world()
            .resource::<AssetFolderHandle<ThingId, Thing>>()
            .is_loaded()
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Regression: a folder of entirely valid files still loads normally.
#[test]
fn all_valid_files_load() {
    let root = unique_asset_root();
    write(&root, "things/one.thing.ron", "(value: 10)");
    write(&root, "things/two.thing.ron", "(value: 20)");

    let mut app = build_app(&root);
    let ok = run_until(&mut app, 1000, |app| {
        loaded_value(app, "one").is_some() && loaded_value(app, "two").is_some()
    });
    assert!(ok, "all valid files should load");

    assert_eq!(loaded_value(&app, "one"), Some(10));
    assert_eq!(loaded_value(&app, "two"), Some(20));
    assert_eq!(
        app.world().resource::<AssetFolder<ThingId, Thing>>().len(),
        2
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// An empty (or absent) folder must not panic; the loader simply reports it has
/// completed with nothing to show.
#[test]
fn empty_folder_is_handled() {
    let root = unique_asset_root();
    std::fs::create_dir_all(root.join("things")).unwrap();

    let mut app = build_app(&root);
    let ok = run_until(&mut app, 200, |app| {
        app.world()
            .resource::<AssetFolderHandle<ThingId, Thing>>()
            .is_loaded()
    });
    assert!(ok, "loader should complete even for an empty folder");
    assert!(
        app.world()
            .resource::<AssetFolder<ThingId, Thing>>()
            .is_empty()
    );

    let _ = std::fs::remove_dir_all(&root);
}
