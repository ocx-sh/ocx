// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::Parser;
use ocx_lib::{cli, env};

use crate::options;

#[derive(Debug, Parser)]
pub struct ContextOptions {
    /// Path to the ocx configuration file.
    ///
    /// Can also be set via the `OCX_CONFIG_FILE` environment variable.
    /// To disable config discovery entirely, set `OCX_NO_CONFIG=1`.
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<std::path::PathBuf>,

    /// Path to the project-level `ocx.toml` (project-tier toolchain config).
    ///
    /// Can also be set via the `OCX_PROJECT_FILE` environment variable.
    /// To disable project discovery entirely, set `OCX_NO_PROJECT=1`.
    /// Any filename is accepted via this flag (matches Cargo `--manifest-path`);
    /// the CWD walk still looks for the literal name `ocx.toml`.
    ///
    /// Symlink policy: paths given via this flag (and `OCX_PROJECT_FILE`) are
    /// trusted and followed through symlinks. Paths discovered by the CWD walk
    /// reject symlinks and continue upward to avoid a writer with control
    /// over an intermediate directory redirecting discovery to arbitrary
    /// files.
    #[arg(long, value_name = "FILE")]
    pub project: Option<std::path::PathBuf>,

    /// Route mutable lookups (tag list, catalog, tag→manifest) to the
    /// remote registry instead of the local index.
    ///
    /// Pure queries (`index list`, `index catalog`, `package info`) do
    /// **not** persist the result to the local index — use
    /// `ocx index update` to refresh the local index explicitly. Implies
    /// network access. Combined with `--offline` the result is
    /// "pinned-only mode": no source contact, and any tag-addressed
    /// resolution that cannot be satisfied locally errors instead of
    /// silently falling back. Equivalent env var: `OCX_REMOTE`.
    #[arg(long, default_value_t = env::flag("OCX_REMOTE", false))]
    pub remote: bool,

    /// Disable all network access.
    ///
    /// Tag→digest resolution must be satisfied by the local index or a
    /// digest-pinned identifier; unpinned tags missing from the local
    /// index will error. Useful for hermetic CI runs and air-gapped
    /// environments. Equivalent env var: `OCX_OFFLINE`.
    #[arg(long, default_value_t = env::flag("OCX_OFFLINE", false))]
    pub offline: bool,

    /// The format to use when outputting information.
    #[arg(long, value_enum, value_name = "FORMAT", default_value_t = Default::default())]
    pub format: options::Format,

    /// Suppress stdout report output (errors and progress on stderr remain).
    ///
    /// When set, the CLI's structured report (table or JSON) is not printed.
    /// Equivalent env var: `OCX_QUIET`.
    #[arg(short = 'q', long, default_value_t = env::flag("OCX_QUIET", false))]
    pub quiet: bool,

    /// Maximum number of root packages to pull concurrently.
    ///
    /// `0` means "use all logical cores" (GNU Parallel convention). Omitting
    /// the flag falls back to the `OCX_JOBS` environment variable; when
    /// neither is set, pulls run unbounded (legacy behavior). Negative values
    /// are rejected at parse time.
    ///
    /// Caps only the outer dispatch — transitive dependency and layer
    /// extraction stay unbounded so they cannot deadlock against an
    /// ancestor's permit.
    #[arg(long, value_name = "N")]
    pub jobs: Option<usize>,

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
