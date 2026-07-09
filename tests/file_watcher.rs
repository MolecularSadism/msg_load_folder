//! End-to-end test against Bevy's *real* OS file watcher.
//!
//! Unlike `hot_reload.rs` — which drives the exact triggers Bevy's watcher
//! produces (a per-file `reload`, a folder `AssetEvent`) directly, so it is fast
//! and deterministic — this test enables Bevy's `file_watcher` feature and
//! mutates real files on disk, proving the *whole* pipeline works: OS event →
//! Bevy watcher → folder reload → our rescan → library update.
//!
//! OS file watching is inherently timing-dependent (and unavailable on some
//! sandboxed CI runners), so this test is `#[ignore]`d by default. Run it
//! explicitly with:
//!
//! ```text
//! cargo test --test file_watcher -- --ignored --nocapture
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

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
    let dir = std::env::temp_dir().join(format!("msg_load_folder_fw_{}_{n}", std::process::id()));
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
            // Turn on the real OS watcher.
            watch_for_changes_override: Some(true),
            ..default()
        })
        .add_plugins(RonAssetPlugin::<Thing>::new(&["thing.ron"]))
        .add_plugins(FolderLoaderPlugin::<ThingId, Thing>::new(
            "things",
            ".thing.ron",
        ));
    app
}

/// Pumps the app for up to ~15s, giving the OS watcher generous time to deliver
/// events. Returns whether `cond` held.
fn run_until(app: &mut App, cond: impl Fn(&App) -> bool) -> bool {
    for _ in 0..1500 {
        app.update();
        if cond(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
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

/// The full lifecycle against the real watcher: initial load, edit, add, remove,
/// and re-add — all driven purely by writing to and deleting from disk.
#[test]
#[ignore = "depends on OS file watching; run with --ignored"]
fn real_watcher_handles_edit_add_and_remove() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");

    let mut app = build_app(&root);

    // Initial load.
    assert!(
        run_until(&mut app, |a| loaded_value(a, "alpha") == Some(1)),
        "alpha should load initially"
    );

    // Edit an existing file: reloaded in place behind its stable handle.
    write(&root, "things/alpha.thing.ron", "(value: 11)");
    assert!(
        run_until(&mut app, |a| loaded_value(a, "alpha") == Some(11)),
        "editing alpha should hot-reload its value"
    );

    // Add a brand-new file: discovered by the folder rescan.
    write(&root, "things/beta.thing.ron", "(value: 2)");
    assert!(
        run_until(&mut app, |a| loaded_value(a, "beta") == Some(2)),
        "adding beta should be discovered by the real watcher"
    );

    // Remove a file: dropped from the library.
    std::fs::remove_file(root.join("things/beta.thing.ron")).unwrap();
    assert!(
        run_until(&mut app, |a| !in_library(a, "beta")),
        "removing beta should drop it from the library"
    );

    // Re-add the same path with new contents: must load the *fresh* value, not
    // the stale cached one.
    write(&root, "things/beta.thing.ron", "(value: 22)");
    assert!(
        run_until(&mut app, |a| loaded_value(a, "beta") == Some(22)),
        "re-adding beta should load its new value, not the stale cached one"
    );

    // The untouched original survived the whole sequence.
    assert_eq!(loaded_value(&app, "alpha"), Some(11));

    let _ = std::fs::remove_dir_all(&root);
}
