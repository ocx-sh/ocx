// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use ocx_lib::{env, oci, shell::Shell};

use crate::conventions::resolve_shell_arg;

/// Emit shell activation lines for the current OCX installation.
///
/// Prints eval-safe shell lines to stdout that:
/// - Prepend the absolute resolved `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin`
///   path to `PATH`.
/// - Inject shell completions (unless `OCX_NO_COMPLETIONS=1`).
/// - Evaluate the global toolchain env (`ocx --global env --shell=NAME`).
///
/// Intended to be sourced from `$OCX_HOME/env.sh` at shell startup.
/// `env.sh` sets `OCX_HOME` before invoking `ocx self activate`, so the
/// absolute path is resolved from the binary's perspective — no shell-level
/// `$OCX_HOME` variable reference is emitted.
///
/// ```sh
/// if command -v ocx >/dev/null 2>&1; then
///     eval "$(ocx self activate --shell=sh)"
/// fi
/// ```
#[derive(Parser)]
pub struct SelfActivate {
    /// Target shell for activation output.
    ///
    /// Legal values: `bash`, `zsh`, `fish`, `ash`, `dash`, `ksh`, `sh`,
    /// `pwsh`, `elvish`, `nushell`, `batch` (`sh` ≡ `dash`, POSIX alias).
    ///
    /// Must be supplied with `=` (`--shell=bash`). Bare `--shell` (no `=`)
    /// triggers autodetection from `$SHELL`/parent process; exit 64 if
    /// undetectable — pass `--shell=NAME` explicitly to override.
    ///
    /// When absent, defaults to autodetect (same as bare `--shell`).
    #[arg(
        long,
        value_enum,
        value_name = "SHELL",
        num_args = 0..=1,
        require_equals = true
    )]
    shell: Option<Option<Shell>>,
}

impl SelfActivate {
    /// Context-free execution path — called from `app.rs` before `Context::try_init`.
    ///
    /// `self activate` runs on every shell startup and must not pay the full
    /// `Context::try_init` cost (ConfigLoader file walk, OCI client, RemoteIndex,
    /// PackageManager). It only needs a `FileStructure` to resolve the absolute
    /// `$OCX_HOME/symlinks/…/bin` path. `FileStructure::new()` reads `OCX_HOME`
    /// from the environment and is cheap to construct — no I/O beyond the env
    /// lookup.
    pub async fn execute(&self) -> anyhow::Result<ExitCode> {
        // Resolve the target shell.  Bare `--shell` (Some(None)) or absent
        // (None) both trigger autodetect; explicit `--shell=bash` (Some(Some))
        // is passed through unchanged.
        //
        // The `self activate` bare-absent case differs from `ocx env`/`ocx
        // package env` where absent means "use default format path".  Here,
        // absent means autodetect (documented in the struct doc comment).
        //
        // `self.shell.or(Some(None))` collapses both "absent" and "bare" into
        // `Some(_)`, so `resolve_shell_arg` always returns `Some` here. The
        // `.expect()` documents that invariant — `unreachable!()` masked it as
        // an arm of the outer match (Q-W3).
        let shell_arg = self.shell.or(Some(None));
        let shell = resolve_shell_arg(shell_arg)?
            .expect("resolve_shell_arg(Some(_)) always returns Some; see shell_arg remap above");

        // Resolve the absolute install bin path from the OCX CLI symlink.
        // `env.sh` guarantees OCX_HOME is set before running `ocx self activate`,
        // so `FileStructure::new()` already knows the correct root via OCX_HOME.
        // Constructing FileStructure directly avoids the full Context::try_init
        // overhead (ConfigLoader, OCI client, PackageManager) on every shell startup.
        let file_structure = ocx_lib::file_structure::FileStructure::new();
        let bin_path = ocx_install_bin_path(&file_structure);

        emit_activation(shell, &bin_path);
        Ok(ExitCode::SUCCESS)
    }
}

/// Returns the absolute path to the OCX CLI binary directory.
///
/// Resolves `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin` using the
/// file structure's symlink store.  The path is derived from the runtime-known
/// `OCX_HOME`, not from a shell variable reference.
fn ocx_install_bin_path(fs: &ocx_lib::file_structure::FileStructure) -> PathBuf {
    let ocx_cli_id = oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY);
    fs.symlinks.current(&ocx_cli_id).join("content").join("bin")
}

/// Emit all activation lines to stdout for the given shell.
fn emit_activation(shell: Shell, bin_path: &std::path::Path) {
    // ── PATH prepend ─────────────────────────────────────────────────────────
    // Use the absolute resolved path — no $VAR references, so Shell::export_path
    // is safe here (escape_value won't transform the path characters since
    // they contain no shell metacharacters).
    emit_path_prepend(shell, bin_path);

    // ── Shell completions ────────────────────────────────────────────────────
    // Omitted when OCX_NO_COMPLETIONS=1 (opt-out).
    if !env::flag("OCX_NO_COMPLETIONS", false) {
        emit_completions(shell);
    }

    // ── Global toolchain env (guarded) ───────────────────────────────────────
    // Evaluate the global toolchain env once per shell — re-sourcing the user's
    // shell profile must not re-run the expensive `ocx --global env` subprocess
    // (SOTA-W3, mirrors mise's `MISE_SHELL` guard).  The guard line and the
    // marker line are emitted together so a re-source becomes a cheap no-op.
    emit_global_env_eval(shell);
    emit_activated_marker(shell);
}

/// Emit the PATH prepend line for the given shell using an absolute `bin_path`.
///
/// Uses [`Shell::export_path`] since the value is an absolute filesystem path
/// containing no shell variable references or metacharacters.
fn emit_path_prepend(shell: Shell, bin_path: &std::path::Path) {
    let path_str = bin_path.to_string_lossy();
    if let Some(line) = shell.export_path("PATH", path_str.as_ref()) {
        println!("{line}");
    }
}

/// Emit shell completion script for the given shell (if supported).
fn emit_completions(shell: Shell) {
    // Map `ocx_lib::shell::Shell` → `clap_complete::Shell`.  Not all shells
    // supported by OCX have clap_complete support; silently skip unsupported
    // ones.
    let clap_shell: clap_complete::Shell = match shell.try_into() {
        Ok(s) => s,
        Err(_) => return, // shell has no clap_complete backend — skip silently
    };
    let mut cmd = crate::app::Cli::command();
    let cmd_name = cmd.get_name().to_string();
    clap_complete::generate(clap_shell, &mut cmd, cmd_name, &mut std::io::stdout());
}

/// Emit the eval line that loads the global toolchain env, gated on
/// `OCX_ACTIVATED` so re-sourcing the user's shell profile becomes a no-op.
fn emit_global_env_eval(shell: Shell) {
    println!("{}", format_global_env_eval(shell));
}

/// Emit the marker line that sets `OCX_ACTIVATED=1`.
fn emit_activated_marker(shell: Shell) {
    println!("{}", format_activated_marker(shell));
}

/// Format the gated global-env-eval line for `shell` without emitting.
///
/// Mirrors the `MISE_SHELL` double-activation guard pattern. The marker is set
/// separately by [`format_activated_marker`] after this line so a fresh shell
/// runs the eval exactly once and a re-source becomes a cheap no-op.
fn format_global_env_eval(shell: Shell) -> String {
    let shell_name = shell_name_for_eval(shell);
    match shell {
        Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Bash | Shell::Zsh => format!(
            r#"if [ -z "${{OCX_ACTIVATED:-}}" ] && command -v ocx >/dev/null 2>&1; then eval "$(ocx --global env --shell={shell_name})"; fi"#
        ),
        Shell::Fish => {
            format!("if not set -q OCX_ACTIVATED; and type -q ocx; ocx --global env --shell={shell_name} | source; end")
        }
        Shell::PowerShell => format!(
            "if (-not $env:OCX_ACTIVATED -and (Get-Command ocx -ErrorAction SilentlyContinue)) {{ Invoke-Expression (& ocx --global env --shell={shell_name}) }}"
        ),
        Shell::Batch => {
            // CMD: skip if OCX_ACTIVATED is already set. `FOR /F` evaluates each
            // line of the subprocess output.
            format!(
                "if not defined OCX_ACTIVATED FOR /F \"usebackq\" %i IN (`ocx --global env --shell={shell_name}`) DO @%i"
            )
        }
        Shell::Elvish => format!(
            "if (and (not (has-env OCX_ACTIVATED)) (has-external ocx)) {{ ocx --global env --shell={shell_name} | slurp | eval }}"
        ),
        Shell::Nushell => format!(
            "if ($env.OCX_ACTIVATED? == null) and (which ocx | length) > 0 {{ ocx --global env --shell={shell_name} | nu -c $in }}"
        ),
    }
}

/// Format the per-shell line that sets the `OCX_ACTIVATED` marker.
///
/// Always emitted after [`format_global_env_eval`] — so the first activation
/// runs the eval and then sets the marker, while a re-source skips the eval
/// (its `OCX_ACTIVATED` guard now sees the marker) and only re-sets the marker
/// (idempotent).
fn format_activated_marker(shell: Shell) -> String {
    match shell {
        Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Bash | Shell::Zsh => "export OCX_ACTIVATED=1".to_string(),
        Shell::Fish => "set -gx OCX_ACTIVATED 1".to_string(),
        Shell::PowerShell => r#"$env:OCX_ACTIVATED = "1""#.to_string(),
        Shell::Batch => "set OCX_ACTIVATED=1".to_string(),
        Shell::Elvish => "set-env OCX_ACTIVATED 1".to_string(),
        Shell::Nushell => "$env.OCX_ACTIVATED = '1'".to_string(),
    }
}

/// Returns the short shell name suitable for use in `--shell=NAME` arguments.
fn shell_name_for_eval(shell: Shell) -> &'static str {
    match shell {
        Shell::Ash => "ash",
        Shell::Ksh => "ksh",
        Shell::Dash => "sh",
        Shell::Bash => "bash",
        Shell::Elvish => "elvish",
        Shell::Fish => "fish",
        Shell::Batch => "batch",
        Shell::PowerShell => "pwsh",
        Shell::Zsh => "zsh",
        Shell::Nushell => "nushell",
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use ocx_lib::shell::Shell;

    use super::{emit_path_prepend, format_activated_marker, format_global_env_eval, ocx_install_bin_path};

    // ── PATH prepend: absolute path via Shell::export_path ──────────────────
    //
    // `emit_path_prepend` must use the resolved absolute path — no `$VAR`
    // references in the value. The emitted line must contain the absolute path
    // string and NOT contain `${OCX_HOME}` or `%OCX_HOME%`.

    #[test]
    fn path_prepend_bash_contains_absolute_path() {
        let path = PathBuf::from("/tmp/known/.ocx/symlinks/ocx_sh/ocx/cli/current/content/bin");
        // Use Shell::export_path directly to mirror what emit_path_prepend emits.
        let line = Shell::Bash
            .export_path("PATH", path.to_string_lossy().as_ref())
            .expect("valid env-var name");
        assert!(
            line.contains("/tmp/known"),
            "PATH prepend must contain the absolute path; got: {line:?}"
        );
        assert!(
            !line.contains("${OCX_HOME}"),
            "PATH prepend must not contain ${{OCX_HOME}} variable reference; got: {line:?}"
        );
        assert!(
            !line.contains("%OCX_HOME%"),
            "PATH prepend must not contain %OCX_HOME% variable reference; got: {line:?}"
        );
    }

    #[test]
    fn path_prepend_batch_absolute_path() {
        let path = PathBuf::from(r"C:\Users\test\.ocx\symlinks\ocx_sh\ocx\cli\current\content\bin");
        let line = Shell::Batch
            .export_path("PATH", path.to_string_lossy().as_ref())
            .expect("valid env-var name");
        assert!(
            line.contains("ocx_sh"),
            "CMD PATH prepend must contain the absolute path; got: {line:?}"
        );
        assert!(
            !line.contains("%OCX_HOME%"),
            "CMD PATH prepend must not contain %OCX_HOME% variable reference; got: {line:?}"
        );
    }

    /// Smoke-test that `emit_path_prepend` does not panic for any shell variant.
    #[test]
    fn emit_path_prepend_does_not_panic_for_any_shell() {
        let path = PathBuf::from("/tmp/known/.ocx/symlinks/ocx_sh/ocx/cli/current/content/bin");
        let shells = [
            Shell::Ash,
            Shell::Ksh,
            Shell::Dash,
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Nushell,
            Shell::Elvish,
            Shell::Batch,
        ];
        for shell in shells {
            let path_clone = path.clone();
            std::thread::spawn(move || emit_path_prepend(shell, &path_clone))
                .join()
                .unwrap_or_else(|_| panic!("emit_path_prepend panicked for {shell:?}"));
        }
    }

    // ── OCX_ACTIVATED double-activation guard (SOTA-W3) ─────────────────────

    /// POSIX shells must guard the eval on `OCX_ACTIVATED` and probe `ocx`
    /// existence. Removing the guard turns every re-source into a redundant
    /// `ocx --global env` subprocess.
    #[test]
    fn global_env_eval_guards_posix_shells_on_ocx_activated() {
        for shell in [Shell::Ash, Shell::Ksh, Shell::Dash, Shell::Bash, Shell::Zsh] {
            let line = format_global_env_eval(shell);
            assert!(
                line.contains(r#"[ -z "${OCX_ACTIVATED:-}" ]"#),
                "{shell:?} eval line must check OCX_ACTIVATED is unset; got: {line:?}"
            );
            assert!(
                line.contains("command -v ocx"),
                "{shell:?} eval line must probe ocx via command -v; got: {line:?}"
            );
            assert!(
                line.contains("ocx --global env --shell="),
                "{shell:?} eval line must invoke ocx --global env; got: {line:?}"
            );
        }
    }

    /// Fish must use `not set -q OCX_ACTIVATED` to gate the eval.
    #[test]
    fn global_env_eval_guards_fish_on_ocx_activated() {
        let line = format_global_env_eval(Shell::Fish);
        assert!(
            line.contains("not set -q OCX_ACTIVATED"),
            "fish eval line must check OCX_ACTIVATED is unset; got: {line:?}"
        );
        assert!(
            line.contains("type -q ocx"),
            "fish eval line must probe ocx via type -q; got: {line:?}"
        );
    }

    /// PowerShell must use `-not $env:OCX_ACTIVATED` to gate the eval.
    #[test]
    fn global_env_eval_guards_powershell_on_ocx_activated() {
        let line = format_global_env_eval(Shell::PowerShell);
        assert!(
            line.contains("-not $env:OCX_ACTIVATED"),
            "pwsh eval line must check $env:OCX_ACTIVATED is falsy; got: {line:?}"
        );
        assert!(
            line.contains("Get-Command ocx"),
            "pwsh eval line must probe ocx via Get-Command; got: {line:?}"
        );
    }

    /// CMD must use `if not defined OCX_ACTIVATED` to gate the eval.
    #[test]
    fn global_env_eval_guards_batch_on_ocx_activated() {
        let line = format_global_env_eval(Shell::Batch);
        assert!(
            line.contains("if not defined OCX_ACTIVATED"),
            "CMD eval line must check OCX_ACTIVATED is undefined; got: {line:?}"
        );
    }

    /// Elvish + Nushell each emit an env-existence guard.
    #[test]
    fn global_env_eval_guards_elvish_nushell_on_ocx_activated() {
        let line_elv = format_global_env_eval(Shell::Elvish);
        assert!(
            line_elv.contains("not (has-env OCX_ACTIVATED)"),
            "elvish eval line must check OCX_ACTIVATED via has-env; got: {line_elv:?}"
        );

        let line_nu = format_global_env_eval(Shell::Nushell);
        assert!(
            line_nu.contains("$env.OCX_ACTIVATED? == null"),
            "nushell eval line must check $env.OCX_ACTIVATED is null; got: {line_nu:?}"
        );
    }

    /// Every shell must emit a marker line that sets `OCX_ACTIVATED=1` — the
    /// re-source no-op contract relies on it. New shell variants must be added
    /// here in the same commit that adds them to the `Shell` enum.
    #[test]
    fn activated_marker_emitted_for_every_shell() {
        let shells = [
            Shell::Ash,
            Shell::Ksh,
            Shell::Dash,
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Batch,
            Shell::Elvish,
            Shell::Nushell,
        ];
        for shell in shells {
            let line = format_activated_marker(shell);
            assert!(
                line.contains("OCX_ACTIVATED"),
                "{shell:?} marker must reference OCX_ACTIVATED; got: {line:?}"
            );
            assert!(
                line.contains('1'),
                "{shell:?} marker must set the value to 1; got: {line:?}"
            );
        }
    }

    /// `ocx_install_bin_path` must return a path ending with `current/content/bin`.
    #[test]
    fn ocx_install_bin_path_structure() {
        use ocx_lib::file_structure::FileStructure;
        let fs = FileStructure::with_root(PathBuf::from("/tmp/ocx_home"));
        let bin_path = ocx_install_bin_path(&fs);
        assert!(
            bin_path.ends_with(Path::new("current/content/bin")),
            "bin path must end with current/content/bin; got: {bin_path:?}"
        );
        assert!(
            bin_path.starts_with("/tmp/ocx_home/symlinks"),
            "bin path must be rooted under $OCX_HOME/symlinks; got: {bin_path:?}"
        );
    }
}
