// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::Parser;
use ocx_config::env;
use ocx_config::env::OcxConfigView;
use ocx_console::{ColorMode, ColorModeConfig, DataInterface, Printer};

use crate::{api, options};

#[derive(Debug, Parser)]
pub struct ContextOptions {
    /// Path to the ocx configuration file.
    ///
    /// Can also be set via the `OCX_CONFIG` environment variable.
    /// To disable config discovery entirely, set `OCX_NO_CONFIG=1`.
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<std::path::PathBuf>,

    /// Project directory, or the project file itself (project-tier toolchain
    /// config).
    ///
    /// A directory resolves to the `ocx.toml` inside it, so `--project .` and
    /// `--project /path/to/repo` both work; a file may have any name (like Cargo
    /// `--manifest-path`). Equivalent env var: `OCX_PROJECT`; `OCX_NO_PROJECT=1`
    /// disables project discovery. Paths given here or via `OCX_PROJECT` are
    /// trusted and followed through symlinks. The CWD walk looks for the literal
    /// name `ocx.toml`, rejects symlinks and continues upward, so a writer with
    /// control over an intermediate directory cannot redirect discovery.
    #[arg(long, value_name = "PATH")]
    pub project: Option<std::path::PathBuf>,

    /// Select the global toolchain (`$OCX_HOME/ocx.toml`) as the project
    /// file in effect, instead of discovering a project by CWD walk.
    ///
    /// Mutually exclusive with `--project` (both pick a project file).
    /// Resolution-affecting: forwarded to child ocx processes via
    /// `OCX_GLOBAL`. Equivalent env var: `OCX_GLOBAL`. The global
    /// toolchain never composes into project resolution; `ocx exec` and
    /// `ocx package exec` stay hermetic and never read it.
    // `-g` is `--group` after a subcommand (`options/group_selection.rs`); position disambiguates, do not unify.
    #[arg(short = 'g', long, conflicts_with = "project", default_value_t = ocx_util::env::flag(env::keys::OCX_GLOBAL, false))]
    pub global: bool,

    /// Route mutable lookups (tag list, catalog, tag->manifest) to the
    /// remote registry instead of the local index.
    ///
    /// Pure queries (`index list`, `index catalog`, `package description pull`) do
    /// **not** persist the result to the local index - use
    /// `ocx index update` to refresh the local index explicitly. Implies
    /// network access. Combined with `--offline` the result is
    /// "pinned-only mode": no source contact, and any tag-addressed
    /// resolution that cannot be satisfied locally errors instead of
    /// silently falling back. Equivalent env var: `OCX_REMOTE`.
    #[arg(short = 'r', long, default_value_t = ocx_util::env::flag(env::keys::OCX_REMOTE, false))]
    pub remote: bool,

    /// Disable all network access.
    ///
    /// Tag->digest resolution must be satisfied by the local index or a
    /// digest-pinned identifier; unpinned tags missing from the local
    /// index will error. Useful for hermetic CI runs and air-gapped
    /// environments. Equivalent env var: `OCX_OFFLINE`.
    #[arg(long, default_value_t = ocx_util::env::flag(env::keys::OCX_OFFLINE, false))]
    pub offline: bool,

    /// Freeze tag resolution to the local index; never fetch an unknown tag.
    ///
    /// A tag in the local index resolves from cache and a digest-pinned
    /// reference (`repo@sha256:...`, or a tag `ocx.lock` pins) still fetches its
    /// content, but an unpinned tag missing from the index errors instead of
    /// being fetched. Populate the index first with `ocx index update`, without
    /// this flag (a frozen index update is refused, exit 81). Scoped to
    /// packages: patch companions and managed configuration still resolve live.
    /// Unlike `--offline` and Cargo's `--frozen`, the network stays reachable
    /// for known content. Conflicts with `--remote`. Env var: `OCX_FROZEN`.
    #[arg(long, conflicts_with = "remote", default_value_t = ocx_util::env::flag(env::keys::OCX_FROZEN, false))]
    pub frozen: bool,

    #[clap(flatten)]
    pub format: options::Format,

    /// Suppress the stdout report and progress bars (errors and warnings remain).
    ///
    /// When set, the CLI's structured report (table or JSON) is not printed
    /// and no transfer progress is rendered on stderr.
    /// Equivalent env var: `OCX_QUIET`.
    #[arg(short = 'q', long, default_value_t = ocx_util::env::flag("OCX_QUIET", false))]
    pub quiet: bool,

    /// Maximum number of root packages to pull concurrently.
    ///
    /// `0` means "use all logical cores" (GNU Parallel convention). Omitting
    /// the flag falls back to the `OCX_JOBS` environment variable; when
    /// neither is set, pulls run unbounded. Negative values
    /// are rejected at parse time.
    ///
    /// Caps only the outer dispatch - transitive dependency and layer
    /// extraction stay unbounded.
    // Capping the inner work too would deadlock it against an ancestor's permit.
    #[arg(long, value_name = "N")]
    pub jobs: Option<usize>,

    /// Overrides the local index home directory.
    ///
    /// The local index is a collection of per-source subtrees, one per
    /// configured registry or index, each holding tag roots plus their
    /// manifest data - so resolution can work fully offline once populated.
    /// This flag redirects the whole collection, replacing the default at
    /// `$OCX_HOME/index`. Digest-addressed lookups still consult the local
    /// index even under `--remote`; only tag listings and tag->manifest
    /// lookups route to the remote registry there. Can also be set via the
    /// `OCX_INDEX` environment variable.
    #[arg(long, value_name = "PATH")]
    pub index: Option<std::path::PathBuf>,

    /// The log level to use
    #[arg(short, long, value_enum)]
    pub log_level: Option<crate::tracing_init::LogLevel>,

    // Read early by `ColorMode::from_args` in `App::run`; declared so clap accepts `--color` and lists it.
    /// When to use ANSI colors in output.
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Default::default())]
    pub color: ColorMode,
}

impl ContextOptions {
    /// Builds the reporting [`Api`](api::Api); the only construction, shared by
    /// [`crate::app::Context::try_init`] and Context-free commands (`ocx version`).
    // A second construction site would drop `--color` or drift the format default.
    pub fn build_api(&self, color_config: ColorModeConfig) -> api::Api {
        let printer = Printer::new(color_config.stdout, color_config.stderr);
        let data = DataInterface::new(printer);
        api::Api::new(self.format.mode(), data, self.quiet)
    }

    /// Builds the resolution-affecting policy snapshot forwarded to child ocx
    /// processes; `self_exe` is the running `ocx`'s absolute path.
    pub fn as_view(&self, self_exe: std::path::PathBuf) -> OcxConfigView {
        // Fields no root flag carries start unset; `Context::try_init` or the spawning command fills
        // the ones it forwards.
        OcxConfigView {
            self_exe,
            offline: self.offline,
            remote: self.remote,
            frozen: self.frozen,
            config: self.config.clone(),
            project: self.project.clone(),
            global: self.global,
            no_consent: false,
            index: self.index.clone(),
            toolchain_dir: None,
            mirrors: Vec::new(),
            patches: None,
            patch_snapshot: None,
            records: ocx_package_manager::record::RecordsOptions::default(),
            managed_config_source: None,
            no_verify: false,
            no_config: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_json(args: &[&str]) -> bool {
        ContextOptions::try_parse_from(args)
            .expect("args parse")
            .build_api(ColorModeConfig {
                stdout: false,
                stderr: false,
                relayed: false,
            })
            .is_json()
    }

    /// The flattened `Format` group reaches the `Api` the context builds —
    /// the wiring `options::format`'s own tests cannot see. Resolution
    /// semantics are covered there.
    #[test]
    fn build_api_honors_the_flattened_format_group() {
        assert!(!is_json(&["ocx"]), "default is plain");
        assert!(is_json(&["ocx", "--json"]));
        assert!(is_json(&["ocx", "--format", "json"]));
    }
}
