// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod about;
pub mod announce;
pub mod attestation;
pub mod catalog;
// ci_export deleted (C4 — handshake §6: ocx ci removed)
pub mod clean;
pub mod config_setup;
pub mod config_test;
pub mod config_update;
pub mod deps;
pub mod env;
pub mod index;
pub mod install;
pub mod lock;
pub mod login;
pub mod package_cascade_check;
pub mod package_cascade_repair;
pub mod package_copy;
pub mod package_description;
pub mod package_inspect;
pub mod patch_freeze;
pub mod patch_publish;
pub mod patch_sync;
pub mod patch_test;
pub mod patch_why;
pub mod path_kind;
pub mod paths;
pub mod pull_dry_run;
pub mod push;
pub mod removed;
pub mod sbom;
pub mod script_run;
pub mod self_setup;
pub mod self_update;
pub mod signature;
pub mod status;
pub mod tag;
pub mod verification;
pub mod version;
pub mod warmed_paths;

/// Neutralizes terminal-control sequences in an untrusted name bound for an
/// operator's screen (CWE-150).
///
/// A name that reaches a report can come out of a wire document or a filesystem
/// walk, so it can carry an ESC (a CSI sequence repaints the screen or moves the
/// cursor), a C1 introducer, a newline (a forged report row), or a
/// bidirectional override (a name that *renders* as a name it does not
/// contain). Dropping the control range and the Unicode bidi-control set leaves
/// an ordinary `<ns>/<pkg>` path byte-for-byte unchanged.
///
/// Applied **at the print site**, deliberately, rather than on the value where
/// it is produced: a per-producer fix has to be repeated for every producer and
/// leaves the class open at the next one.
///
/// # Scope, stated exactly
///
/// `ocx` writes to a terminal through three channels, and this covers two:
///
/// - **stdout, plain** — covered **for the index payloads only**: [`index`]
///   (`regenerate`, `sync --dry-run`), [`catalog`] and [`tag`]. Those are the
///   payloads this work package owns. The other `api/data` payloads are **not
///   routed** — [`deps`], [`package_inspect`] and [`removed`] render
///   publisher-authored names read off fetched manifests straight into
///   `print_table` / `print_tree`, and `ocx_lib::cli` does no neutralizing of
///   its own. Closing those is a follow-up this function is ready for, not
///   something it already did.
/// - **stderr, operator prose** — the rule, because four rounds of review broke
///   a per-site list that asserted a property of a set nobody had defined:
///
///   > **In the files this work package owns, every stderr-writing site that
///   > interpolates a value not authored by the operator routes through this
///   > function.** Sites in other files do not, and are listed individually
///   > below.
///
///   The owned files are those `hex/servable-index-snapshot` touches:
///   `main.rs`, `api/data{,/index,/catalog,/tag}.rs`, `app{,/context}.rs`,
///   `command/index{,_regenerate,_update,_catalog,_sync,_common}.rs`. One
///   `grep -n 'log::\(error\|warn\|info\)!\|eprintln!'` over that set checks the
///   rule, which is the point of stating it this way — the previous four
///   sentences could only be checked by trusting them. The set is part of the
///   rule: `index_sync.rs` and `index_common.rs` arrived carrying it and were
///   missing from this list for a review round, which is the failure mode a
///   universally quantified sentence has — its scope moves and it stays true
///   about less.
///
///   That grep also returns three sites in the owned set which are **not**
///   routed and do not need to be, each for a reason a reader can confirm on
///   the line itself: `app/context.rs:133` interpolates nothing,
///   `app/context.rs:432` prints a local OS error about resolving the current
///   exe, and `app/context.rs:890` uses `{v:?}`, which escapes by construction.
///   Everything else the grep returns is a comment or a test needle.
///
///   The `main.rs` boundary does not make the per-command sites redundant. A
///   command that aggregates failures returns only the lowest-index error, so
///   its siblings' chains print at the command and never arrive; an identifier
///   a command interpolates alongside the chain the boundary never prints at
///   all; and a `warn!` on a non-fatal path never reaches it either. Sanitizing
///   only one of the two leaves a raw line on the same stream — measured, both
///   ways round.
///
///   Those per-command sites build the chain with [`sanitize_error_chain`], not
///   with `{error:#}`. The boundary can use the alternate because it holds an
///   `anyhow::Error`, whose `{:#}` walks `source()`; the commands hold an
///   `ocx_lib::Error`, whose `thiserror` Display ignores the flag — so
///   `{error:#}` there printed the outer message and dropped the cause on
///   exactly the failures no other line reports.
///
///   **Unrouted, outside the owned set**, each printing a name or chain that
///   may not be operator-authored: `command/package_info.rs:93` (argv
///   identifier, but the error's own text is remote-derived),
///   `app/update_check.rs:72` (`{identifier}` from the index chain),
///   `command/direnv_export.rs:153` (a pull failure's chain),
///   `app/managed_config_check.rs:71` (`resolved.source`, which the managed
///   tier can set) and `command/package_push.rs:221` (a registry's tag-listing
///   failure). Each is a one-line fix of this shape.
///   `conventions.rs:201,205` need none — `{:?}` escapes control characters by
///   construction.
///
///   `tracing-subscriber` is no help: its `EscapingWriter` escapes exactly
///   `\x1b`, `\x07`, `\x08`, `\x0c`, `\x7f` and `\u{80}`–`\u{9f}`, so `\n`,
///   `\r`, NUL, the rest of C0 and **every** `Cf` bidi control reach the
///   terminal untouched. A bare `eprintln!` bypasses the boundary entirely.
/// - **stdout, `--format json`** — deliberately **not** covered. That is a
///   machine channel and carries the key verbatim by design, so a consumer can
///   diff the report against the tree. `serde_json` escapes `< 0x20` and
///   nothing else, so raw C1 and raw bidi *do* reach a terminal that renders
///   JSON directly; that is the accepted cost of the verbatim guarantee.
///
/// Out of scope by choice: zero-width and tag characters (U+200B–D, U+2060,
/// U+FEFF, U+00AD, U+E0000–U+E007F). They are invisible rather than active —
/// copy-paste confusion, not screen control.
///
/// This is neutralization for display only. It is **not** a containment check
/// and must never be mistaken for one: a key that becomes a path is guarded by
/// `IndexStore::ensure_repository_contained` long before it reaches a report.
///
/// Within the three covered payloads every plain row is routed through it, each
/// pinned by a `plain_rows` test against a hostile fixture. In [`tag`] the call
/// sits *inside* `theme.tag(...)`: the theme wraps its input in ANSI of its own,
/// so neutralizing its output would strip the colour rather than the attack.
/// An error's **full cause chain**, neutralized — the form every per-command
/// failure line uses.
///
/// `{error:#}` does not give this. `anyhow::Error`'s alternate walks `source()`,
/// but these sites hold an `ocx_lib::Error`, whose `thiserror`-derived `Display`
/// ignores the alternate flag entirely — so `{error:#}` printed the outer
/// message and dropped the cause. That mattered exactly where these sites are
/// the only reporter: a batch returns the lowest-index error alone, so on a
/// 20-package run 19 diagnoses were truncated to their outer message.
///
/// Taking `&dyn Error` rather than a format string is also what makes the guard
/// on these sites checkable: a positional `{:#}` rewrite silently defeated a
/// needle that counted `{error:#}` against its sanitized form, because both
/// counts fell to zero together.
pub(crate) fn sanitize_error_chain(error: &dyn std::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    sanitize_for_terminal(&chain)
}

pub(crate) fn sanitize_for_terminal(raw: &str) -> String {
    // Three predicates, and none is redundant: `char::is_control` is `Cc`
    // only, while both the bidi overrides and the zero-width codepoints are
    // `Cf`. Dropping either `Cf` filter as a "simplification" silently
    // re-opens half the finding — the RLO re-orders what follows it, and a
    // zero-width joiner renders as nothing at all, so `you@exam\u{200b}ple.com`
    // is pixel-identical to the identity a reader believes they approved.
    raw.chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c) && !is_zero_width(*c))
        .collect()
}

/// The Unicode bidirectional formatting characters — exactly the codepoints with
/// `Bidi_Control=Yes`.
///
/// Not covered by [`char::is_control`] — they are general category `Cf`, not
/// `Cc` — and each one re-orders the glyphs after it, so a name carrying one
/// displays as text it does not contain (the Trojan Source shape, CVE-2021-42574).
pub(crate) fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// The zero-width formatting characters SEC-34 names, plus the BOM.
///
/// Category `Cf` like the bidi controls, so [`char::is_control`] misses them
/// too, but the attack is the opposite one: these render as *nothing*, so they
/// split a name into pieces that read as one word while comparing unequal to
/// it. Stripping U+200D costs the fidelity of emoji ZWJ sequences, which is
/// the trade a package manager takes for a name that means what it looks like.
pub(crate) fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row per stripped codepoint — a corpus that shares a single row per
    /// *class* cannot tell "all four are stripped" from "the first one is".
    /// The positive control is what stops a sanitizer that strips everything
    /// from passing this table.
    #[test]
    fn every_zero_width_codepoint_is_stripped_and_ordinary_text_survives() {
        let identity = "you@example.com";
        for (name, injected) in [
            ("ZWSP U+200B", "you@exam\u{200b}ple.com"),
            ("ZWNJ U+200C", "you@exam\u{200c}ple.com"),
            ("ZWJ U+200D", "you@exam\u{200d}ple.com"),
            ("BOM U+FEFF", "you@exam\u{feff}ple.com"),
        ] {
            let cleaned = sanitize_for_terminal(injected);
            assert_eq!(
                cleaned, identity,
                "{name} must not survive: a reader cannot see it, so it must not reach the terminal",
            );
        }

        // Positive control: the sanitizer is a filter, not a redactor.
        assert_eq!(
            sanitize_for_terminal(identity),
            identity,
            "ordinary text passes through untouched",
        );
    }

    /// The three predicates defend three disjoint sets; a test that only ever
    /// feeds one class cannot notice a filter being dropped.
    #[test]
    fn each_predicate_covers_a_class_the_others_miss() {
        assert!('\u{0007}'.is_control() && !is_bidi_control('\u{0007}') && !is_zero_width('\u{0007}'));
        assert!(!'\u{202e}'.is_control() && is_bidi_control('\u{202e}') && !is_zero_width('\u{202e}'));
        assert!(!'\u{200b}'.is_control() && !is_bidi_control('\u{200b}') && is_zero_width('\u{200b}'));
    }
}
