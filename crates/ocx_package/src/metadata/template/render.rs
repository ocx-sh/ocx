// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `:native` / `:posix` render modifiers for resolved interpolation tokens.
//!
//! Rendering composes after `dunce::simplified`, never instead of it; UNC and verbatim
//! prefixes are non-goals, since OCX only renders paths it generated.

use std::borrow::Cow;

/// The closed set of token render modifiers; never serialized, and distinct from a var's
/// wire `type` (`env::modifier::Modifier`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderModifier {
    /// The host-native form; identical to omitting the modifier.
    Native,
    /// On Windows every `\` becomes `/` (`C:\Users\x` → `C:/Users/x`); the identity on Unix,
    /// where a filename may contain a backslash.
    Posix,
}

/// The host [`render`] renders for, passed in so `render` stays pure and testable.
// Not `OperatingSystem`: this must key on `cfg(windows)`, the predicate `dunce::simplified` uses.
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

/// Renders an already-resolved token value for `modifier` on `host`
/// (`adr_interpolation_token_grammar.md`).
#[must_use]
pub fn render(value: &str, modifier: RenderModifier, host: Host) -> Cow<'_, str> {
    // No `_` arm, so a new variant on either enum is a compile error, not a silent identity.
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
