// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether the shell that invoked `ocx self activate` is an interactive one.
// Hidden: emitted by the `$OCX_HOME/env.*` shims from their shell's own interactivity test.
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
    /// Whether this session is interactive: the flags, else `probed`.
    ///
    /// Feeds only the `auto` rung of [`super::hook::resolve_ladder`]; a shim must send this pair, never
    /// `--hook`, which outranks `OCX_NO_HOOK` and `[shell] hook` and so revokes both opt-outs.
    pub fn resolve(&self, probed: bool) -> bool {
        if self.no_interactive {
            false
        } else if self.interactive {
            true
        } else {
            probed
        }
    }

    /// [`Self::resolve`] with this process's own terminal probe as the fallback.
    // ORs stdin and stderr: every shim redirects stderr, so a stderr-only probe answers `false` for every shell.
    pub fn resolve_probed(&self) -> bool {
        use std::io::IsTerminal as _;

        self.resolve(std::io::stdin().is_terminal() || std::io::stderr().is_terminal())
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
