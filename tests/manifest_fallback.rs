//! Integration test for the `.dir_manifest` fallback that lets folder
//! scanning work on asset readers that cannot list directories — the
//! situation on Bevy's web/wasm `AssetReader`, which has no protocol-level
//! way to enumerate a URL's contents, so `read_directory` there always comes
//! back with an empty (but `Ok`) stream.
//!
//! A hand-rolled `AssetReader` here reproduces that exact behavior against a
//! real temp-dir asset root: `read_directory` always answers empty, while
//! everything else (reading files, checking `is_directory`) delegates to the
//! real filesystem reader. This exercises the actual scan path end to end
//! through a real `App`/`AssetServer`, rather than only unit-testing the
//! manifest-parsing logic in isolation.

#[cfg(feature = "bevy_0_18")]
extern crate bevy018 as bevy;
#[cfg(feature = "bevy_0_18")]
extern crate bevy_common_assets018 as bevy_common_assets;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bevy::asset::AssetApp;
use bevy::asset::io::file::FileAssetReader;
use bevy::asset::io::{
    AssetReader, AssetReaderError, AssetSourceBuilder, AssetSourceId, ErasedAssetReader,
    PathStream, Reader,
};
use bevy::prelude::*;
use bevy::tasks::futures_lite;
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
// A reader that cannot list directories, exactly like Bevy's web reader
// =============================================================================

/// Wraps the real filesystem reader but always reports a directory listing
/// as empty — the observable behavior of Bevy's web/wasm `AssetReader`, which
/// has no way to ask a plain HTTP server what lives under a URL. Everything
/// else delegates to the real reader, so file content and `is_directory`
/// checks behave exactly as they do natively.
struct NoListingReader(FileAssetReader);

impl AssetReader for NoListingReader {
    async fn read<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        AssetReader::read(&self.0, path).await
    }

    async fn read_meta<'a>(&'a self, path: &'a Path) -> Result<impl Reader + 'a, AssetReaderError> {
        AssetReader::read_meta(&self.0, path).await
    }

    // Always empty (never awaits), by design — that's the exact web/wasm behavior under test.
    #[allow(clippy::unused_async_trait_impl)]
    async fn read_directory<'a>(
        &'a self,
        _path: &'a Path,
    ) -> Result<Box<PathStream>, AssetReaderError> {
        Ok(Box::new(futures_lite::stream::empty()))
    }

    async fn is_directory<'a>(&'a self, path: &'a Path) -> Result<bool, AssetReaderError> {
        AssetReader::is_directory(&self.0, path).await
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn unique_asset_root() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "msg_load_folder_manifest_it_{}_{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create temp asset root");
    dir
}

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(path, contents).expect("write asset file");
}

/// Builds a headless app whose default asset source cannot list directories,
/// wired up to load `Thing`s from `<root>/things`.
fn build_app(root: &Path) -> App {
    let mut app = App::new();

    let reader_root = root.to_path_buf();
    app.register_asset_source(
        AssetSourceId::Default,
        AssetSourceBuilder::new(move || {
            Box::new(NoListingReader(FileAssetReader::new(reader_root.clone())))
                as Box<dyn ErasedAssetReader>
        }),
    );

    app.add_plugins(MinimalPlugins)
        .add_plugins(AssetPlugin {
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

// =============================================================================
// Tests
// =============================================================================

/// Without a `.dir_manifest`, a reader that cannot list directories sees
/// nothing — this reproduces the original bug report exactly: the loader
/// still completes (no hang, no error), just with an empty library.
#[test]
fn no_manifest_means_nothing_loads() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");

    let mut app = build_app(&root);
    let ok = run_until(&mut app, 200, |app| {
        app.world()
            .resource::<AssetFolderHandle<ThingId, Thing>>()
            .is_loaded()
    });
    assert!(ok, "loader should still complete, just with nothing found");
    assert!(
        app.world()
            .resource::<AssetFolder<ThingId, Thing>>()
            .is_empty(),
        "without a manifest, a non-listing reader must see no files"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// With a `.dir_manifest` alongside the files, the same non-listing reader
/// discovers and loads them — the actual fix under test.
#[test]
fn manifest_makes_folder_loading_work() {
    let root = unique_asset_root();
    write(&root, "things/alpha.thing.ron", "(value: 1)");
    write(&root, "things/beta.thing.ron", "(value: 2)");
    write(
        &root,
        "things/.dir_manifest",
        "alpha.thing.ron\nbeta.thing.ron\n",
    );

    let mut app = build_app(&root);
    let ok = run_until(&mut app, 1000, |app| {
        loaded_value(app, "alpha").is_some() && loaded_value(app, "beta").is_some()
    });
    assert!(
        ok,
        "manifest-listed files should load through a non-listing reader"
    );

    assert_eq!(loaded_value(&app, "alpha"), Some(1));
    assert_eq!(loaded_value(&app, "beta"), Some(2));

    let _ = std::fs::remove_dir_all(&root);
}

/// A manifest entry ending in `/` names a subdirectory, and scanning
/// recurses into it via the same manifest mechanism.
#[test]
fn manifest_recurses_into_subdirectories() {
    let root = unique_asset_root();
    write(&root, "things/nested/gamma.thing.ron", "(value: 3)");
    write(&root, "things/.dir_manifest", "nested/\n");
    write(&root, "things/nested/.dir_manifest", "gamma.thing.ron\n");

    let mut app = build_app(&root);
    let ok = run_until(&mut app, 1000, |app| loaded_value(app, "gamma").is_some());
    assert!(ok, "nested manifest-listed files should load");
    assert_eq!(loaded_value(&app, "gamma"), Some(3));

    let _ = std::fs::remove_dir_all(&root);
}
