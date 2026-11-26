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
/// `separator` is read for [`ApplyKind::List`] only (an absent one reads as a space); `fold` selects the
/// segment comparison of [`ApplyKind::Path`]. An absent `existing` yields `value`.
pub fn apply_one(
    existing: Option<&OsStr>,
    kind: ApplyKind,
    value: &OsStr,
    separator: Option<&str>,
    fold: KeyFold,
) -> OsString {
    match kind {
        ApplyKind::Constant => value.to_owned(),
        ApplyKind::Path => match existing {
            Some(existing) => move_to_front_with(existing, value, fold == KeyFold::AsciiCaseInsensitive),
            None => value.to_owned(),
        },
        ApplyKind::List => {
            // Lossy, as `ocx exec` reads the ambient value: the list fold is text-only.
            let existing = existing.map(|v| v.to_string_lossy().into_owned());
            OsString::from(append_unique(
                existing.as_deref().unwrap_or_default(),
                &value.to_string_lossy(),
                separator.unwrap_or(" "),
            ))
        }
    }
}

/// Whether two `PATH` elements name the same entry under the Windows rule (`windows`) or byte-exact.
///
/// The Windows rule folds ASCII case and treats `/` and `\` as one character: package metadata writes
/// `${installPath}/bin` over a native install path, and the MSYS runtime hands the entry back all-backslash.
/// Non-ASCII bytes compare exactly. The caller picks `windows`, so the rule is testable on any host.
pub fn path_element_eq(left: &OsStr, right: &OsStr, windows: bool) -> bool {
    if !windows {
        return left == right;
    }
    let (left, right) = (left.as_encoded_bytes(), right.as_encoded_bytes());
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| fold_byte(*a) == fold_byte(*b))
}

/// The hashable form of [`path_element_eq`]'s equivalence class: equal under it iff equal here.
pub fn path_element_norm(value: &str, windows: bool) -> String {
    if windows {
        value
            .chars()
            .map(|c| if c == '/' { '\\' } else { c.to_ascii_lowercase() })
            .collect()
    } else {
        value.to_owned()
    }
}

fn fold_byte(byte: u8) -> u8 {
    if byte == b'/' { b'\\' } else { byte.to_ascii_lowercase() }
}

/// Strip one, and only one, surrounding pair of `"` from a `PATH` element.
///
/// Windows quotes space-bearing segments and `split_paths` unquotes them, so both spellings must compare equal.
fn strip_one_quote_pair(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
}

/// [`strip_one_quote_pair`] over an `OsStr`; a non-UTF-8 value is returned untouched.
fn strip_one_quote_pair_os(value: &OsStr) -> &OsStr {
    match value.to_str() {
        Some(text) => OsStr::new(strip_one_quote_pair(text)),
        None => value,
    }
}

/// The form of `value` [`move_to_front`] compares against, never the form it writes.
///
/// Stripped on Windows only, where `split_paths` unquotes: without it `PATH` grows a copy per prompt.
/// Not in the segment comparison, or [`remove_segment`]'s already-stripped operand loses a second pair.
fn comparison_operand(value: &OsStr, windows: bool) -> &OsStr {
    if windows { strip_one_quote_pair_os(value) } else { value }
}

/// Re-join `PATH` segments with [`std::env::join_paths`], the inverse of `split_paths`.
///
/// A bare join re-emits a Windows segment holding `;` unquoted, so the next split yields a relative segment (CWE-426).
fn join_segments(segments: &[OsString]) -> OsString {
    // Unreachable: `join_paths` rejects only what `split_paths` cannot emit.
    std::env::join_paths(segments).unwrap_or_else(|_| {
        let mut joined = OsString::new();
        for segment in segments {
            if !joined.is_empty() {
                joined.push(PATH_SEPARATOR);
            }
            joined.push(segment);
        }
        joined
    })
}

/// The non-empty segments of `existing` that are not `value`, re-joined.
fn retained(existing: &OsStr, value: &OsStr, windows: bool) -> OsString {
    // Raw bytes, not `Path` equality, which normalises trailing slashes and diverges from the emitted snippets.
    // Must match every `Shell::export_path` / `remove_list_element` arm, or emitted and in-process `PATH`s diverge.
    let segments: Vec<OsString> = std::env::split_paths(existing)
        .map(std::path::PathBuf::into_os_string)
        .filter(|segment| !segment.is_empty() && !path_element_eq(segment, value, windows))
        .collect();
    join_segments(&segments)
}

/// [`move_to_front`] minus the prepend: drops every occurrence of `value`.
///
/// Segment-exact, not containment: a differently spelled alias survives, so a fail-closed caller re-checks the resolved answer.
pub fn remove_segment(existing: &OsStr, value: &OsStr) -> OsString {
    // As `Shell::remove_list_element`: the live env spells a space-bearing Windows segment either way.
    // Host rule, not the shell's: the process env and `$GITHUB_PATH` follow it even under MSYS.
    retained(existing, strip_one_quote_pair_os(value), cfg!(windows))
}

/// Move-to-front dedup for a `PATH`-style value: drops empty segments and every copy of `value`, then prepends it.
///
/// Mirrors `shell::Shell::export_path`; infallible and idempotent.
///
/// **Precondition:** `value` is one directory with no `PATH_SEPARATOR` (nor `"` on Windows), or a re-apply does not round-trip.
pub fn move_to_front(existing: &OsStr, value: &OsStr) -> OsString {
    move_to_front_with(existing, value, cfg!(windows))
}

fn move_to_front_with(existing: &OsStr, value: &OsStr, windows: bool) -> OsString {
    // Comparison only: `value` is prepended byte for byte, as `export_path`'s pwsh arm writes it.
    let survivors = retained(existing, comparison_operand(value, windows), windows);
    if value.is_empty() {
        return survivors;
    }

    // Outside `join_segments`: a `"`-bearing `value` makes `join_paths` reject, dropping every survivor's re-quoting.
    let mut result = OsString::with_capacity(value.len() + 1 + survivors.len());
    result.push(value);
    if !survivors.is_empty() {
        result.push(PATH_SEPARATOR);
        result.push(survivors);
    }
    result
}

/// Unique append for a separator-joined list value: moves `value` to the back.
///
/// Must match the emitted shell snippets (`adr_env_modifier_types.md § Decision`), or the in-process env and the shell env diverge.
///
/// **Precondition:** `value` is not flanked by `separator` and `separator` is non-empty, or `value` is cut out of other elements.
pub fn append_unique(existing: &str, value: &str, separator: &str) -> String {
    if value.is_empty() {
        return existing.to_string();
    }

    let mut wrapped = String::with_capacity(existing.len() + 2 * separator.len());
    wrapped.push_str(separator);
    wrapped.push_str(existing);
    wrapped.push_str(separator);

    let occurrence = format!("{separator}{value}{separator}");
    // Loop to a fixpoint: one `replace` pass misses adjacent duplicates, which breaks idempotence.
    while wrapped.contains(&occurrence) {
        wrapped = wrapped.replace(&occurrence, separator);
    }

    // A fully collapsed list leaves one bare separator, which fails the strip and means empty.
    let survivors = wrapped
        .strip_prefix(separator)
        .and_then(|inner| inner.strip_suffix(separator))
        .unwrap_or("");

    if survivors.is_empty() {
        value.to_string()
    } else {
        format!("{survivors}{separator}{value}")
    }
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
