// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether to delete the packages a command unlinks.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Purge {
    /// Also delete each package this command unlinks, when nothing else
    /// references it.
    #[clap(long = "purge", overrides_with = "no_purge")]
    purge: bool,

    /// Leave unlinked packages on disk for `ocx clean`. The default.
    #[clap(long = "no-purge", overrides_with = "purge")]
    no_purge: bool,
}

impl Purge {
    /// Whether purging is enabled; off unless `--purge` is the last of the pair.
    pub fn enabled(&self) -> bool {
        self.purge && !self.no_purge
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Harness {
        #[clap(flatten)]
        purge: Purge,
    }

    fn enabled(args: &[&str]) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").purge.enabled()
    }

    #[test]
    fn no_flags_keep_packages() {
        assert!(!enabled(&[]));
    }

    #[test]
    fn explicit_flags() {
        assert!(enabled(&["--purge"]));
        assert!(!enabled(&["--no-purge"]));
    }

    /// POSIX last-wins with both flags.
    #[test]
    fn last_wins() {
        assert!(!enabled(&["--purge", "--no-purge"]));
        assert!(enabled(&["--no-purge", "--purge"]));
    }
}
