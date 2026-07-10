// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether to materialize resolved packages into the object store.
///
/// Flatten into a command with `#[clap(flatten)]` to add the paired
/// `--pull` / `--no-pull` flags. The two use POSIX last-wins semantics
/// (`overrides_with`) — combining the flags is not an error (git
/// `--[no-]verify` idiom). The default differs per command (eager for
/// `add`/`lock`/`update`, lazy for `env`), so resolve with
/// [`Pull::enabled`], passing the command's default.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Pull {
    /// Materialize resolved packages into the object store, installing
    /// on a local miss.
    #[clap(long = "pull", overrides_with = "no_pull")]
    pull: bool,

    /// Skip materialization; resolve against local state only.
    /// Materialization is deferred to `ocx pull` or the first
    /// `ocx run` / direnv hit.
    #[clap(long = "no-pull", overrides_with = "pull")]
    no_pull: bool,
}

impl Pull {
    /// Resolve whether materialization is enabled. `default` is the
    /// command's behavior when neither flag is given; an explicit
    /// (last-wins) flag overrides it.
    pub fn enabled(&self, default: bool) -> bool {
        if self.pull {
            true
        } else if self.no_pull {
            false
        } else {
            default
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
        pull: Pull,
    }

    fn enabled(args: &[&str], default: bool) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv).expect("parse").pull.enabled(default)
    }

    /// Neither flag → the command's default decides, both ways.
    #[test]
    fn no_flags_yield_default() {
        assert!(enabled(&[], true), "eager default must hold without flags");
        assert!(!enabled(&[], false), "lazy default must hold without flags");
    }

    /// `--pull` alone → enabled regardless of default.
    #[test]
    fn explicit_pull_enables() {
        assert!(enabled(&["--pull"], true));
        assert!(enabled(&["--pull"], false));
    }

    /// `--no-pull` alone → disabled regardless of default.
    #[test]
    fn explicit_no_pull_disables() {
        assert!(!enabled(&["--no-pull"], true));
        assert!(!enabled(&["--no-pull"], false));
    }

    /// POSIX last-wins with both flags, under both defaults.
    #[test]
    fn last_wins() {
        for default in [true, false] {
            assert!(
                !enabled(&["--pull", "--no-pull"], default),
                "--no-pull wins when last (default {default})"
            );
            assert!(
                enabled(&["--no-pull", "--pull"], default),
                "--pull wins when last (default {default})"
            );
        }
    }
}
