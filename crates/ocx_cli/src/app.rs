// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};
use ocx_lib::cli;

use crate::command;

mod context;
pub use context::{Context, ManagedConfigGate};

mod context_options;
pub use context_options::ContextOptions;

mod managed_config_check;

pub mod plugin_dispatch;

pub mod project_context;

mod update_check;

mod version;
pub use version::version;

pub mod build_info;

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
// Disable clap's auto-generated `help` subcommand so `ocx help <name>` falls
// through to the External variant and is rewritten to `ocx-<name> --help` by
// `plugin_dispatch::rewrite_help_invocation`. Without this, clap intercepts
// `help <X>` before External fires, producing a confusing "unrecognized
// subcommand" error instead of dispatching to the plugin.
#[command(disable_help_subcommand = true)]
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
        // `Version::execute`, `ShellCompletion::execute`, `SelfActivate::execute`,
        // and bare `ocx` (None) are Context-free — the entire `Context::try_init`
        // path (which calls `ConfigLoader::load` and aborts on bad TOML /
        // oversized files) is unnecessary for them.
        //
        // `Self_(SelfActivate)` is in this list because `self activate` runs on
        // every shell startup (sourced from `$OCX_HOME/env.sh`).  The full
        // `Context::try_init` cost — ConfigLoader file walk, OCI client,
        // RemoteIndex, PackageManager construction — is paid unnecessarily on
        // every new shell session.  `SelfActivate::execute` only needs a
        // `FileStructure` (to resolve the absolute symlink bin path), which it
        // constructs cheaply via `FileStructure::new()` directly.
        //
        // `Info` is deliberately NOT in this list: it reads
        // `context.default_registry()` from the loaded config and is expected
        // to surface more config-derived fields in the future. When ambient
        // config is broken, falling back to `ocx version` is the documented
        // diagnostic path. The regression guard lives at
        // `test/tests/test_config.py::test_info_still_requires_valid_config_when_ambient_broken`.
        // Consuming match so the External arm can take argv by value (no clone).
        // The fallthrough arm binds `command` by value; the separate
        // `should_check_for_update` call below re-borrows from the re-bound name.
        match cli.command {
            Some(command::Command::Version(ref v)) => return v.execute(&cli.context, color_config).await,
            Some(command::Command::Shell(command::shell::Shell::Completion(ref c))) => return c.execute().await,
            Some(command::Command::Self_(command::self_group::SelfGroup::Activate(ref a))) => return a.execute().await,
            Some(command::Command::External(argv)) => {
                return plugin_dispatch::dispatch(argv, &cli.context).await;
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
        .await?;
        if should_check_for_update(&cli.command) {
            update_check::check_for_update(&context).await;
        }
        if should_check_managed_config_refresh(&cli.command) {
            managed_config_check::check_for_managed_config_refresh(&context).await;
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
                | command::Command::About(_)
                | command::Command::Shell(command::shell::Shell::Completion(_))
                | command::Command::Self_(_)
        )
    )
}

/// Skip the managed-config background-refresh probe for commands that only
/// print static info, plus the `ocx config` group itself — an explicit
/// `config update` already performs the full (non-throttled) refresh, and
/// `config push` is the operator-side publish command that never consults
/// the local tier.
fn should_check_managed_config_refresh(command: &Option<command::Command>) -> bool {
    !matches!(
        command,
        Some(
            command::Command::Version(_)
                | command::Command::About(_)
                | command::Command::Shell(command::shell::Shell::Completion(_))
                | command::Command::Self_(_)
                | command::Command::Config(_)
        )
    )
}

/// Gates `Context::try_init`'s `[managed]` required-snapshot enforcement
/// (ADR Decision E, criterion 6: `required = true` + no matching snapshot →
/// exit 78 for ordinary commands).
///
/// Exempts the same command set as [`should_check_managed_config_refresh`]:
/// `ocx config update` is the ONLY command that can create the missing
/// snapshot in the first place (the CI-ephemeral recipe is
/// `OCX_MANAGED_CONFIG=... && ocx config update && ocx <cmd>` — the update
/// step itself must be reachable with no snapshot yet), and the `self`/
/// static commands never consult the managed-config tier.
fn should_enforce_managed_config_required(command: &Option<command::Command>) -> bool {
    should_check_managed_config_refresh(command)
}

/// Names the two commands that can adopt a brand-new managed-config source
/// with no seed present (`ocx config update`, `ocx self setup
/// --managed-config <ref>`): only they get the managed-fetch client built
/// when no source resolves yet. Deliberately narrower than the
/// required-gate exemption above — `ocx self activate` runs on every shell
/// startup and must not pay the client-build cost for an unconfigured tier.
fn is_managed_config_onboarding_command(command: &Option<command::Command>) -> bool {
    matches!(
        command,
        Some(
            command::Command::Config(command::config::ConfigGroup::Update(_))
                | command::Command::Self_(command::self_group::SelfGroup::Setup(_))
        )
    )
}

pub async fn run() -> anyhow::Result<ExitCode> {
    App::new().run().await
}

#[cfg(test)]
mod tests {
    use super::should_check_for_update;
    use crate::command::{self, self_group, version};

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
}
