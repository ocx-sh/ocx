// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::Parser;
use ocx_lib::env::OcxConfigView;
use ocx_lib::{cli, env};

use crate::options;

#[derive(Debug, Parser)]
pub struct ContextOptions {
    /// Path to the ocx configuration file.
    ///
    /// Can also be set via the `OCX_CONFIG` environment variable.
    /// To disable config discovery entirely, set `OCX_NO_CONFIG=1`.
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<std::path::PathBuf>,

    /// Use the remote index by default instead of the local index.
    #[arg(long, default_value_t = env::flag(env::keys::OCX_REMOTE, false))]
    pub remote: bool,

    /// Run in offline mode, which will not attempt to fetch any remote information.
    ///
    /// If a required package is not already installed, the command will fail.
    #[arg(long, default_value_t = env::flag(env::keys::OCX_OFFLINE, false))]
    pub offline: bool,

    /// The format to use when outputting information.
    #[arg(long, value_enum, value_name = "FORMAT", default_value_t = Default::default())]
    pub format: options::Format,

    /// Alternative path to the local index directory (ignored if --remote is set).
    ///
    /// Can also be set via the OCX_INDEX environment variable.
    #[arg(long, value_name = "PATH")]
    pub index: Option<std::path::PathBuf>,

    /// The log level to use
    #[arg(short, long, value_enum)]
    pub log_level: Option<cli::LogLevel>,

    // Parsed early in App::run() via ColorMode::from_args(); this field exists
    // so clap recognizes --color and shows it in --help.
    /// When to use ANSI colors in output.
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Default::default())]
    pub color: cli::ColorMode,
}

impl ContextOptions {
    /// Builds the resolution-affecting policy snapshot forwarded to child ocx
    /// processes. `self_exe` is the absolute path of the running `ocx`
    /// executable (captured by `Context::try_init` from `current_exe()`).
    pub fn as_view(&self, self_exe: std::path::PathBuf) -> OcxConfigView {
        OcxConfigView {
            self_exe,
            offline: self.offline,
            remote: self.remote,
            config: self.config.clone(),
            index: self.index.clone(),
        }
    }
}
