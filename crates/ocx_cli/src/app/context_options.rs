// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use clap::Parser;
use ocx_config::exec_state::{Inherited, RootFlags};
use ocx_console::{ColorMode, ColorModeConfig, DataInterface, Printer};

use crate::{api, options};

#[derive(Debug, Parser)]
pub struct ContextOptions {
    /// Path to the ocx configuration file.
    ///
    /// Can also be set via the `OCX_CONFIG` environment variable. The file must exist, or the command exits 79.
    /// To disable config discovery entirely, set `OCX_NO_CONFIG=1`.
    ///
    /// The file layers on top of the discovered system, user and `$OCX_HOME` files and of the
    /// managed tier; it does not replace them.
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<std::path::PathBuf>,

    /// Project directory, or the project file itself (project-tier toolchain
    /// config).
    ///
    /// A directory resolves to the `ocx.toml` inside it, so `--project .` and `--project
    /// /path/to/repo` both work; a file may have any name (like Cargo `--manifest-path`).
    /// Equivalent env var: `OCX_PROJECT`; `OCX_NO_PROJECT=1` disables project discovery.
    ///
    /// A path that does not exist exits 79, a directory with no `ocx.toml` exits 64, and an
    /// unreadable path exits 74.
    #[arg(long, value_name = "PATH")]
    pub project: Option<std::path::PathBuf>,

    /// Select the global toolchain (`$OCX_HOME/ocx.toml`) as the project
    /// file in effect, instead of discovering a project by CWD walk.
    ///
    /// Mutually exclusive with `--project`. Applies to this invocation only: a child ocx process selects its own tier.
    ///
    /// The global toolchain never composes into project resolution; `ocx exec` and `ocx package exec` stay hermetic and never read it.
    ///
    /// Before the subcommand `-g` means `--global`; after it, `-g` means `--group`. Nothing
    /// selects the global toolchain implicitly.
    // `-g` is `--group` after a subcommand (`options/group_selection.rs`); position disambiguates, do not unify.
    #[arg(short = 'g', long, conflicts_with = "project")]
    pub global: bool,

    /// Route mutable lookups (tag list, catalog, tag->manifest) to the
    /// remote registry instead of the local index.
    ///
    /// Pure queries (`index list`, `index catalog`, `package description pull`) do **not** persist
    /// the result to the local index - use `ocx index update` to refresh the local index
    /// explicitly. Implies network access.
    ///
    /// Combined with `--offline` the result is "pinned-only mode": no source contact. Any
    /// tag-addressed resolution that cannot be satisfied locally errors instead of silently falling
    /// back. Equivalent env var: `OCX_REMOTE`.
    #[arg(short = 'r', long)]
    pub remote: bool,

    /// Disable all network access.
    ///
    /// Tag->digest resolution must be satisfied by the local index or a digest-pinned identifier;
    /// unpinned tags missing from the local index exit 81. Run `ocx index update` online first,
    /// or switch to a digest-pinned identifier. Useful for hermetic CI runs and air-gapped
    /// environments.
    ///
    /// Equivalent env var: `OCX_OFFLINE`.
    #[arg(long)]
    pub offline: bool,

    /// Freeze tag resolution to the local index; never fetch an unknown tag.
    ///
    /// A tag in the local index resolves from cache. A digest-pinned reference (or a tag
    /// `ocx.lock` pins) still fetches its content. An unpinned tag missing from the index
    /// errors. Populate the index first with `ocx index update` (refused under this flag, exit 81).
    ///
    /// Scoped to packages: patch companions and managed configuration still resolve live. Unlike
    /// `--offline` and Cargo's `--frozen`, the network stays reachable for known content.
    ///
    /// Conflicts with `--remote`. Env var: `OCX_FROZEN`.
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
    /// `0` means "use all logical cores" (GNU Parallel convention). Omitting the flag falls back to
    /// the `OCX_JOBS` environment variable; when neither is set, pulls run unbounded.
    ///
    /// Negative values are rejected at parse time.
    ///
    /// Caps only the outer dispatch - transitive dependency and layer
    /// extraction stay unbounded.
    // Capping the inner work too would deadlock it against an ancestor's permit.
    #[arg(long, value_name = "N")]
    pub jobs: Option<usize>,

    /// Overrides the local index home directory.
    ///
    /// The local index is a collection of per-source subtrees, one per configured registry or
    /// index, each holding tag roots plus their manifest data. Resolution can work fully
    /// offline once it is populated. This flag redirects the whole collection, replacing the default at
    /// `$OCX_HOME/index`.
    ///
    /// Digest-addressed lookups still consult the local index even under `--remote`; only tag
    /// listings and tag->manifest lookups route to the remote registry there. Can also be set via
    /// the `OCX_INDEX` environment variable.
    #[arg(long, value_name = "PATH")]
    pub index: Option<std::path::PathBuf>,

    /// The log level to use.
    ///
    /// Default `info`. Logs go to stderr, never stdout. At `info` and below a line reads `error: message` or `warning: message`;
    /// `debug` and `trace` add a timestamp and level. Can also be set via the `OCX_LOG_LEVEL` environment variable.
    #[arg(short, long, value_enum)]
    pub log_level: Option<crate::tracing_init::LogLevel>,

    // Read early by `ColorMode::from_args` in `App::run`; declared so clap accepts `--color` and lists it.
    /// When to use ANSI colors in output.
    ///
    /// Default `auto`: color only on a terminal. An explicit `--color` outranks `NO_COLOR`,
    /// `CLICOLOR` and `CLICOLOR_FORCE`.
    #[arg(long, value_enum, value_name = "WHEN", default_value_t = Default::default())]
    pub color: ColorMode,

    /// The three switches as typed, captured by [`Self::apply_env`] before it folds the environment in.
    #[arg(skip)]
    typed: Option<TypedSwitches>,
}

/// `--remote`, `--offline` and `--frozen` exactly as the command line gave them.
#[derive(Debug, Clone, Copy)]
struct TypedSwitches {
    remote: bool,
    offline: bool,
    frozen: bool,
}

impl ContextOptions {
    /// Turns on each root switch whose environment variable is truthy; a flag on the command line
    /// already won. Kept out of the clap build so the grammar's defaults stay literal.
    ///
    /// # Errors
    ///
    /// [`ocx_env::InvalidEnv`] when a variable declared to refuse an unparsable value holds one.
    pub fn apply_env(&mut self) -> Result<(), ocx_env::InvalidEnv> {
        // An ambient `OCX_*` folded in below must never read back as a flag the user typed, so the
        // first capture stands: a second call would see the first call's fold.
        let typed = *self.typed.get_or_insert(TypedSwitches {
            remote: self.remote,
            offline: self.offline,
            frozen: self.frozen,
        });
        for (flag, var) in [
            (&mut self.remote, &ocx_env::OCX_REMOTE),
            (&mut self.offline, &ocx_env::OCX_OFFLINE),
            (&mut self.frozen, &ocx_env::OCX_FROZEN),
            (&mut self.quiet, &ocx_env::OCX_QUIET),
        ] {
            *flag |= var.bool_or(false)?;
        }
        // The received header's narrowing outlives a child's `OCX_OFFLINE=0`; `credential_helper`
        // implies `offline`, so a helper's ocx never reaches the registry it resolves credentials for.
        // The legacy marker stays read until 0.7. None of this is a typed root flag.
        let inherited = Inherited::from_process_env();
        let received = inherited.narrowing();
        self.offline |= received.offline || ocx_env::__OCX_CREDENTIAL_HELPER.bool_or(false)?;
        self.frozen |= received.frozen;
        // `remote` is neutral: the typed flag, then the parent's value, then the ambient variable.
        if !typed.remote
            && let Some(remote) = super::context::received_policy(&inherited).and_then(|policy| policy.remote)
        {
            self.remote = remote;
        }
        Ok(())
    }

    /// The root flags that may reach a child's policy, as typed on the command line: an ambient
    /// `OCX_OFFLINE`, `OCX_REMOTE` or `OCX_FROZEN` folded in by [`Self::apply_env`] is not one.
    pub fn root_flags(&self) -> RootFlags {
        let typed = self.typed.unwrap_or(TypedSwitches {
            remote: self.remote,
            offline: self.offline,
            frozen: self.frozen,
        });
        RootFlags {
            config: self.config.clone(),
            remote: typed.remote,
            offline: typed.offline,
            frozen: typed.frozen,
            index: self.index.clone(),
        }
    }

    /// Builds the reporting [`Api`](api::Api); the only construction, shared by
    /// [`crate::app::Context::try_init`] and Context-free commands (`ocx version`).
    // A second construction site would drop `--color` or drift the format default.
    pub fn build_api(&self, color_config: ColorModeConfig) -> api::Api {
        let printer = Printer::new(color_config.stdout, color_config.stderr);
        let data = DataInterface::new(printer);
        api::Api::new(self.format.mode(), data, self.quiet)
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
            (&ocx_env::OCX_QUIET, "on"),
        ] {
            env.set(key, value);
        }
        let from_env = parsed(&["ocx"]);
        assert!(from_env.offline && from_env.remote && from_env.quiet && !from_env.global);
        assert!(!from_env.frozen);
    }

    /// An ocx started by a credential helper never reaches a registry, whatever else is set.
    #[test]
    fn the_credential_helper_marker_forces_offline() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        let mut options = ContextOptions::try_parse_from(["ocx"]).expect("args parse");
        options.apply_env().expect("no variable is set");
        assert!(!options.offline);

        env.set(&ocx_env::__OCX_CREDENTIAL_HELPER, "1");
        let mut options = ContextOptions::try_parse_from(["ocx", "--remote"]).expect("args parse");
        options.apply_env().expect("the marker is a boolean");
        assert!(options.offline);
    }

    /// An exported `OCX_GLOBAL` selects nothing: only the `--global` flag does.
    #[test]
    fn an_exported_ocx_global_never_selects_the_global_tier() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        env.set(&ocx_env::OCX_GLOBAL, "1");
        let mut options = ContextOptions::try_parse_from(["ocx"]).expect("args parse");
        options.apply_env().expect("every variable holds a boolean");
        assert!(!options.global);

        let mut flagged = ContextOptions::try_parse_from(["ocx", "--global"]).expect("args parse");
        flagged.apply_env().expect("every variable holds a boolean");
        assert!(flagged.global);
    }

    /// A received `offline` narrowing outlives `OCX_OFFLINE=0` yet is not a typed root flag.
    #[test]
    fn a_received_offline_narrowing_holds_against_ambient_zero() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        env.set(&ocx_env::__OCX_ENV_STATE, "2;n=offline");
        env.set(&ocx_env::OCX_OFFLINE, "0");
        let mut options = ContextOptions::try_parse_from(["ocx"]).expect("args parse");
        options.apply_env().expect("every variable holds a boolean");
        assert!(options.offline);
        assert!(!options.root_flags().offline, "received narrowing is not a typed flag");
    }

    /// A received `frozen` narrowing outlives `OCX_FROZEN=0` yet is not a typed root flag.
    #[test]
    fn a_received_frozen_narrowing_holds_against_ambient_zero() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        env.set(&ocx_env::__OCX_ENV_STATE, "2;n=frozen");
        env.set(&ocx_env::OCX_FROZEN, "0");
        let mut options = ContextOptions::try_parse_from(["ocx"]).expect("args parse");
        options.apply_env().expect("every variable holds a boolean");
        assert!(options.frozen);
        assert!(!options.root_flags().frozen, "received narrowing is not a typed flag");
    }

    /// A parent's `--remote` reaches a child through the state, ahead of the ambient variable, and is no typed flag.
    #[test]
    fn a_received_remote_holds_without_the_public_variable() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        let state = ocx_config::exec_state::encode(&ocx_config::exec_state::ExecState {
            policy: ocx_config::exec_state::Policy {
                remote: Some(true),
                ..Default::default()
            },
            ..Default::default()
        })
        .value
        .expect("a default-sized state encodes");
        env.set(&ocx_env::__OCX_ENV_STATE, state);
        let apply = |args: &[&str]| {
            let mut options = ContextOptions::try_parse_from(args).expect("args parse");
            options.apply_env().expect("every variable holds a boolean");
            options
        };

        let plain = apply(&["ocx"]);
        assert!(plain.remote, "the parent's --remote reaches the child");
        assert!(!plain.root_flags().remote, "a received value is not a typed flag");

        env.set(&ocx_env::OCX_REMOTE, "0");
        assert!(
            apply(&["ocx"]).remote,
            "neutral: the parent's value outranks the ambient variable"
        );
    }

    /// The header's `credential_helper` narrowing implies offline without the legacy marker.
    #[test]
    fn a_received_credential_helper_narrowing_forces_offline() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        env.set(&ocx_env::__OCX_ENV_STATE, "2;n=credential_helper");
        let mut options = ContextOptions::try_parse_from(["ocx", "--remote"]).expect("args parse");
        options.apply_env().expect("every variable holds a boolean");
        assert!(options.offline);
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

    /// A typed flag is a root flag; the same value arriving through the environment is not.
    #[test]
    fn root_flags_ignore_an_ambient_offline() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        env.set(&ocx_env::OCX_OFFLINE, "1");
        let mut options = ContextOptions::try_parse_from(["ocx"]).expect("args parse");
        options.apply_env().expect("every variable holds a boolean");
        assert!(options.offline, "the environment folds into the switch");
        let RootFlags {
            config,
            remote,
            offline,
            frozen,
            index,
        } = options.root_flags();
        assert_eq!(
            (config, remote, offline, frozen, index),
            (None, false, false, false, None)
        );

        let mut typed = ContextOptions::try_parse_from(["ocx", "--offline", "--config", "c.toml", "--index", "i"])
            .expect("args parse");
        typed.apply_env().expect("every variable holds a boolean");
        let RootFlags {
            config,
            remote,
            offline,
            frozen,
            index,
        } = typed.root_flags();
        assert_eq!(config.as_deref(), Some(std::path::Path::new("c.toml")));
        assert_eq!(index.as_deref(), Some(std::path::Path::new("i")));
        assert!(offline && !remote && !frozen);
    }

    /// A second fold must not capture the first fold's ambient values as typed.
    #[test]
    fn root_flags_stay_as_typed_when_the_environment_is_folded_twice() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        env.set(&ocx_env::OCX_OFFLINE, "1");
        env.set(&ocx_env::OCX_FROZEN, "1");
        let mut options = ContextOptions::try_parse_from(["ocx"]).expect("args parse");
        options.apply_env().expect("every variable holds a boolean");
        options.apply_env().expect("every variable holds a boolean");
        let flags = options.root_flags();
        assert!(
            !flags.offline && !flags.frozen,
            "ambient values became root flags: {flags:?}"
        );
    }

    #[test]
    fn build_api_honors_the_flattened_format_group() {
        assert!(!is_json(&["ocx"]), "default is plain");
        assert!(is_json(&["ocx", "--json"]));
        assert!(is_json(&["ocx", "--format", "json"]));
    }
}
