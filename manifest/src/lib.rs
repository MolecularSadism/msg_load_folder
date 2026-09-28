//! The `dir.manifest` contract between a web build's packaging step and
//! `msg_load_folder`'s folder scanner.
//!
//! Asset readers that cannot list a directory (Bevy's web/HTTP reader) make
//! the scanner fall back to a [`DIR_MANIFEST_FILE`] inside that directory:
//! one child name per line, subdirectories with a trailing `/`.
//! [`write_dir_manifests`] produces those files for a staged asset tree and
//! [`parse_manifest`] is what the scanner reads them with, so both sides of
//! the format live here. The crate is std-only, which keeps it usable from
//! build scripts and packaging tools that do not compile Bevy.

use std::fs;
use std::io;
use std::path::Path;

/// Name of the manifest file listing a directory's immediate children. It
/// avoids `msg_load_folder`'s hidden/disabled (`.`/`_`-prefix) convention, so
/// asset-stripping tooling that follows that convention keeps it.
pub const DIR_MANIFEST_FILE: &str = "dir.manifest";

/// One child listed in a manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ManifestEntry<'a> {
    /// The child's file or directory name, relative to the manifest's directory.
    pub name: &'a str,
    /// Whether the child is a subdirectory.
    pub is_dir: bool,
}

/// Parses manifest text into its entries. Lines are trimmed and blank lines
/// skipped; a trailing `/` marks a subdirectory.
pub fn parse_manifest(text: &str) -> impl Iterator<Item = ManifestEntry<'_>> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| match line.strip_suffix('/') {
            Some(name) => ManifestEntry { name, is_dir: true },
            None => ManifestEntry {
                name: line,
                is_dir: false,
            },
        })
}

/// Writes a [`DIR_MANIFEST_FILE`] into `dir` and every directory beneath it,
/// replacing any existing one. Entries are sorted by name and never list the
/// manifest itself. Symlinks are followed, as a web server serving the tree
/// would.
///
/// # Errors
///
/// Fails on any I/O error, and with [`io::ErrorKind::InvalidData`] on a child
/// name [`parse_manifest`] could not read back unchanged: non-UTF-8, or
/// containing a line break or leading/trailing whitespace.
pub fn write_dir_manifests(dir: &Path) -> io::Result<()> {
    let mut children = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().into_string().map_err(|name| {
            invalid_name(
                &entry.path(),
                &format!("{} is not valid UTF-8", name.display()),
            )
        })?;
        if name == DIR_MANIFEST_FILE {
            continue;
        }
        if name.contains(['\n', '\r']) || name.trim() != name {
            return Err(invalid_name(
                &entry.path(),
                "a line break or leading/trailing whitespace cannot round-trip through a manifest",
            ));
        }
        let is_dir = fs::metadata(entry.path())?.is_dir();
        children.push((name, is_dir));
    }
    children.sort_unstable();

    let mut manifest = String::new();
    for (name, is_dir) in &children {
        manifest.push_str(name);
        if *is_dir {
            manifest.push('/');
            write_dir_manifests(&dir.join(name))?;
        }
        manifest.push('\n');
    }
    fs::write(dir.join(DIR_MANIFEST_FILE), manifest)
}

fn invalid_name(path: &Path, reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "cannot list '{}' in a {DIR_MANIFEST_FILE}: {reason}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_root() -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "msg_load_folder_manifest_{}_{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("has parent")).expect("create parent");
        fs::write(path, "").expect("write file");
    }

    /// Children of `dir` as the filesystem reports them, manifest excluded.
    fn listed(dir: &Path) -> BTreeSet<(String, bool)> {
        fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| entry.expect("dir entry"))
            .filter(|entry| entry.file_name() != DIR_MANIFEST_FILE)
            .map(|entry| {
                (
                    entry.file_name().into_string().expect("utf-8 name"),
                    entry.path().is_dir(),
                )
            })
            .collect()
    }

    /// Children of `dir` as its written manifest reports them.
    fn parsed(dir: &Path) -> BTreeSet<(String, bool)> {
        let text = fs::read_to_string(dir.join(DIR_MANIFEST_FILE)).expect("manifest written");
        parse_manifest(&text)
            .map(|entry| (entry.name.to_owned(), entry.is_dir))
            .collect()
    }

    fn assert_round_trips(dir: &Path) {
        assert_eq!(parsed(dir), listed(dir), "manifest of {}", dir.display());
        for (name, is_dir) in listed(dir) {
            if is_dir {
                assert_round_trips(&dir.join(name));
            }
        }
    }

    #[test]
    fn written_manifests_parse_back_to_every_directory_listing() {
        let root = temp_root();
        touch(&root, "a.spell.ron");
        touch(&root, "b.item.ron");
        touch(&root, "_disabled.spell.ron");
        touch(&root, "nested/deeper/c.spell.ron");
        touch(&root, "nested/with space.ron");
        fs::create_dir_all(root.join("empty")).expect("create empty dir");

        write_dir_manifests(&root).expect("write manifests");

        assert_round_trips(&root);
        assert!(parsed(&root.join("empty")).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rewriting_replaces_a_stale_manifest_and_never_lists_itself() {
        let root = temp_root();
        touch(&root, "kept.ron");
        fs::write(root.join(DIR_MANIFEST_FILE), "gone.ron\n").expect("stale manifest");

        write_dir_manifests(&root).expect("write manifests");
        write_dir_manifests(&root).expect("rewrite manifests");

        assert_eq!(
            fs::read_to_string(root.join(DIR_MANIFEST_FILE)).expect("manifest"),
            "kept.ron\n"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn output_is_sorted_with_trailing_slash_on_directories() {
        let root = temp_root();
        touch(&root, "zeta.ron");
        touch(&root, "alpha/inner.ron");
        touch(&root, "mid.ron");

        write_dir_manifests(&root).expect("write manifests");

        assert_eq!(
            fs::read_to_string(root.join(DIR_MANIFEST_FILE)).expect("manifest"),
            "alpha/\nmid.ron\nzeta.ron\n"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn names_that_cannot_round_trip_are_rejected() {
        for bad in ["line\nbreak.ron", " leading.ron", "trailing.ron "] {
            let root = temp_root();
            touch(&root, bad);

            let err = write_dir_manifests(&root).expect_err(bad);
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{bad:?}");
            let _ = fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn parse_trims_skips_blank_lines_and_marks_directories() {
        let entries: Vec<_> = parse_manifest("  a.ron \n\nsub/\r\n").collect();
        assert_eq!(
            entries,
            [
                ManifestEntry {
                    name: "a.ron",
                    is_dir: false
                },
                ManifestEntry {
                    name: "sub",
                    is_dir: true
                },
            ]
        );
    }

    #[test]
    fn missing_directory_is_an_error() {
        let root = temp_root();
        let err = write_dir_manifests(&root.join("absent")).expect_err("no such dir");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(&root);
    }
}
