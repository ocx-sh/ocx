// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};
use ocx_lib::cli;

use crate::command;

mod context;
pub use context::Context;

mod context_options;
pub use context_options::ContextOptions;

pub mod project_context;

mod update_check;

mod version;
pub use version::version;

/// A CLI-local command failure carrying its own exit code.
///
/// Mirrors `ocx_lib::cli::UsageError` (message + fixed classification) but
/// lets the caller pick the code, for command-boundary validations that map
/// to exits other than 64 (e.g. `NotFound`, `ConfigError`, `DataError`) and
/// whose originating type is not a library error. Commands return this
/// instead of `eprintln!` + `Ok(ExitCode::…)` so the message flows through
/// the single `main.rs` `log::error!` + classify boundary.
#[derive(Debug)]
pub struct CommandError {
    message: String,
    code: cli::ExitCode,
}

impl CommandError {
    pub fn new(message: impl Into<String>, code: cli::ExitCode) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

impl ocx_lib::cli::ClassifyExitCode for CommandError {
    fn classify(&self) -> Option<cli::ExitCode> {
        Some(self.code)
    }
}

/// Classify an error chain into an [`ExitCode`], extending the library
/// classifier with CLI-local error types.
///
/// `ocx_lib::cli::classify_error` only knows library error types — it cannot
/// downcast `ocx_cli`-local types like [`project_context::ProjectContextError`]
/// (the dependency only points one way). This wrapper walks the chain for the
/// CLI-local types first, then delegates the remainder to the library
/// classifier. It is the single exit-code authority for `main.rs`; commands
/// must return typed errors rather than hand-mapping exit codes.
pub fn classify_error(err: &(dyn std::error::Error + 'static)) -> cli::ExitCode {
    use ocx_lib::cli::ClassifyExitCode as _;
    for cause in std::iter::successors(Some(err), |e| e.source()) {
        if let Some(pce) = cause.downcast_ref::<project_context::ProjectContextError>()
            && let Some(code) = pce.classify()
        {
            return code;
        }
        if let Some(ce) = cause.downcast_ref::<CommandError>()
            && let Some(code) = ce.classify()
        {
            return code;
        }
    }
    cli::classify_error(err)
}

#[derive(Parser)]
#[command(name = "ocx", about, long_about = None)]
#[command(about = "A simple package manager for pre-built binaries.", long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub context: ContextOptions,

    #[command(subcommand)]
    pub command: Option<command::Command>,
}

pub struct App {}

impl App {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn run(self) -> anyhow::Result<ExitCode> {
        // Pre-parse --color before clap so help/error output respects it.
        let color_mode = cli::ColorMode::from_args();
        let color_config = color_mode.config();
        color_config.apply();

        let styles = cli::clap_styles(color_config.stdout);
        // Route every clap failure (value-validation, missing args, unknown
        // flags) through `cli::clap::parse` so backend tools see typed exit
        // codes (`EX_USAGE` = 64) instead of clap's default `2`. Help, version
        // and `DisplayHelpOnMissingArgumentOrSubcommand` paths still print
        // and exit `0` via clap's renderer inside the helper.
        let matches = match cli::clap::parse(Cli::command().color(color_mode.into()).styles(styles.clone())) {
            Ok(matches) => matches,
            Err(code) => return Ok(code.into()),
        };
        let cli = Cli::from_arg_matches(&matches)?;

        // Static commands dispatch without constructing a Context so they
        // survive a malformed ambient config (`~/.ocx/config.toml`).
        // `Version::execute`, `ShellCompletion::execute`, and bare `ocx` (None)
        // are Context-free — the entire `Context::try_init` path (which calls
        // `ConfigLoader::load` and aborts on bad TOML / oversized files) is
        // unnecessary for them.
        //
        // `Info` is deliberately NOT in this list: it reads
        // `context.default_registry()` from the loaded config and is expected
        // to surface more config-derived fields in the future. When ambient
        // config is broken, falling back to `ocx version` is the documented
        // diagnostic path. The regression guard lives at
        // `test/tests/test_config.py::test_info_still_requires_valid_config_when_ambient_broken`.
        match &cli.command {
            Some(command::Command::Version(v)) => return v.execute().await,
            Some(command::Command::Shell(command::shell::Shell::Completion(c))) => return c.execute().await,
            None => {
                Cli::command().color(color_mode.into()).styles(styles).print_help()?;
                return Ok(ExitCode::SUCCESS);
            }
            _ => {}
        }

        let context = Context::try_init(&cli.context, color_config).await?;
        if should_check_for_update(&cli.command) {
            update_check::check_for_update(&context).await;
        }
        let Some(command) = &cli.command else {
            unreachable!("None handled in static-command bypass above");
        };
        command.execute(context).await
    }
}

/// Skip the update check for commands that only print static info.
fn should_check_for_update(command: &Option<command::Command>) -> bool {
    !matches!(
        command,
        Some(
            command::Command::Version(_)
                | command::Command::Info(_)
                | command::Command::Shell(command::shell::Shell::Completion(_))
        )
    )
}

pub async fn run() -> anyhow::Result<ExitCode> {
    App::new().run().await
}
