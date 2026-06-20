// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Move-to-front deduplication for `PATH`-style environment values.

use std::ffi::{OsStr, OsString};

/// Move-to-front dedup for a `PATH`-style value.
///
/// Splits `existing` on the platform path separator, drops empty segments and
/// every segment exactly equal to `value`, then returns `value` followed by the
/// survivors, re-joined with the separator. Infallible.
///
/// Re-applying the result is a no-op (idempotent), and re-adding a segment that
/// is already present removes the stale occurrence and moves it to the front —
/// "last activation wins" for lookup. This mirrors the self-contained idempotent
/// shell snippets emitted by [`crate::shell::Shell::export_path`], so the
/// in-process child env (`ocx run` / `ocx package exec`) and the emitted shell
/// text agree on the same semantics.
///
/// `OsStr`-based to match [`crate::env::Env`]'s `OsString` storage and avoid a
/// lossy UTF-8 round-trip on non-UTF-8 paths. Segment comparison is exact (no
/// prefix/substring match): `/usr/bin` never matches `/usr/bin/extra`. On
/// Windows the value comparison stays case-sensitive — case folding belongs to
/// the key (handled by `EnvKey`), not the path value.
///
/// **Precondition:** `value` is a single directory containing no
/// `PATH_SEPARATOR` (the env resolver yields one resolved `bin/` dir per
/// entry). A value embedding the separator is treated as one opaque segment and
/// would not round-trip a re-apply. As a defensive measure an empty `value` is
/// simply not prepended (the survivors are still de-duplicated), so the result
/// never carries a leading empty segment.
///
/// # Examples
///
/// ```ignore
/// move_to_front("".as_ref(), "/a".as_ref())          == "/a"
/// move_to_front("/b:/c".as_ref(), "/a".as_ref())     == "/a:/b:/c"
/// move_to_front("/b:/a:/c".as_ref(), "/a".as_ref())  == "/a:/b:/c" // moved to front
/// move_to_front("/a".as_ref(), "/a".as_ref())        == "/a"        // idempotent
/// move_to_front("/b:".as_ref(), "/a".as_ref())       == "/a:/b"     // empty dropped
/// ```
pub fn move_to_front(existing: &OsStr, value: &OsStr) -> OsString {
    let mut result = OsString::with_capacity(existing.len() + value.len() + 1);
    if !value.is_empty() {
        result.push(value);
    }
    // `split_paths` uses the platform path separator (`:` on Unix, `;` on
    // Windows) — identical to `crate::env::PATH_SEPARATOR` — and is `OsStr`
    // native, so no lossy conversion happens. We compare each segment's exact
    // `OsStr` bytes against `value` rather than via `Path` equality, which would
    // normalise trailing slashes and diverge from the exact-string comparison
    // the emitted shell snippets perform.
    for segment in std::env::split_paths(existing) {
        let segment = segment.into_os_string();
        if segment.is_empty() || segment == value {
            continue;
        }
        if !result.is_empty() {
            result.push(crate::env::PATH_SEPARATOR);
        }
        result.push(segment);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::move_to_front;
    use crate::env::PATH_SEPARATOR as SEP;
    use std::ffi::{OsStr, OsString};

    /// Build a separator-joined path string for the host platform so the
    /// assertions read naturally on both Unix (`:`) and Windows (`;`).
    fn join(parts: &[&str]) -> OsString {
        OsString::from(parts.join(SEP))
    }

    fn mtf(existing: &OsStr, value: &str) -> OsString {
        move_to_front(existing, OsStr::new(value))
    }

    #[test]
    fn empty_existing_yields_value() {
        assert_eq!(mtf(OsStr::new(""), "/a"), OsString::from("/a"));
    }

    #[test]
    fn prepends_new_value() {
        assert_eq!(mtf(&join(&["/b", "/c"]), "/a"), join(&["/a", "/b", "/c"]));
    }

    #[test]
    fn moves_existing_to_front() {
        // `/a` is in the middle → removed from its old slot, prepended.
        assert_eq!(mtf(&join(&["/b", "/a", "/c"]), "/a"), join(&["/a", "/b", "/c"]));
    }

    #[test]
    fn already_front_is_unchanged() {
        assert_eq!(mtf(&join(&["/a", "/b"]), "/a"), join(&["/a", "/b"]));
    }

    #[test]
    fn idempotent_when_reapplied() {
        let once = mtf(&join(&["/b", "/a", "/c"]), "/a");
        let twice = move_to_front(&once, OsStr::new("/a"));
        assert_eq!(once, twice);
    }

    #[test]
    fn drops_trailing_empty_segment() {
        // `/b:` (trailing separator) must not reintroduce an empty `.`-like slot.
        assert_eq!(mtf(&join(&["/b", ""]), "/a"), join(&["/a", "/b"]));
    }

    #[test]
    fn drops_leading_and_interior_empty_segments() {
        assert_eq!(mtf(&join(&["", "/b", "", "/c"]), "/a"), join(&["/a", "/b", "/c"]));
    }

    #[test]
    fn partial_path_is_not_matched() {
        // `/usr/bin` must not be considered equal to `/usr/bin/extra`.
        assert_eq!(
            mtf(&join(&["/usr/bin"]), "/usr/bin/extra"),
            join(&["/usr/bin/extra", "/usr/bin"]),
        );
    }

    #[test]
    fn removes_every_repeated_occurrence() {
        // Two stale copies of `/a` collapse to a single front occurrence.
        assert_eq!(mtf(&join(&["/a", "/b", "/a", "/c"]), "/a"), join(&["/a", "/b", "/c"]),);
    }

    #[test]
    fn single_segment_equal_to_value_is_idempotent() {
        assert_eq!(mtf(&join(&["/a"]), "/a"), OsString::from("/a"));
    }

    #[test]
    fn empty_value_is_not_prepended() {
        // Defensive: an empty value must not introduce a leading empty segment;
        // the existing entries are still de-duplicated of empties.
        assert_eq!(mtf(&join(&["/a", "", "/b"]), ""), join(&["/a", "/b"]));
        assert_eq!(mtf(OsStr::new(""), ""), OsString::new());
    }
}
