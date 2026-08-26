# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **`FolderLoaderPlugin` no longer registers with the `LoadedFolders` gate via
  the untyped `LoadedFolder` handle's recursive dependency state.** That state
  does not settle for every asset type in every environment — decoded audio
  samples loaded generically via `AssetServer::load_folder` have been observed
  to sit in `Loading` forever on a machine with no audio device, even though
  the same files load fine when `FolderLoaderPlugin`'s own per-file scan
  requests them by their concrete type. The gate now holds closed until that
  per-file scan itself reports done, via two new `LoadedFolders` methods:
  `watch_external` (returns an `ExternalWatchId`, registered the moment a
  loader starts) and `mark_external_ready` (called once its own scan
  completes), with `external_count`/`external_ready_count` alongside the
  existing `seen_count`/`settled_count` for progress displays. `LoadedFolders::watch`
  is unchanged and still the right choice for a folder loaded directly via
  `AssetServer::load_folder` with no `FolderLoaderPlugin` in front of it.

### Added

- **Dual Bevy support: 0.18 and 0.19 from one branch.** The Bevy major is now
  selected by mutually exclusive cargo features — `bevy_0_19` (default) and
  `bevy_0_18` — backed by package-renamed optional dependencies, so only the
  selected major is ever compiled (a toolchain that cannot build one major is
  never exposed to it). The public API is identical under both features; the
  active engine crate is re-exported as `msg_load_folder::bevy`. Bevy 0.18
  consumers depend on the crate with `default-features = false` and
  `features = ["bevy_0_18"]`.
  - `bevy_common_assets` (used by the examples and integration tests) follows
    the same selection: `0.17` under `bevy_0_19`, `0.15` under `bevy_0_18`.
  - A new `file_watcher` feature forwards Bevy's real OS file watcher to
    whichever major is active; the ignored end-to-end watcher test now runs
    with `cargo test --test file_watcher --features file_watcher -- --ignored`.
  - The only observable engine divergence for this crate is Bevy 0.19's
    resources-as-components reflection (`#[reflect(Resource)]` registering
    `ReflectComponent` vs 0.18's `ReflectResource`); it is covered by
    per-version regression tests and does not affect the crate's API.

## [0.4.0] - 2026-07-09

Upgrade to **Bevy 0.19**.

### Changed

- **Bevy 0.19 support.** Bumped `bevy` from `0.18` to `0.19` and the dev
  dependency `bevy_common_assets` from `0.15` to `0.17`. The public API is
  unchanged; existing user code compiles as-is.
  - The crate already used Bevy's message API (`MessageReader`,
    `MessageWriter`, `write_message`), which carries forward unchanged in 0.19.
  - Under Bevy 0.19's *resources-as-components* model, `#[derive(Resource)]`
    now also implements `Component` and `#[reflect(Resource)]` reflects the
    `Component` trait via `ReflectComponent`. `AssetFolder` and
    `AssetFolderHandle` continue to be used purely as resources; no changes are
    required in downstream code.
  - Requires Rust `1.95.0` or newer (a Bevy 0.19 requirement).

### Fixed

- **Stale contents after remove-then-re-add.** When a file was removed and then
  re-added at the same path, the library could serve the file's previous,
  cached contents because dropping the handle on removal does not evict the
  asset from Bevy's asset server. The loader now detects a re-added path (the
  asset server reports it already settled the instant it is loaded) and forces a
  reload, so the freshly written file is always read from disk. First-time loads
  are unaffected.

### Added

- **Expanded add/remove robustness tests** (`tests/hot_reload.rs`), covering:
  adding many files in one scan, removing every file, add → remove → re-add
  churn, interleaved add-and-remove within a single scan, handle stability of
  untouched entries across structural changes, discovery of files added in
  nested subdirectories, dropping a removed *broken* file, and no-op signals.
- **Real OS file-watcher end-to-end test** (`tests/file_watcher.rs`, behind the
  `file_watcher` dev feature). It drives the full pipeline — OS event → Bevy
  watcher → folder reload → rescan → library update — for edit, add, remove, and
  re-add. It is `#[ignore]`d by default (OS watching is timing-dependent and
  unavailable on some CI runners); run it with
  `cargo test --test file_watcher -- --ignored`.
- **Bevy 0.19 regression tests** locking in that a `#[reflect(Resource)]` type
  registers `ReflectComponent` and that `#[derive(Resource)]` types still behave
  as plain resources under the component-backed model.

## [0.3.1] - 2026-07-09

- Public release of the resilient, hot-reloading folder loader on Bevy 0.18.

## [0.3.0] - Bevy 0.18

### Added

- Resilient per-file loading: a single malformed file no longer blocks the rest
  of the folder from loading.
- Hot reloading of edited, added, and removed files when asset watching is
  enabled.
- `with_extension()` builder for loading folders with multiple file formats.

### Fixed

- Guarded `init_asset` so a second `FolderLoaderPlugin` sharing an asset type can
  no longer wipe the shared `Assets<A>` collection.
- Keyed `AssetFolderHandle` by `(Id, A)` to prevent collisions between folder
  loaders that share the same asset type.

## [0.2.0] - Bevy 0.17

- Bevy 0.17 support.

## [0.1.0] - Bevy 0.16

- Initial release with Bevy 0.16 support.

[0.4.0]: https://github.com/MolecularSadism/msg_load_folder/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/MolecularSadism/msg_load_folder/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/MolecularSadism/msg_load_folder/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/MolecularSadism/msg_load_folder/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/MolecularSadism/msg_load_folder/releases/tag/v0.1.0
