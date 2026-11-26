// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! How one environment record is applied to an existing value, shared by `ocx` and the direct launcher.
//!
//! Std and `serde` only: the launcher links this crate and carries a size budget.

pub mod direct;

use std::ffi::{OsStr, OsString};

use serde::{Deserialize, Serialize};

/// The host's `PATH` list separator: `;` on Windows, `:` elsewhere.
#[cfg(target_os = "windows")]
pub const PATH_SEPARATOR: &str = ";";

/// The host's `PATH` list separator: `;` on Windows, `:` elsewhere.
#[cfg(not(target_os = "windows"))]
pub const PATH_SEPARATOR: &str = ":";

/// How a record combines with the variable's existing value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyKind {
    /// Move-to-front into a `PATH`-style list.
    Path,
    /// Replace the existing value.
    Constant,
    /// Move-to-back into a separator-joined list.
    List,
}

/// How environment keys compare: byte-exact, or ASCII case-insensitive as Windows does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFold {
    Exact,
    AsciiCaseInsensitive,
}

/// Applies one record to the variable's `existing` value and returns the new value.
///
/// `separator` is read for [`ApplyKind::List`] only; `fold` selects the segment comparison of [`ApplyKind::Path`].
pub fn apply_one(
    existing: Option<&OsStr>,
    kind: ApplyKind,
    value: &OsStr,
    separator: Option<&str>,
    fold: KeyFold,
) -> OsString {
    let _ = (existing, kind, value, separator, fold);
    unimplemented!("apply_one")
}

/// Whether two `PATH` elements name the same entry under the Windows rule (`windows`) or byte-exact.
pub fn path_element_eq(left: &OsStr, right: &OsStr, windows: bool) -> bool {
    let _ = (left, right, windows);
    unimplemented!("path_element_eq")
}

/// The hashable form of [`path_element_eq`]'s equivalence class.
pub fn path_element_norm(value: &str, windows: bool) -> String {
    let _ = (value, windows);
    unimplemented!("path_element_norm")
}

/// Drops every occurrence of `value` from a `PATH`-style `existing`.
pub fn remove_segment(existing: &OsStr, value: &OsStr) -> OsString {
    let _ = (existing, value);
    unimplemented!("remove_segment")
}

/// Move-to-front dedup for a `PATH`-style value: drops empty segments and every copy of `value`, then prepends it.
pub fn move_to_front(existing: &OsStr, value: &OsStr) -> OsString {
    let _ = (existing, value);
    unimplemented!("move_to_front")
}

/// Unique append for a separator-joined list value: moves `value` to the back.
pub fn append_unique(existing: &str, value: &str, separator: &str) -> String {
    let _ = (existing, value, separator);
    unimplemented!("append_unique")
}

#[cfg(test)]
mod tests {
    use super::PATH_SEPARATOR as SEP;
    use super::{
        ApplyKind, KeyFold, append_unique, apply_one, move_to_front, path_element_eq, path_element_norm, remove_segment,
    };
    use std::ffi::{OsStr, OsString};

    #[test]
    fn apply_one_path_moves_to_front() {
        let existing = OsString::from(format!("/b{SEP}/a"));
        let got = apply_one(Some(&existing), ApplyKind::Path, OsStr::new("/a"), None, KeyFold::Exact);
        assert_eq!(got, OsString::from(format!("/a{SEP}/b")));
    }

    #[test]
    fn apply_one_constant_replaces() {
        let got = apply_one(
            Some(OsStr::new("old")),
            ApplyKind::Constant,
            OsStr::new("new"),
            None,
            KeyFold::Exact,
        );
        assert_eq!(got, OsString::from("new"));
    }

    #[test]
    fn apply_one_list_moves_to_back() {
        let got = apply_one(
            Some(OsStr::new("-ea -Xmx1g")),
            ApplyKind::List,
            OsStr::new("-ea"),
            Some(" "),
            KeyFold::Exact,
        );
        assert_eq!(got, OsString::from("-Xmx1g -ea"));
    }

    #[test]
    fn apply_one_absent_existing_yields_value() {
        let got = apply_one(None, ApplyKind::Path, OsStr::new("/a"), None, KeyFold::Exact);
        assert_eq!(got, OsString::from("/a"));
    }

    #[test]
    fn windows_rule_folds_case_and_slashes() {
        assert!(path_element_eq(
            OsStr::new("C:/Tools/Bin"),
            OsStr::new("c:\\tools\\bin"),
            true
        ));
        assert!(!path_element_eq(OsStr::new("/opt/Bin"), OsStr::new("/opt/bin"), false));
        assert_eq!(
            path_element_norm("C:/Tools", true),
            path_element_norm("c:\\tools", true)
        );
    }

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

    fn rm(existing: &OsStr, value: &str) -> OsString {
        remove_segment(existing, OsStr::new(value))
    }

    #[test]
    fn removes_the_named_segment_from_any_position() {
        assert_eq!(rm(&join(&["/a", "/b", "/c"]), "/b"), join(&["/a", "/c"]));
        assert_eq!(rm(&join(&["/b", "/a"]), "/b"), OsString::from("/a"));
        assert_eq!(rm(&join(&["/a", "/b"]), "/b"), OsString::from("/a"));
    }

    #[test]
    fn removes_every_occurrence_and_drops_empties() {
        // A duplicated shim slot must not survive as a second chance to loop.
        assert_eq!(rm(&join(&["/b", "/a", "", "/b"]), "/b"), OsString::from("/a"));
    }

    #[test]
    fn removing_the_only_segment_yields_empty() {
        assert_eq!(rm(&join(&["/b"]), "/b"), OsString::new());
    }

    #[test]
    fn absent_segment_leaves_the_rest_intact() {
        assert_eq!(rm(&join(&["/a", "/b"]), "/zz"), join(&["/a", "/b"]));
    }

    #[test]
    fn partial_path_is_not_removed() {
        // The exact-segment rule `move_to_front` uses, in the other direction:
        // removing `/usr/bin` must not take `/usr/bin/extra` with it.
        assert_eq!(
            rm(&join(&["/usr/bin/extra", "/usr/bin"]), "/usr/bin"),
            OsString::from("/usr/bin/extra")
        );
    }

    #[test]
    fn empty_existing_yields_the_bare_value() {
        assert_eq!(append_unique("", "-ea", " "), "-ea");
    }

    #[test]
    fn appends_to_the_back() {
        assert_eq!(append_unique("-Xmx1g", "-ea", " "), "-Xmx1g -ea");
    }

    #[test]
    fn moves_an_existing_contribution_to_the_back() {
        // Last applier wins for a consumer that resolves duplicates last-wins,
        // which is the whole point of re-applying rather than skipping.
        assert_eq!(append_unique("-ea -Xmx1g", "-ea", " "), "-Xmx1g -ea");
    }

    #[test]
    fn already_at_the_back_is_unchanged() {
        assert_eq!(append_unique("-Xmx1g -ea", "-ea", " "), "-Xmx1g -ea");
    }

    #[test]
    fn list_idempotent_when_reapplied() {
        let once = append_unique("-ea -Xmx1g -server", "-ea", " ");
        assert_eq!(append_unique(&once, "-ea", " "), once);
    }

    /// Mirrors `utility::path`'s `removes_every_repeated_occurrence`: an
    /// ambient value that already carried duplicates must collapse to one, or
    /// re-application is not idempotent.
    #[test]
    fn list_removes_every_repeated_occurrence() {
        assert_eq!(append_unique("a,b,a,c", "a", ","), "b,c,a");
    }

    /// Adjacent duplicates share the separator the first match consumes, so a
    /// single `replace` pass leaves one behind — the fixpoint loop is what
    /// makes the removal total.
    #[test]
    fn removes_adjacent_duplicates() {
        assert_eq!(append_unique("a,a,b", "a", ","), "b,a");
        assert_eq!(append_unique("a,a,a", "a", ","), "a");
    }

    /// The flank rule matches whole elements only: `-ea` must not be found
    /// inside `-eabc`, or a longer option would be silently deleted.
    #[test]
    fn a_longer_element_is_not_a_flank_match() {
        assert_eq!(append_unique("-eabc", "-ea", " "), "-eabc -ea");
        assert_eq!(append_unique("-ea", "-eabc", " "), "-ea -eabc");
    }

    /// A contribution carrying the separator is still one contribution — ocx
    /// never tokenizes elements — so it round-trips as a unit.
    #[test]
    fn value_containing_the_separator_is_one_contribution() {
        let once = append_unique("x", "a,b", ",");
        assert_eq!(once, "x,a,b");
        assert_eq!(append_unique(&once, "a,b", ","), "x,a,b");
    }

    #[test]
    fn multi_character_separator() {
        let once = append_unique("-Wall", "-Wextra", "; ");
        assert_eq!(once, "-Wall; -Wextra");
        assert_eq!(append_unique(&once, "-Wall", "; "), "-Wextra; -Wall");
    }

    #[test]
    fn empty_value_is_a_no_op() {
        assert_eq!(append_unique("a,b", "", ","), "a,b");
        assert_eq!(append_unique("", "", ","), "");
    }

    /// Named boundary, accepted by ADR D1: a contribution spanning two adjacent
    /// prior elements is flanked by separators just like a single element, so
    /// the span is removed and re-appended as one. Asserted rather than fixed —
    /// the result is stable and idempotent, and detecting the case would mean
    /// parsing element grammar ocx deliberately does not own.
    #[test]
    fn a_contribution_spanning_two_elements_is_removed_as_one_span() {
        assert_eq!(append_unique("a,b,c", "b,c", ","), "a,b,c");
        let once = append_unique("a,b,c", "b,c", ",");
        assert_eq!(append_unique(&once, "b,c", ","), once, "still idempotent");
    }

    /// A separator-edged value is refused before the fold ever sees it; this
    /// pins what the primitive does anyway, so a future parse-gate regression
    /// surfaces as a changed assertion rather than silent corruption.
    #[test]
    fn a_separator_edged_value_still_folds_deterministically() {
        assert_eq!(append_unique("a", ",b", ","), "a,,b");
    }
}
