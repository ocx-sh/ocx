// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Layer the project onto the environment already active in the shell.
#[derive(clap::Args, Clone, Copy, Debug, Default)]
pub struct Stack {
    /// Layer this project onto the active environment instead of replacing it
    ///
    /// By default the toolchain an activated parent project put on the
    /// environment is dropped before this project is composed. With `--stack`
    /// it stays, and this project's tools come first on `PATH`.
    ///
    /// <https://ocx.sh/docs/explanation/shell-integration/#activation-nested-projects>
    #[clap(long = "stack")]
    stack: bool,
}

impl Stack {
    /// Whether the parent's applied entries are kept.
    pub fn enabled(self) -> bool {
        self.stack
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Probe {
        #[clap(flatten)]
        stack: Stack,
    }

    #[test]
    fn off_by_default_and_on_with_the_flag() {
        assert!(!Probe::parse_from(["probe"]).stack.enabled());
        assert!(Probe::parse_from(["probe", "--stack"]).stack.enabled());
    }
}
