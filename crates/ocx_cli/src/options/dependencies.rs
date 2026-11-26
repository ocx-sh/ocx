// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Whether a command also covers every package in the dependency closure of the one named.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct Dependencies {
    /// Cover the named package and every package in its dependency closure.
    #[clap(long = "dependencies", overrides_with = "no_dependencies")]
    dependencies: bool,

    /// Cover the named package alone.
    #[clap(long = "no-dependencies", overrides_with = "dependencies")]
    no_dependencies: bool,
}

impl Dependencies {
    /// Whether the closure is covered; `default` is the command's behavior when neither flag is given.
    pub fn enabled(&self, default: bool) -> bool {
        if self.dependencies {
            true
        } else if self.no_dependencies {
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
        dependencies: Dependencies,
    }

    fn enabled(args: &[&str], default: bool) -> bool {
        let mut argv = vec!["harness"];
        argv.extend_from_slice(args);
        Harness::try_parse_from(argv)
            .expect("parse")
            .dependencies
            .enabled(default)
    }

    #[test]
    fn no_flags_yield_default() {
        assert!(enabled(&[], true));
        assert!(!enabled(&[], false));
    }

    #[test]
    fn last_flag_wins() {
        assert!(enabled(&["--dependencies"], false));
        assert!(!enabled(&["--no-dependencies"], true));
        assert!(enabled(&["--no-dependencies", "--dependencies"], false));
        assert!(!enabled(&["--dependencies", "--no-dependencies"], true));
    }
}
