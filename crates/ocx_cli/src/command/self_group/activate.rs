// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use ocx_lib::{oci, shell::Shell};

use crate::conventions::resolve_shell_arg;
use crate::options;

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

    /// Shell-completion injection policy.
    ///
    /// `--completion` forces completions on, `--no-completion` off; with
    /// neither, completions load only for an interactive session (stderr is a
    /// TTY). The `env.sh`/`env.ps1` shim passes the explicit flag based on its
    /// own interactivity check (`$-` / `[Environment]::UserInteractive`), so the
    /// binary never has to probe a stderr the shim may have redirected.
    #[clap(flatten)]
    completion: options::Completion,
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

        // Materialise the shell-completion file when completions are enabled
        // for this session (see `Completion::enabled` / `resolve_completion`).
        // The stderr-TTY probe is the auto signal when neither `--completion`
        // nor `--no-completion` is passed. The returned path, if any, is
        // dot-sourced from the activation stream.
        let load_completions = self.completion.enabled(std::io::stderr().is_terminal());
        let completion = resolve_completion(shell, load_completions, &file_structure).await;

        emit_activation(shell, &bin_path, completion.as_deref());
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
///
/// `completion_file` is the materialised completion script to dot-source, or
/// `None` when completions are disabled, the session is non-interactive, or the
/// shell has no completion backend (see [`resolve_completion`]).
fn emit_activation(shell: Shell, bin_path: &std::path::Path, completion_file: Option<&std::path::Path>) {
    // ── PATH prepend ─────────────────────────────────────────────────────────
    // Use the absolute resolved path — no $VAR references, so Shell::export_path
    // is safe here (escape_value won't transform the path characters since
    // they contain no shell metacharacters).
    emit_path_prepend(shell, bin_path);

    // ── Shell completions ────────────────────────────────────────────────────
    // Source the generated completion file (if any). The script lives in a real
    // file so PowerShell's `using namespace` directives are valid; the line
    // emitted here is just a `source`/`.` directive, safe in every shell.
    if let Some(path) = completion_file {
        println!("{}", completion_source_line(shell, path));
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

/// Materialise the completion file when `enabled` and the shell has a backend.
///
/// Returns the path to dot-source, or `None` when completions are disabled
/// (`enabled` is false) or the shell has no `clap_complete` backend. The
/// `enabled` decision is made upstream by [`options::Completion::enabled`],
/// which folds the `--completion`/`--no-completion` flags, `OCX_NO_COMPLETIONS`,
/// and the stderr-TTY auto probe into a single boolean.
///
/// The completion script is written to a real file (rather than emitted inline)
/// because clap_complete's PowerShell output opens with `using namespace`
/// directives, which PowerShell only accepts as the first statements of a
/// *script file* — not mid-stream under the `Invoke-Expression` that loads
/// activation output. Sourcing a file is uniform across shells and valid on
/// every PowerShell version (5.1 through 7).
async fn resolve_completion(
    shell: Shell,
    enabled: bool,
    file_structure: &ocx_lib::file_structure::FileStructure,
) -> Option<PathBuf> {
    if !enabled {
        return None;
    }
    let (clap_shell, extension) = completion_target(shell)?;
    // A completion-file write failure must never break activation — the
    // PATH/env lines are what matter, so drop the error and skip completions.
    ensure_completion_file(file_structure, clap_shell, extension).await.ok()
}

/// Maps a shell to its `clap_complete` backend and completion-file extension,
/// or `None` when the shell has no completion backend.
fn completion_target(shell: Shell) -> Option<(clap_complete::Shell, &'static str)> {
    use clap_complete::Shell as Clap;
    Some(match shell {
        Shell::Bash => (Clap::Bash, "bash"),
        Shell::Zsh => (Clap::Zsh, "zsh"),
        Shell::Fish => (Clap::Fish, "fish"),
        Shell::Elvish => (Clap::Elvish, "elv"),
        Shell::PowerShell => (Clap::PowerShell, "ps1"),
        // ash/ksh/dash/batch/nushell have no clap_complete backend.
        _ => return None,
    })
}

/// Materialise the version-stamped completion script and return its path.
///
/// The script's first line is `# ocx-completion-version: {version}`. If a file
/// with the current stamp already exists it is reused untouched — so the common
/// path on an interactive shell start is a single small read. Otherwise the
/// script is regenerated and published atomically (PID-suffixed temp file +
/// rename), so concurrent shells never observe a torn file and an
/// `ocx self update` refreshes the script on the next interactive shell.
///
/// The publish goes through `ocx_lib::utility::fs::persist_temp_file` so a
/// shell holding `ocx.ps1` open on Windows (which would otherwise fail the
/// rename with `ERROR_ACCESS_DENIED`/`ERROR_SHARING_VIOLATION`) is retried
/// instead of silently degrading.
async fn ensure_completion_file(
    file_structure: &ocx_lib::file_structure::FileStructure,
    clap_shell: clap_complete::Shell,
    extension: &str,
) -> std::io::Result<PathBuf> {
    let path = file_structure.state.completion_file(extension);
    let stamp = format!("# ocx-completion-version: {}", crate::app::version());

    if let Ok(existing) = tokio::fs::read_to_string(&path).await
        && existing.lines().next() == Some(stamp.as_str())
    {
        return Ok(path);
    }

    let mut script = format!("{stamp}\n").into_bytes();
    let mut cmd = crate::app::Cli::command();
    let cmd_name = cmd.get_name().to_string();
    clap_complete::generate(clap_shell, &mut cmd, cmd_name, &mut script);

    let dir = file_structure.state.completions_dir();
    tokio::fs::create_dir_all(&dir).await?;
    // Publish atomically via tempfile + `persist_temp_file` — the shared
    // primitive that retries on Windows transient lock/access errors (a shell
    // holding `ocx.ps1` open). It is blocking (sleeps between retries on
    // Windows), so run it off the async runtime. The version-stamped content is
    // idempotent, so a racing writer is harmless.
    let path_for_blocking = path.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        std::io::Write::write_all(&mut tmp, &script)?;
        tmp.as_file().sync_data()?;
        ocx_lib::utility::fs::persist_temp_file(tmp, &path_for_blocking)
    })
    .await
    .map_err(std::io::Error::other)??;
    Ok(path)
}

/// Builds the shell line that loads (`source`/dot-source) the completion file.
fn completion_source_line(shell: Shell, path: &std::path::Path) -> String {
    let path = path.to_string_lossy();
    match shell {
        // PowerShell and Elvish single-quote strings escape `'` by doubling it.
        Shell::PowerShell => format!(". '{}'", path.replace('\'', "''")),
        Shell::Elvish => format!("eval (slurp < '{}')", path.replace('\'', "''")),
        // POSIX shells escape `'` as `'\''`.
        _ => format!("source '{}'", path.replace('\'', "'\\''")),
    }
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
        // Capture the exporter output into a variable and only evaluate it when
        // non-empty. `& ocx …` yields `$null` when the command emits nothing
        // (no global toolchain yet), and `Invoke-Expression $null` throws
        // "Cannot bind argument to parameter 'Command' because it is null".
        // `| Out-String` also collapses the multi-line export output into one
        // string with newlines preserved — passing the raw object array to
        // `Invoke-Expression` would join lines with spaces and corrupt the
        // script. Works in both Windows PowerShell 5.1 and PowerShell 7+.
        Shell::PowerShell => format!(
            "if (-not $env:OCX_ACTIVATED -and (Get-Command ocx -ErrorAction SilentlyContinue)) {{ $__ocx_global_env = (& ocx --global env --shell={shell_name} | Out-String); if ($__ocx_global_env) {{ Invoke-Expression $__ocx_global_env }} }}"
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

    use super::{
        completion_source_line, completion_target, emit_path_prepend, format_activated_marker, format_global_env_eval,
        ocx_install_bin_path,
    };

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

    /// The pwsh eval line must capture the exporter output, collapse it with
    /// `Out-String`, and only `Invoke-Expression` it when non-empty — otherwise
    /// `Invoke-Expression $null` throws on every shell start with no toolchain
    /// (regression: TODO "First" bug).
    #[test]
    fn global_env_eval_powershell_guards_against_null_output() {
        let line = format_global_env_eval(Shell::PowerShell);
        assert!(
            line.contains("Out-String"),
            "pwsh eval line must pipe exporter output through Out-String; got: {line:?}"
        );
        assert!(
            !line.contains("Invoke-Expression (&"),
            "pwsh eval line must NOT pass `(& ocx …)` directly to Invoke-Expression \
             (yields $null when empty → bind error); got: {line:?}"
        );
        // The Invoke-Expression must be guarded by a non-empty check on the
        // captured variable.
        assert!(
            line.contains("if ($__ocx_global_env) { Invoke-Expression $__ocx_global_env }"),
            "pwsh eval line must guard Invoke-Expression on non-empty output; got: {line:?}"
        );
    }

    // ── Completion targets + source lines (TODO "Second" bug) ───────────────

    /// Shells with a clap_complete backend map to an extension; the rest opt out.
    #[test]
    fn completion_target_covers_clap_shells_only() {
        for (shell, ext) in [
            (Shell::Bash, "bash"),
            (Shell::Zsh, "zsh"),
            (Shell::Fish, "fish"),
            (Shell::Elvish, "elv"),
            (Shell::PowerShell, "ps1"),
        ] {
            let (_, got) =
                completion_target(shell).unwrap_or_else(|| panic!("{shell:?} must have a completion target"));
            assert_eq!(got, ext, "{shell:?} completion extension");
        }
        for shell in [Shell::Ash, Shell::Ksh, Shell::Dash, Shell::Batch, Shell::Nushell] {
            assert!(
                completion_target(shell).is_none(),
                "{shell:?} has no clap_complete backend and must opt out"
            );
        }
    }

    /// The emitted source line dot-sources the file with the shell's syntax and
    /// carries no `using` statement (that lives inside the sourced file).
    #[test]
    fn completion_source_line_uses_shell_syntax() {
        let path = std::path::Path::new("/home/u/.ocx/state/completions/ocx.bash");
        assert_eq!(
            completion_source_line(Shell::Bash, path),
            "source '/home/u/.ocx/state/completions/ocx.bash'"
        );
        let ps = std::path::Path::new("C:\\Users\\u\\.ocx\\state\\completions\\ocx.ps1");
        let line = completion_source_line(Shell::PowerShell, ps);
        assert!(
            line.starts_with(". '") && line.contains("ocx.ps1"),
            "pwsh dot-source; got: {line:?}"
        );
        assert!(
            !line.contains("using namespace"),
            "source line must carry no `using`; got: {line:?}"
        );
        assert!(
            completion_source_line(Shell::Elvish, std::path::Path::new("/x/ocx.elv")).starts_with("eval (slurp < '"),
            "elvish must use eval (slurp < ...)"
        );
    }

    /// Single quotes in the path are escaped per shell family so the source
    /// line stays well-formed.
    #[test]
    fn completion_source_line_escapes_single_quotes() {
        let p = std::path::Path::new("/o'x/ocx.bash");
        assert_eq!(
            completion_source_line(Shell::Bash, p),
            r"source '/o'\''x/ocx.bash'",
            "POSIX escapes ' as '\\''"
        );
        let pps = std::path::Path::new("/o'x/ocx.ps1");
        assert_eq!(
            completion_source_line(Shell::PowerShell, pps),
            ". '/o''x/ocx.ps1'",
            "PowerShell escapes ' by doubling"
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

    // ── Completion interactivity gate ──────────────────────────────────────
    //
    // The gate decision (flags + `OCX_NO_COMPLETIONS` + interactivity) now lives
    // in `options::Completion::enabled` and is unit-tested there. `resolve_completion`
    // just honours the resolved boolean.

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
