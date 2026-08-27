// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Whether to install the per-prompt reconcile hook during
//! `ocx self activate` / write it during `ocx self setup`.
//!
//! Hosts the five-rung enablement ladder both `[shell]` toggles share
//! (C-038, C-039): [`resolve_ladder`] is the single implementation, and
//! [`super::Completion`] evaluates it with its own flag pair and its own
//! environment key.

use ocx_lib::env;

/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--hook` / `--no-hook` flags.
///
/// `--hook` forces the per-prompt hook on, `--no-hook` forces it off. The two
/// are POSIX last-wins, so passing both is not an error. With neither flag the
/// decision follows the ladder in [`Hook::enabled`].
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Hook {
    /// Force the per-prompt hook on, regardless of session interactivity.
    #[clap(long = "hook", overrides_with = "no_hook")]
    hook: bool,

    /// Force the per-prompt hook off.
    #[clap(long = "no-hook", overrides_with = "hook")]
    no_hook: bool,
}

/// Which rung of the five-rung ladder decided the answer (C-038).
///
/// Exposed alongside [`Hook::enabled`] and [`super::Completion::enabled`] so
/// that `ocx shell state` reads the decision instead of deriving it a second
/// time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    /// Rung 1 — `--no-hook` / `--no-completion`.
    FlagOff,
    /// Rung 2 — `--hook` / `--completion`.
    FlagOn,
    /// Rung 3 — `OCX_NO_HOOK` / `OCX_NO_COMPLETIONS` truthy.
    EnvOptOut,
    /// Rung 4 — `[shell] hook` / `[shell] completions`.
    Configured,
    /// Rung 5 — auto: the caller's interactivity probe.
    Auto,
}

/// Resolve the enablement ladder shared by `[shell] hook` and
/// `[shell] completions`, returning both the decision and the rung that made
/// it.
///
/// Rungs, most specific first:
///
/// 1. `flag` is `Some(false)` (`--no-X`) → off
/// 2. `flag` is `Some(true)` (`--X`) → on
/// 3. `env_opt_out` (`OCX_NO_X` truthy) → off
/// 4. `configured` (`[shell] X`) → as set
/// 5. auto → `interactive`
// One implementation for both keys, deliberately: a precedence that differed
// between `hook` and `completions` would make the `[shell]` grammar
// unlearnable, and the arm order below is the whole contract.
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
    ///
    /// Ladder, most specific first:
    ///
    /// 1. `--no-hook` → off
    /// 2. `--hook` → on
    /// 3. `OCX_NO_HOOK` truthy → off
    /// 4. `[shell] hook` (`configured`) → as set
    /// 5. auto: `interactive`
    ///
    /// The default is on, in interactive shells only.
    //
    // `interactive` is decided shell-side and passed in, by every shim, through
    // the `--interactive`/`--no-interactive` pair ([`super::Interactive`]): `$-`
    // on POSIX, `status is-interactive` on fish, `[Console]::IsInputRedirected`
    // on pwsh, `test -t 0` on elvish. The binary probes only when no caller
    // spoke, because no descriptor it can see answers this correctly — it
    // redirects its own stderr, and `ssh -t host 'bash -lc …'` hands a terminal
    // on stdin to a shell that never renders a prompt.
    //
    // The pair feeds this rung's INPUT and never becomes a rung: `--interactive`
    // at rung 2 would outrank `OCX_NO_HOOK` and `[shell] hook`, revoking both
    // opt-outs for every shell the shims start.
    pub fn enabled(&self, interactive: bool, configured: Option<bool>) -> bool {
        self.resolve(interactive, configured).0
    }

    /// Which rung of the ladder decided [`Self::enabled`] for the same inputs.
    pub fn rung(&self, interactive: bool, configured: Option<bool>) -> Rung {
        self.resolve(interactive, configured).1
    }

    // One ladder evaluation feeds both accessors, so the reported rung can
    // never disagree with the decision it explains.
    fn resolve(&self, interactive: bool, configured: Option<bool>) -> (bool, Rung) {
        let flag = if self.no_hook {
            Some(false)
        } else if self.hook {
            Some(true)
        } else {
            None
        };
        // A bare literal, not an `ocx_lib::env::keys` entry: the sibling this
        // ladder mirrors reads `OCX_NO_COMPLETIONS` the same way, and moving
        // one new key into `keys` would change a shipped module for nothing.
        // Negative-only like every other toggle here — `--hook` is the positive
        // channel, and "auto" is what an unset variable already means.
        resolve_ladder(flag, env::flag("OCX_NO_HOOK", false), configured, interactive)
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
    /// duration. Consolidated into one test so exactly one test function
    /// mutates the process environment; precedent:
    /// `ocx_lib::oci::host_capabilities`.
    /// EC-CFG-011 — the hook and completions ladders read their own keys and never each other's.
    #[test]
    fn each_ladder_reads_its_own_environment_key() {
        // SAFETY: this is the only test that touches OCX_NO_HOOK or
        // OCX_NO_COMPLETIONS; a single #[test] gives the ordering guarantee.
        unsafe {
            std::env::remove_var("OCX_NO_HOOK");
            std::env::remove_var("OCX_NO_COMPLETIONS");
        }
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

        // SAFETY: see above.
        unsafe { std::env::set_var("OCX_NO_HOOK", "1") };
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

        // SAFETY: see above.
        unsafe {
            std::env::remove_var("OCX_NO_HOOK");
            std::env::set_var("OCX_NO_COMPLETIONS", "1");
        }
        assert_eq!(
            (
                Completion::default().enabled(true, Some(true)),
                Completion::default().rung(true, Some(true))
            ),
            (false, Rung::EnvOptOut),
            "OCX_NO_COMPLETIONS=1 must disable completions at rung 3"
        );
        assert!(
            Hook::default().enabled(true, Some(true)),
            "OCX_NO_COMPLETIONS must not reach the hook ladder"
        );

        // SAFETY: see above.
        unsafe { std::env::remove_var("OCX_NO_COMPLETIONS") };
    }
}
