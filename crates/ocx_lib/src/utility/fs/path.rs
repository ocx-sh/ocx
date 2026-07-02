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

/// Maximum number of path components a [`RelativePath`] may carry (W4). Bounds
/// synthetic-directory amplification driven by an untrusted output prefix.
const MAX_RELPATH_COMPONENTS: usize = 32;
/// Maximum byte length of any single [`RelativePath`] component (W4).
const MAX_RELPATH_COMPONENT_BYTES: usize = 255;
/// Maximum total byte length across all [`RelativePath`] components (W4).
const MAX_RELPATH_TOTAL_BYTES: usize = 4096;

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

/// Reason an untrusted relative path was rejected as unsafe to join under a
/// containment root. Produced by [`join_under_root`] and [`RelativePath::parse`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PathEscapeError {
    /// The path is absolute (e.g. `/etc`); only relative paths may be joined.
    #[error("path must be relative")]
    Absolute,
    /// The path carries a Windows drive-letter (`C:\…`), UNC (`\\srv\share`),
    /// or verbatim (`\\?\…`) prefix. Rejected host-independently — a Linux
    /// publish host would otherwise let such a prefix through to escape a
    /// Windows read host.
    #[error("Windows drive-letter or UNC/verbatim path is not allowed")]
    WindowsPrefix,
    /// The path escapes the containment root via residual `..` traversal.
    #[error("path escapes the containment root")]
    Escapes,
    /// The path exceeds the allowed component-count or length bound, so an
    /// untrusted value cannot drive synthetic-directory amplification (W4).
    #[error("path exceeds the allowed component or length bound")]
    TooLong,
    /// A path component contains a control character. A hostile annotation
    /// could embed a newline or NUL to forge log lines when the value is later
    /// echoed (CWE-117), mirroring the sanitization already applied to strip
    /// annotations.
    #[error("path contains a control character")]
    ControlCharacter,
}

impl crate::cli::ClassifyExitCode for PathEscapeError {
    fn classify(&self) -> Option<crate::cli::ExitCode> {
        // A rejected containment path is malformed input data — surfaced when a
        // hostile manifest annotation is re-validated at read time (65). Publish
        // (CLI parse) wraps it in `LayerRefParseError`, which maps to 64.
        Some(crate::cli::ExitCode::DataError)
    }
}

/// Joins an untrusted relative path under `root`, guaranteeing the result stays
/// contained within `root`.
///
/// The check is **lexical** (no filesystem access) and **host-independent**:
/// Windows drive-letter / UNC / verbatim prefixes are rejected on every
/// platform, not just Windows, because [`Path::is_absolute`] only parses those
/// prefixes on Windows. See the module doc for why lexical (not
/// [`dunce::canonicalize`]) is the right tool for a not-yet-on-disk path.
///
/// # Errors
///
/// Returns [`PathEscapeError`] when `untrusted_relative` is absolute, carries a
/// Windows prefix, or escapes `root` via `..`.
pub fn join_under_root(root: &Path, untrusted_relative: &Path) -> std::result::Result<PathBuf, PathEscapeError> {
    // 1. Host-independent Windows drive-letter / UNC / verbatim rejection FIRST.
    //    `//srv/x` is `is_absolute() == true` on Unix, so it must be caught here
    //    as `WindowsPrefix` before the generic `is_absolute` check below would
    //    misclassify it as `Absolute`.
    if has_windows_prefix(untrusted_relative) {
        return Err(PathEscapeError::WindowsPrefix);
    }

    // 2. A single-slash absolute path (`/etc`) is not a relative path.
    if untrusted_relative.is_absolute() {
        return Err(PathEscapeError::Absolute);
    }

    // 3. Fold `.` / `..` lexically (backslash-aware) without touching the disk.
    let normalized = lexical_normalize(untrusted_relative);

    // Empty and `.`-only inputs normalize to the empty path — they denote the
    // containment root itself.
    if normalized.as_os_str().is_empty() {
        return Ok(root.to_path_buf());
    }

    // 4. Any residual leading `..` escapes the root.
    if escapes_root(&normalized) {
        return Err(PathEscapeError::Escapes);
    }

    // 5. Belt-and-suspenders: join and re-verify the result stays under `root`.
    let joined = root.join(&normalized);
    let normalized_root = lexical_normalize(root);
    if !lexical_normalize(&joined).starts_with(&normalized_root) {
        return Err(PathEscapeError::Escapes);
    }
    Ok(joined)
}

/// Returns `true` when `path` begins with a Windows drive-letter (`C:`),
/// UNC/verbatim prefix (`\\`, `//`), or any two-separator lead.
///
/// The check is byte-level on the raw `OsStr` so it is **host-independent**:
/// [`Path::is_absolute`] only parses Windows prefixes on Windows, so a Linux
/// publish host would otherwise let `C:\Windows` or `\\srv\share` through to a
/// Windows read host.
fn has_windows_prefix(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    // Drive-letter: `[A-Za-z]:` (e.g. `C:\Windows`).
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    // UNC (`\\srv\share`), verbatim (`\\?\C:\x`), or `//srv/x` — any path that
    // leads with two path separators (either slash flavour).
    let is_sep = |b: u8| b == b'\\' || b == b'/';
    bytes.len() >= 2 && is_sep(bytes[0]) && is_sep(bytes[1])
}

/// A validated, non-escaping, **bounded** relative path. Its [`Default`] is the
/// empty path (the containment root itself).
///
/// Used for an output prefix supplied by an untrusted manifest annotation or a
/// CLI layer-ref: the component-count / length bound (W4) prevents an oversized
/// value from amplifying into unbounded synthetic directories during assembly.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelativePath(PathBuf);

impl RelativePath {
    /// Parses `s` into a bounded, non-escaping relative path.
    ///
    /// Applies the same absolute / Windows-prefix / escape checks as
    /// [`join_under_root`] plus a component-count and length bound.
    ///
    /// # Errors
    ///
    /// Returns [`PathEscapeError`] when `s` is absolute, Windows-prefixed,
    /// escapes the root, or exceeds the bound ([`PathEscapeError::TooLong`]).
    pub fn parse(s: &str) -> std::result::Result<Self, PathEscapeError> {
        let path = Path::new(s);

        // Steps 1-2 of `join_under_root`: reject Windows prefixes host-
        // independently, then reject absolute paths.
        if has_windows_prefix(path) {
            return Err(PathEscapeError::WindowsPrefix);
        }
        if path.is_absolute() {
            return Err(PathEscapeError::Absolute);
        }

        // Steps 3-4: normalize, then reject residual `..` escapes.
        let normalized = lexical_normalize(path);
        if escapes_root(&normalized) {
            return Err(PathEscapeError::Escapes);
        }

        // W4 bound: cap component count and per-component / total byte length so
        // an untrusted annotation cannot drive synthetic-directory amplification
        // during assembly. Only `Normal` components remain after normalization
        // (no root, `.`, or escaping `..`).
        let mut components = 0usize;
        let mut total = 0usize;
        for component in normalized.components() {
            let os = component.as_os_str();
            // Reject control characters (CWE-117): the input originates from an
            // untrusted CLI layer-ref or manifest annotation, so a newline or NUL
            // must not survive to a later error message or log line. The value is
            // valid UTF-8 (parsed from `&str`), so `to_string_lossy` is exact.
            if os.to_string_lossy().chars().any(|c| c.is_control()) {
                return Err(PathEscapeError::ControlCharacter);
            }
            let len = os.as_encoded_bytes().len();
            if len > MAX_RELPATH_COMPONENT_BYTES {
                return Err(PathEscapeError::TooLong);
            }
            components += 1;
            total += len;
        }
        if components > MAX_RELPATH_COMPONENTS || total > MAX_RELPATH_TOTAL_BYTES {
            return Err(PathEscapeError::TooLong);
        }

        Ok(RelativePath(normalized))
    }

    /// Returns the validated relative path.
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Returns `true` when this is the empty path (the containment root).
    pub fn is_empty(&self) -> bool {
        self.0.as_os_str().is_empty()
    }
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
    fn lexical_normalize_backslash_parent_no_trailing_segment() {
        // `foo\..` — parent-dir segment with no trailing component must
        // collapse to an empty path, not a literal single component named
        // `foo\..`. Without the collapse, `escapes_root` could miss it as a
        // current-directory equivalent.
        assert_eq!(lexical_normalize(Path::new("foo\\..")), PathBuf::new());
        assert!(!escapes_root(Path::new("foo\\..")));
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

    // ── Part 2 containment primitives: join_under_root + RelativePath (U12–U14) ────
    //
    // Cover `join_under_root` and `RelativePath::parse`: host-independent Windows
    // prefix rejection, absolute/escape classification, and the component/length
    // bound. Rows cite the plan Test Matrix tag (U#).

    /// U12 (Windows-drive-rejection-on-Linux · D8/D10): a Windows drive-letter,
    /// UNC, or verbatim prefix must be rejected as `WindowsPrefix` on THIS Linux
    /// host — the critical cross-platform bypass a Linux publisher could
    /// otherwise smuggle to a Windows read host. Both `join_under_root` and
    /// `RelativePath::parse` reject them. `//srv/x` leads with a double slash (UNC),
    /// so it classifies as `WindowsPrefix`, NOT the plain `Absolute` that a
    /// single-slash `/etc` gets in U13 — the discriminating ordering the
    /// implementer must honour.
    #[test]
    fn windows_prefixes_rejected_on_linux() {
        let root = Path::new("/root");
        for spec in ["C:\\Windows", "\\\\srv\\share", "\\\\?\\C:\\x", "//srv/x"] {
            assert!(
                matches!(
                    join_under_root(root, Path::new(spec)),
                    Err(PathEscapeError::WindowsPrefix)
                ),
                "join_under_root must reject {spec:?} as WindowsPrefix"
            );
            assert!(
                matches!(RelativePath::parse(spec), Err(PathEscapeError::WindowsPrefix)),
                "RelativePath::parse must reject {spec:?} as WindowsPrefix"
            );
        }
    }

    /// U13 (D8): `join_under_root` classifies a plain (single-slash) absolute
    /// path as `Absolute`, residual `..` as `Escapes`, resolves an internal
    /// `..` under the root, and treats empty / `.` as the root itself.
    #[test]
    fn join_under_root_classifies_and_normalizes() {
        let root = Path::new("/root");
        assert!(
            matches!(join_under_root(root, Path::new("/etc")), Err(PathEscapeError::Absolute)),
            "a single-slash absolute path must be Absolute, not WindowsPrefix"
        );
        assert!(
            matches!(join_under_root(root, Path::new("../x")), Err(PathEscapeError::Escapes)),
            "residual `..` traversal must be Escapes"
        );
        assert_eq!(
            join_under_root(root, Path::new("a/../b")).expect("internal `..` resolves"),
            root.join("b"),
            "an internal `..` resolves under the root"
        );
        assert_eq!(
            join_under_root(root, Path::new("")).expect("empty is root"),
            root.to_path_buf(),
            "empty relative path is the root itself"
        );
        assert_eq!(
            join_under_root(root, Path::new(".")).expect("`.` is root"),
            root.to_path_buf(),
            "`.` is the root itself"
        );
    }

    /// U14 (W4): `RelativePath::parse` bounds component count and length so an
    /// untrusted annotation cannot drive synthetic-directory amplification. A
    /// normal prefix parses and is preserved; an over-deep or over-long one is
    /// `TooLong`.
    #[test]
    fn relpath_parse_bounds_depth_and_length() {
        let ok = RelativePath::parse("share/lib").expect("a normal prefix parses");
        assert!(!ok.is_empty(), "a non-empty prefix must not report empty");
        assert_eq!(ok.as_path(), Path::new("share/lib"), "prefix preserved verbatim");

        // Well past any reasonable component-count bound.
        let deep = vec!["a"; 500].join("/");
        assert!(
            matches!(RelativePath::parse(&deep), Err(PathEscapeError::TooLong)),
            "an over-deep prefix must be TooLong"
        );

        // Well past any reasonable per-component / total length bound.
        let long = "x".repeat(5000);
        assert!(
            matches!(RelativePath::parse(&long), Err(PathEscapeError::TooLong)),
            "an over-long prefix must be TooLong"
        );
    }

    /// A component carrying a control character (newline / NUL) is rejected as
    /// `ControlCharacter` so a hostile annotation cannot forge log lines when the
    /// prefix is later echoed (CWE-117) — mirrors the strip-annotation sanitization.
    #[test]
    fn relpath_parse_rejects_control_characters() {
        for spec in ["share/lib\nrm -rf", "share/\u{0}evil", "a\tb"] {
            assert!(
                matches!(RelativePath::parse(spec), Err(PathEscapeError::ControlCharacter)),
                "a control character must be ControlCharacter: {spec:?}"
            );
        }
    }
}
