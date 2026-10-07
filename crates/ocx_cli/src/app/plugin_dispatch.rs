// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Git-style plugin dispatch: `ocx <name> args` runs `ocx-<name> args` from PATH, before
//! `Context::try_init`. See `.claude/artifacts/adr_cli_plugin_pattern.md`.
//!
//! Plugins inherit the parent env like cargo/git plugins, minus bearer credentials;
//! `ocx exec` and `ocx package exec` run with a clean env.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::error::UsageError;
use clap::CommandFactory as _;

use crate::tracing_init::LogSettings;
use ocx_config::env::Env;
use ocx_util::child_process::propagate_exit_code;

use crate::app::context_options::ContextOptions;

/// Runs the plugin for `Command::External(argv)`, `argv[0]` being the unrecognized
/// subcommand name, and propagates its exit status.
pub async fn dispatch(argv: Vec<OsString>, opts: &ContextOptions) -> anyhow::Result<ExitCode> {
    // This path skips `Context::try_init`'s logging setup; without it `app::finish` logs returned errors nowhere.
    LogSettings::default()
        .with_console_level(opts.log_level)
        .with_stderr_color(opts.color.config().stderr)
        .init()
        .ok();

    let argv = rewrite_help_invocation(argv);

    // An empty argv must be a usage error, never a panic.
    let Some(name) = argv.first() else {
        return Err(UsageError::new("no subcommand name provided").into());
    };

    // `disable_help_subcommand` routes bare `ocx help` here; print top-level help, never look up `ocx-help`.
    if argv.len() == 1 && name == "help" {
        crate::app::Cli::command().print_help()?;
        // clap's print_help() omits trailing newline
        println!();
        return Ok(ExitCode::SUCCESS);
    }

    // `ocx help <built-in>` prints clap help, not an install hint for a missing `ocx-<built-in>`.
    let name_str = name.to_string_lossy();
    if argv.get(1).map(|a| a == "--help").unwrap_or(false)
        && let Some(mut sub) = crate::app::Cli::command().find_subcommand(&*name_str).cloned()
    {
        sub.print_help()?;
        // clap's print_help() omits trailing newline
        println!();
        return Ok(ExitCode::SUCCESS);
    }

    let bin_name = format!("ocx-{name_str}");
    let binary = tokio::task::spawn_blocking(move || which::which(bin_name))
        .await?
        .ok()
        .ok_or_else(|| UsageError::new(unknown_subcommand_hint(&name_str)))?;

    let mut cmd = build_plugin_command(&binary, &argv, opts)?;
    let status = cmd.status().await?;
    Ok(propagate_exit_code(status))
}

/// The hint shown when `ocx <name>` matches no built-in and no `ocx-<name>` binary is on PATH.
fn unknown_subcommand_hint(name: &str) -> String {
    format!(
        "unknown subcommand '{name}'; if `ocx-{name}` is an official OCX plugin, \
         install it with `ocx --global add ocx.sh/ocx/{name}`, otherwise put an \
         `ocx-{name}` binary on your PATH"
    )
}

/// Rewrites `[help, X]` to `[X, --help]`, so `ocx help foo` runs `ocx-foo --help`, not `ocx-help foo`.
fn rewrite_help_invocation(argv: Vec<OsString>) -> Vec<OsString> {
    if argv.len() == 2 && argv[0] == "help" {
        vec![argv[1].clone(), OsString::from("--help")]
    } else {
        argv
    }
}

/// Builds the plugin command: the user args after the subcommand name (git style, not cargo's
/// re-passed name), resolution-affecting config forwarded, bearer credentials removed.
#[expect(
    clippy::disallowed_types,
    reason = "git-style `ocx-<name>` plugin dispatch; an ocx extension, not a resolved package tool or a recording frame"
)]
fn build_plugin_command(
    binary: &Path,
    argv: &[OsString],
    opts: &ContextOptions,
) -> anyhow::Result<tokio::process::Command> {
    // Pins child `ocx` calls inside the plugin to this binary via `OCX_BINARY_PIN`; on failure they fall back to PATH.
    let self_exe = std::env::current_exe().unwrap_or_else(|e| {
        log::warn!("could not resolve current exe: {e}");
        PathBuf::from("ocx")
    });
    let config_view = opts.as_view(self_exe);

    // Full ambient `Env::new()`, not `Env::inherited()`: plugins inherit the parent env (module doc).
    let mut env = Env::new();
    env.apply_ocx_config(&config_view);

    let mut cmd = tokio::process::Command::new(binary);
    // Re-passing the name (cargo style) makes a plain clap plugin reject it as an unknown subcommand.
    cmd.args(argv.get(1..).unwrap_or(&[]));
    cmd.envs(env);
    // `envs` cannot unset an inherited key, so without this a bearer credential reaches the plugin.
    for credential in ocx_env::all().filter(|var| var.child == ocx_env::Child::Scrub) {
        cmd.env_remove(credential.name);
    }

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

    /// A plugin is third-party code launched with the **full** ambient
    /// environment, so the one thing that must not ride along is a bearer
    /// credential. `Command::envs` only adds and overrides — dropping a key
    /// from the map cannot unset one this process inherited — so the removal
    /// has to be an explicit `env_remove`, which surfaces here as a `None`
    /// value in `get_envs()`.
    #[test]
    fn plugin_command_unsets_every_credential_key() {
        let argv = osvec(&["mirror"]);
        let cmd = build_plugin_command(Path::new("/usr/bin/ocx-mirror"), &argv, &opts()).unwrap();
        let removed: Vec<_> = cmd
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        let credentials: Vec<&str> = ocx_env::all()
            .filter(|var| var.child == ocx_env::Child::Scrub)
            .map(|var| var.name)
            .collect();
        assert!(
            !credentials.is_empty(),
            "an empty credential list would make the loop below vacuous"
        );
        for credential in credentials {
            assert!(
                removed.iter().any(|key| key == credential),
                "{credential} reaches the plugin; removed = {removed:?}"
            );
        }
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
