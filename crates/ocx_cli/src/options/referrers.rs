// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether a command also carries the artifacts anchored to each manifest —
/// signatures, SBOMs, attestations.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--referrers` / `--no-referrers` flags. Carrying them is the default:
/// `--referrers` is the affirmative form of the default, `--no-referrers` opts
/// out. The two use POSIX last-wins semantics (`overrides_with`), matching
/// [`KeepTag`](super::KeepTag). Resolve with [`Referrers::enabled`] —
/// never by reading the two raw booleans at the call site.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Referrers {
    /// Carry the signatures, SBOMs and attestations anchored to each manifest
    /// (default).
    ///
    /// Requires the OCI 1.1 Referrers API at both ends; a registry without it
    /// fails rather than silently moving an artifact whose signature stayed
    /// behind.
    #[clap(long = "referrers", overrides_with = "no_referrers")]
    referrers: bool,

    /// Leave the signatures, SBOMs and attestations behind.
    #[clap(long = "no-referrers", overrides_with = "referrers")]
    no_referrers: bool,
}

impl Referrers {
    /// Resolve whether referrers travel. Default is on; only an explicit
    /// (last-wins) `--no-referrers` turns it off.
    pub fn enabled(&self) -> bool {
        !self.no_referrers
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        referrers: Referrers,
    }

    fn enabled(args: &[&str]) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").referrers.enabled()
    }

    #[test]
    fn default_is_enabled() {
        assert!(enabled(&[]), "referrers must travel by default");
    }

    #[test]
    fn no_referrers_disables() {
        assert!(!enabled(&["--no-referrers"]));
    }

    #[test]
    fn explicit_referrers_enables() {
        assert!(enabled(&["--referrers"]));
    }

    #[test]
    fn last_wins() {
        assert!(
            !enabled(&["--referrers", "--no-referrers"]),
            "--no-referrers wins when last"
        );
        assert!(
            enabled(&["--no-referrers", "--referrers"]),
            "--referrers wins when last"
        );
    }
}
