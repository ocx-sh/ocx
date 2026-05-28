// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::env;

/// Whether to inject shell completions during `ocx self activate`.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--completion` / `--no-completion` flags. `--completion` forces completions
/// on, `--no-completion` forces them off; the two use POSIX last-wins semantics
/// (`overrides_with`), matching the `--pull` / `--no-pull` convention. When
/// neither flag is given the decision is automatic — see [`Completion::enabled`].
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Completion {
    /// Force shell-completion injection on, regardless of session interactivity.
    #[clap(long = "completion", overrides_with = "no_completion")]
    completion: bool,

    /// Force shell-completion injection off.
    #[clap(long = "no-completion", overrides_with = "completion")]
    no_completion: bool,
}

impl Completion {
    /// Resolve whether completions should be loaded for this session.
    ///
    /// Precedence (most specific first):
    ///
    /// 1. `--no-completion` → off
    /// 2. `--completion` → on
    /// 3. `OCX_NO_COMPLETIONS` set → off
    /// 4. auto: `interactive` — the caller's interactivity probe (typically
    ///    whether stderr is a TTY)
    ///
    /// The interactivity signal is passed in rather than probed here so the
    /// decision stays pure and unit-testable across every branch.
    pub fn enabled(&self, interactive: bool) -> bool {
        if self.no_completion {
            return false;
        }
        if self.completion {
            return true;
        }
        if env::flag("OCX_NO_COMPLETIONS", false) {
            return false;
        }
        interactive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pref(completion: bool, no_completion: bool) -> Completion {
        Completion {
            completion,
            no_completion,
        }
    }

    /// `--completion` forces completions on even when the session is not
    /// interactive — the production condition where the shim sets the flag and
    /// stderr is redirected (so the auto probe would otherwise be false).
    #[test]
    fn explicit_completion_overrides_non_interactive() {
        assert!(pref(true, false).enabled(false));
    }

    /// `--no-completion` forces completions off even in an interactive session.
    #[test]
    fn explicit_no_completion_overrides_interactive() {
        assert!(!pref(false, true).enabled(true));
    }

    /// With neither flag the auto interactivity signal decides — but only when
    /// `OCX_NO_COMPLETIONS` is unset. Guarded on the ambient env so a hostile
    /// opt-out cannot make the assertion vacuously pass.
    #[test]
    fn auto_follows_interactivity_when_env_unset() {
        if env::flag("OCX_NO_COMPLETIONS", false) {
            return; // ambient opt-out: gate is correctly false regardless
        }
        assert!(pref(false, false).enabled(true), "interactive auto → on");
        assert!(!pref(false, false).enabled(false), "non-interactive auto → off");
    }
}
