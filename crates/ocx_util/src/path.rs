// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Move-to-front deduplication and segment removal for `PATH`-style environment values.

use std::ffi::{OsStr, OsString};

/// The host's `PATH` list separator: `;` on Windows, `:` elsewhere.
#[cfg(target_os = "windows")]
pub const PATH_SEPARATOR: &str = ";";

/// The host's `PATH` list separator: `;` on Windows, `:` elsewhere.
#[cfg(not(target_os = "windows"))]
pub const PATH_SEPARATOR: &str = ":";

/// Whether a `PATH` segment names `value`: exact on Unix, ASCII-case-insensitive on Windows.
///
/// Must match every `Shell::export_path` / `remove_list_element` arm, or emitted and in-process `PATH`s diverge.
fn same_element(segment: &OsStr, value: &OsStr) -> bool {
    // Host rule, not the shell's: the process env and `$GITHUB_PATH` follow it even under MSYS.
    if cfg!(windows) {
        segment.eq_ignore_ascii_case(value)
    } else {
        segment == value
    }
}

/// Strip one — and only one — surrounding pair of `"` from a `PATH` element.
///
/// Windows quotes space-bearing segments and `split_paths` unquotes them, so both spellings must compare equal.
pub fn strip_one_quote_pair(value: &str) -> &str {
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
/// Not in [`same_element`], or [`remove_segment`]'s already-stripped operand loses a second pair.
fn comparison_operand(value: &OsStr) -> &OsStr {
    if cfg!(windows) {
        strip_one_quote_pair_os(value)
    } else {
        value
    }
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
fn retained(existing: &OsStr, value: &OsStr) -> OsString {
    // Raw bytes, not `Path` equality, which normalises trailing slashes and diverges from the emitted snippets.
    let segments: Vec<OsString> = std::env::split_paths(existing)
        .map(std::path::PathBuf::into_os_string)
        .filter(|segment| !segment.is_empty() && !same_element(segment, value))
        .collect();
    join_segments(&segments)
}

/// [`move_to_front`] minus the prepend: drops every occurrence of `value`.
///
/// Segment-exact, not containment: a differently spelled alias survives, so a fail-closed caller re-checks the resolved answer.
pub fn remove_segment(existing: &OsStr, value: &OsStr) -> OsString {
    // As `Shell::remove_list_element`: the live env spells a space-bearing Windows segment either way.
    retained(existing, strip_one_quote_pair_os(value))
}

/// Move-to-front dedup for a `PATH`-style value: drops empty segments and every copy of `value`, then prepends it.
///
/// Mirrors `shell::Shell::export_path`; infallible and idempotent.
///
/// **Precondition:** `value` is one directory with no `PATH_SEPARATOR` (nor `"` on Windows), or a re-apply does not round-trip.
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
    // Comparison only: `value` is prepended byte for byte, as `export_path`'s pwsh arm writes it.
    let operand = comparison_operand(value);
    let survivors = retained(existing, operand);
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

#[cfg(test)]
mod tests {
    use super::PATH_SEPARATOR as SEP;
    use super::{move_to_front, remove_segment};
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;

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

    // ══ A-19 / C-021 — one PATH-element comparison rule ══════════════════
    //
    // The in-process appliers and the emitted shell snippets must decide
    // "same element?" identically, because the reconciler's `C == L.applied`
    // guard compares exactly those two products. The three tests that pin the
    // two halves *to one another* live in `shell/tests_path_parity.rs`: they
    // name `crate::shell`, and `utility` may name nothing above itself (plan
    // C-042, boundary `utility_imports_nothing`). What stays here is the
    // in-process half on its own.

    /// A differently-cased directory is a genuinely different one on Unix, and
    /// deleting it is the defect A-19 measured with pwsh 7 on Linux.
    #[cfg(unix)]
    #[test]
    fn a_differently_cased_directory_survives_on_unix() {
        assert_eq!(
            mtf(&join(&["/opt/Bin", "/x"]), "/opt/bin"),
            join(&["/opt/bin", "/opt/Bin", "/x"])
        );
        assert_eq!(rm(&join(&["/opt/Bin", "/x"]), "/opt/bin"), join(&["/opt/Bin", "/x"]));
    }

    /// Only the outermost pair goes — a directory genuinely named `""x""`
    /// keeps one, and a one-sided quote is part of the name.
    ///
    /// Unix-only because the quoted *ambient* spelling cannot be staged on
    /// Windows: `split_paths` unquotes there before this function sees a
    /// segment, which is the very reason the operand needs the strip.
    #[cfg(unix)]
    #[test]
    fn only_one_surrounding_quote_pair_is_stripped_from_the_operand() {
        assert_eq!(rm(&join(&["\"/a\"", "/b"]), "\"\"/a\"\""), OsString::from("/b"));
        assert_eq!(
            rm(&join(&["/a", "/b"]), "\"/a"),
            join(&["/a", "/b"]),
            "a one-sided quote is part of the segment, not a wrapper"
        );
    }

    /// `move_to_front` prepends the value verbatim, because `export_path` does:
    /// the applier normalises the ambient segments it compares against, never
    /// the value it writes.
    #[test]
    fn move_to_front_prepends_the_value_verbatim() {
        assert_eq!(mtf(&join(&["/b"]), "\"/a\""), join(&["\"/a\"", "/b"]));
    }

    #[test]
    fn undoes_move_to_front() {
        // The two are inverses on the segment they share, which is what the
        // shim guard relies on: the composer adds the slot, the guard drops it.
        let with = mtf(&join(&["/a", "/b"]), "/shim");
        assert_eq!(remove_segment(&with, OsStr::new("/shim")), join(&["/a", "/b"]));

        // …and inverses **byte for byte**, not merely segment for segment, which
        // the bare join lost: a quoted separator-bearing ambient came back torn,
        // so the guard handed `execvp` a `PATH` the composer had never been given.
        let ambient = OsString::from(REJOIN_AMBIENT);
        let with = move_to_front(&ambient, OsStr::new("/shim"));
        assert_eq!(remove_segment(&with, OsStr::new("/shim")), ambient);
    }

    /// E3's rule, asserted on **either** host: the operand is normalised
    /// exactly where `split_paths` unquotes a segment, and nowhere else.
    ///
    /// The *behaviour* the gate decides is only observable on Windows — on a
    /// Unix host, deleting the strip outright changes nothing this module can
    /// see. So the gate itself is the assertion: making the strip unconditional
    /// reds here, and deleting it leaves `comparison_operand` unused, which
    /// `-D warnings` reds. Both mutations red on a Windows runner behaviourally,
    /// through the two tests below.
    #[test]
    fn a019_the_comparison_operand_is_normalised_exactly_where_split_paths_unquotes() {
        let quoted = OsStr::new("\"/opt/b in\"");
        assert_eq!(
            super::comparison_operand(quoted) != quoted,
            cfg!(windows),
            "stripped on Windows, where `split_paths` unquotes the segment; untouched elsewhere"
        );
        assert_eq!(
            super::comparison_operand(OsStr::new("/opt/bin")),
            OsStr::new("/opt/bin"),
            "an unquoted operand is untouched on every platform"
        );
    }

    /// E3, Windows half — the operand is compared after one surrounding pair of
    /// `"` comes off, **on Windows only**.
    ///
    /// Asserted against `cfg!(windows)` rather than behind a `#[cfg]` so both
    /// arms execute on every platform, the same way
    /// `reconcile.rs`'s `a019_key_equality_follows_the_platform_and_so_does_the_exit_guard`
    /// does. The two arms are genuinely different behaviours, not one behaviour
    /// and one skip: `split_paths` unquotes a segment on Windows and nowhere
    /// else, so an operand strip is required there to recognise the ambient
    /// spelling and forbidden elsewhere, where a leading `"` is part of the
    /// directory name.
    ///
    /// Red state: drop the `cfg!(windows)` strip and the Windows arm keeps the
    /// bare segment; make it unconditional and the Unix arm drops it.
    #[test]
    fn a019_the_operand_quote_strip_follows_the_platform() {
        let ambient = join(&["/opt/b in", "/x"]);
        let folded = mtf(&ambient, "\"/opt/b in\"");
        let expected = if cfg!(windows) {
            // The bare ambient segment IS the quoted operand, so it is moved to
            // the front rather than left in place beside a second copy.
            join(&["\"/opt/b in\"", "/x"])
        } else {
            join(&["\"/opt/b in\"", "/opt/b in", "/x"])
        };
        assert_eq!(folded, expected);
    }

    /// The consequence the strip exists for: re-applying a quoted value is a
    /// no-op on **every** platform.
    ///
    /// On Windows the first application writes the quoted spelling and
    /// `split_paths` hands the second one the unquoted segment; without the
    /// operand strip the two never compare equal and the variable grows by one
    /// copy per prompt, without bound — measured on pwsh 7 before the fix. On
    /// Unix nothing unquotes either side, so the quoted copy matches itself.
    #[test]
    fn a019_reapplying_a_quoted_value_is_idempotent_on_every_platform() {
        let once = mtf(&join(&["/x"]), "\"/opt/b in\"");
        let twice = move_to_front(&once, OsStr::new("\"/opt/b in\""));
        assert_eq!(once, twice, "one copy, however many prompts fire");
    }

    // ══ R-W40 — the re-join preserves the segment list ════════════════════
    //
    // `split_paths` strips the quotes off a Windows segment that legally
    // contains `;`, so re-emitting it bare turns one directory into two and
    // plants the tail as a **relative** entry on `PATH` (CWE-426).
    // `join_segments` re-quotes it.

    /// An ambient `PATH` whose every element is absolute **on the host**, and
    /// which — on Windows, the only platform where such an element can exist —
    /// holds one quoted element embedding the separator. std's own
    /// `c:\foo;c:\som"e;di"r;c:\bar` shape.
    ///
    /// Spelled per platform for the same reason `CASE_PAIR` in
    /// `shell/tests_path_parity.rs` is: a
    /// `C:\`-rooted fixture is torn into two segments off Windows, and a
    /// `/`-rooted one is not absolute on Windows (it has a root but no drive
    /// prefix), so neither spelling states the property on both hosts.
    #[cfg(windows)]
    const REJOIN_AMBIENT: &str = "\"C:\\Tools;Legacy\\bin\";C:\\other";
    #[cfg(not(windows))]
    const REJOIN_AMBIENT: &str = "/opt/tools:/other";

    fn segments_of(value: &OsStr) -> Vec<PathBuf> {
        std::env::split_paths(value).collect()
    }

    /// A quoted, separator-bearing segment survives a `move_to_front` round
    /// trip as **one** segment: re-splitting the result yields the prepended
    /// value followed by exactly the ambient's own segment list, never one more.
    ///
    /// **Off Windows this proves nothing about quoting, and is not claimed to**
    /// — `split_paths` splits on every `:` there, so no segment can contain the
    /// separator and the bare join was byte-identical to `join_paths` for every
    /// input. There is no Linux-observable red for the tearing. What runs here
    /// off Windows is the plain round trip. It is unconditional rather than
    /// `#[cfg(windows)]` because `task rust:check:windows-cfg` is scoped to
    /// `ocx_shim`, so a Windows-gated test in this crate would not even be
    /// compiled by the gate, let alone run.
    #[test]
    fn move_to_front_re_splits_to_exactly_the_segments_it_kept() {
        let ambient = OsString::from(REJOIN_AMBIENT);
        let before = segments_of(&ambient);

        let after = segments_of(&mtf(&ambient, "/v"));

        assert_eq!(after.first(), Some(&PathBuf::from("/v")), "the value is prepended");
        assert_eq!(
            &after[1..],
            &before[..],
            "a segment that legally contains the separator must be re-emitted quoted, \
             so the survivors re-split one for one"
        );
    }

    /// The removal direction of the same rule: `remove_segment` never hands the
    /// next `PATH` consumer a segment the ambient did not already have — in
    /// particular never a **relative** one, which resolves against the working
    /// directory (CWE-426) and is what tearing `C:\Tools;Legacy\bin` produces.
    ///
    /// The first assertion is the positive control for the second: it pins that
    /// the fixture is all-absolute to begin with, so the absoluteness assertion
    /// cannot pass vacuously on a fixture that never was.
    ///
    /// **That control establishes non-vacuity, not a red.** Off Windows only the
    /// segment-list equality can go red: no Unix segment can hold the separator,
    /// so nothing tears, and a result segment can only turn relative by way of a
    /// list that already changed — the absoluteness assertion has no independent
    /// Unix red and none is claimed. Where it reds is the `windows-latest` leg of
    /// `.github/workflows/verify-deep.yml`'s "Build & Unit Test" matrix, which
    /// runs this same `cargo nextest run --workspace` on `x86_64-pc-windows-msvc`.
    #[test]
    fn remove_segment_plants_no_segment_the_ambient_did_not_have() {
        let ambient = OsString::from(REJOIN_AMBIENT);
        assert!(
            segments_of(&ambient).iter().all(|segment| segment.is_absolute()),
            "control: every ambient segment is absolute on this host, so the assertion below bites"
        );

        let result = rm(&ambient, "/absent");

        assert!(
            segments_of(&result).iter().all(|segment| segment.is_absolute()),
            "removing an absent segment must not split an absolute one into a relative tail"
        );
        assert_eq!(
            segments_of(&result),
            segments_of(&ambient),
            "and must not change the list at all"
        );
    }

    /// A separator-bearing segment takes a different arm on each platform, and
    /// the name says both because the assertion covers both: Windows **quotes**
    /// it (`join_paths` succeeds and nothing falls back), Unix **refuses** it and
    /// the bare separator join runs, byte-identical to the pre-fix behaviour —
    /// the infallibility both public functions promise.
    ///
    /// Neither arm is reachable through those functions, since `join_paths`
    /// rejects only what its own `split_paths` cannot emit, so `join_segments` is
    /// called directly. **The Unix arm is the one assertion in this group with a
    /// red state on this host**: `join_paths` rejects a segment containing `:`
    /// there, so the fallback runs and its output is observable.
    #[test]
    fn the_rejoin_quotes_a_separator_bearing_segment_on_windows_and_falls_back_off_it() {
        let segments = [OsString::from(format!("/a{SEP}b")), OsString::from("/x")];
        let expected = if cfg!(windows) {
            format!("\"/a{SEP}b\"{SEP}/x")
        } else {
            format!("/a{SEP}b{SEP}/x")
        };
        assert_eq!(super::join_segments(&segments), OsString::from(expected));
    }
}
