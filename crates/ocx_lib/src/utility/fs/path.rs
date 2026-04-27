// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Lexical path helpers shared across archive extraction, symlink validation,
//! and the layer assembly walker.
//!
//! These helpers operate **without filesystem access** and are designed for
//! pre-flight containment checks. They are not a substitute for
//! [`dunce::canonicalize`], which follows symlinks and reads the filesystem.
//! Use this module for validating paths (possibly not yet on disk) that must
//! not escape a logical root; use `dunce::canonicalize` for resolving paths
//! that already exist.

use std::path::{Component, Path, PathBuf};

use crate::prelude::*;

/// Lexically normalizes a path by resolving `.` and `..` components
/// **without filesystem access**.
///
/// Preserves leading `..` components when they would escape past the logical
/// root, so that [`escapes_root`] can detect them.
///
/// Backslashes (`\`) are treated as path separators on all platforms.
/// This ensures that Windows-style paths embedded in archive metadata or
/// user input are correctly split into components even on Unix build hosts.
pub fn lexical_normalize(path: &Path) -> PathBuf {
    // Pre-pass: normalize backslash separators to forward slash so that
    // `Path::components()` splits them correctly on all platforms.
    // On Linux, `\` is a legal filename character, not a separator, so
    // without this step `foo\bar` would be a single component.
    let normalized_sep;
    let path: &Path = if path.as_os_str().as_encoded_bytes().contains(&b'\\') {
        let s = path.to_string_lossy().replace('\\', "/");
        normalized_sep = PathBuf::from(s);
        &normalized_sep
    } else {
        path
    };

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                } else if !matches!(components.last(), Some(Component::RootDir | Component::Prefix(_))) {
                    components.push(component);
                }
            }
            Component::CurDir => {}
            other => components.push(other),
        }
    }
    components.iter().collect()
}

/// Returns `true` if the lexically normalized path contains any `..`
/// components, meaning it escapes its logical root.
pub fn escapes_root(path: &Path) -> bool {
    lexical_normalize(path)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}

/// Recursively validates that every symlink under `dir` resolves within
/// `root` via [`crate::symlink::validate_target`].
///
/// Used as a defence-in-depth sweep after archive extraction and wherever a
/// layer population path might bypass the archive extractor.
pub fn validate_symlinks_in_dir(root: &Path, dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| crate::archive::Error::Io {
        path: dir.to_path_buf(),
        source: e,
    })? {
        let entry = entry.map_err(|e| crate::archive::Error::Io {
            path: dir.to_path_buf(),
            source: e,
        })?;
        let ft = entry.file_type().map_err(|e| crate::archive::Error::Io {
            path: entry.path(),
            source: e,
        })?;
        if ft.is_symlink() {
            let target = std::fs::read_link(entry.path()).map_err(|e| crate::archive::Error::Io {
                path: entry.path(),
                source: e,
            })?;
            crate::symlink::validate_target(root, &entry.path(), &target)?;
        } else if ft.is_dir() {
            validate_symlinks_in_dir(root, &entry.path())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_normalize_resolves_dot() {
        assert_eq!(lexical_normalize(Path::new("a/./b")), PathBuf::from("a/b"));
    }

    #[test]
    fn lexical_normalize_resolves_parent() {
        assert_eq!(lexical_normalize(Path::new("a/../b")), PathBuf::from("b"));
    }

    #[test]
    fn lexical_normalize_resolves_mixed() {
        assert_eq!(lexical_normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
    }

    #[test]
    fn lexical_normalize_clamps_at_root() {
        assert_eq!(lexical_normalize(Path::new("/a/../../b")), PathBuf::from("/b"));
    }

    #[test]
    fn lexical_normalize_preserves_escaping_parent() {
        assert_eq!(lexical_normalize(Path::new("../../x")), PathBuf::from("../../x"));
    }

    #[test]
    fn lexical_normalize_empty() {
        assert_eq!(lexical_normalize(Path::new("")), PathBuf::new());
    }

    #[test]
    fn escapes_root_detects_leading_parent() {
        assert!(escapes_root(Path::new("../outside")));
    }

    #[test]
    fn escapes_root_allows_internal_parent() {
        assert!(!escapes_root(Path::new("a/../b")));
    }

    #[test]
    fn escapes_root_rejects_parent_past_absolute_root() {
        // Absolute path `..` clamps, so this is safe.
        assert!(!escapes_root(Path::new("/a/../b")));
    }

    // ── Backslash separator tests ────────────────────────────────────────────

    #[test]
    fn lexical_normalize_backslash_parent_collapses() {
        // `foo\..\bar` must collapse to `bar` even on Linux.
        assert_eq!(lexical_normalize(Path::new("foo\\..\\bar")), PathBuf::from("bar"));
    }

    #[test]
    fn escapes_root_backslash_escape_detected() {
        // `..\..\secret` escapes root regardless of separator.
        assert!(escapes_root(Path::new("..\\..\\secret")));
    }

    #[test]
    fn lexical_normalize_backslash_two_components() {
        // `foo\bar` must be two components yielding `foo/bar`, not a single
        // component named `foo\bar`.
        let result = lexical_normalize(Path::new("foo\\bar"));
        let mut iter = result.components();
        assert_eq!(iter.next(), Some(Component::Normal("foo".as_ref())));
        assert_eq!(iter.next(), Some(Component::Normal("bar".as_ref())));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn lexical_normalize_mixed_separators_three_components() {
        // `foo\bar/baz` — mix of `\` and `/` — must yield three components.
        let result = lexical_normalize(Path::new("foo\\bar/baz"));
        let components: Vec<_> = result.components().collect();
        assert_eq!(components.len(), 3);
        assert_eq!(components[0], Component::Normal("foo".as_ref()));
        assert_eq!(components[1], Component::Normal("bar".as_ref()));
        assert_eq!(components[2], Component::Normal("baz".as_ref()));
    }

    #[test]
    fn lexical_normalize_forward_slash_unchanged() {
        // Forward-slash behaviour must be identical to before this change.
        assert_eq!(lexical_normalize(Path::new("a/b/../c")), PathBuf::from("a/c"));
    }
}
