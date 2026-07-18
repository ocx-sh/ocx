// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::Parser;
use ocx_lib::cli::{ColorModeConfig, DataInterface, Printer};
use ocx_lib::env::OcxConfigView;
use ocx_lib::{cli, env};

use crate::{api, options};

#[derive(Debug, Parser)]
pub struct ContextOptions {
    /// Path to the ocx configuration file.
    ///
    /// Can also be set via the `OCX_CONFIG` environment variable.
    /// To disable config discovery entirely, set `OCX_NO_CONFIG=1`.
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<std::path::PathBuf>,

    /// Path to the project-level `ocx.toml` (project-tier toolchain config).
    ///
    /// Can also be set via the `OCX_PROJECT` environment variable.
    /// To disable project discovery entirely, set `OCX_NO_PROJECT=1`.
    /// Any filename is accepted via this flag (matches Cargo `--manifest-path`);
    /// the CWD walk still looks for the literal name `ocx.toml`.
    ///
    /// Symlink policy: paths given via this flag (and `OCX_PROJECT`) are
    /// trusted and followed through symlinks. Paths discovered by the CWD walk
    /// reject symlinks and continue upward to avoid a writer with control
    /// over an intermediate directory redirecting discovery to arbitrary
    /// files.
    #[arg(long, value_name = "FILE")]
    pub project: Option<std::path::PathBuf>,

    /// Select the global toolchain (`$OCX_HOME/ocx.toml`) as the project
    /// file in effect, instead of discovering a project by CWD walk.
    ///
    /// Mutually exclusive with `--project` (both pick a project file).
    /// Resolution-affecting: forwarded to child ocx processes via
    /// `OCX_GLOBAL`. Equivalent env var: `OCX_GLOBAL`. The global
    /// toolchain never composes into project resolution; `ocx run` and
    /// `ocx exec` stay hermetic and never read it.
    #[arg(long, conflicts_with = "project", default_value_t = env::flag(env::keys::OCX_GLOBAL, false))]
    pub global: bool,

    /// Route mutable lookups (tag list, catalog, tag->manifest) to the
    /// remote registry instead of the local index.
    ///
    /// Pure queries (`index list`, `index catalog`, `package info`) do
    /// **not** persist the result to the local index - use
    /// `ocx index update` to refresh the local index explicitly. Implies
    /// network access. Combined with `--offline` the result is
    /// "pinned-only mode": no source contact, and any tag-addressed
    /// resolution that cannot be satisfied locally errors instead of
    /// silently falling back. Equivalent env var: `OCX_REMOTE`.
    #[arg(long, default_value_t = env::flag(env::keys::OCX_REMOTE, false))]
    pub remote: bool,

    /// Disable all network access.
    ///
    /// Tag->digest resolution must be satisfied by the local index or a
    /// digest-pinned identifier; unpinned tags missing from the local
    /// index will error. Useful for hermetic CI runs and air-gapped
    /// environments. Equivalent env var: `OCX_OFFLINE`.
    #[arg(long, default_value_t = env::flag(env::keys::OCX_OFFLINE, false))]
    pub offline: bool,

    /// Freeze tag resolution to the local index; never fetch an unknown tag.
    ///
    /// A tag already in the local index resolves from cache; a digest-pinned
    /// reference (`repo@sha256:...`, or a tag pinned by `ocx.lock`) still
    /// fetches its content. But an unpinned tag missing from the local index
    /// errors instead of being fetched and recorded. Run
    /// `ocx index update` to populate the index first, then resolve under
    /// `--frozen`. Unlike `--offline`, frozen still reaches the network for
    /// known/pinned content. Unlike Cargo's `--frozen`, the network is not
    /// disabled; `--offline` alone (which also refuses unpinned tags) matches
    /// Cargo's behavior. Conflicts with `--remote`. Equivalent env var:
    /// `OCX_FROZEN`.
    #[arg(long, conflicts_with = "remote", default_value_t = env::flag(env::keys::OCX_FROZEN, false))]
    pub frozen: bool,

    /// Output format for stdout reports: `plain` (default) or `json`.
    ///
    /// Applies to every command; there is no per-command `--format`. The
    /// `--shell[=NAME]` output of `env` / `package env` is unaffected.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub format: Option<options::Format>,

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
    /// Caps only the outer dispatch - transitive dependency and layer
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

impl ContextOptions {
    /// Build the reporting [`Api`](api::Api) from parsed flags + resolved
    /// color config.  Single source of truth for the printer +
    /// format-default + quiet wiring, shared by
    /// [`crate::app::Context::try_init`] and the Context-free
    /// static-command bypass paths (`ocx version`).
    ///
    /// Layering: `ContextOptions` is the CLI-surface parser tier and may
    /// reach down into `api`; the reverse is forbidden (api has no
    /// knowledge of the parser).  Centralising this constructor avoids two
    /// historical failure modes: the static-command path hardcoding
    /// `Printer::new(false, false)` (lost `--color` honouring) and the
    /// `format.unwrap_or(Plain)` default drifting between init sites.
    pub fn build_api(&self, color_config: ColorModeConfig) -> api::Api {
        let printer = Printer::new(color_config.stdout, color_config.stderr);
        let data = DataInterface::new(printer);
        api::Api::new(self.format.unwrap_or(options::Format::Plain), data, self.quiet)
    }

    /// Builds the resolution-affecting policy snapshot forwarded to child ocx
    /// processes. `self_exe` is the absolute path of the running `ocx`
    /// executable (captured by `Context::try_init` from `current_exe()`).
    pub fn as_view(&self, self_exe: std::path::PathBuf) -> OcxConfigView {
        OcxConfigView {
            self_exe,
            offline: self.offline,
            remote: self.remote,
            frozen: self.frozen,
            config: self.config.clone(),
            project: self.project.clone(),
            global: self.global,
            index: self.index.clone(),
            // The resolved mirror map is not derivable from `ContextOptions`
            // alone — it merges `[mirrors]` config with the inherited
            // `OCX_MIRRORS` env. `Context::try_init` builds it and populates
            // this field on the returned view; the parser tier starts empty.
            mirrors: Vec::new(),
            // The resolved patch config is not derivable from `ContextOptions`
            // alone — it merges the `[patches]` config tier with the inherited
            // `OCX_PATCHES` env. `Context::try_init` builds it via
            // `resolve_patch_config` and populates this field on the returned
            // view; the parser tier starts empty.
            patches: None,
            // The active patch snapshot path is resolved by `Context::try_init`
            // from `OCX_PATCH_SNAPSHOT` (or a future `--patch-snapshot` flag)
            // and populated on the returned view; the parser tier starts empty.
            patch_snapshot: None,
            // The effective managed-config source (flag > env > seed) is not
            // derivable from `ContextOptions` alone; `Context::try_init` will
            // populate this field once the managed-config tier is wired in
            // (managed-config phase 4). The parser tier starts empty.
            managed_config_source: None,
            // The auto-verify opt-out is a per-command flag (`--no-verify`), not
            // a `ContextOptions` field. `Context::try_init` reads `OCX_NO_VERIFY`
            // and populates this field on the returned view; the parser tier
            // starts as the env-unset default.
            no_verify: false,
        }
    }
}
