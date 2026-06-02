// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Git-style plugin dispatch for `ocx <name>` → `ocx-<name>` on PATH.
//!
//! "Git-style" specifically in the argument convention: the subcommand name is
//! dropped, not re-passed (`ocx mirror sync` → `ocx-mirror sync`), so a plugin
//! is just a normal CLI. See `build_plugin_command`.
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
        .ok_or_else(|| UsageError::new(unknown_subcommand_hint(&name_str)))?;

    let mut cmd = build_plugin_command(&binary, &argv, opts)?;
    let status = cmd.status().await?;
    Ok(propagate_exit_code(status))
}

/// Build the not-found hint shown when `ocx <name>` matches no built-in and no
/// `ocx-<name>` binary is on PATH.
///
/// Deliberately does not over-promise: only first-party plugins are published
/// at `ocx.sh/ocx/<name>`, so the `add` form is framed as the official-plugin
/// path, with the generic PATH convention as the fallback for third-party
/// plugins. The OCI registry is `ocx.sh` and the identifier is three-segment
/// (`ocx.sh/ocx/<name>`) — distinct from the `ocx-sh` GitHub org slug and from
/// the `ocx-<name>` binary name.
fn unknown_subcommand_hint(name: &str) -> String {
    format!(
        "unknown subcommand '{name}'; if `ocx-{name}` is an official OCX plugin, \
         install it with `ocx --global add ocx.sh/ocx/{name}`, otherwise put an \
         `ocx-{name}` binary on your PATH"
    )
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
/// Git-style argument convention: the subcommand name (`argv[0]`) is dropped,
/// so the plugin receives only the user args (`argv[1..]`). `ocx mirror sync`
/// and `ocx help mirror` reach `ocx-mirror` as `sync` and `--help` — identical
/// to running the standalone binary directly. (Cargo re-passes the name as
/// `argv[1]`; OCX deliberately does not, so a plain clap plugin like
/// `ocx-mirror` needs no `mirror` wrapper subcommand to avoid rejecting its own
/// name.) Applies `Env::apply_ocx_config` for resolution-affecting environment
/// forwarding. Inherits stdio.
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
    // Git convention: drop the subcommand name (argv[0]) and forward only the
    // user args (argv[1..]). `Command::new` sets the OS argv[0] to the binary
    // path, so the plugin sees `ocx-mirror <user args>` — identical to a direct
    // invocation. Re-passing the name (cargo style) would make a plain clap
    // plugin reject its own name as an unrecognized subcommand. `get(1..)`
    // tolerates an empty argv defensively (the caller guarantees argv[0]).
    cmd.args(argv.get(1..).unwrap_or(&[]));
    cmd.envs(env);

    Ok(cmd)
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    fn osvec(items: &[&str]) -> Vec<OsString> {
        items.iter().map(|s| OsString::from(*s)).collect()
    }

    fn opts() -> ContextOptions {
        ContextOptions::parse_from(["ocx"])
    }

    /// Regression: git-style dispatch drops the subcommand name so the plugin
    /// receives only the user args. `ocx help mirror` rewrites to
    /// `["mirror", "--help"]`, which must reach `ocx-mirror` as `["--help"]`
    /// (i.e. `ocx-mirror --help`), NOT `ocx-mirror mirror --help` (cargo style),
    /// which clap rejects because `ocx-mirror` has no `mirror` subcommand.
    #[test]
    fn plugin_command_drops_subcommand_name() {
        let argv = osvec(&["mirror", "--help"]);
        let cmd = build_plugin_command(Path::new("/usr/bin/ocx-mirror"), &argv, &opts()).unwrap();
        let args: Vec<_> = cmd.as_std().get_args().collect();
        assert_eq!(args, vec![std::ffi::OsStr::new("--help")]);
    }

    /// `ocx mirror sync --exact-version` forwards every user arg after the name.
    #[test]
    fn plugin_command_forwards_user_args_after_name() {
        let argv = osvec(&["mirror", "sync", "--exact-version"]);
        let cmd = build_plugin_command(Path::new("/usr/bin/ocx-mirror"), &argv, &opts()).unwrap();
        let args: Vec<_> = cmd.as_std().get_args().collect();
        assert_eq!(
            args,
            vec![std::ffi::OsStr::new("sync"), std::ffi::OsStr::new("--exact-version")]
        );
    }

    /// Bare `ocx mirror` forwards zero args; the plugin then prints its own
    /// missing-subcommand help, identical to running `ocx-mirror` directly.
    #[test]
    fn plugin_command_bare_name_forwards_no_args() {
        let argv = osvec(&["mirror"]);
        let cmd = build_plugin_command(Path::new("/usr/bin/ocx-mirror"), &argv, &opts()).unwrap();
        assert_eq!(cmd.as_std().get_args().count(), 0);
    }

    /// Regression: the not-found hint uses the three-segment OCI registry form
    /// `ocx.sh/ocx/<name>`, never the `ocx-sh` GitHub org slug or the old wrong
    /// `ocx-sh/ocx-<name>` identifier.
    #[test]
    fn unknown_subcommand_hint_uses_registry_identifier() {
        let hint = unknown_subcommand_hint("mirror");
        assert!(hint.contains("ocx.sh/ocx/mirror"), "hint = {hint}");
        assert!(!hint.contains("ocx-sh"), "must not use github-org slug: {hint}");
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
