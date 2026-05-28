// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Git/cargo-style plugin dispatch for `ocx <name>` → `ocx-<name>` on PATH.
//!
//! Wired from [`crate::app::App::run`] before `Context::try_init` to bypass
//! library context setup that plugins don't need. See
//! `.claude/artifacts/adr_cli_plugin_pattern.md`.
//!
//! ## Trust model
//!
//! Plugins run in OCX's trust boundary: they inherit the parent env (PATH,
//! HOME, auth tokens, etc.) verbatim, matching cargo / git / kubectl plugin
//! conventions. This is by design — plugins are first-party or user-vetted
//! extensions of `ocx`, not arbitrary user packages running via `ocx exec`.
//! Use `ocx exec` (or `ocx run`) when you need clean-env execution semantics.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::CommandFactory as _;
use ocx_lib::cli::{LogSettings, UsageError};
use ocx_lib::env::Env;
use ocx_lib::log;
use ocx_lib::utility::child_process::propagate_exit_code;

use crate::app::context_options::ContextOptions;

/// Dispatch entry point. Called from `App::run` for `Command::External(argv)`.
///
/// `argv[0]` is the unrecognized subcommand name (clap guarantee). Resolves
/// `ocx-<name>` on PATH, forwards environment via `Env::apply_ocx_config`,
/// execs the plugin, and propagates its exit status.
///
/// Initializes a minimal stderr log subscriber so that any error returned from
/// this function is printed by the `main.rs` `log::error!` boundary.
/// `Context::try_init` is never reached on this path, so there is no risk of
/// double-init.
pub async fn dispatch(argv: Vec<OsString>, opts: &ContextOptions) -> anyhow::Result<ExitCode> {
    // Initialize a minimal subscriber here because this path bypasses
    // `Context::try_init` (which normally sets up logging). Errors returned
    // from `dispatch` are then visible at the `main.rs` `log::error!` boundary.
    // `.ok()` is correct here: a second `ocx` invocation inside a plugin would
    // find the global subscriber already set; silencing that error is intentional.
    LogSettings::default().with_console_level(opts.log_level).init().ok();

    // Rewrite `[help, X]` → `[X, --help]` before dispatch so `ocx help foo`
    // produces `ocx-foo --help` rather than looking for `ocx-help`.
    let argv = rewrite_help_invocation(argv);

    // clap guarantees argv[0] is the unrecognized subcommand name, but guard
    // defensively so an empty argv produces a clean usage error instead of a panic.
    let Some(name) = argv.first() else {
        return Err(UsageError::new("no subcommand name provided").into());
    };

    // Bare `ocx help` (no subcommand argument) → print top-level help, exit 0.
    // Without this, `disable_help_subcommand = true` on `Cli` routes bare `help`
    // into External; we must NOT then look for `ocx-help` on PATH. Tested by
    // `test_help_survives_invalid_ambient_config` and `test_bare_help_prints_top_level`.
    if argv.len() == 1 && name == "help" {
        crate::app::Cli::command().print_help()?;
        // clap's print_help() omits trailing newline
        println!();
        return Ok(ExitCode::SUCCESS);
    }

    // After `rewrite_help_invocation`, `[help, X]` becomes `[X, --help]`.
    // If X is a real built-in subcommand, route to clap's help printer instead
    // of dispatching to a non-existent `ocx-<builtin>` binary.  Without this,
    // `ocx help package` would print a misleading install hint for a built-in
    // that ships in core.  External subcommands registered via
    // `#[command(external_subcommand)]` are not findable by `find_subcommand`,
    // so they correctly fall through to the plugin path below.
    let name_str = name.to_string_lossy();
    if argv.get(1).map(|a| a == "--help").unwrap_or(false)
        && let Some(mut sub) = crate::app::Cli::command().find_subcommand(name_str.as_ref()).cloned()
    {
        sub.print_help()?;
        // clap's print_help() omits trailing newline
        println!();
        return Ok(ExitCode::SUCCESS);
    }

    // Resolve `ocx-<name>` on PATH, surfacing a user-friendly install hint on failure.
    // `which::which` is sync filesystem I/O; move it off the async executor.
    let bin_name = format!("ocx-{name_str}");
    let binary = tokio::task::spawn_blocking(move || which::which(bin_name))
        .await?
        .ok()
        .ok_or_else(|| {
            UsageError::new(format!(
                "unknown subcommand '{name_str}'; install with: ocx --global add ocx-sh/ocx-{name_str}"
            ))
        })?;

    let mut cmd = build_plugin_command(&binary, &argv, opts)?;
    let status = cmd.status().await?;
    Ok(propagate_exit_code(status))
}

/// Rewrite `[help, X]` → `[X, --help]`; otherwise return `argv` unchanged.
///
/// Without this, `ocx help foo` produces `External(["help", "foo"])` which
/// would dispatch to `ocx-help foo` — wrong. This rewrites it to dispatch
/// `ocx-foo --help` instead.
fn rewrite_help_invocation(argv: Vec<OsString>) -> Vec<OsString> {
    if argv.len() == 2 && argv[0] == "help" {
        vec![argv[1].clone(), OsString::from("--help")]
    } else {
        argv
    }
}

/// Build a `tokio::process::Command` for the resolved plugin binary.
///
/// Sets argv[0]=binary, argv[1]=subcommand name, argv[2..]=rest (cargo
/// convention). Applies `Env::apply_ocx_config` for resolution-affecting
/// environment forwarding. Inherits stdio.
fn build_plugin_command(
    binary: &Path,
    argv: &[OsString],
    opts: &ContextOptions,
) -> anyhow::Result<tokio::process::Command> {
    // Capture the absolute path of the running ocx so any child `ocx`
    // invocations inside the plugin pin to the same binary via OCX_BINARY_PIN.
    // Degrading to "ocx" on failure matches Context::try_init's pattern: the
    // child launcher's `${OCX_BINARY_PIN:-ocx}` form then falls back to PATH lookup.
    let self_exe = std::env::current_exe().unwrap_or_else(|e| {
        log::warn!("could not resolve current exe: {e}");
        PathBuf::from("ocx")
    });
    let config_view = opts.as_view(self_exe);

    // Forward the running ocx's resolution-affecting config to the plugin so
    // any child `ocx` invocations inside the plugin see the same policy flags
    // (offline, remote, config, index, binary pin).
    let mut env = Env::new();
    env.apply_ocx_config(&config_view);

    let mut cmd = tokio::process::Command::new(binary);
    // Cargo convention: argv[1] = subcommand name, argv[2..] = rest. The OS
    // sets argv[0] = binary path automatically via Command::new, so we push
    // argv[0] (name) as the first explicit arg, then the remainder.
    cmd.args(argv);
    cmd.envs(env);

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osvec(items: &[&str]) -> Vec<OsString> {
        items.iter().map(|s| OsString::from(*s)).collect()
    }

    /// `[help, X]` rewrites to `[X, --help]` so `ocx help foo` dispatches to
    /// `ocx-foo --help` instead of `ocx-help foo`. Ref: plan Step 3.3 case 1.
    #[test]
    fn rewrites_help_followed_by_name() {
        let input = osvec(&["help", "foo"]);
        let output = rewrite_help_invocation(input);
        assert_eq!(output, osvec(&["foo", "--help"]));
    }

    /// `[help]` alone has no target name to rewrite to; pass through unchanged.
    /// Ref: plan Step 3.3 case 2.
    #[test]
    fn passes_through_help_alone() {
        let input = osvec(&["help"]);
        assert_eq!(rewrite_help_invocation(input.clone()), input);
    }

    /// `[help, foo, bar]` has extra args beyond the two-element form; pass through
    /// unchanged so the caller can handle multi-arg help forms separately.
    /// Ref: plan Step 3.3 case 3.
    #[test]
    fn passes_through_help_with_extra_args() {
        let input = osvec(&["help", "foo", "bar"]);
        assert_eq!(rewrite_help_invocation(input.clone()), input);
    }

    /// Argv whose first element is not `"help"` passes through unchanged.
    /// Ref: plan Step 3.3 case 4.
    #[test]
    fn passes_through_non_help() {
        let input = osvec(&["mirror", "--flag"]);
        assert_eq!(rewrite_help_invocation(input.clone()), input);
    }
}
