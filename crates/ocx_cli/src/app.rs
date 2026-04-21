// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};
use ocx_lib::{cli, log};

use crate::command;
use crate::error_envelope::render_error_envelope;
use crate::options::Format;

mod context;
pub use context::Context;

mod context_options;
pub use context_options::ContextOptions;

pub mod project_context;

mod update_check;

mod version;
pub use version::version;

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

        // Capture format + canonical command name so JSON mode can render an
        // error envelope on stdout before `main.rs` returns the exit code.
        // Error branch only: success envelopes are owned by each command's
        // `api().report(...)` call, which already honors `--format json`.
        let format = cli.context.format;
        let command_name = canonical_command_name(command);
        match command.execute(context).await {
            Ok(code) => Ok(code),
            Err(err) if matches!(format, Format::Json) => {
                match render_error_envelope(command_name, &err) {
                    Ok(rendered) => println!("{rendered}"),
                    Err(render_err) => {
                        // Envelope rendering is infallible-by-design, but if serde
                        // ever fails we surface both causes rather than swallowing
                        // either one — the operator gets the underlying failure AND
                        // a diagnostic about the broken envelope path.
                        log::error!("error envelope render failed: {render_err:#}");
                    }
                }
                Err(err)
            }
            Err(err) => Err(err),
        }
    }
}

/// Map a parsed `Command` to the canonical space-separated string consumers
/// match on (e.g., `"package sign"`, `"verify"`, `"index update"`).
///
/// Frozen per ADR C-S1-1 §"`command` field". Any change to an existing mapping
/// is a v1 → v2 schema bump.
fn canonical_command_name(command: &command::Command) -> &'static str {
    use command::Command;
    use command::ci::Ci as CiCmd;
    use command::index::Index as IndexCmd;
    use command::package::Package as PackageCmd;
    use command::shell::Shell as ShellCmd;
    use command::shell_profile::ShellProfile as ShellProfileCmd;
    match command {
        Command::Ci(sub) => match sub {
            CiCmd::Export(_) => "ci export",
        },
        Command::Clean(_) => "clean",
        Command::Deps(_) => "deps",
        Command::Deselect(_) => "deselect",
        Command::Find(_) => "find",
        Command::Index(sub) => match sub {
            IndexCmd::Catalog(_) => "index catalog",
            IndexCmd::List(_) => "index list",
            IndexCmd::Update(_) => "index update",
        },
        Command::Info(_) => "info",
        Command::Install(_) => "install",
        Command::Uninstall(_) => "uninstall",
        Command::Verify(_) => "verify",
        Command::Exec(_) => "exec",
        Command::Env(_) => "env",
        Command::Package(sub) => match sub {
            PackageCmd::Create(_) => "package create",
            PackageCmd::Describe(_) => "package describe",
            PackageCmd::Info(_) => "package info",
            PackageCmd::Pull(_) => "package pull",
            PackageCmd::Push(_) => "package push",
            PackageCmd::Sign(_) => "package sign",
        },
        Command::Select(_) => "select",
        Command::Shell(sub) => match sub {
            ShellCmd::Completion(_) => "shell completion",
            ShellCmd::Env(_) => "shell env",
            ShellCmd::Profile(profile) => match profile {
                ShellProfileCmd::Add(_) => "shell profile add",
                ShellProfileCmd::List(_) => "shell profile list",
                ShellProfileCmd::Load(_) => "shell profile load",
                ShellProfileCmd::Remove(_) => "shell profile remove",
            },
        },
        Command::Version(_) => "version",
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
