// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod about;
pub mod announce;
pub mod attestation;
pub mod catalog;
pub mod claim;
pub mod clean;
pub mod config_setup;
pub mod config_test;
pub mod config_update;
pub mod deps;
pub mod env;
/// Report vocabulary shared by `package claim` and `package announce`; no `Printable`, so no `report_roots!` row.
pub mod forge_report;
pub mod index;
pub mod install;
pub mod lock;
pub mod login;
pub mod package_cascade_check;
pub mod package_cascade_repair;
pub mod package_copy;
pub mod package_description;
pub mod package_inspect;
pub mod package_receipt;
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
pub mod shell_state;
pub mod signature;
pub mod status;
pub mod sweep;
pub mod tag;
pub mod update;
pub mod verification;
pub mod version;
pub mod warmed_paths;

use ocx_console::{Theme, VisibilityStyle};
use ocx_oci::PackageRef;
use ocx_package::metadata::visibility::Visibility;

/// Picks the palette entry an env-entry visibility renders in.
pub fn visibility_style(visibility: Visibility) -> VisibilityStyle {
    match (visibility.private, visibility.interface) {
        (true, true) => VisibilityStyle::Public,
        (true, false) => VisibilityStyle::Private,
        (false, true) => VisibilityStyle::Interface,
        (false, false) => VisibilityStyle::Sealed,
    }
}

/// Composes an identifier in colour (tag, `@` and digest each distinct); with colour off it
/// equals the identifier's `Display` byte for byte.
pub fn ink_identifier(theme: &Theme, identifier: &PackageRef) -> String {
    let mut out = format!("{}/{}", identifier.registry(), identifier.repository());
    if let Some(tag) = identifier.tag() {
        out.push_str(&theme.tag(format!(":{tag}")));
    }
    if let Some(digest) = identifier.digest() {
        out.push_str(&theme.punct("@"));
        out.push_str(&theme.digest(digest.to_string()));
    }
    out
}

/// An error's full cause chain, neutralized, for a per-command failure line.
///
/// Not `{error:#}`: a `thiserror` `Display` ignores the flag, so every cause is lost.
/// Needed beside the `main.rs` boundary: a batch's other errors and a `warn!` never reach it.
pub(crate) fn sanitize_error_chain(error: &dyn std::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push_str(&format!(": {cause}"));
        source = cause.source();
    }
    sanitize_for_terminal(&chain)
}

/// Neutralizes terminal-control characters (CWE-150) in an untrusted name bound for an operator's
/// screen; an ordinary `<ns>/<pkg>` passes byte-for-byte. Display only, not a containment check.
///
/// Apply at the print site: `tracing-subscriber` escapes too little and a bare `eprintln!` bypasses
/// the `main.rs` boundary. Routed sites: `adr_servable_index_snapshot.md` § Rationale from code: ocx_cli.
pub fn sanitize_for_terminal(raw: &str) -> String {
    // `is_control` covers `Cc` only: dropping either `Cf` filter lets a bidi override reorder a
    // name or a zero-width char make it pixel-identical to one a reader approved.
    raw.chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c) && !is_zero_width(*c))
        .collect()
}

/// The `Bidi_Control=Yes` codepoints, which [`char::is_control`] misses (`Cf`); each re-orders
/// the glyphs after it (Trojan Source, CVE-2021-42574).
pub(crate) fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// The zero-width space, non-joiner and joiner, plus the BOM: `Cf`, so [`char::is_control`]
/// misses them, and they render as nothing. Stripping U+200D breaks emoji ZWJ sequences on purpose.
pub(crate) fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(spec: &str) -> PackageRef {
        PackageRef::parse_with_default_registry(spec, "ocx.sh").unwrap()
    }

    #[test]
    fn ink_plain_equals_display_for_all_part_combinations() {
        let theme = Theme::new(false);
        let specs = [
            "ocx.sh/cmake".to_string(),
            "ocx.sh/cmake:3.28".to_string(),
            format!("ocx.sh/cmake@sha256:{}", "a".repeat(64)),
            format!("ocx.sh/cmake:3.28@sha256:{}", "b".repeat(64)),
        ];
        for spec in specs {
            let identifier = id(&spec);
            assert_eq!(
                ink_identifier(&theme, &identifier),
                identifier.to_string(),
                "plain ink must match Display"
            );
        }
    }

    #[test]
    fn ink_colored_strips_back_to_display() {
        let theme = Theme::new(true);
        let identifier = id(&format!("ocx.sh/cmake:3.28@sha256:{}", "a".repeat(64)));
        let inked = ink_identifier(&theme, &identifier);
        assert!(inked.contains("\x1b["), "expected ANSI in colored ink");
        assert_eq!(console::strip_ansi_codes(&inked), identifier.to_string());
    }

    /// The visibility palette is reached through two hops now — this crate maps
    /// the axes onto a [`VisibilityStyle`], the theme maps that onto a colour —
    /// and a swapped arm in either hop renders a wrong-but-plausible colour that
    /// no colour-off assertion can see. Four distinct axis pairs must still
    /// produce four distinct renderings.
    #[test]
    fn each_visibility_pair_reaches_its_own_colour() {
        let theme = Theme::new(true);
        let mut seen: Vec<(Visibility, String)> = Vec::new();
        for visibility in [
            Visibility::PUBLIC,
            Visibility::PRIVATE,
            Visibility::INTERFACE,
            Visibility::SEALED,
        ] {
            let painted = theme.visibility(visibility_style(visibility), "vis");
            assert_eq!(
                console::strip_ansi_codes(&painted),
                "vis",
                "{visibility:?} must keep its text"
            );
            for (other, other_painted) in &seen {
                assert_ne!(
                    &painted, other_painted,
                    "{visibility:?} and {other:?} render identically — one of the two mapping hops \
                     collapsed them"
                );
            }
            seen.push((visibility, painted));
        }
    }

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
