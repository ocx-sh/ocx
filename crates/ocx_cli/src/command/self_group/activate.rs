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
    /// `pwsh`, `elvish`, `nushell`, `batch` (`sh` == `dash`, POSIX alias).
    ///
    /// Must be supplied with `=` (`--shell=bash`). Bare `--shell` (no `=`)
    /// triggers autodetection from `$SHELL`/parent process; exit 64 if
    /// undetectable - pass `--shell=NAME` explicitly to override.
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

        // Generate the shell-completion script to emit inline when completions
        // are enabled for this session (see `Completion::enabled`). The
        // stderr-TTY probe is the auto signal when neither `--completion` nor
        // `--no-completion` is passed.
        let load_completions = self.completion.enabled(std::io::stderr().is_terminal());
        let completion = load_completions.then(|| generate_completion_inline(shell)).flatten();

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
    let ocx_cli_id = oci::ocx_cli_identifier();
    fs.symlinks.current(&ocx_cli_id).join("content").join("bin")
}

/// Emit all activation lines to stdout for the given shell.
///
/// `completion` is the generated completion script to emit inline, or `None`
/// when completions are disabled, the session is non-interactive, or the shell
/// has no completion backend (see [`generate_completion_inline`]).
fn emit_activation(shell: Shell, bin_path: &std::path::Path, completion: Option<&str>) {
    // ── Shell completions (emitted FIRST) ────────────────────────────────────
    // The completion block must lead the stream: clap_complete's PowerShell
    // output opens with `using namespace`, which `Invoke-Expression` (the pwsh
    // shim's loader) accepts only as the *first* statement — any earlier line
    // makes pwsh reject the whole script. Other shells are order-insensitive
    // here, and no completion block references `ocx` at definition time, so
    // emitting it before the PATH prepend is safe.
    if let Some(script) = completion {
        println!("{script}");
    }

    // ── PATH prepend ─────────────────────────────────────────────────────────
    // Use the absolute resolved path — no $VAR references, so Shell::export_path
    // is safe here (escape_value won't transform the path characters since
    // they contain no shell metacharacters).
    emit_path_prepend(shell, bin_path);

    // ── Global toolchain env ─────────────────────────────────────────────────
    // Evaluate the global toolchain env. The emitted env lines never duplicate on
    // every shell — PATH uses idempotent move-to-front (cmd included, via
    // substring-delete; see `Shell::export_path`), constants are absolute sets —
    // so the eval runs unconditionally with NO `OCX_ACTIVATED` state guard. An
    // exported guard leaks into child processes (e.g. a VS Code Remote server
    // whose terminals inherit it) and wrongly suppresses activation in a shell
    // that needs it. Running unconditionally also lets a re-source pick up a
    // changed global toolchain.
    emit_global_env_eval(shell);
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

/// Maps a shell to its `clap_complete` backend, or `None` when the shell has no
/// completion generator (ash/ksh/dash/batch/nushell).
fn completion_clap_shell(shell: Shell) -> Option<clap_complete::Shell> {
    use clap_complete::Shell as Clap;
    Some(match shell {
        Shell::Bash => Clap::Bash,
        Shell::Zsh => Clap::Zsh,
        Shell::Fish => Clap::Fish,
        Shell::Elvish => Clap::Elvish,
        Shell::PowerShell => Clap::PowerShell,
        // ash/ksh/dash/batch/nushell have no clap_complete backend.
        _ => return None,
    })
}

/// Generate the shell-completion script to emit inline into the activation
/// stream, or `None` when the shell has no completion backend.
///
/// Emitted directly into the eval'd activation stream rather than written to a
/// file: the stream is already `eval`/`Invoke-Expression`'d by the shim, so a
/// `complete -F` / `compdef` / `Register-ArgumentCompleter` block installs
/// completions with no file to manage, version-stamp, or read.
///
/// Delegates to [`crate::command::shell_completion::render_completion_script`]
/// — the single generator shared with `ocx shell completion`, which adds the
/// zsh `compinit` guard so the script registers wherever it is sourced. The one
/// activation-specific concern is order: PowerShell's `using namespace` must be
/// the first statement of the stream, which [`emit_activation`] guarantees by
/// emitting this block before the PATH prepend (verified on Windows PowerShell
/// 5.1 and PowerShell 7).
fn generate_completion_inline(shell: Shell) -> Option<String> {
    let clap_shell = completion_clap_shell(shell)?;
    let mut cmd = crate::app::Cli::command();
    let cmd_name = cmd.get_name().to_string();
    Some(crate::command::shell_completion::render_completion_script(
        &mut cmd, &cmd_name, clap_shell,
    ))
}

/// Emit the eval line that loads the global toolchain env.
fn emit_global_env_eval(shell: Shell) {
    println!("{}", format_global_env_eval(shell));
}

/// Format the global-env-eval line for `shell` without emitting.
///
/// On every shell this runs `ocx --global env` guarded only by an `ocx`-existence
/// probe — PATH uses idempotent move-to-front (cmd included, via substring-delete)
/// and constants are absolute sets, so there is no `OCX_ACTIVATED` state guard (an
/// exported guard would leak into child shells, e.g. a VS Code Remote server whose
/// terminals inherit it, and suppress activation where it is needed).
fn format_global_env_eval(shell: Shell) -> String {
    let shell_name = shell_name_for_eval(shell);
    match shell {
        Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Bash | Shell::Zsh => {
            format!(r#"if command -v ocx >/dev/null 2>&1; then eval "$(ocx --global env --shell={shell_name})"; fi"#)
        }
        Shell::Fish => {
            format!("if type -q ocx; ocx --global env --shell={shell_name} | source; end")
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
            "if (Get-Command ocx -ErrorAction SilentlyContinue) {{ $__ocx_global_env = (& ocx --global env --shell={shell_name} | Out-String); if ($__ocx_global_env) {{ Invoke-Expression $__ocx_global_env }} }}"
        ),
        // CMD: `FOR /F` evaluates each line of the subprocess output. `delims=`
        // is required so `%i` is the WHOLE line — the default whitespace split
        // would leave `%i` as just the first token (`SET`), executing a bare
        // `SET` and dropping the assignment. Batch PATH emission is now idempotent
        // move-to-front (single-statement substring-delete, see
        // `Shell::export_path`), so like every other shell it runs unguarded —
        // no `OCX_ACTIVATED` session guard, no marker.
        Shell::Batch => {
            format!("FOR /F \"usebackq delims=\" %i IN (`ocx --global env --shell={shell_name}`) DO @%i")
        }
        Shell::Elvish => {
            format!("if (has-external ocx) {{ ocx --global env --shell={shell_name} | slurp | eval }}")
        }
        Shell::Nushell => {
            format!("if (which ocx | length) > 0 {{ ocx --global env --shell={shell_name} | nu -c $in }}")
        }
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
        completion_clap_shell, emit_path_prepend, format_global_env_eval, generate_completion_inline,
        ocx_install_bin_path,
    };

    /// Every shell variant — the activation surface is generic over the full set.
    const ALL_SHELLS: [Shell; 10] = [
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

    // ── No OCX_ACTIVATED guard (idempotent activation, no cross-process leak) ─

    /// No shell's global-env-eval line may carry an `OCX_ACTIVATED` state guard —
    /// Batch included now that its PATH emission is idempotent move-to-front
    /// (substring-delete). An exported guard leaks into child processes (e.g. a
    /// VS Code Remote server whose terminals inherit it) and suppresses
    /// activation in a shell that needs it — the exact SSH-vs-VS-Code divergence
    /// this removal fixes. Idempotent env emission (PATH move-to-front, constants
    /// absolute) makes the guard unnecessary for correctness.
    #[test]
    fn global_env_eval_has_no_activated_guard_for_any_shell() {
        for shell in ALL_SHELLS {
            let line = format_global_env_eval(shell);
            assert!(
                !line.contains("OCX_ACTIVATED"),
                "{shell:?} eval line must not reference OCX_ACTIVATED; got: {line:?}"
            );
        }
    }

    /// Every shell still invokes `ocx --global env` — removing the guard must
    /// strip only the state gate, never the eval itself.
    #[test]
    fn global_env_eval_invokes_global_env_for_any_shell() {
        for shell in ALL_SHELLS {
            let line = format_global_env_eval(shell);
            assert!(
                line.contains("ocx --global env --shell="),
                "{shell:?} eval line must invoke ocx --global env; got: {line:?}"
            );
        }
    }

    /// The eval stays gated on an `ocx`-existence probe (not a state guard) for
    /// every shell that has one, so a shell with ocx uninstalled is a clean
    /// no-op rather than an error. Batch has no probe — `FOR /F` runs ocx and a
    /// missing binary simply emits nothing.
    #[test]
    fn global_env_eval_probes_ocx_existence() {
        let probes = [
            (Shell::Bash, "command -v ocx"),
            (Shell::Zsh, "command -v ocx"),
            (Shell::Dash, "command -v ocx"),
            (Shell::Ash, "command -v ocx"),
            (Shell::Ksh, "command -v ocx"),
            (Shell::Fish, "type -q ocx"),
            (Shell::PowerShell, "Get-Command ocx"),
            (Shell::Elvish, "has-external ocx"),
            (Shell::Nushell, "which ocx"),
        ];
        for (shell, probe) in probes {
            let line = format_global_env_eval(shell);
            assert!(
                line.contains(probe),
                "{shell:?} eval line must probe ocx via `{probe}`; got: {line:?}"
            );
        }
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

    // ── Inline completion generation ────────────────────────────────────────

    /// Shells with a clap_complete backend produce a non-empty inline script;
    /// the rest opt out (no backend → no completion emitted).
    #[test]
    fn generate_completion_inline_covers_clap_shells_only() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::Elvish, Shell::PowerShell] {
            let script =
                generate_completion_inline(shell).unwrap_or_else(|| panic!("{shell:?} must produce a completion"));
            assert!(!script.is_empty(), "{shell:?} completion must be non-empty");
        }
        for shell in [Shell::Ash, Shell::Ksh, Shell::Dash, Shell::Batch, Shell::Nushell] {
            assert!(
                completion_clap_shell(shell).is_none() && generate_completion_inline(shell).is_none(),
                "{shell:?} has no clap_complete backend and must opt out"
            );
        }
    }

    /// The zsh completion self-loads `compinit` before clap's trailing
    /// `compdef`, so it works even when sourced before the user's own
    /// `compinit` (e.g. from `.zprofile`).
    #[test]
    fn zsh_completion_guards_compinit_before_compdef() {
        let script = generate_completion_inline(Shell::Zsh).expect("zsh has a backend");
        let guard = script
            .find("autoload -Uz compinit")
            .expect("zsh completion must self-load compinit");
        let compdef = script
            .rfind("compdef _ocx ocx")
            .expect("zsh completion must call compdef");
        assert!(guard < compdef, "the compinit guard must precede the compdef call");
    }

    /// The PowerShell completion must lead with `using namespace` so that, when
    /// emitted first into the activation stream, `Invoke-Expression` accepts it
    /// (valid only as the first statement; verified on WinPS 5.1 and pwsh 7).
    #[test]
    fn powershell_completion_leads_with_using_namespace() {
        let script = generate_completion_inline(Shell::PowerShell).expect("pwsh has a backend");
        let first = script.trim_start().lines().next().unwrap_or_default();
        assert!(
            first.starts_with("using namespace"),
            "pwsh completion must lead with `using namespace`; got: {first:?}"
        );
    }

    /// No completion output may contain non-ASCII bytes (ASCII guard, G4).
    ///
    /// clap embeds CLI help text into every completion script. A stray Unicode
    /// character (e.g. `→`) is invisible on UTF-8 shells but corrupts on Windows
    /// PowerShell 5.1, which decodes the captured activation stream with the
    /// console codepage — turning `→` into mojibake whose stray quote byte breaks
    /// the surrounding single-quoted PowerShell string. Keeping help ASCII makes
    /// the inline stream robust on every shell. If this fails, find the offending
    /// `#[arg]`/`#[command(about=...)]` help text and replace the non-ASCII
    /// character (e.g. `→` -> `->`).
    #[test]
    fn completion_output_is_ascii_for_all_shells() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::Elvish, Shell::PowerShell] {
            let script = generate_completion_inline(shell).expect("backend present");
            let bad = script.bytes().position(|byte| !byte.is_ascii());
            assert!(
                bad.is_none(),
                "{shell:?} completion must be ASCII-only; first non-ASCII byte at offset {:?}. \
                 Find the help text with a non-ASCII char and replace it (e.g. `→` -> `->`).",
                bad
            );
        }
    }

    // ── Completion interactivity gate ──────────────────────────────────────
    //
    // The gate decision (flags + `OCX_NO_COMPLETIONS` + interactivity) now lives
    // in `options::Completion::enabled` and is unit-tested there. `execute` just
    // honours the resolved boolean before calling `generate_completion_inline`.

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
