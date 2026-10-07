// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Whether `ocx self activate` / `ocx self setup` install the per-prompt
//! reconcile hook, and the enablement ladder both `[shell]` toggles share.

/// The paired `--hook` / `--no-hook` flags, POSIX last-wins; with neither,
/// `Hook::enabled` follows the ladder.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Hook {
    /// Force the per-prompt hook on, regardless of session interactivity.
    #[clap(long = "hook", overrides_with = "no_hook")]
    hook: bool,

    /// Force the per-prompt hook off.
    #[clap(long = "no-hook", overrides_with = "hook")]
    no_hook: bool,
}

/// Which rung of the five-rung ladder decided the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    /// Rung 1 — `--no-hook` / `--no-completion`.
    FlagOff,
    /// Rung 2 — `--hook` / `--completion`.
    FlagOn,
    /// Rung 3 — `OCX_NO_HOOK` / `OCX_NO_COMPLETION` truthy.
    EnvOptOut,
    /// Rung 4 — `[shell] hook` / `[shell] completions`.
    Configured,
    /// Rung 5 — auto: the caller's interactivity probe.
    Auto,
}

/// Resolve the enablement ladder shared by `[shell] hook` and
/// `[shell] completions`, returning both the decision and the rung that made it.
// The arm order is the precedence of both `[shell]` keys.
pub(crate) fn resolve_ladder(
    flag: Option<bool>,
    env_opt_out: bool,
    configured: Option<bool>,
    interactive: bool,
) -> (bool, Rung) {
    match (flag, env_opt_out, configured) {
        (Some(false), _, _) => (false, Rung::FlagOff),
        (Some(true), _, _) => (true, Rung::FlagOn),
        (None, true, _) => (false, Rung::EnvOptOut),
        (None, false, Some(configured)) => (configured, Rung::Configured),
        (None, false, None) => (interactive, Rung::Auto),
    }
}

impl Hook {
    /// Resolve whether the per-prompt hook is enabled for this session.
    // `interactive` comes from the shim, never a probe: shims redirect stderr, and `ssh -t` hands a
    // non-prompting shell a terminal. Never give `--interactive` a rung, or it outranks both opt-outs.
    pub fn enabled(&self, interactive: bool, configured: Option<bool>) -> bool {
        self.resolve(interactive, configured).0
    }

    /// Which rung of the ladder decided [`Self::enabled`] for the same inputs.
    pub fn rung(&self, interactive: bool, configured: Option<bool>) -> Rung {
        self.resolve(interactive, configured).1
    }

    // One evaluation feeds both accessors, so the reported rung cannot disagree with the decision.
    fn resolve(&self, interactive: bool, configured: Option<bool>) -> (bool, Rung) {
        let flag = if self.no_hook {
            Some(false)
        } else if self.hook {
            Some(true)
        } else {
            None
        };
        resolve_ladder(
            flag,
            ocx_env::OCX_NO_HOOK.bool_or(false).unwrap_or(false),
            configured,
            interactive,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::Completion;

    fn hook(hook: bool, no_hook: bool) -> Hook {
        Hook { hook, no_hook }
    }

    /// C-038/C-039 rung 1 (S-014): `--no-X` decides off, over an env opt-out, a
    /// `[shell]` value and an interactivity signal that each say otherwise —
    /// and it is the absence of the flag, not its value, that lets rung 3 run.
    /// EC-CFG-009 — the explicit flag is rung 1 and outranks env, config and auto.
    #[test]
    fn rung_one_flag_off_outranks_every_lower_rung() {
        assert_eq!(
            resolve_ladder(Some(false), false, Some(true), true),
            (false, Rung::FlagOff),
            "--no-X must beat `[shell] X = true` and an interactive session"
        );
        assert_eq!(
            resolve_ladder(Some(false), true, Some(true), true),
            (false, Rung::FlagOff),
            "--no-X must decide even when the env opt-out would answer the same"
        );
        assert_eq!(
            resolve_ladder(None, true, Some(true), true),
            (false, Rung::EnvOptOut),
            "without the flag, rung 3 decides — rung 1 must not fire on absence"
        );
    }

    /// C-038/C-039 rung 2: `--X` decides on, over the env opt-out that would
    /// otherwise turn it off — the precedence a swapped pair of arms inverts.
    #[test]
    fn rung_two_flag_on_outranks_env_config_and_auto() {
        assert_eq!(
            resolve_ladder(Some(true), true, Some(false), false),
            (true, Rung::FlagOn),
            "--X must beat OCX_NO_X, `[shell] X = false` and a non-interactive session"
        );
        assert_eq!(
            resolve_ladder(None, true, Some(false), false),
            (false, Rung::EnvOptOut),
            "without the flag, rung 3 decides — rung 2 must not fire on absence"
        );
    }

    /// C-038/C-039 rung 3 (S-015): a truthy `OCX_NO_X` decides off over a
    /// `[shell]` value and an interactive session, and yields to rung 4 when
    /// it is falsy.
    /// EC-CFG-010 — OCX_NO_HOOK is rung 3, above config and auto.
    #[test]
    fn rung_three_env_opt_out_outranks_config_and_auto() {
        assert_eq!(
            resolve_ladder(None, true, Some(true), true),
            (false, Rung::EnvOptOut),
            "OCX_NO_X must beat `[shell] X = true` and an interactive session"
        );
        assert_eq!(
            resolve_ladder(None, false, Some(true), false),
            (true, Rung::Configured),
            "a falsy OCX_NO_X must let rung 4 decide"
        );
    }

    /// C-038/C-039 rung 4: `[shell] X` decides in both directions, over the
    /// interactivity signal, and yields to rung 5 when unset.
    #[test]
    fn rung_four_configured_decides_both_directions_over_auto() {
        assert_eq!(
            resolve_ladder(None, false, Some(true), false),
            (true, Rung::Configured),
            "`[shell] X = true` must turn a non-interactive session on"
        );
        assert_eq!(
            resolve_ladder(None, false, Some(false), true),
            (false, Rung::Configured),
            "`[shell] X = false` must turn an interactive session off"
        );
        assert_eq!(
            resolve_ladder(None, false, None, true),
            (true, Rung::Auto),
            "an unset `[shell] X` must let rung 5 decide"
        );
    }

    /// C-038/C-039 rung 5: with nothing above it set, the decision is the
    /// caller's interactivity signal, in both directions.
    #[test]
    fn rung_five_auto_follows_interactivity() {
        assert_eq!(
            resolve_ladder(None, false, None, true),
            (true, Rung::Auto),
            "interactive auto must be on"
        );
        assert_eq!(
            resolve_ladder(None, false, None, false),
            (false, Rung::Auto),
            "non-interactive auto must be off"
        );
    }

    /// C-038: `Hook` maps its flag pair onto rungs 1 and 2, `--no-hook` wins
    /// when both are set, and `enabled` never disagrees with `rung`.
    #[test]
    fn hook_flags_map_onto_the_first_two_rungs() {
        for (flags, expected) in [
            (hook(false, true), (false, Rung::FlagOff)),
            (hook(true, false), (true, Rung::FlagOn)),
            (hook(true, true), (false, Rung::FlagOff)),
        ] {
            assert_eq!(
                (flags.enabled(true, Some(true)), flags.rung(true, Some(true))),
                expected,
                "flag rungs decide before the environment is consulted, so this holds \
                 whatever OCX_NO_HOOK carries ambiently"
            );
        }
    }

    /// C-038/C-039 rung 3 wiring: each struct reads **its own** environment key
    /// and threads `configured` through, proven by owning both keys for the
    /// duration.
    /// EC-CFG-011 — the hook and completions ladders read their own keys and never each other's.
    #[test]
    fn each_ladder_reads_its_own_environment_key() {
        let env = ocx_env::overrides::lock();
        env.remove(&ocx_env::OCX_NO_HOOK);
        env.remove(&ocx_env::OCX_NO_COMPLETION);
        assert!(
            Hook::default().enabled(false, Some(true)),
            "rung 4 must reach the hook ladder: `[shell] hook = true` beats a non-interactive session"
        );
        assert!(
            Completion::default().enabled(false, Some(true)),
            "rung 4 must reach the completions ladder too"
        );
        assert_eq!(
            Hook::default().rung(true, None),
            Rung::Auto,
            "with no flag, no env key and no config, rung 5 decides"
        );

        env.set(&ocx_env::OCX_NO_HOOK, "1");
        assert_eq!(
            (
                Hook::default().enabled(true, Some(true)),
                Hook::default().rung(true, Some(true))
            ),
            (false, Rung::EnvOptOut),
            "OCX_NO_HOOK=1 must disable the hook at rung 3"
        );
        assert!(
            Completion::default().enabled(true, Some(true)),
            "OCX_NO_HOOK must not reach the completions ladder"
        );

        env.remove(&ocx_env::OCX_NO_HOOK);
        env.set(&ocx_env::OCX_NO_COMPLETION, "1");
        assert_eq!(
            (
                Completion::default().enabled(true, Some(true)),
                Completion::default().rung(true, Some(true))
            ),
            (false, Rung::EnvOptOut),
            "OCX_NO_COMPLETION=1 must disable completions at rung 3"
        );
        assert!(
            Hook::default().enabled(true, Some(true)),
            "OCX_NO_COMPLETION must not reach the hook ladder"
        );
    }
}
