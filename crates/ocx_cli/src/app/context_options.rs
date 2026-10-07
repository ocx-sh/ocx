// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::Parser;
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
    #[arg(short = 'g', long, conflicts_with = "project")]
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
    #[arg(short = 'r', long)]
    pub remote: bool,

    /// Disable all network access.
    ///
    /// Tag->digest resolution must be satisfied by the local index or a
    /// digest-pinned identifier; unpinned tags missing from the local
    /// index will error. Useful for hermetic CI runs and air-gapped
    /// environments. Equivalent env var: `OCX_OFFLINE`.
    #[arg(long)]
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
    #[arg(long, conflicts_with = "remote")]
    pub frozen: bool,

    #[clap(flatten)]
    pub format: options::Format,

    /// Suppress the stdout report and progress bars (errors and warnings remain).
    ///
    /// When set, the CLI's structured report (table or JSON) is not printed
    /// and no transfer progress is rendered on stderr.
    /// Equivalent env var: `OCX_QUIET`.
    #[arg(short = 'q', long)]
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
    /// Turns on each root switch whose environment variable is truthy; a flag on the command line
    /// already won. Kept out of the clap build so the grammar's defaults stay literal.
    ///
    /// # Errors
    ///
    /// [`ocx_env::InvalidEnv`] when a variable declared to refuse an unparsable value holds one.
    pub fn apply_env(&mut self) -> Result<(), ocx_env::InvalidEnv> {
        for (flag, var) in [
            (&mut self.global, &ocx_env::OCX_GLOBAL),
            (&mut self.remote, &ocx_env::OCX_REMOTE),
            (&mut self.offline, &ocx_env::OCX_OFFLINE),
            (&mut self.frozen, &ocx_env::OCX_FROZEN),
            (&mut self.quiet, &ocx_env::OCX_QUIET),
        ] {
            *flag |= var.bool_or(false)?;
        }
        Ok(())
    }

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
    /// The root switches read their variables after parsing, and a flag on the command line
    /// still turns one on when the variable is off.
    #[test]
    fn apply_env_turns_on_the_switches_whose_variables_are_set() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        let parsed = |args: &[&str]| {
            let mut options = ContextOptions::try_parse_from(args).expect("args parse");
            options.apply_env().expect("every variable holds a boolean");
            options
        };

        let plain = parsed(&["ocx", "--frozen"]);
        assert!(plain.frozen && !plain.offline && !plain.remote && !plain.global && !plain.quiet);

        for (key, value) in [
            (&ocx_env::OCX_OFFLINE, "1"),
            (&ocx_env::OCX_REMOTE, "yes"),
            (&ocx_env::OCX_GLOBAL, "true"),
            (&ocx_env::OCX_QUIET, "on"),
        ] {
            env.set(key, value);
        }
        let from_env = parsed(&["ocx"]);
        assert!(from_env.offline && from_env.remote && from_env.global && from_env.quiet);
        assert!(!from_env.frozen);
    }

    /// A refusing variable's unparsable value is an error naming the key, not a silent off.
    #[test]
    fn apply_env_refuses_an_unparsable_refusing_variable() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        env.set(&ocx_env::OCX_FROZEN, "not-a-bool");
        let mut options = ContextOptions::try_parse_from(["ocx"]).expect("args parse");
        let error = options.apply_env().expect_err("OCX_FROZEN refuses an invalid value");
        assert_eq!(error.key, "OCX_FROZEN");

        // A lenient variable still falls back to off.
        env.remove(&ocx_env::OCX_FROZEN);
        env.set(&ocx_env::OCX_REMOTE, "maybe");
        let mut options = ContextOptions::try_parse_from(["ocx"]).expect("args parse");
        options.apply_env().expect("OCX_REMOTE falls back");
        assert!(!options.remote);
    }

    #[test]
    fn build_api_honors_the_flattened_format_group() {
        assert!(!is_json(&["ocx"]), "default is plain");
        assert!(is_json(&["ocx", "--json"]));
        assert!(is_json(&["ocx", "--format", "json"]));
    }
}
