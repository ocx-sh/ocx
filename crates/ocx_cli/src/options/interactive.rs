// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether the shell that invoked `ocx self activate` is an interactive one.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--interactive` / `--no-interactive` flags. `--interactive` declares the
/// session interactive, `--no-interactive` declares it not. The two are POSIX
/// last-wins, so passing both is not an error. With neither flag the caller's
/// own terminal probe decides — see [`Interactive::resolve`].
///
/// Both flags are hidden: they are machine surface, emitted by the
/// `$OCX_HOME/env.*` shims from the interactivity test their own shell language
/// provides, not something to type.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Interactive {
    /// Declare this shell session interactive, instead of probing for a terminal.
    #[clap(long = "interactive", overrides_with = "no_interactive", hide = true)]
    interactive: bool,

    /// Declare this shell session non-interactive.
    #[clap(long = "no-interactive", overrides_with = "interactive", hide = true)]
    no_interactive: bool,
}

impl Interactive {
    /// Resolve whether this session is interactive, most specific first:
    ///
    /// 1. `--no-interactive` → false
    /// 2. `--interactive` → true
    /// 3. `probed` — the caller's own terminal probe
    ///
    /// This is the **input to** the `auto` rung of the `[shell] hook` and
    /// `[shell] completions` ladders ([`super::hook::resolve_ladder`] rung 5),
    /// never a rung of its own. A shim must pass this pair and not `--hook`:
    /// rung 2 outranks `OCX_NO_HOOK` and `[shell] hook`, so a shim spelling its
    /// answer as `--hook` would revoke both opt-outs for every shell it starts.
    //
    // The shell knows the answer and the binary cannot ask for it. Every shipped
    // shim runs `self activate` inside a command substitution with stderr
    // redirected, and stdin is no better: `ssh -t host 'bash -lc …'` allocates a
    // pty for a shell that reads the login profile and exits without ever
    // rendering a prompt, while Emacs `M-x shell` drives a genuinely interactive
    // session over pipes on all three descriptors. `$-`, `status is-interactive`
    // and `[Console]::IsInputRedirected` answer both correctly.
    //
    // `probed` stays as the fallback rather than a required argument so a shim
    // written by an older `ocx self setup` — which sends no flag until the user
    // re-runs setup or `self update` refreshes it — resolves exactly as it does
    // today.
    pub fn resolve(&self, probed: bool) -> bool {
        if self.no_interactive {
            false
        } else if self.interactive {
            true
        } else {
            probed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preference(interactive: bool, no_interactive: bool) -> Interactive {
        Interactive {
            interactive,
            no_interactive,
        }
    }

    /// C-038 rung 5: the flag decides when the shim sends one, in **both**
    /// directions and against a probe that says the opposite — the production
    /// condition, where the probe is wrong in both directions (`ssh -t` gives a
    /// non-interactive shell a terminal; a comint shell has none).
    #[test]
    fn an_explicit_flag_outranks_the_probe_in_both_directions() {
        assert!(
            preference(true, false).resolve(false),
            "--interactive must decide over a probe answering false"
        );
        assert!(
            !preference(false, true).resolve(true),
            "--no-interactive must decide over a probe answering true"
        );
    }

    /// C-038 rung 5, the other half: with no flag the probe still decides, in
    /// both directions. This is what keeps a not-yet-refreshed shim working —
    /// it is rung 5's existing behaviour, not a compatibility shim.
    #[test]
    fn without_a_flag_the_probe_decides_in_both_directions() {
        assert!(
            Interactive::default().resolve(true),
            "no flag and a terminal must resolve interactive"
        );
        assert!(
            !Interactive::default().resolve(false),
            "no flag and no terminal must resolve non-interactive"
        );
    }

    /// `--no-interactive` wins when both flags are set. `overrides_with` makes
    /// clap last-wins, so this state is unreachable from a command line; the
    /// tie-break is pinned anyway because the struct is constructible.
    #[test]
    fn no_interactive_wins_when_both_flags_are_set() {
        assert!(!preference(true, true).resolve(true));
    }
}
