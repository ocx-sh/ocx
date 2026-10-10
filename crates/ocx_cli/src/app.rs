// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};

use crate::command;
use crate::error_document::render_error_document;
use crate::options::FormatMode;

mod background_check;

mod boundary;
pub use boundary::finish;

mod context;
pub use context::{Context, ManagedConfigGate, is_published_namespace};

mod context_options;
pub use context_options::ContextOptions;

mod env_flags;
pub use env_flags::ENV_FLAGS;

mod managed_config_check;

pub mod plugin_dispatch;

pub mod project_context;

mod refusal;
pub use refusal::CliRefusal;

#[cfg(any(test, feature = "__testing"))]
pub mod seam;

mod toolchain_drift_check;

pub(crate) mod update_check;
mod update_notice;

mod version;
pub use version::version;

pub mod build_info;

#[derive(Parser)]
#[command(name = "ocx", about, long_about = None)]
#[command(about = "A simple package manager for pre-built binaries.", long_about = None)]
// Off so `ocx help <name>` reaches `External` as `ocx-<name> --help`; clap's own would reject it first.
#[command(disable_help_subcommand = true)]
pub struct Cli {
    #[command(flatten)]
    pub context: ContextOptions,

    #[command(subcommand)]
    pub command: Option<command::Command>,
}

pub struct App {}

// Clippy's `new_without_default`: `App::new` is public API of the library target.
impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn run(self) -> anyhow::Result<ExitCode> {
        // Pre-parse --color before clap so help/error output respects it.
        let color_mode = ocx_console::ColorMode::from_args();
        let color_config = color_mode.config();
        color_config.apply();
        self.run_from(std::env::args_os().collect(), color_mode, color_config)
            .await
    }

    async fn run_from(
        self,
        argv: Vec<std::ffi::OsString>,
        color_mode: ocx_console::ColorMode,
        color_config: ocx_console::ColorModeConfig,
    ) -> anyhow::Result<ExitCode> {
        let styles = ocx_console::clap_styles(color_config.stdout);
        // Through `parse`, so a clap failure exits 64 (`EX_USAGE`) rather than clap's 2.
        let matches = match parse(Cli::command().color(color_mode.into()).styles(styles.clone()), &argv) {
            Ok(matches) => matches,
            Err(code) => {
                if json_requested(&argv) {
                    print_usage_document(&argv);
                }
                return Ok(code);
            }
        };
        let mut cli = Cli::from_arg_matches(&matches)?;
        let format = cli.context.format.mode();
        let command_name = cli.command.as_ref().map(canonical_command_name).unwrap_or_default();
        let fail = |err: anyhow::Error| report_failure(format, &command_name, err);
        // No subscriber exists yet: without one, a refused `OCX_*` value exits 78 with empty stderr.
        cli.context.apply_env().map_err(|err| {
            init_bypass_logging(&cli.context, color_config);
            fail(err.into())
        })?;
        // Before the static bypass, or a non-admitted static verb still runs under the seam.
        #[cfg(any(test, feature = "__testing"))]
        if seam::active() {
            seam::admit(cli.command.as_ref()).map_err(fail)?;
        }

        // Static commands bypass `Context::try_init`, which aborts on bad ambient config.
        match cli.command {
            Some(command::Command::Version(ref v)) => return v.execute(&cli.context, color_config).await.map_err(fail),
            Some(command::Command::Shell(command::shell::Shell::Completion(ref c))) => {
                let result = c.execute(&cli.context).await;
                if result.is_err() {
                    init_bypass_logging(&cli.context, color_config);
                }
                return result.map_err(fail);
            }
            Some(command::Command::Self_(command::self_group::SelfGroup::Activate(ref a))) => {
                // Runs on every shell startup, so it skips `Context::try_init`'s cost.
                let result = a.execute(&cli.context, color_config).await;
                if result.is_err() {
                    init_bypass_logging(&cli.context, color_config);
                }
                return result.map_err(fail);
            }
            Some(command::Command::External(argv)) => {
                return plugin_dispatch::dispatch(argv, &cli.context).await.map_err(fail);
            }
            None => {
                Cli::command().color(color_mode.into()).styles(styles).print_help()?;
                return Ok(ExitCode::SUCCESS);
            }
            _ => {}
        }

        let context = Context::try_init(
            &cli.context,
            color_config,
            ManagedConfigGate {
                enforce_required: should_enforce_managed_config_required(&cli.command),
                onboarding: is_managed_config_onboarding_command(&cli.command),
            },
        )
        .await
        .map_err(fail)?;
        command::deprecated::warn_renamed_flags(&context, &matches);
        let Some(command) = &cli.command else {
            unreachable!("None handled in static-command bypass above");
        };
        // Every probe reaches the network; the seam never makes a request.
        let mut notice_rows = Vec::new();
        let mut pending_update = None;
        if !in_seam() && should_check_for_update(&cli.command) {
            match update_check::check_for_update(&context).await {
                Some(update_check::SelfUpdate::Notice(row)) => notice_rows.push(row),
                Some(update_check::SelfUpdate::Apply(identifier)) => pending_update = Some(identifier),
                None => {}
            }
        }
        if !in_seam() && should_check_managed_config_refresh(&cli.command) {
            managed_config_check::check_for_managed_config_refresh(&context).await;
        }
        if !in_seam() && should_check_toolchain_drift(command) {
            notice_rows.extend(toolchain_drift_check::check_for_toolchain_drift(&context).await);
        }
        update_notice::print(context.ui(), &notice_rows);
        // Captured before `execute` consumes the context: this manager carries `with_auto_verify`,
        // so trust policy also covers the unattended install.
        let pending_update = pending_update.map(|identifier| (context.manager().clone(), identifier));

        // No error document after a report, or stdout carries two JSON documents.
        let reported = context.api().reported_handle();
        let result = command.execute(context).await;
        // After the command, on success and failure alike, and never changing its result.
        if let Some((manager, identifier)) = pending_update {
            update_check::apply_pending(&manager, &identifier).await;
        }
        match result {
            Ok(code) => Ok(code),
            Err(err) if !reported.load(std::sync::atomic::Ordering::Relaxed) => Err(fail(err)),
            Err(err) => Err(err),
        }
    }
}

/// Install the log subscriber a command that bypassed `Context::try_init` failed without: `finish`
/// logs the error through `tracing`, so without one it vanishes. `.ok()`: `self activate --reconcile`
/// already installed it through `Context::try_init`.
fn init_bypass_logging(options: &ContextOptions, color_config: ocx_console::ColorModeConfig) {
    // The seam logs through its call-scoped subscriber; a global one would outlive the run.
    if in_seam() {
        return;
    }
    crate::tracing_init::LogSettings::default()
        .with_console_level(options.log_level)
        .with_stderr_color(color_config.stderr)
        .init()
        .ok();
}

/// Under `--format json`, print `err` as the error document; `err` is returned either way.
fn report_failure(format: FormatMode, command: &str, err: anyhow::Error) -> anyhow::Error {
    if format == FormatMode::Json {
        print_error_document(command, &err);
    }
    err
}

fn print_error_document(command: &str, err: &anyhow::Error) {
    match render_error_document(command, err) {
        // Through the printer, the one stdout path every report takes; `--quiet` does not reach it.
        Ok(rendered) => ocx_console::Printer::new(false, false)
            .cout()
            .plain(rendered)
            .end_line(),
        // Logged, and the caller still returns `err`, so neither cause is swallowed.
        Err(render_err) => log::error!("error document render failed: {render_err:#}"),
    }
}

/// Whether ocx's own `--format`/`--json` asks for JSON: the last spelling wins, and a flag after
/// `--` belongs to the child command, so the scan stops there.
fn json_requested(argv: &[std::ffi::OsString]) -> bool {
    let mut json = false;
    let mut tokens = argv.iter().skip(1).filter_map(|token| token.to_str());
    while let Some(token) = tokens.next() {
        match token {
            "--" => break,
            "--json" => json = true,
            "--format" => match tokens.next() {
                Some("--") | None => break,
                Some(value) => json = value == "json",
            },
            _ => {
                if let Some(value) = token.strip_prefix("--format=") {
                    json = value == "json";
                }
            }
        }
    }
    json
}

/// Print clap's refusal of `argv` as one error document (exit 64, kind `usage_error`).
fn print_usage_document(argv: &[std::ffi::OsString]) {
    use clap::error::ErrorKind;

    let Err(error) = Cli::command().try_get_matches_from(argv) else {
        return;
    };
    if matches!(
        error.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        return;
    }
    // `StyledStr`'s `Display` never emits colour, so no escape code reaches the document.
    let rendered = error.render().to_string();
    let first_line = rendered.lines().next().unwrap_or_default();
    let message = first_line.strip_prefix("error: ").unwrap_or(first_line);
    let err = anyhow::Error::new(CliRefusal::InvalidCommandLine(message.to_owned()));
    print_error_document(&usage_command_path(argv), &err);
}

/// The subcommand words of `argv` clap recognised before refusing it; `""` when none were.
fn usage_command_path(argv: &[std::ffi::OsString]) -> String {
    let root = Cli::command();
    let mut current = &root;
    let mut words = Vec::new();
    for token in argv.iter().skip(1) {
        if token == "--" || !current.has_subcommands() {
            break;
        }
        // A flag, or a flag's value: neither names a subcommand.
        if let Some(sub) = token
            .to_str()
            .filter(|token| !token.starts_with('-'))
            .and_then(|token| current.find_subcommand(token))
        {
            words.push(sub.get_name());
            current = sub;
        }
    }
    words.join(" ")
}

/// Canonical space-separated command name in the error document.
///
/// Frozen v1 (`adr_oci_referrers_signing_v1.md`): changing an existing mapping is a v2 schema bump.
fn canonical_command_name(command: &command::Command) -> String {
    use command::leaf::Leaf;
    match command.leaf() {
        // The v1 envelope named the whole group, so `direnv init` and `direnv export` stay `direnv`.
        Some(Leaf::DirenvInit | Leaf::DirenvExport) => "direnv".to_owned(),
        Some(leaf) => leaf.path().join(" "),
        None => "external".to_owned(),
    }
}

/// Skips the network update check for static, `self`, `config` and `shell` commands, and for
/// the trampoline and per-prompt hot paths (`exec`, `run`, `launcher`, `env`, `direnv`).
///
/// `Shell` variants are listed, not wildcarded, so a new one is checked by default
/// (`should_check_for_update_skips_all_shell_variants_canary`).
fn should_check_for_update(command: &Option<command::Command>) -> bool {
    !matches!(
        command,
        Some(
            command::Command::Version(_)
                | command::Command::About(_)
                | command::Command::Shell(
                    command::shell::Shell::Allow(_)
                        | command::shell::Shell::Completion(_)
                        | command::shell::Shell::Revoke(_)
                        | command::shell::Shell::State(_)
                )
                | command::Command::Self_(_)
                | command::Command::Config(_)
                | command::Command::Exec(_)
                | command::Command::DeprecatedRun(_)
                | command::Command::Launcher(_)
                | command::Command::Env(_)
                | command::Command::Direnv(_)
        )
    )
}

/// Whether the toolchain drift notice runs: the update-check skip set plus the commands that
/// already re-resolve or rewrite the lock (`update`, `lock`, `add`, `remove`, `init`).
///
/// An exhaustive match, so a new command is a compile error until its membership is decided.
fn should_check_toolchain_drift(command: &command::Command) -> bool {
    use command::Command;
    match command {
        Command::Version(_)
        | Command::About(_)
        | Command::Shell(_)
        | Command::Self_(_)
        | Command::Config(_)
        | Command::Exec(_)
        | Command::DeprecatedRun(_)
        | Command::Launcher(_)
        | Command::Env(_)
        | Command::Direnv(_)
        | Command::Update(_)
        | Command::Upgrade(_)
        | Command::Lock(_)
        | Command::Add(_)
        | Command::Remove(_)
        | Command::Init(_)
        | Command::External(_) => false,
        Command::Clean(_)
        | Command::Index(_)
        | Command::Inspect(_)
        | Command::Status(_)
        | Command::Login(_)
        | Command::Logout(_)
        | Command::Package(_)
        | Command::Patch(_)
        | Command::Pull(_) => true,
    }
}

/// Skips the managed-config refresh probe for static, `self`, `shell` and `config` commands.
///
/// Also the required-snapshot exemption: without `shell state` here, `required = true` with no
/// snapshot exits 78 before the diagnostic that reports it.
fn should_check_managed_config_refresh(command: &Option<command::Command>) -> bool {
    !matches!(
        command,
        Some(
            command::Command::Version(_)
                | command::Command::About(_)
                | command::Command::Shell(
                    command::shell::Shell::Allow(_)
                        | command::shell::Shell::Completion(_)
                        | command::shell::Shell::Revoke(_)
                        | command::shell::Shell::State(_)
                )
                | command::Command::Self_(_)
                | command::Command::Config(_)
        )
    )
}

/// Gates `[managed] required = true` enforcement: exit 78 with no matching snapshot.
///
/// Exempts the refresh set, which must keep `ocx config update`, the only command that creates the missing snapshot.
fn should_enforce_managed_config_required(command: &Option<command::Command>) -> bool {
    should_check_managed_config_refresh(command)
}

/// The commands that build the managed-fetch client before any source resolves.
///
/// Narrower than the required-gate exemption: `self activate` runs on every shell startup and
/// must not pay that build.
fn is_managed_config_onboarding_command(command: &Option<command::Command>) -> bool {
    matches!(
        command,
        Some(
            command::Command::Config(command::config::ConfigGroup::Setup(_))
                | command::Command::Config(command::config::ConfigGroup::Update(_))
                | command::Command::Self_(command::self_group::SelfGroup::Setup(_))
        )
    )
}

pub async fn run() -> anyhow::Result<ExitCode> {
    App::new().run().await
}

/// Drive clap over `argv`. Under the seam, help and version render into the
/// capture instead of exiting the process.
fn parse(cmd: clap::Command, argv: &[std::ffi::OsString]) -> Result<clap::ArgMatches, ExitCode> {
    #[cfg(any(test, feature = "__testing"))]
    if seam::active() {
        return seam::parse(cmd, argv);
    }
    crate::clap_parse::parse(cmd, argv).map_err(Into::into)
}

/// Whether this runs inside the in-process seam; always `false` without the testing gate.
fn in_seam() -> bool {
    #[cfg(any(test, feature = "__testing"))]
    let active = seam::active();
    #[cfg(not(any(test, feature = "__testing")))]
    let active = false;
    active
}

#[cfg(test)]
mod tests {
    use super::{
        should_check_for_update, should_check_managed_config_refresh, should_check_toolchain_drift,
        should_enforce_managed_config_required,
    };
    use crate::command::{self, self_group, version};

    // ── Record frame vocabulary ──────────────────────────────────────────────

    /// Each `FrameCommand` names its command **exactly** as [`canonical_command_name`] does.
    ///
    /// The v1 error document and the execution record describe the same invocation,
    /// so a consumer joining them must grep one string. This is a real comparison,
    /// not restated literals, because this crate links both sides. It checks **per
    /// variant**, so swapping two spellings cannot pass, and the `Cli` parse names
    /// the arm so a rename cannot drift arm and spelling together without reding.
    ///
    /// Red state: change a `#[serde(rename = …)]` on `FrameCommand` or an arm in
    /// `canonical_command_name`, and its row fails.
    #[test]
    fn every_record_frame_command_matches_the_canonical_cli_name() {
        use clap::Parser as _;
        use ocx_package_manager::record::FrameCommand;

        // The deprecated `ocx run` is deliberately absent: it reports `"run"`
        // through the error envelope (a released binary already emitted that)
        // while its record says `"exec"`. The record states what executed, and
        // both spellings execute the project tier.
        let rows: [(FrameCommand, &[&str]); 4] = [
            (FrameCommand::Exec, &["ocx", "exec", "--", "true"]),
            (
                FrameCommand::PackageExec,
                &["ocx", "package", "exec", "cmake", "--", "true"],
            ),
            (
                FrameCommand::LauncherExec,
                &["ocx", "launcher", "exec", "/pkg", "--", "cmake"],
            ),
            (
                FrameCommand::LauncherShim,
                &[
                    "ocx",
                    "launcher",
                    "shim",
                    "index.ocx.sh/ocx/cmake@sha256:3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0",
                    "--",
                    "cmake",
                ],
            ),
        ];

        for (frame, argv) in rows {
            let cli =
                super::Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("`{}` must parse: {e}", argv.join(" ")));
            let command = cli.command.expect("the fixture names a subcommand");
            let wire = serde_json::to_value(frame)
                .expect("a unit enum variant serializes")
                .as_str()
                .expect("FrameCommand serializes as a string")
                .to_owned();
            assert_eq!(
                wire,
                super::canonical_command_name(&command),
                "the record and the error envelope must name `{}` identically",
                argv.join(" ")
            );
        }
    }

    // ── Exit-code classification ─────────────────────────────────────────────

    /// [#343](https://github.com/ocx-sh/ocx/issues/343) — moving the two
    /// project-tier refusals out of `ProjectContextError` and into
    /// `ocx_lib`'s `SessionError` must not move their exit codes.
    ///
    /// The refusals travel as `anyhow` from `self activate --reconcile`, so
    /// they reach [`classify_error`] as a bare cause: this function's own
    /// downcast ladder does not know `SessionError`, and the code comes from
    /// `crate::exit::classify_library_error`'s `families!` list instead. A
    /// `SessionError` left out of it silently degrades 78/65 to the generic
    /// failure code, which no other test would notice.
    ///
    /// Red state: delete `SessionError` from the `families!` list in
    /// `crates/ocx_cli/src/exit.rs` and both assertions report `Failure`.
    #[test]
    fn c343_the_session_refusals_keep_their_exit_codes_through_anyhow() {
        use std::path::PathBuf;

        use ocx_package_manager::activation::SessionError;
        use ocx_project::LockCurrency;

        let missing = anyhow::Error::from(SessionError::from(LockCurrency::Missing {
            path: PathBuf::from("/work/proj/ocx.lock"),
        }));
        assert_eq!(
            crate::exit::classify_error(missing.as_ref()),
            ocx_exit::ExitCode::ConfigError,
            "an absent lock is a configuration gap (78), the same as from `ocx pull`"
        );

        let stale = anyhow::Error::from(SessionError::from(LockCurrency::Stale {
            lock_path: PathBuf::from("/work/proj/ocx.lock"),
        }));
        assert_eq!(
            crate::exit::classify_error(stale.as_ref()),
            ocx_exit::ExitCode::DataError,
            "a stale lock is stale on-disk data (65), the same as from `ocx pull`"
        );
    }

    // ── CLI definition validity ──────────────────────────────────────────────

    /// The whole `ocx` command tree must satisfy clap's structural invariants.
    ///
    /// `clap::Command::debug_assert` runs the same checks clap runs when it
    /// *builds* the command — which it does on every parse and, crucially,
    /// inside `clap_complete::generate`. A violation (e.g. an optional
    /// positional before a required one) panics there only in debug builds, so
    /// without this test it slips past release CI and detonates the moment a
    /// developer generates completions from a debug binary. Locking it down here
    /// turns that latent landmine into a fast, deterministic unit-test failure.
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory as _;
        super::Cli::command().debug_assert();
    }

    // ── ASCII help guard (WinPS 5.1 mojibake hazard) ─────────────────────────

    /// No clap-facing help text anywhere in the command tree may contain a
    /// non-ASCII byte.
    ///
    /// clap turns the first paragraph of each `///` doc comment into the
    /// arg/command SHORT help, which `clap_complete` embeds as the completion
    /// tooltip, and the full doc comment into LONG help, rendered by `--help`.
    /// Both paths put the text in front of Windows PowerShell 5.1, which decodes
    /// a captured completion stream (and a piped `--help`) under the console
    /// codepage — turning a stray Unicode char (em-dash, arrow, ellipsis) into
    /// mojibake whose bytes can break the parsed completion script outright.
    ///
    /// `self_group::activate::tests::completion_output_is_ascii_for_all_shells`
    /// guards the generated OUTPUT; this guards the SOURCE help tree so a
    /// non-ASCII char cannot enter via a single-line doc comment in the first
    /// place. On failure, replace the offender: `->` for `→`, `-` for `—`,
    /// `...` for `…`.
    /// Collect every (location, help-text) pair clap renders across the whole
    /// `ocx` command tree: each command's about/long_about + before/after(_long)
    /// help, every argument's help/long_help, and every possible-value name +
    /// help.
    ///
    /// Shared by `cli_help_text_is_ascii` and
    /// `cli_help_text_has_no_internal_references` so both guards cover an
    /// identical, authoritative set of clap-facing strings — the traversal is
    /// the single definition of "what a user sees via `--help` / completions".
    fn collect_clap_help_texts() -> Vec<(String, String)> {
        use clap::CommandFactory as _;

        fn walk(cmd: &clap::Command, path: &str, out: &mut Vec<(String, String)>) {
            let here = if path.is_empty() {
                cmd.get_name().to_string()
            } else {
                format!("{path} {}", cmd.get_name())
            };
            for (label, styled) in [
                ("about", cmd.get_about()),
                ("long_about", cmd.get_long_about()),
                ("before_help", cmd.get_before_help()),
                ("after_help", cmd.get_after_help()),
                ("before_long_help", cmd.get_before_long_help()),
                ("after_long_help", cmd.get_after_long_help()),
            ] {
                if let Some(styled) = styled {
                    out.push((format!("[{here}] {label}"), styled.to_string()));
                }
            }
            for arg in cmd.get_arguments() {
                let id = arg.get_id().as_str();
                if let Some(help) = arg.get_help() {
                    out.push((format!("[{here}] arg {id} help"), help.to_string()));
                }
                if let Some(help) = arg.get_long_help() {
                    out.push((format!("[{here}] arg {id} long_help"), help.to_string()));
                }
                for value in arg.get_possible_values() {
                    let name = value.get_name();
                    out.push((format!("[{here}] arg {id} value-name"), name.to_string()));
                    if let Some(help) = value.get_help() {
                        out.push((format!("[{here}] arg {id} value {name} help"), help.to_string()));
                    }
                }
            }
            for sub in cmd.get_subcommands() {
                walk(sub, &here, out);
            }
        }

        let mut out = Vec::new();
        walk(&super::Cli::command(), "", &mut out);
        out
    }

    #[test]
    fn cli_help_text_is_ascii() {
        let offenders: Vec<String> = collect_clap_help_texts()
            .into_iter()
            .filter(|(_, text)| !text.is_ascii())
            .map(|(loc, text)| format!("{loc}: {text:?}"))
            .collect();
        assert!(
            offenders.is_empty(),
            "clap help text must be ASCII-only (Windows PowerShell 5.1 reads completion \
             tooltips and piped `--help` under the console codepage and mojibakes non-ASCII). \
             Replace `->` for `→`, `-` for `—`, `...` for `…`. Offenders:\n{}",
            offenders.join("\n")
        );
    }

    // ── Internal-reference help guard ────────────────────────────────────────

    /// No clap-facing help string may leak internal design provenance.
    ///
    /// Help text is a user-facing product surface; handshake/ADR references,
    /// amendment dates, and build timestamps belong in ADRs or `//` comments,
    /// never in `--help` (see `quality-cli-help.md`). This walks the same built
    /// command tree as `cli_help_text_is_ascii` and fails on unambiguous markers
    /// — a backstop so internal references cannot silently re-enter help.
    #[test]
    fn cli_help_text_has_no_internal_references() {
        /// Longest run of consecutive ASCII digits — catches build timestamps
        /// (`20260514120000`) without pulling in a regex dependency.
        fn max_digit_run(text: &str) -> usize {
            let mut max = 0usize;
            let mut run = 0usize;
            for byte in text.bytes() {
                if byte.is_ascii_digit() {
                    run += 1;
                    max = max.max(run);
                } else {
                    run = 0;
                }
            }
            max
        }

        /// Detect a `C-S<digit>` design-clause label (e.g. `C-S1-3`,
        /// `C-S1-4`). Lower-cased input, so `c-s` immediately followed by a
        /// digit is the marker. These clause tags are signing-slice ADR
        /// provenance and must never surface in `--help`.
        fn has_clause_label(lower: &str) -> bool {
            lower
                .as_bytes()
                .windows(4)
                .any(|window| window[0] == b'c' && window[1] == b'-' && window[2] == b's' && window[3].is_ascii_digit())
        }

        /// Detect an ISO-8601 calendar date `20dd-dd-dd` (e.g. `2026-05-19`).
        fn has_iso_date(text: &str) -> bool {
            text.as_bytes().windows(10).any(|w| {
                w[0] == b'2'
                    && w[1] == b'0'
                    && w[2].is_ascii_digit()
                    && w[3].is_ascii_digit()
                    && w[4] == b'-'
                    && w[5].is_ascii_digit()
                    && w[6].is_ascii_digit()
                    && w[7] == b'-'
                    && w[8].is_ascii_digit()
                    && w[9].is_ascii_digit()
            })
        }

        fn marker(text: &str) -> Option<&'static str> {
            let lower = text.to_ascii_lowercase();
            if text.contains('§') {
                return Some("`§` section reference");
            }
            if lower.contains("handshake") {
                return Some("`handshake` reference");
            }
            if lower.contains("adr_") {
                return Some("`adr_` reference");
            }
            if lower.contains("amended") {
                return Some("`amended` (design history)");
            }
            if has_clause_label(&lower) {
                return Some("`C-S<n>` design-clause label");
            }
            if has_iso_date(text) {
                return Some("ISO date (YYYY-MM-DD)");
            }
            if max_digit_run(text) >= 8 {
                return Some("8+ digit timestamp");
            }
            None
        }

        let offenders: Vec<String> = collect_clap_help_texts()
            .into_iter()
            .filter_map(|(loc, text)| marker(&text).map(|why| format!("{loc}: {why}: {text:?}")))
            .collect();
        assert!(
            offenders.is_empty(),
            "clap help must not leak internal design references (handshake/ADR/date \
             markers). State what the command or flag does for the user; move design \
             history to ADRs or `//` comments (see quality-cli-help.md). Offenders:\n{}",
            offenders.join("\n")
        );
    }

    // ── should_check_for_update skip-list canary ─────────────────────────────

    /// `Self_(SelfGroup::Activate(_))` must NOT trigger the background update
    /// check.  `self activate` runs on every shell startup (sourced from
    /// `$OCX_HOME/env.sh`); running the auto-check there would add noticeable
    /// latency to every new shell session.
    ///
    /// This test enumerates a representative `Self_` variant and asserts that
    /// `should_check_for_update` returns `false`.  It is a canary — any change
    /// to the skip list that accidentally removes `Self_` coverage will fail
    /// loudly here rather than silently regressing shell startup performance.
    #[test]
    fn should_check_for_update_skips_self_activate() {
        // Construct a minimal `SelfActivate` via its clap `Args` derive.
        // We use `clap::Parser::parse_from` on an empty argv so we get the
        // struct with all defaults — no flags needed for this test.
        use clap::Parser as _;
        let activate = self_group::activate::SelfActivate::parse_from(["self-activate"]);
        let cmd = Some(command::Command::Self_(self_group::SelfGroup::Activate(activate)));
        assert!(
            !should_check_for_update(&cmd),
            "Self_(Activate) must not trigger update check (shell-startup hot path)"
        );
    }

    /// `Self_(SelfGroup::Update(_))` must NOT trigger the background update
    /// check.  `self update` is the explicit user-facing update command; the
    /// auto-check path must never run alongside it.
    #[test]
    fn should_check_for_update_skips_self_update() {
        use clap::Parser as _;
        let update = self_group::update::SelfUpdate::parse_from(["self-update"]);
        let cmd = Some(command::Command::Self_(self_group::SelfGroup::Update(update)));
        assert!(
            !should_check_for_update(&cmd),
            "Self_(Update) must not trigger update check (user is explicitly managing version)"
        );
    }

    /// `Command::Version(_)` must NOT trigger the background update check.
    /// `ocx version` is a static-info command in the skip list; any regression
    /// here wastes a network probe on a version-info command.
    #[test]
    fn should_check_for_update_skips_version() {
        use clap::Parser as _;
        let ver = version::Version::parse_from(["version"]);
        let cmd = Some(command::Command::Version(ver));
        assert!(
            !should_check_for_update(&cmd),
            "Version must not trigger update check (static-info command)"
        );
    }

    /// `Command::Config(_)` must NOT trigger the background update check.
    /// The group is fleet tooling, and `config test` promises no network and
    /// no state writes at all — an auto-check would break that contract on a
    /// command whose whole point is validating a payload offline.
    #[test]
    fn should_check_for_update_skips_config_group() {
        use clap::Parser as _;
        let args = command::config_test::ConfigTestArgs::parse_from(["config-test", "candidate.toml"]);
        let cmd = Some(command::Command::Config(command::config::ConfigGroup::Test(args)));
        assert!(
            !should_check_for_update(&cmd),
            "Config group must not trigger update check (config test promises no network)"
        );
    }

    /// Every machine-driven surface must be in the skip list, and one ordinary
    /// command must NOT be — the pair, so a green here cannot come from a
    /// predicate that skips everything.
    ///
    /// `Exec` is what a rendered `<home>/toolchain/active/bin/<name>` trampoline
    /// `exec`s, `DeprecatedRun` is its still-shipped `ocx run` spelling,
    /// `Launcher` is what every generated package launcher runs
    /// (`ocx_package_manager::launcher::body`), and `Env`/`Direnv` are what a shell
    /// evaluates — `.envrc` re-runs `ocx direnv export` on every directory
    /// change. On any of them the probe's live tag listing plus `ocx --format
    /// json version` subprocess spawn lands on a command the user never typed.
    ///
    /// `Status` is the negative half: it is an ordinary command, it must keep
    /// the check, and a regression that widened the skip list to everything
    /// would red here rather than passing quietly.
    #[test]
    fn should_check_for_update_skips_the_machine_driven_surfaces() {
        use clap::Parser as _;
        let exec = command::toolchain_exec::ToolchainExec::parse_from(["exec", "--", "true"]);
        let deprecated_run = command::toolchain_exec::ToolchainExec::parse_from(["run", "--", "true"]);
        let env = command::toolchain_env::ToolchainEnv::parse_from(["env"]);
        let direnv = command::direnv::Direnv::parse_from(["direnv"]);
        let launcher_exec = parse_command(&["launcher", "exec", "/pkg", "--", "cmake"]);
        let launcher_shim = parse_command(&[
            "launcher",
            "shim",
            "index.ocx.sh/ocx/cmake@sha256:3f7a2b9c5d1e8f04a6b3c7d2e9f1a5b8c4d6e0f2a3b7c9d1e5f8a0b2c4d6e8f0",
            "--",
            "cmake",
        ]);

        for (label, cmd) in [
            ("exec", command::Command::Exec(exec)),
            ("run", command::Command::DeprecatedRun(deprecated_run)),
            ("launcher exec", launcher_exec),
            ("launcher shim", launcher_shim),
            ("env", command::Command::Env(env)),
            ("direnv", command::Command::Direnv(direnv)),
        ] {
            assert!(
                !should_check_for_update(&Some(cmd)),
                "`ocx {label}` is issued by a trampoline or a shell, not typed — it must not trigger the update check"
            );
        }

        let status = command::status::Status::parse_from(["status"]);
        assert!(
            should_check_for_update(&Some(command::Command::Status(status))),
            "an ordinary command must still carry the check — otherwise the skip list above proves nothing"
        );
    }

    /// `None` (bare `ocx` with no subcommand) is handled in the static-command
    /// bypass block before `should_check_for_update` is ever called, so the
    /// function is not invoked for `None` in normal operation.
    ///
    /// This test documents the actual return value (`true`) to make it explicit
    /// that `None` is NOT in the skip list — the guard is the early-return in
    /// `App::run`, not `should_check_for_update`.
    #[test]
    fn should_check_for_update_returns_true_for_none() {
        // None is handled by the static-command bypass before this function is
        // called; documenting the raw return value here as a design canary.
        assert!(
            should_check_for_update(&None),
            "None returns true from should_check_for_update; the guard is the early-return in App::run"
        );
    }

    /// Exhaustive canary — every `SelfGroup` variant must be in the skip list.
    ///
    /// `self activate` runs on every shell startup (sourced from
    /// `$OCX_HOME/env.sh`); `self update` is the explicit user-facing update
    /// command.  Neither must trigger a background `self_check_update` —
    /// `activate` would add latency to every new shell, and `update` would
    /// race with the user's explicit invocation.
    ///
    /// This test uses an exhaustive `match` on `SelfGroup` so adding a new
    /// `Self_` variant in the future is a compile error here — the contributor
    /// is forced to decide whether the new variant belongs in the skip list
    /// (typically yes — anything under `self` is install-management).  The
    /// canary fails loudly rather than silently regressing shell-startup
    /// performance or producing recursive update-check fan-out.
    #[test]
    fn should_check_for_update_skips_all_self_variants_canary() {
        use clap::Parser as _;

        // Constructor table: one entry per `SelfGroup` variant.  Adding a new
        // variant requires adding a matching row here AND extending the
        // exhaustive match below — the compiler enforces both.
        let activate = self_group::activate::SelfActivate::parse_from(["self-activate"]);
        let setup = self_group::setup::SelfSetup::parse_from(["self-setup"]);
        let update = self_group::update::SelfUpdate::parse_from(["self-update"]);

        // Exhaustive enumeration of `SelfGroup`.  This match has no wildcard;
        // adding a new variant breaks the build until updated.
        let all_variants: Vec<self_group::SelfGroup> = vec![
            self_group::SelfGroup::Activate(activate),
            self_group::SelfGroup::Setup(setup),
            self_group::SelfGroup::Update(update),
        ];
        for variant in &all_variants {
            // Exhaustiveness guard: forces the contributor to make a deliberate
            // skip-list decision when adding a new variant.
            match variant {
                self_group::SelfGroup::Activate(_)
                | self_group::SelfGroup::Setup(_)
                | self_group::SelfGroup::Update(_) => {}
            }
        }

        for (idx, variant) in all_variants.into_iter().enumerate() {
            let label = match &variant {
                self_group::SelfGroup::Activate(_) => "SelfGroup::Activate",
                self_group::SelfGroup::Setup(_) => "SelfGroup::Setup",
                self_group::SelfGroup::Update(_) => "SelfGroup::Update",
            };
            let cmd = Some(command::Command::Self_(variant));
            assert!(
                !should_check_for_update(&cmd),
                "every Self_ variant must be in the skip list (canary against shell-startup recursive update-check fan-out); \
                 variant idx={idx} ({label}) returned true from should_check_for_update"
            );
        }
    }

    /// Exhaustive canary — every `Shell` variant must be in **all three** skip
    /// lists (C-050, C-051).
    ///
    /// `ocx shell state` is the command a confused user is told to run when the
    /// shell integration misbehaves, and `ocx shell allow` is what that user is
    /// then told to type. Both are needed in exactly the state a broken
    /// managed tier produces, so both are exempt for one reason. Left out of
    /// these lists they pay the
    /// background update check and the managed-config refresh probe, and —
    /// decisively — `Context::try_init`'s `[managed] required = true` snapshot
    /// gate applies, so with `required = true` and no matching snapshot on disk
    /// it exits **78 before the command body runs**. That contradicts C-051's
    /// normative row: 0 in every reportable state, 74 the only non-zero path.
    ///
    /// The `match` below has no wildcard, so adding a `Shell` variant is a
    /// compile error here — the contributor is forced to decide whether the new
    /// subcommand belongs in the skip lists, and `should_check_for_update`'s
    /// own arm lists the variants one by one so the answer is never "skipped by
    /// default".
    #[test]
    fn should_check_for_update_skips_all_shell_variants_canary() {
        use clap::Parser as _;

        // Constructor table: one entry per `Shell` variant. A new variant needs
        // a row here AND an arm in the exhaustive match below.
        let all_variants: Vec<command::shell::Shell> = vec![
            command::shell::Shell::Allow(command::shell_allow::ShellAllow::parse_from(["shell-allow"])),
            command::shell::Shell::Completion(command::shell_completion::ShellCompletion::parse_from([
                "shell-completion",
            ])),
            command::shell::Shell::Revoke(command::shell_revoke::ShellRevoke::parse_from(["shell-revoke"])),
            command::shell::Shell::State(command::shell_state::ShellState::parse_from(["shell-state"])),
        ];

        for variant in &all_variants {
            // Exhaustiveness guard: no wildcard, so a new variant breaks the
            // build until its skip-list membership is decided.
            match variant {
                command::shell::Shell::Allow(_)
                | command::shell::Shell::Completion(_)
                | command::shell::Shell::Revoke(_)
                | command::shell::Shell::State(_) => {}
            }
        }

        for (idx, variant) in all_variants.into_iter().enumerate() {
            let label = match &variant {
                command::shell::Shell::Allow(_) => "Shell::Allow",
                command::shell::Shell::Completion(_) => "Shell::Completion",
                command::shell::Shell::Revoke(_) => "Shell::Revoke",
                command::shell::Shell::State(_) => "Shell::State",
            };
            let cmd = Some(command::Command::Shell(variant));
            assert!(
                !should_check_for_update(&cmd),
                "every Shell variant must be in the update-check skip list; \
                 variant idx={idx} ({label}) returned true from should_check_for_update"
            );
            assert!(
                !should_check_managed_config_refresh(&cmd),
                "every Shell variant must be in the managed-config refresh skip list; \
                 variant idx={idx} ({label}) returned true from should_check_managed_config_refresh"
            );
            assert!(
                !should_enforce_managed_config_required(&cmd),
                "every Shell variant must be exempt from the [managed] required-snapshot gate — \
                 otherwise `ocx shell state` exits 78 in exactly the broken state it exists to diagnose; \
                 variant idx={idx} ({label}) returned true from should_enforce_managed_config_required"
            );
        }
    }

    fn parse_command(argv: &[&str]) -> command::Command {
        use clap::Parser as _;
        let cli = super::Cli::try_parse_from(std::iter::once("ocx").chain(argv.iter().copied()))
            .unwrap_or_else(|error| panic!("`ocx {}` must parse: {error}", argv.join(" ")));
        cli.command.expect("a subcommand was given")
    }

    /// The drift notice skips everything the update check skips, plus the commands that already
    /// re-resolve or rewrite the lock — a notice there would tell the user to run what they just ran.
    #[test]
    fn should_check_toolchain_drift_skips_the_lock_writers_and_the_update_skip_set() {
        for argv in [
            &["update"][..],
            &["upgrade"],
            &["lock"],
            &["add", "ocx.sh/cmake:3"],
            &["remove", "cmake"],
            &["init"],
            &["version"],
            &["exec", "--", "true"],
            &["launcher", "exec", "/pkg", "--", "cmake"],
            &["env"],
            &["direnv"],
            &["shell", "state"],
            &["self", "update"],
            &["config", "test", "candidate.toml"],
        ] {
            let command = parse_command(argv);
            assert!(
                !should_check_toolchain_drift(&command),
                "`ocx {}` must not run the toolchain drift check",
                argv.join(" ")
            );
        }
    }

    /// `upgrade` keeps the self check and the managed tick, the same as `update`.
    #[test]
    fn upgrade_keeps_the_self_check_and_the_managed_tick() {
        for argv in [&["update"][..], &["upgrade"]] {
            let command = Some(parse_command(argv));
            assert!(should_check_for_update(&command), "`ocx {}`", argv.join(" "));
            assert!(
                should_check_managed_config_refresh(&command),
                "`ocx {}`",
                argv.join(" ")
            );
        }
    }

    /// The negative half, so a predicate that skips everything cannot pass the test above.
    #[test]
    fn should_check_toolchain_drift_runs_for_ordinary_commands() {
        for argv in [&["status"][..], &["pull"], &["clean"]] {
            let command = parse_command(argv);
            assert!(
                should_check_toolchain_drift(&command),
                "`ocx {}` must run the toolchain drift check",
                argv.join(" ")
            );
        }
    }

    /// Canary: no command that skips the update check may run the drift check.
    #[test]
    fn should_check_toolchain_drift_is_a_superset_of_the_update_skip_set() {
        for argv in [
            &["status"][..],
            &["pull"],
            &["clean"],
            &["update"],
            &["lock"],
            &["version"],
            &["exec", "--", "true"],
            &["run", "--", "true"],
            &["launcher", "exec", "/pkg", "--", "cmake"],
            &["env"],
            &["direnv"],
            &["shell", "allow"],
            &["shell", "revoke"],
            &["shell", "state"],
            &["self", "activate"],
            &["self", "setup"],
            &["self", "update"],
            &["config", "update"],
        ] {
            let command = parse_command(argv);
            if !should_check_for_update(&Some(parse_command(argv))) {
                assert!(
                    !should_check_toolchain_drift(&command),
                    "`ocx {}` skips the update check, so it must skip the drift check too",
                    argv.join(" ")
                );
            }
        }
    }

    fn argv(words: &[&str]) -> Vec<std::ffi::OsString> {
        std::iter::once("ocx")
            .chain(words.iter().copied())
            .map(Into::into)
            .collect()
    }

    #[test]
    fn json_is_requested_by_the_last_format_spelling_before_double_dash() {
        let json = |words: &[&str]| super::json_requested(&argv(words));
        assert!(json(&["--json", "package", "install"]));
        assert!(json(&["--format", "json", "exec"]));
        assert!(json(&["--format=json", "exec"]));
        assert!(!json(&["--json", "--format", "plain", "exec"]));
        assert!(!json(&["--json", "--format=plain", "exec"]));
        assert!(json(&["--format=plain", "--json", "exec"]));
        assert!(!json(&["exec", "--", "tool", "--json"]));
        assert!(!json(&["exec", "--", "tool", "--format", "json"]));
        assert!(!json(&["package", "install", "x:1"]));
    }

    #[test]
    fn the_usage_command_is_the_subcommand_words_clap_recognised() {
        let path = |words: &[&str]| super::usage_command_path(&argv(words));
        assert_eq!(
            path(&["--format", "json", "package", "install", "--nope", "x:1"]),
            "package install"
        );
        assert_eq!(
            path(&["--config", "/tmp/c.toml", "exec", "--nope", "--", "package"]),
            "exec"
        );
        assert_eq!(path(&["--json", "--nope"]), "");
    }
}
