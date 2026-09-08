// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether to record a shell-activation consent stamp for the project a
/// command targets.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--consent` / `--no-consent` flags. The two use POSIX last-wins semantics
/// (`overrides_with`) — combining them is not an error (git `--[no-]verify`
/// idiom). Carried by `ocx init`, `ocx pull` and `ocx exec`.
///
/// Resolve with [`Consent::explicit`] and hand the `Option<bool>` to the write
/// seam, which fills a `None` in from
/// [`OCX_NO_CONSENT`](ocx_lib::env::keys::OCX_NO_CONSENT) and otherwise stamps.
/// The ladder is flag, then env, then stamp.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Consent {
    /// Record a consent stamp (the default unless OCX_NO_CONSENT is set)
    ///
    /// The stamp is what lets a shell prompt in this directory apply the
    /// project's tools and environment; without one the project is inert.
    /// It is the same stamp `ocx add`, `ocx lock`, `ocx pull` and `ocx exec`
    /// write as a side effect, and `ocx shell revoke` takes back. Pass this
    /// to stamp anyway where OCX_NO_CONSENT is set.
    ///
    /// https://ocx.sh/docs/in-depth/shell-integration
    #[clap(long = "consent", overrides_with = "no_consent")]
    consent: bool,

    /// Run without consenting to this project's shell activation
    ///
    /// Consent later with `ocx shell allow`, or by running any mutating
    /// command in the directory. OCX_NO_CONSENT=1 is the same choice for
    /// every command at once.
    #[clap(long = "no-consent", overrides_with = "consent")]
    no_consent: bool,
}

impl Consent {
    /// What the user typed, or `None` when they typed neither flag.
    ///
    /// The tri-state is the whole point: "neither flag" and "`--consent`" are
    /// different answers, and collapsing them to a `bool` at the command would
    /// let [`OCX_NO_CONSENT`](ocx_lib::env::keys::OCX_NO_CONSENT) — read further
    /// down, at the write seam — outrank a flag the user typed. `None` travels
    /// to that seam so the env speaks only where nothing else did.
    pub fn explicit(&self) -> Option<bool> {
        if self.consent {
            Some(true)
        } else if self.no_consent {
            Some(false)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        consent: Consent,
    }

    fn explicit(args: &[&str]) -> Option<bool> {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").consent.explicit()
    }

    /// ocx-sh/ocx#400 — "neither flag" must stay distinguishable from
    /// `--consent`, because that is what lets `OCX_NO_CONSENT` speak at the
    /// write seam without ever outranking a flag the user typed.
    ///
    /// Red state: return `Some(true)` from [`Consent::explicit`]'s `else` arm.
    #[test]
    fn explicit_is_none_only_when_neither_flag_was_typed() {
        assert_eq!(explicit(&[]), None, "no flag must not answer for the user");
        assert_eq!(explicit(&["--consent"]), Some(true));
        assert_eq!(explicit(&["--no-consent"]), Some(false));
    }

    /// The tri-state carries POSIX last-wins too, or `--no-consent --consent`
    /// would reach the seam as `None` and let the env decide a question the
    /// user answered.
    #[test]
    fn explicit_is_last_wins() {
        assert_eq!(explicit(&["--consent", "--no-consent"]), Some(false));
        assert_eq!(explicit(&["--no-consent", "--consent"]), Some(true));
    }
}
