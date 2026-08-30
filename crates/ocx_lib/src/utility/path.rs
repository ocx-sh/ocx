// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Move-to-front deduplication and segment removal for `PATH`-style
//! environment values.

use std::ffi::{OsStr, OsString};

/// Whether a `PATH` segment names the same element as `value`.
///
/// One rule, shared by both functions in this module and by every
/// [`Shell::export_path`](crate::shell::Shell::export_path) /
/// `remove_list_element` arm (A-19): segment-exact, case-sensitively on Unix
/// and ASCII-case-insensitively on Windows, where `C:\Opt` and `C:\opt` are
/// one directory.
///
/// The platform is decided at compile time by `cfg!(windows)`, the same way
/// [`crate::env::PATH_SEPARATOR`] is, because **these helpers operate on
/// in-process values** — the process environment and the `$GITHUB_ENV` /
/// `$GITHUB_PATH` sinks — where the host convention is the correct one. That
/// holds under MSYS too: a Windows-native child spawned from Git-Bash is handed
/// a `;`-separated `PATH`, not the `:`-separated one its parent shell sees.
///
/// It is emphatically **not** because host and shell agree on a separator —
/// they do not, and nothing here depends on their agreeing. Emitted text is a
/// different concern with a different rule: `PATH` always routes through
/// [`Shell::export_path`](crate::shell::Shell::export_path), which keys the
/// separator per shell, and `export_list` / `remove_list_element` take theirs
/// from the package metadata's `List` modifier. Keying this module on the shell
/// instead would break the process-env path it actually serves.
fn same_element(segment: &OsStr, value: &OsStr) -> bool {
    if cfg!(windows) {
        segment.eq_ignore_ascii_case(value)
    } else {
        segment == value
    }
}

/// Strip one — and only one — surrounding pair of `"` from a `PATH` element.
///
/// Delegates to the emitter's own normaliser so the in-process and emitted
/// halves cannot drift. A non-UTF-8 value is returned untouched: quoting is a
/// Windows PATH convention and Windows paths are always representable.
fn strip_one_quote_pair(value: &OsStr) -> &OsStr {
    match value.to_str() {
        Some(text) => OsStr::new(crate::shell::strip_one_quote_pair(text)),
        None => value,
    }
}

/// The form of `value` [`move_to_front`] **compares** against, never the form it
/// writes (E3).
///
/// `std::env::split_paths` strips one surrounding pair of `"` from a segment on
/// **Windows only**, so the operand has to be stripped there to recognise the
/// ambient spelling of the very copy this module wrote a prompt earlier —
/// without it the value never matched itself and `PATH` grew by one copy per
/// prompt, without bound. Off Windows nothing unquotes a segment, so a leading
/// `"` is part of the directory name and stripping the operand would break the
/// opposite way.
///
/// Named rather than inlined so the platform gate is assertable on either host:
/// the behaviour it decides is only observable on Windows, but the *rule* is
/// observable everywhere.
///
/// [`remove_segment`] deliberately does not use this — it strips
/// unconditionally, because it never prepends, so it has no written copy for a
/// stripped operand to stop matching. That asymmetry is also why the gate is
/// here and not in [`same_element`], which both functions share: applying it
/// there would take a second pair off `remove_segment`'s already-stripped
/// operand.
fn comparison_operand(value: &OsStr) -> &OsStr {
    if cfg!(windows) {
        strip_one_quote_pair(value)
    } else {
        value
    }
}

/// Drops every occurrence of `value` from a `PATH`-style value.
///
/// The inverse of [`move_to_front`] minus the prepend, and it shares that
/// function's splitting, empty-segment dropping and exact-segment comparison —
/// so a directory added by one is removed by the other.
///
/// The caller is a security guard, not a convenience: `ocx launcher shim` must
/// resolve the invoked name on a `PATH` that no longer contains the shim
/// directory it was itself invoked from. Leaving it there makes a name the
/// package claimed but does not ship resolve back to the shim launcher, and
/// the following `execvp` re-enters the same process forever.
///
/// One surrounding pair of `"` is stripped from `value` before comparing, as
/// [`Shell::remove_list_element`](crate::shell::Shell::remove_list_element)
/// does on its path-kind arm: the caller enumerates the operand from the live
/// environment, which spells a space-bearing Windows segment either way.
///
/// **Precondition** (same as [`move_to_front`]): `value` is a single directory
/// containing no `PATH_SEPARATOR`. Comparison is segment-exact — deliberately,
/// so that it matches the emitted shell snippets — which means a segment naming
/// the same directory by a different string survives untouched: a trailing
/// slash, a symlink alias, a root spelled one way by the composing process and
/// another way by the invoked one. This function is therefore not a containment
/// check and must not be read as one. Callers that must fail closed re-check the
/// *resolved* answer against the resolved directory, which is the only
/// comparison those spellings collapse under.
pub fn remove_segment(existing: &OsStr, value: &OsStr) -> OsString {
    let value = strip_one_quote_pair(value);
    let mut result = OsString::with_capacity(existing.len());
    for segment in std::env::split_paths(existing) {
        let segment = segment.into_os_string();
        if segment.is_empty() || same_element(&segment, value) {
            continue;
        }
        if !result.is_empty() {
            result.push(crate::env::PATH_SEPARATOR);
        }
        result.push(segment);
    }
    result
}

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
/// in-process child env (`ocx exec` / `ocx package exec`) and the emitted shell
/// text agree on the same semantics.
///
/// `OsStr`-based to match [`crate::env::Env`]'s `OsString` storage and avoid a
/// lossy UTF-8 round-trip on non-UTF-8 paths. Segment comparison is exact (no
/// prefix/substring match): `/usr/bin` never matches `/usr/bin/extra`, and it
/// folds ASCII case on Windows only (A-19) — `/opt/Bin` and `/opt/bin` are two
/// directories on Unix and one on Windows — and it compares through
/// [`comparison_operand`], which takes one surrounding pair of `"` off the
/// operand on Windows only. `value` is prepended verbatim regardless: the
/// applier normalises what it compares against, never what it writes.
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
    // E3 — **comparison only**. `value` is still prepended byte for byte,
    // because that is what `export_path`'s pwsh arm writes and the parity
    // between them is the whole contract: normalise what you compare against,
    // never what you write.
    let operand = comparison_operand(value);

    let mut result = OsString::with_capacity(existing.len() + value.len() + 1);
    if !value.is_empty() {
        result.push(value);
    }
    // `split_paths` uses the platform path separator (`:` on Unix, `;` on
    // Windows) — identical to `crate::env::PATH_SEPARATOR` — is `OsStr` native,
    // so no lossy conversion happens, and unquotes on Windows, which is the
    // ambient half of A-19's quote normalisation. `same_element` compares raw
    // bytes rather than via `Path` equality, which would normalise trailing
    // slashes and diverge from the emitted shell snippets.
    for segment in std::env::split_paths(existing) {
        let segment = segment.into_os_string();
        if segment.is_empty() || same_element(&segment, operand) {
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
    use super::{move_to_front, remove_segment};
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
    // guard compares exactly those two products. Each test below pins the two
    // halves to one another, so a change to either one alone goes red.

    /// A case-differing `(ambient, value)` pair spelled for the host platform.
    ///
    /// Spelling matters: `split_paths` splits on `:` off Windows, so a
    /// `C:\Opt\Bin` fixture would be torn into two segments there and the
    /// comparison under test would never run on the whole element.
    #[cfg(windows)]
    const CASE_PAIR: (&str, &str) = (r"C:\Opt\Bin", r"C:\opt\bin");
    #[cfg(not(windows))]
    const CASE_PAIR: (&str, &str) = ("/opt/Bin", "/opt/bin");

    /// The comparison folds ASCII case exactly where `Shell::export_path`'s
    /// emit does — on Windows, and only there.
    ///
    /// Red state: leave `move_to_front` case-sensitive on Windows (EC-PATH-008),
    /// or revert the PowerShell arm to the case-insensitive `-ne`.
    #[test]
    fn move_to_front_folds_case_exactly_where_the_emit_does() {
        let (ambient, value) = CASE_PAIR;
        let ambient = OsString::from(ambient);
        let folded = mtf(&ambient, value) == *value;

        let emit = crate::shell::Shell::PowerShell.export_path("PATH", value).unwrap();
        assert_eq!(
            folded,
            emit.contains("OrdinalIgnoreCase"),
            "the in-process fold and the emitted comparison must agree; emit: {emit}"
        );
        assert_eq!(
            folded,
            cfg!(windows),
            "A-19: PATH elements fold ASCII case on Windows and nowhere else"
        );
    }

    /// The same rule in the removal direction, against the same emitted arm.
    ///
    /// Red state: fold in one of the two functions only — the composer would
    /// then add a slot the reconciler cannot retire.
    #[test]
    fn remove_segment_folds_case_exactly_where_the_emit_does() {
        let (ambient, value) = CASE_PAIR;
        let ambient = join(&[ambient, "/keep"]);
        let removed = rm(&ambient, value) == *"/keep";

        let emit = crate::shell::Shell::PowerShell
            .remove_list_element("PATH", value, None)
            .unwrap();
        assert_eq!(
            removed,
            emit.contains("OrdinalIgnoreCase"),
            "the in-process fold and the emitted comparison must agree; emit: {emit}"
        );
        assert_eq!(removed, cfg!(windows), "A-19: the two directions share one rule");
    }

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

    /// The removal operand carries one surrounding pair of `"` off, exactly as
    /// `Shell::remove_list_element`'s path-kind arm does: the operand is
    /// enumerated from the live environment, which spells a space-bearing
    /// Windows segment either way.
    ///
    /// Red state: drop the strip on either side (EC-PATH-010).
    #[test]
    fn remove_segment_strips_one_quote_pair_from_the_operand_as_the_emit_does() {
        let ambient = join(&["/opt/bin", "/x"]);
        assert_eq!(
            rm(&ambient, "\"/opt/bin\""),
            rm(&ambient, "/opt/bin"),
            "a quoted operand must normalise to the bare one, as the emitted arm's does"
        );

        let shell = crate::shell::Shell::Bash;
        assert_eq!(
            shell.remove_list_element("PATH", "\"/opt/bin\"", None),
            shell.remove_list_element("PATH", "/opt/bin", None),
            "the emitted half must carry the same normalisation"
        );
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
}
