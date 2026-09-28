// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Lexical path helpers, without filesystem access, for containment checks on paths that may not exist yet.

use std::path::{Component, Path, PathBuf};

/// Bounds a [`RelativePath`] so an untrusted prefix cannot amplify into synthetic directories.
const MAX_RELPATH_COMPONENTS: usize = 32;
const MAX_RELPATH_COMPONENT_BYTES: usize = 255;
const MAX_RELPATH_TOTAL_BYTES: usize = 4096;

/// Lexically resolves `.` and `..`, keeping a leading `..` so [`escapes_root`] can see it.
///
/// `\` separates on every platform, so a Windows path in archive metadata splits on a Unix host too.
pub fn lexical_normalize(path: &Path) -> PathBuf {
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

/// Returns `true` if the lexically normalized path keeps a `..`, escaping its logical root.
pub fn escapes_root(path: &Path) -> bool {
    lexical_normalize(path)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
}

/// Why an untrusted relative path was refused under a containment root.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PathEscapeError {
    #[error("path must be relative")]
    Absolute,
    /// Refused on every host, or a Linux publish host lets it through to escape a Windows read host.
    #[error("Windows drive-letter or UNC/verbatim path is not allowed")]
    WindowsPrefix,
    #[error("path escapes the containment root")]
    Escapes,
    #[error("path exceeds the allowed component or length bound")]
    TooLong,
    /// A newline or NUL would forge log lines when the value is echoed (CWE-117).
    #[error("path contains a control character")]
    ControlCharacter,
}

/// Joins an untrusted relative path under `root`, lexically and host-independently, so the result stays inside.
///
/// # Errors
///
/// [`PathEscapeError`] when `untrusted_relative` is absolute, Windows-prefixed or escapes `root`.
pub fn join_under_root(root: &Path, untrusted_relative: &Path) -> std::result::Result<PathBuf, PathEscapeError> {
    // Before `is_absolute`, which on Unix would misclassify `//srv/x` as `Absolute`.
    if has_windows_prefix(untrusted_relative) {
        return Err(PathEscapeError::WindowsPrefix);
    }

    if has_leading_separator(untrusted_relative) || untrusted_relative.is_absolute() {
        return Err(PathEscapeError::Absolute);
    }

    let normalized = lexical_normalize(untrusted_relative);

    // Empty and `.`-only inputs denote the root itself.
    if normalized.as_os_str().is_empty() {
        return Ok(root.to_path_buf());
    }

    if escapes_root(&normalized) {
        return Err(PathEscapeError::Escapes);
    }

    // Belt-and-suspenders: re-verify the joined result stays under `root`.
    let joined = root.join(&normalized);
    let normalized_root = lexical_normalize(root);
    if !lexical_normalize(&joined).starts_with(&normalized_root) {
        return Err(PathEscapeError::Escapes);
    }
    Ok(joined)
}

/// Whether `path` leads with a drive letter (`C:`) or two separators (UNC, verbatim, `//srv`).
///
/// Byte-level so it holds on every host: [`Path::is_absolute`] parses these prefixes only on Windows.
fn has_windows_prefix(path: &Path) -> bool {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return true;
    }
    let is_sep = |b: u8| b == b'\\' || b == b'/';
    bytes.len() >= 2 && is_sep(bytes[0]) && is_sep(bytes[1])
}

/// Whether `path` leads with one separator (`/etc`), which Windows `is_absolute` reports as relative.
///
/// Check [`has_windows_prefix`] first, so a two-separator lead classifies as a Windows prefix.
fn has_leading_separator(path: &Path) -> bool {
    matches!(path.as_os_str().as_encoded_bytes().first(), Some(b'/' | b'\\'))
}

/// A validated, non-escaping, bounded relative path; its [`Default`] is the empty path (the root).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelativePath(PathBuf);

impl RelativePath {
    /// Parses `s` with [`join_under_root`]'s checks plus a component and length bound.
    ///
    /// # Errors
    ///
    /// [`PathEscapeError`] for any refused input.
    pub fn parse(s: &str) -> std::result::Result<Self, PathEscapeError> {
        let path = Path::new(s);

        if has_windows_prefix(path) {
            return Err(PathEscapeError::WindowsPrefix);
        }
        if has_leading_separator(path) || path.is_absolute() {
            return Err(PathEscapeError::Absolute);
        }

        let normalized = lexical_normalize(path);
        if escapes_root(&normalized) {
            return Err(PathEscapeError::Escapes);
        }

        let mut components = 0usize;
        let mut total = 0usize;
        for component in normalized.components() {
            let os = component.as_os_str();
            // `to_string_lossy` is exact: the value came from a `&str`.
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

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// The canonical `/`-separated wire form; `display()` would emit `\` on Windows and break the round-trip.
    pub fn to_wire(&self) -> String {
        self.0
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Returns `true` when this is the empty path (the containment root).
    pub fn is_empty(&self) -> bool {
        self.0.as_os_str().is_empty()
    }
}

/// Per-layer placement before the overlap merge: drop `strip` leading components, then place under `prefix`.
#[derive(Debug, Clone)]
pub struct LayerPlacement {
    pub strip: u8,
    pub prefix: RelativePath,
}

/// How a local-file reference was spelled; never `file:<path>`, which cosign reads as a literal name (`adr_key_reference_grammar.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spelling {
    /// The value is the path itself — `etc/acme.pub`, `/srv/ocx-index`.
    Bare,
    /// `file://<path>`, payload verbatim; the only spelling that can name a path containing `://`.
    FileUrl,
}

/// A local-file reference as an operator writes one: a bare path, or a `file://` URL.
///
/// No `as_path()`: each exit names a resolution policy, and the callers resolve relative references differently on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileReference<'a> {
    spelling: Spelling,
    /// The whole value for [`Spelling::Bare`], the text after `file://` for [`Spelling::FileUrl`].
    path: &'a str,
}

impl<'a> FileReference<'a> {
    /// Reads the spelling of `value`, matching `file://` case-insensitively; total, so each caller keeps its own errors.
    pub fn parse(value: &'a str) -> Self {
        match value.split_at_checked("file://".len()) {
            Some((prefix, path)) if prefix.eq_ignore_ascii_case("file://") => Self {
                spelling: Spelling::FileUrl,
                path,
            },
            _ => Self {
                spelling: Spelling::Bare,
                path: value,
            },
        }
    }

    /// A [`Spelling::Bare`] reference over an already-extracted payload; re-splitting would stop `file://file:x` naming `file:x`.
    pub const fn bare(path: &'a str) -> Self {
        Self {
            spelling: Spelling::Bare,
            path,
        }
    }

    /// The spelling this reference was written in.
    pub const fn spelling(&self) -> Spelling {
        self.spelling
    }

    /// Policy: take the path as written, relative to the working directory; for CLI and environment values.
    pub fn as_written(&self) -> &'a Path {
        Path::new(self.path)
    }

    /// Policy: a relative path resolves against `dir`, the declaring file's directory.
    ///
    /// "Relative" is `!has_root()`, not `is_relative()`, or Windows moves a driveless `/etc/x` onto `dir`'s drive.
    pub fn anchored_at(&self, dir: &Path) -> PathBuf {
        let path = self.as_written();
        if path.has_root() {
            path.to_path_buf()
        } else {
            dir.join(path)
        }
    }

    /// Policy: only an already-absolute `file:///<abs>` reference is acceptable; `None` otherwise.
    ///
    /// Tested on the leading `/`, never `has_root`, which on Windows accepts `C:/…`, the URL's authority.
    /// Trailing separators are trimmed, so `file:///` is `None`, not the filesystem root.
    pub fn absolute(&self) -> Option<&'a str> {
        let path = self.path.trim_end_matches('/');
        path.starts_with('/').then_some(path)
    }
}

/// Recursively validates that every symlink under `dir` resolves within `root`.
pub fn validate_symlinks_in_dir(root: &Path, dir: &Path) -> Result<(), crate::archive::Error> {
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
            crate::fs::symlink::validate_target(root, &entry.path(), &target)?;
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
        // Host-independent: a single leading separator (either flavour) is an
        // absolute path on every platform. `Path::is_absolute` misses this on
        // Windows (drive-relative), so `RelativePath::parse` must reject both
        // itself rather than accept `/etc` / `\etc` as a containable relative path.
        for spec in ["/etc", "\\etc"] {
            assert!(
                matches!(join_under_root(root, Path::new(spec)), Err(PathEscapeError::Absolute)),
                "a single-separator absolute path {spec:?} must be Absolute"
            );
            assert!(
                matches!(RelativePath::parse(spec), Err(PathEscapeError::Absolute)),
                "RelativePath::parse must reject {spec:?} as Absolute"
            );
        }
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

    /// `to_wire` renders the canonical `/`-separated form regardless of the host
    /// separator — the layer-ref grammar and the `sh.ocx.layer.prefix` annotation
    /// are platform-independent wire formats. On Windows the internal `PathBuf`
    /// stores `share\lib`; `to_wire` must still emit `share/lib` so the
    /// Display→FromStr round-trip and cross-platform annotation reads hold.
    /// Backslash-separated input normalizes to the same wire form on all hosts.
    #[test]
    fn relpath_to_wire_is_always_forward_slash() {
        for spec in ["share/lib", "share\\lib", "a/b/c"] {
            let wire = RelativePath::parse(spec).expect("a normal prefix parses").to_wire();
            assert!(
                !wire.contains('\\'),
                "wire form must not carry a backslash: {spec:?} → {wire:?}"
            );
        }
        assert_eq!(RelativePath::parse("share/lib").unwrap().to_wire(), "share/lib");
        assert_eq!(RelativePath::parse("share\\lib").unwrap().to_wire(), "share/lib");
        assert_eq!(RelativePath::parse("a").unwrap().to_wire(), "a");
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

    // ── FileReference (#379) ─────────────────────────────────────────────────

    /// The whole grammar, in one table: two spellings and nothing else.
    /// `file:` with a single colon is deliberately **not** a third — it is a
    /// bare path whose first component happens to start with `file:`, exactly
    /// as it is to cosign (`adr_key_reference_grammar.md`).
    #[test]
    fn file_reference_reads_two_spellings_and_nothing_else() {
        for (value, spelling, path) in [
            ("etc/acme.pub", Spelling::Bare, "etc/acme.pub"),
            ("/srv/ocx-index", Spelling::Bare, "/srv/ocx-index"),
            ("C:\\keys\\acme.pub", Spelling::Bare, "C:\\keys\\acme.pub"),
            ("file:etc/acme.pub", Spelling::Bare, "file:etc/acme.pub"),
            ("awskms:us-east-1/abc", Spelling::Bare, "awskms:us-east-1/abc"),
            ("", Spelling::Bare, ""),
            ("file:///srv/x", Spelling::FileUrl, "/srv/x"),
            // Case-insensitive, matching the `index` door's own `scheme_of`.
            ("FILE:///srv/x", Spelling::FileUrl, "/srv/x"),
            ("file://etc/acme.pub", Spelling::FileUrl, "etc/acme.pub"),
            // The escape the spelling exists for: a path containing `://`.
            ("file://./weird://name", Spelling::FileUrl, "./weird://name"),
            ("file://", Spelling::FileUrl, ""),
        ] {
            let reference = FileReference::parse(value);
            assert_eq!(reference.spelling(), spelling, "{value:?}");
            assert_eq!(reference.as_written(), Path::new(path), "{value:?}");
        }
    }

    /// `KeyRef` consumes `<scheme>://` itself and hands the remainder here, so
    /// re-splitting it would break the one escape the `file://` spelling
    /// provides: `--key file://file:x` names a file called `file:x`, and by the
    /// same rule `file://file://x` names one called `file://x`.
    #[test]
    fn bare_never_re_splits_an_already_extracted_payload() {
        let handed_over = FileReference::bare("file://x");
        assert_eq!(handed_over.spelling(), Spelling::Bare);
        assert_eq!(handed_over.as_written(), Path::new("file://x"));
        assert_eq!(
            FileReference::parse("file://x").as_written(),
            Path::new("x"),
            "`parse` is the other half of the contrast — it does split"
        );
    }

    /// The config-file policy: a rootless reference resolves against the
    /// directory of the file that declared it, and a rooted one already names
    /// one file. Both spellings take the same rule (#379 / C-083).
    #[test]
    fn anchored_at_joins_only_a_rootless_reference() {
        let dir = Path::new("/etc/ocx");
        for (value, expected) in [
            ("acme.pub", dir.join("acme.pub")),
            ("sigstore/trusted-root.json", dir.join("sigstore/trusted-root.json")),
            ("file://acme.pub", dir.join("acme.pub")),
            ("/srv/keys/acme.pub", PathBuf::from("/srv/keys/acme.pub")),
            ("file:///srv/keys/acme.pub", PathBuf::from("/srv/keys/acme.pub")),
        ] {
            assert_eq!(FileReference::parse(value).anchored_at(dir), expected, "{value:?}");
        }
    }

    /// The `index` policy: an empty authority and a path that names a
    /// directory, tested on bytes so a base is valid or not independently of
    /// the host reading it.
    #[test]
    fn absolute_answers_only_for_an_empty_authority_naming_a_directory() {
        for (value, expected) in [
            ("file:///srv/x", Some("/srv/x")),
            ("file:///srv/x/", Some("/srv/x")),
            ("file:///C:/srv/x", Some("/C:/srv/x")),
            // A bare drive survives here and is refused by the caller, which
            // owns the reason (`/C:` resolves against Win32's per-drive CWD).
            ("file:///C:/", Some("/C:")),
            // Authority forms: UNC/remote, never a local tree.
            ("file://host.example/srv/x", None),
            ("file://srv", None),
            // On Windows `Path::new("C:/srv/x")` reports a root; this payload
            // is still an authority, and the answer must not depend on the OS.
            ("file://C:/srv/x", None),
            // Names no directory.
            ("file:///", None),
            ("file://", None),
        ] {
            assert_eq!(FileReference::parse(value).absolute(), expected, "{value:?}");
        }
    }
}
