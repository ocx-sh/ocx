// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Render modifiers for resolved interpolation tokens.
//!
//! A recognised `${…}` token (`scanner.rs`) may carry an optional render
//! modifier — `:native` or `:posix` — that governs how its *already-resolved*
//! value is rendered before landing in the output string. [`render`] is the
//! pure transform: no I/O, no `cfg!` read inside the body, `host` is always
//! the caller's explicit parameter. That is what makes both `:posix` legs
//! testable on any CI host (`adr_interpolation_token_grammar.md` D5, C-014,
//! C-015).
//!
//! Rendering composes **after** `dunce::simplified`, never instead of it —
//! stripping a Windows `\\?\` verbatim prefix stays `TemplateResolver`'s job
//! in the parent module. UNC paths and verbatim prefixes are explicit
//! non-goals: OCX's input space is paths it generated itself
//! (`$OCX_HOME`-rooted, digest-sharded, ASCII-slugified), never an arbitrary
//! user-typed filesystem path.

use std::borrow::Cow;

/// The closed set of token render modifiers.
///
/// Distinct from a variable's wire *type* (`env::modifier::Modifier` —
/// `path`/`constant`/`list`, a **combination** axis: how a declared value
/// combines with an existing one). `RenderModifier` answers a **rendering**
/// axis instead — how an already-resolved value is rendered — and is never
/// serialized. The modifier never carries free text: a future modifier that
/// takes an argument needs a new ADR, not a new arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderModifier {
    /// The resolved value in the host's native form. Identical to omitting
    /// the modifier — the identity function on every host, for every input.
    Native,
    /// On [`Host::Windows`]: [`RenderModifier::Native`], then every `\`
    /// replaced by `/`, drive letter preserved (`C:\Users\x` →
    /// `C:/Users/x`). On [`Host::Unix`]: the identity function, so a POSIX
    /// filename that happens to contain a backslash is not corrupted.
    Posix,
}

/// The host [`render`] renders for, passed explicitly rather than read from
/// `cfg!` inside the transform.
///
/// [`Host::current`] is the real host and carries no `cfg`-gated seam of its
/// own — `render` requires it to stay pure (D5), so no `__OCX_*` env read
/// belongs here. `TemplateResolver` (the parent module) is where C-013/C-017
/// need the override: it will carry a `Host` field defaulting to
/// [`Host::current`], overridable through a
/// `#[cfg(any(test, feature = "__testing"))]` constructor on
/// `TemplateResolver` itself, so its unit tests construct a resolver
/// pinned to either host with no runtime env read.
///
/// Deliberately not `oci::platform::OperatingSystem` (`operating_system.rs`):
/// that type is a serialized OCI wire value with an `Option`-shaped read from
/// `std::env::consts::OS` (three arms plus "unknown"), while `Host::current`
/// must key on exactly the predicate `dunce::simplified` uses — `cfg(windows)`
/// — because `render` is required to compose after it (D5). Reusing
/// `OperatingSystem` would import a `None` case this module has no business
/// deciding and split that one predicate across two types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    Windows,
    Unix,
}

impl Host {
    /// The real host this process is running on.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(windows) { Self::Windows } else { Self::Unix }
    }
}

/// Renders an already-resolved token value for `modifier` on `host`.
///
/// Pure: no I/O, no `cfg!` read in the body — `host` is the only source of
/// platform behavior. [`RenderModifier::Native`] is the identity function for
/// every host and every input. [`RenderModifier::Posix`] is host-conditional,
/// not an unconditional slash flip: a POSIX filename may legitimately
/// contain a backslash, so an unconditional flip would corrupt it
/// (`adr_interpolation_token_grammar.md` D5).
///
/// Composes **after** `dunce::simplified` — never emits and never strips a
/// `\\?\` verbatim prefix. UNC paths and verbatim prefixes are explicit
/// non-goals.
#[must_use]
pub fn render(value: &str, modifier: RenderModifier, host: Host) -> Cow<'_, str> {
    // Matched on both axes rather than with a `_` catch-all: a future arm on
    // either enum must then be a compile error here, not a silent identity.
    match modifier {
        RenderModifier::Native => Cow::Borrowed(value),
        RenderModifier::Posix => match host {
            Host::Windows => Cow::Owned(value.replace('\\', "/")),
            Host::Unix => Cow::Borrowed(value),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Values covering every shape the render seam can meet: nothing, a
    /// forward-slash-only path (the shape under which a mis-mapped `Native`
    /// is invisible), a drive-letter path, a mixed-separator value, a
    /// non-ASCII value, and a value with no separator at all.
    const CORPUS: &[&str] = &[
        "",
        "/home/user/content",
        "C:\\Users\\x",
        "a\\b/c\\d",
        "/home/plätzchen/bin",
        "no-separator-here",
    ];

    /// The verbatim (`\\?\`) prefix `render` must never emit — spelled with
    /// escapes rather than a raw literal so the trailing backslash is obvious.
    const VERBATIM_PREFIX: &str = "\\\\?\\";

    // C-012 — `:native` is the identity function, for every host and every
    // input. Breadth leg: the discrimination lives in the test below.
    //
    // The `Cow::Borrowed` assertion guards a property equality cannot see:
    // `assert_eq!(Cow<str>, &str)` compares content, so an implementation that
    // allocated on every call would pass the equality unnoticed.
    #[test]
    fn native_renders_every_input_unchanged_on_both_hosts() {
        for host in [Host::Windows, Host::Unix] {
            for value in CORPUS {
                let rendered = render(value, RenderModifier::Native, host);

                assert_eq!(rendered, *value, "native must not alter {value:?} on {host:?}");
                assert!(
                    matches!(rendered, Cow::Borrowed(_)),
                    "native must not allocate for {value:?} on {host:?}"
                );
            }
        }
    }

    // C-012 — the discriminating leg. On `Host::Windows` a backslash-bearing
    // value is where `Native` and `Posix` produce different bytes, so an
    // implementation that wrongly maps `Native` onto the `Posix` transform
    // reds here. Without the second assertion the first is vacuous: a value
    // `Posix` also leaves alone proves nothing about the mapping.
    #[test]
    fn native_on_windows_keeps_backslashes_that_posix_would_flip() {
        let value = "C:\\Users\\x";

        assert_eq!(render(value, RenderModifier::Native, Host::Windows), value);
        assert_ne!(render(value, RenderModifier::Posix, Host::Windows), value);
    }

    // C-014 — `:posix` on Windows flips separators and preserves the drive
    // letter.
    #[test]
    fn posix_on_windows_flips_backslashes_and_keeps_the_drive_letter() {
        assert_eq!(
            render("C:\\Users\\x", RenderModifier::Posix, Host::Windows),
            "C:/Users/x"
        );
    }

    // C-014 — the three wrong answers, asserted as absences. A drive letter
    // rewritten to `/c/` (MSYS) or `/mnt/c/` (WSL), or a verbatim prefix
    // leaking through, are each a plausible implementation that the equality
    // above already excludes; naming them keeps the exclusion legible.
    #[test]
    fn posix_on_windows_emits_no_verbatim_no_msys_and_no_wsl_drive_form() {
        let rendered = render("C:\\Users\\x", RenderModifier::Posix, Host::Windows);

        assert!(
            !rendered.contains(VERBATIM_PREFIX),
            "verbatim prefix leaked into {rendered:?}"
        );
        assert!(!rendered.starts_with("/c/"), "MSYS drive form emitted: {rendered:?}");
        assert!(!rendered.starts_with("/mnt/c/"), "WSL drive form emitted: {rendered:?}");
    }

    // C-015 — `:posix` is the identity off Windows, so a POSIX filename that
    // legitimately contains a backslash survives intact.
    #[test]
    fn posix_off_windows_keeps_a_backslash_in_a_posix_filename() {
        let rendered = render("/home/a\\b", RenderModifier::Posix, Host::Unix);

        assert_eq!(rendered, "/home/a\\b");
        assert!(
            matches!(rendered, Cow::Borrowed(_)),
            "identity off Windows must not allocate: {rendered:?}"
        );
    }

    // Not a numbered contract, but `Host::current` is a stub whose failure
    // would be silent: every caller would render for the wrong host.
    //
    // The oracle is `std::path::MAIN_SEPARATOR` — std's own answer for this
    // target — rather than `cfg!(windows)`. Re-deriving the implementation's
    // predicate makes the two agree by construction: such a test discriminates
    // an arm swap and nothing else, and would stay green on a `Host::current`
    // rewritten to key on the wrong thing entirely.
    #[test]
    fn host_current_names_the_host_this_target_runs_on() {
        let expected = if std::path::MAIN_SEPARATOR == '\\' {
            Host::Windows
        } else {
            Host::Unix
        };

        assert_eq!(Host::current(), expected);
    }
}
