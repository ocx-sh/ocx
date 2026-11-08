// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Git-style plugin dispatch: `ocx <name> args` runs `ocx-<name> args` from PATH, before
//! `Context::try_init`. See `.claude/artifacts/adr_cli_plugin_pattern.md`.
//!
//! Plugins inherit the parent env like cargo/git plugins, minus bearer credentials, through the
//! one child-environment projection in `Layer` mode with nothing composed. The dispatching
//! parent's narrowing and root flags travel in `__OCX_ENV_STATE`, not as public `OCX_*`
//! variables, so a plugin's own `ocx` child honours them. `ocx exec` and `ocx package exec` run
//! with a composed env of their own.

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use crate::error::UsageError;
use clap::CommandFactory as _;

use crate::tracing_init::LogSettings;
use ocx_config::exec_state::{Inherited, Owner, Selection, WriteMode};
use ocx_package_manager::exec_env::{ExecMode, Spawn, project};
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

    // Plugin dispatch runs before `Context`, so the pin is resolved here: once, off the async threads.
    let pin = tokio::task::spawn_blocking(plugin_binary_pin).await?;

    let mut cmd = build_plugin_command(&binary, &argv, opts, pin.as_deref())?;
    let status = cmd.status().await?;
    Ok(propagate_exit_code(status))
}

/// The `OCX_BINARY_PIN` for the plugin's children, from the home this process resolves.
///
/// A plugin needs no home to run, so an unresolvable one pins nothing instead of failing the dispatch.
fn plugin_binary_pin() -> Option<std::path::PathBuf> {
    let file_structure =
        ocx_config::home::default_ocx_root().map(ocx_store::file_structure::FileStructure::with_root)?;
    let running = std::env::current_exe()
        .inspect_err(|e| log::warn!("could not resolve current exe: {e}"))
        .ok();
    ocx_package_manager::launcher::binary_pin_value(&file_structure, running.as_deref())
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
/// re-passed name), the ambient environment minus bearer credentials, and the execution state the
/// plugin's own `ocx` children inherit.
#[expect(
    clippy::disallowed_types,
    reason = "git-style `ocx-<name>` plugin dispatch; an ocx extension, not a resolved package tool or a recording frame"
)]
fn build_plugin_command(
    binary: &Path,
    argv: &[OsString],
    opts: &ContextOptions,
    pin: Option<&Path>,
) -> anyhow::Result<tokio::process::Command> {
    let inherited = Inherited::from_process_env();
    let written = super::context::with_policy_inputs(&opts.root_flags(), false, &inherited, |inputs| {
        inputs.to_write(WriteMode::Inherit)
    });
    let child = project(
        &inherited,
        &Spawn {
            mode: ExecMode::Layer,
            written: &written,
            binary_pin: pin,
            composed: &[],
            composed_owner: Owner::Inherited,
            composed_scope: None,
            invocation: &[],
            replay_env: &[],
            launch_identities: &[],
            selection: Selection::FollowCwd,
        },
    );
    super::report_projection_warnings(&child.warnings);

    let mut cmd = tokio::process::Command::new(binary);
    // Re-passing the name (cargo style) makes a plain clap plugin reject it as an unknown subcommand.
    cmd.args(argv.get(1..).unwrap_or(&[]));
    // The projection already removed every credential; clearing keeps the child to exactly that environment.
    cmd.env_clear().envs(child.env);

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

    /// A plugin needs no home: with none resolvable the dispatch pins nothing and does not panic.
    #[test]
    fn plugin_dispatch_pins_nothing_when_no_home_resolves() {
        let env = ocx_env::overrides::lock();
        env.hermetic(std::env::temp_dir());
        env.remove(&ocx_env::OCX_HOME);
        env.remove(&ocx_env::__OCX_ENV_STATE);
        env.remove(&ocx_env::HOME);
        assert_eq!(
            ocx_config::home::default_ocx_root(),
            None,
            "the fixture resolves no home"
        );
        assert_eq!(plugin_binary_pin(), None);
    }

    /// Regression: git-style dispatch drops the subcommand name so the plugin
    /// receives only the user args. `ocx help mirror` rewrites to
    /// `["mirror", "--help"]`, which must reach `ocx-mirror` as `["--help"]`
    /// (i.e. `ocx-mirror --help`), NOT `ocx-mirror mirror --help` (cargo style),
    /// which clap rejects because `ocx-mirror` has no `mirror` subcommand.
    #[test]
    fn plugin_command_drops_subcommand_name() {
        let argv = osvec(&["mirror", "--help"]);
        let cmd = build_plugin_command(Path::new("/usr/bin/ocx-mirror"), &argv, &opts(), None).unwrap();
        let args: Vec<_> = cmd.as_std().get_args().collect();
        assert_eq!(args, vec![std::ffi::OsStr::new("--help")]);
    }

    /// `ocx mirror sync --exact-version` forwards every user arg after the name.
    #[test]
    fn plugin_command_forwards_user_args_after_name() {
        let argv = osvec(&["mirror", "sync", "--exact-version"]);
        let cmd = build_plugin_command(Path::new("/usr/bin/ocx-mirror"), &argv, &opts(), None).unwrap();
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
        let cmd = build_plugin_command(Path::new("/usr/bin/ocx-mirror"), &argv, &opts(), None).unwrap();
        assert_eq!(cmd.as_std().get_args().count(), 0);
    }

    /// A plugin is third-party code launched with the ambient environment, so the one thing that
    /// must not ride along is a bearer credential. The command runs on exactly the projected
    /// environment (`env_clear`), and the projection holds none of them.
    #[test]
    fn plugin_command_carries_no_credential_key() {
        let env = ocx_env::overrides::lock();
        let credentials: Vec<&'static ocx_env::EnvVar> = ocx_env::all()
            .filter(|var| var.propagation == ocx_env::Propagation::Credential)
            .collect();
        assert!(
            !credentials.is_empty(),
            "an empty credential list would make the loop below vacuous"
        );
        for credential in &credentials {
            env.set(credential, "fixture-secret-value");
        }
        let argv = osvec(&["mirror"]);
        let cmd = build_plugin_command(Path::new("/usr/bin/ocx-mirror"), &argv, &opts(), None).unwrap();
        let carried: Vec<_> = cmd
            .as_std()
            .get_envs()
            .filter(|(_, value)| value.is_some())
            .map(|(key, _)| key.to_string_lossy().into_owned())
            .collect();
        assert!(!carried.is_empty(), "the projected environment must not be empty");
        for credential in credentials {
            assert!(
                !carried.iter().any(|key| key == credential.name),
                "{} reaches the plugin",
                credential.name
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
