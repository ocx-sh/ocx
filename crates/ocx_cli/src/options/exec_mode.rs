// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_package_manager::exec_env::ExecMode;

/// How a spawned child treats the calling environment.
#[derive(clap::Args, Clone, Copy, Debug, Default)]
pub struct ExecModeOptions {
    /// Remove every value ocx applied to the calling environment before composing
    ///
    /// Values the user set stay. Without `--isolate` or `--clean`, the composition layers
    /// onto the calling environment. Conflicts with `--clean`.
    #[clap(long = "isolate", conflicts_with = "clean")]
    isolate: bool,

    /// Start the child from an empty environment holding only the composed variables
    ///
    /// Conflicts with `--isolate`.
    #[clap(long = "clean")]
    clean: bool,
}

impl ExecModeOptions {
    /// The spawn mode the flags select; [`ExecMode::Layer`] when neither is given.
    pub fn mode(self) -> ExecMode {
        match (self.isolate, self.clean) {
            (_, true) => ExecMode::Clean,
            (true, false) => ExecMode::Isolate,
            (false, false) => ExecMode::Layer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[derive(clap::Parser)]
    struct Probe {
        #[clap(flatten)]
        mode: ExecModeOptions,
    }

    #[test]
    fn neither_flag_layers() {
        assert_eq!(Probe::parse_from(["probe"]).mode.mode(), ExecMode::Layer);
    }

    #[test]
    fn each_flag_selects_its_mode() {
        assert_eq!(Probe::parse_from(["probe", "--isolate"]).mode.mode(), ExecMode::Isolate);
        assert_eq!(Probe::parse_from(["probe", "--clean"]).mode.mode(), ExecMode::Clean);
    }

    #[test]
    fn both_flags_are_a_usage_error() {
        for args in [["probe", "--isolate", "--clean"], ["probe", "--clean", "--isolate"]] {
            let error = Probe::try_parse_from(args).err().expect("the pair conflicts");
            assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        }
    }
}
