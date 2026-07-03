// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether to verify an operation against the registry before committing it.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--verify` / `--no-verify` flags. Verification is the default: `--verify`
/// is the affirmative form, `--no-verify` opts out. The two use POSIX
/// last-wins semantics (`overrides_with`), matching the `--pull` / `--no-pull`
/// convention. Resolve with [`Verify::enabled`].
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Verify {
    /// Verify the operation against the registry before committing it (default).
    #[clap(long = "verify", overrides_with = "no_verify")]
    verify: bool,

    /// Skip verification.
    #[clap(long = "no-verify", overrides_with = "verify")]
    no_verify: bool,
}

impl Verify {
    /// Resolve whether verification is enabled. Default is on; only an explicit
    /// (last-wins) `--no-verify` turns it off.
    pub fn enabled(&self) -> bool {
        !self.no_verify
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        verify: Verify,
    }

    fn enabled(args: &[&str]) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").verify.enabled()
    }

    #[test]
    fn default_is_enabled() {
        assert!(enabled(&[]), "verification must default on");
    }

    #[test]
    fn no_verify_disables() {
        assert!(!enabled(&["--no-verify"]));
    }

    #[test]
    fn explicit_verify_enables() {
        assert!(enabled(&["--verify"]));
    }

    #[test]
    fn last_wins() {
        assert!(!enabled(&["--verify", "--no-verify"]), "--no-verify wins when last");
        assert!(enabled(&["--no-verify", "--verify"]), "--verify wins when last");
    }
}
