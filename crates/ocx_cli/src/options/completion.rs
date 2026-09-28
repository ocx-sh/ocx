// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::hook::{Rung, resolve_ladder};

/// Whether to inject shell completions during `ocx self activate`.
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
    /// Whether completions load: the `[shell] hook` ladder over the flags, `OCX_NO_COMPLETIONS`,
    /// `[shell] completions` (`configured`), then `interactive`.
    // Take `interactive` from the caller, never probe stderr here: the shim may have redirected it.
    pub fn enabled(&self, interactive: bool, configured: Option<bool>) -> bool {
        self.resolve(interactive, configured).0
    }

    /// Which rung of the ladder decided [`Self::enabled`] for the same inputs.
    pub fn rung(&self, interactive: bool, configured: Option<bool>) -> Rung {
        self.resolve(interactive, configured).1
    }

    // Both accessors share this one evaluation, or the reported rung can disagree with the decision.
    fn resolve(&self, interactive: bool, configured: Option<bool>) -> (bool, Rung) {
        let flag = if self.no_completion {
            Some(false)
        } else if self.completion {
            Some(true)
        } else {
            None
        };
        resolve_ladder(
            flag,
            ocx_util::env::flag("OCX_NO_COMPLETIONS", false),
            configured,
            interactive,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preference(completion: bool, no_completion: bool) -> Completion {
        Completion {
            completion,
            no_completion,
        }
    }

    /// C-039 rung 2: `--completion` forces completions on even when the session
    /// is not interactive and `[shell] completions = false` — the production
    /// condition where the shim sets the flag and stderr is redirected.
    ///
    /// Independent of the ambient environment: rungs 1 and 2 are decided before
    /// `OCX_NO_COMPLETIONS` is consulted.
    #[test]
    fn explicit_completion_outranks_config_and_non_interactive() {
        assert_eq!(
            (
                preference(true, false).enabled(false, Some(false)),
                preference(true, false).rung(false, Some(false))
            ),
            (true, Rung::FlagOn)
        );
    }

    /// C-039 rung 1: `--no-completion` forces completions off even in an
    /// interactive session with `[shell] completions = true`.
    #[test]
    fn explicit_no_completion_outranks_config_and_interactive() {
        assert_eq!(
            (
                preference(false, true).enabled(true, Some(true)),
                preference(false, true).rung(true, Some(true))
            ),
            (false, Rung::FlagOff)
        );
    }

    /// C-039: `--no-completion` wins when both flags are set. `overrides_with`
    /// makes clap last-wins, so this state is unreachable from a command line;
    /// the tie-break is pinned anyway because the struct is constructible.
    #[test]
    fn no_completion_wins_when_both_flags_are_set() {
        assert_eq!(preference(true, true).rung(true, None), Rung::FlagOff);
    }
}
