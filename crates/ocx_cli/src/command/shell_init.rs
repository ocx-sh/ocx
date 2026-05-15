// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::shell;

/// Prints a shell-specific init snippet for the user to paste into their shell rc.
///
/// The snippet wires `ocx shell hook` into the shell's prompt cycle so that
/// every prompt re-evaluates the project toolchain. The exact mechanism
/// differs per shell: Bash inserts into `PROMPT_COMMAND`, Zsh uses
/// `add-zsh-hook precmd`, Fish defines `function __ocx_hook --on-event
/// fish_prompt`, and Nushell writes to `$env.NU_VENDOR_AUTOLOAD_DIR/ocx.nu`.
///
/// This is a pure code generator — it never reads the project, never
/// touches disk, and never contacts the network. The user runs it once
/// (typically piping into `>>` or copying the output) and then the shell
/// drives `ocx shell hook` on every subsequent prompt.
#[derive(Parser)]
pub struct ShellInit {
    /// The shell to print the init snippet for.
    ///
    /// Spelled `--shell` (with `-s` short) to match the sibling
    /// `ocx shell hook --shell` form and the W2-P3
    /// `test_global_toolchain.py` contract, which invokes
    /// `ocx shell init --shell bash`.
    #[clap(short = 's', long = "shell", value_name = "SHELL")]
    shell: shell::Shell,
}

/// Bash init: hook into `PROMPT_COMMAND` (idempotent — only inserted if
/// the function name is not already present). Wraps the eval in a function
/// so the saved exit status of the user's prompt commands is preserved
/// across the hook (mise/direnv convention).
const BASH_INIT: &str = r#"_ocx_hook() {
  local previous_exit_status=$?
  eval "$(ocx shell hook --shell bash)"
  return "$previous_exit_status"
}
if ! [[ "${PROMPT_COMMAND:-}" =~ _ocx_hook ]]; then
  PROMPT_COMMAND="_ocx_hook${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
"#;

/// Zsh init: register a `precmd` hook via `add-zsh-hook`. Idempotent — the
/// `(I)` subscript flag returns the index of `_ocx_hook` in
/// `precmd_functions` (0 when absent), so the guard prevents duplicate
/// registration when the snippet is sourced repeatedly.
const ZSH_INIT: &str = r#"autoload -Uz add-zsh-hook
_ocx_hook() {
  eval "$(ocx shell hook --shell zsh)"
}
add-zsh-hook precmd _ocx_hook
"#;

/// Fish init: a function attached to the prompt event. Fires before every
/// prompt (including `cd` and other state changes), matching the cadence
/// of mise/direnv. `--on-variable PWD` was the original choice but only
/// fires on `cd`; `--on-event fish_prompt` is the right granularity for
/// per-prompt re-evaluation.
const FISH_INIT: &str = r#"function __ocx_hook --on-event fish_prompt
  ocx shell hook --shell fish | source
end
"#;

/// Nushell init: file-based, no stdout eval. The snippet is intentionally
/// commented because Nushell's `eval` substitute (`source`) cannot run on
/// a string from stdin without a temp file; the supported pattern is
/// `$env.NU_VENDOR_AUTOLOAD_DIR/ocx.nu` (loaded automatically on shell
/// start). The user redirects this output into that file.
///
/// Hook target: `pre_prompt` (fires before every prompt). The earlier
/// `env_change.PWD` choice only fired on `cd`, leaving edits to
/// `ocx.toml` invisible until the next directory change — out of step
/// with the Bash/Zsh/Fish variants which all re-evaluate per prompt.
/// Per Nushell hooks reference, `pre_prompt` is a list of closures of
/// signature `{||}`. See <https://www.nushell.sh/book/hooks.html>.
///
/// Plan §7 line 784 explicitly forbids `eval` in this snippet.
const NUSHELL_INIT: &str = r#"# Save this file to $env.NU_VENDOR_AUTOLOAD_DIR/ocx.nu so Nushell auto-loads it.
# See: https://www.nushell.sh/book/configuration.html#vendor-autoloads
$env.config.hooks.pre_prompt = ($env.config.hooks.pre_prompt? | default []) ++ [
  {||
    # File-based init: ocx writes exports to a temp file and Nushell sources it.
    # NU_VENDOR_AUTOLOAD_DIR is the canonical autoload directory.
    let tmp = ($nu.temp-path | path join $"ocx-hook-(random uuid).nu")
    ocx shell hook --shell nushell | save --force $tmp
    source $tmp
    rm $tmp
  }
]
"#;

/// Init snippets for shells that lack a documented prompt-hook convention
/// (Ash, Ksh, Dash, Elvish, PowerShell, Batch). Phase 7 ships v1 with
/// targeted support for Bash, Zsh, Fish, and Nushell only; other shells
/// can still call `ocx shell hook` manually.
const UNSUPPORTED_INIT: &str = "# `ocx shell init` does not yet ship a prompt-hook snippet for this shell.\n\
     # Call `ocx shell hook --shell <name>` from your prompt manually until support lands.\n";

/// Write the static `$OCX_HOME/init.<shell>` PATH-prepend entrypoint.
///
/// Distinct from the per-prompt hook (interactive-only, mise-`activate`
/// shape): this static entrypoint prepends the stable global-`current` bin
/// dir to PATH **without** invoking the hook, so a sourced *non-interactive*
/// shell (CI `bash --norc`, `bash -c`, editor terminals) still sees the
/// global tools (adr_global_toolchain_tier.md §Decision 6, SOTA-2a). The
/// POSIX entrypoint uses `.` (dot), not bash `source`, for dash/sh CI
/// compatibility (SOTA-2b). The landing path is
/// [`ocx_lib::ConfigLoader::home_init_path`]. The OS install-script edit
/// that sources this file is out of scope (downstream `setup.ocx.sh`).
/// Wrap `export_lines` in a self-contained, project-aware guard so the
/// global PATH-prepend is suppressed whenever an `ocx.toml` is in scope.
///
/// Strict isolation at the static-entrypoint layer
/// (adr_global_toolchain_tier.md §Decision 6): a non-interactive shell
/// that only sources `$OCX_HOME/init.<shell>` (CI `bash --norc -c`, no
/// prompt hook) must still NOT see global tools inside a project. The
/// guard is a pure-shell upward `ocx.toml` walk from `$PWD` — no `ocx`
/// invocation, no hook — honouring `OCX_NO_PROJECT` (kill switch) and
/// `OCX_PROJECT` (explicit project ⇒ in-project) for parity with
/// `ProjectConfig::resolve`. POSIX-family branch only (the acceptance
/// suite exercises bash; dash/sh compatible — no bashisms). Fish /
/// Nushell / PowerShell / Elvish have no project-aware guard
/// implementation, so the static global-PATH prepend is **omitted
/// entirely** for them (only an explanatory `# ocx:` comment is emitted):
/// emitting it unguarded would leak global tools into a project for any
/// non-interactive invocation, violating strict isolation. Global tools
/// remain reachable outside a project for those shells via the
/// interactive prompt hook (`ocx shell hook`), which still enforces
/// isolation when a project is entered.
fn project_guarded(shell: shell::Shell, export_lines: &str) -> String {
    if export_lines.is_empty() {
        return String::new();
    }
    match shell {
        shell::Shell::Ash | shell::Shell::Ksh | shell::Shell::Dash | shell::Shell::Bash | shell::Shell::Zsh => {
            // Indent the export lines two spaces so they sit inside the
            // `if` body; the guard walks `$PWD` upward looking for
            // `ocx.toml`, mirroring the CWD-walk resolver.
            let indented = export_lines
                .lines()
                .map(|l| format!("  {l}"))
                .collect::<Vec<_>>()
                .join("\n");
            // Loop body uses POSIX parameter expansion `${_d%/*}` instead
            // of `$(dirname "$_d")` so each ancestor level costs zero
            // forks (rustup/cargo non-interactive-init SOTA shape;
            // dash/sh/ash/ksh compatible). Termination: at `_d=/` the
            // root check breaks *before* stripping; otherwise `${_d%/*}`
            // peels one level and eventually yields the empty string
            // (e.g. `/foo` → ``), which fails the `while [ -n "$_d" ]`
            // guard and exits the loop ⇒ not-in-project ⇒ prepend.
            // Mirrors `ProjectConfig::resolve` precedence (OCX_NO_PROJECT
            // kill switch ▸ OCX_PROJECT explicit ▸ upward `ocx.toml`
            // walk) and *deliberately* omits the `.git`/ceiling boundary
            // the Rust walk applies: for a global-isolation guard, "any
            // ancestor `ocx.toml` ⇒ in-project" is the safe direction.
            format!(
                "_ocx_in_project() {{\n\
                 \x20 [ \"${{OCX_NO_PROJECT:-}}\" = 1 ] && return 1\n\
                 \x20 [ -n \"${{OCX_PROJECT:-}}\" ] && return 0\n\
                 \x20 _d=\"$PWD\"\n\
                 \x20 while [ -n \"$_d\" ]; do\n\
                 \x20\x20\x20 [ -f \"$_d/ocx.toml\" ] && return 0\n\
                 \x20\x20\x20 [ \"$_d\" = / ] && break\n\
                 \x20\x20\x20 _d=\"${{_d%/*}}\"\n\
                 \x20 done\n\
                 \x20 return 1\n\
                 }}\n\
                 if ! _ocx_in_project; then\n\
                 {indented}\n\
                 fi\n\
                 unset -f _ocx_in_project 2>/dev/null || true\n"
            )
        }
        // Non-POSIX shells (Fish / Nushell / PowerShell / Elvish): the
        // project-aware upward `ocx.toml` walk is implemented only for the
        // POSIX family. Emitting the static global-PATH prepend unguarded
        // here would leak global tools into a project for any
        // non-interactive invocation (e.g. a CI step that sources this
        // file but never runs the prompt hook), violating the
        // strict-isolation contract (adr_global_toolchain_tier.md
        // §Decision 4/6). Since the guard cannot be guaranteed
        // non-interactively for these shells, omit the static prepend
        // entirely and emit only an explanatory comment. Global tools are
        // still reachable outside a project via the interactive
        // prompt-hook (`ocx shell hook`), which enforces isolation when a
        // project is entered.
        _ => "# ocx: static global PATH prepend omitted for this shell — strict isolation \
              cannot be guaranteed non-interactively (the project-aware guard is POSIX-only); \
              the interactive `ocx shell hook` prompt hook provides global tools outside projects\n"
            .to_string(),
    }
}

async fn write_static_global_entrypoint(context: &crate::app::Context, shell: shell::Shell) -> anyhow::Result<()> {
    use std::io::Write as _;

    use ocx_lib::ConfigLoader;
    use ocx_lib::package::metadata::env::modifier::ModifierKind;

    let Some(init_path) = ConfigLoader::home_init_path(shell) else {
        anyhow::bail!("cannot resolve $OCX_HOME (and no home directory) to write the global init entrypoint");
    };

    // Resolve the global toolchain's installed `current` set via the
    // shared resolver (same source the per-prompt `NoProject` arm uses —
    // feedback_extend_dont_duplicate). The static entrypoint emits the
    // *plain* export lines (PATH-prepend / constant) WITHOUT the
    // `_OCX_APPLIED` sentinel or any `ocx shell hook` invocation, so a
    // sourced non-interactive shell (`bash --norc -c`, dash, CI) sees the
    // global tools (SOTA-2a). The per-prompt hook layers dynamic project
    // switching on top.
    let header = "# Generated by `ocx shell init`. Static global-toolchain PATH entrypoint.\n\
                  # Source this from your shell rc / CI env (POSIX `.` — dash/sh compatible).\n\
                  # Project-aware: the global PATH-prepend is suppressed when an\n\
                  # ocx.toml is in scope (strict isolation — global never leaks\n\
                  # into a project; the project toolchain is authoritative).\n\
                  # Per-prompt project switching is layered by the `ocx shell hook` snippet.\n";

    // Build the raw export lines for the global `current` set (same
    // source the per-prompt `NoProject` arm uses —
    // feedback_extend_dont_duplicate). No `_OCX_APPLIED` sentinel and no
    // `ocx shell hook` invocation, so a sourced non-interactive shell
    // (`bash --norc -c`, dash, CI) sees the global tools (SOTA-2a).
    let mut export_lines = String::new();
    if let Some((entries, _fingerprint)) = crate::command::shell_hook::resolve_global_current_env(context).await? {
        for entry in &entries {
            let line = match entry.kind {
                ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
                ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
            };
            match line {
                Some(line) => {
                    export_lines.push_str(&line);
                    export_lines.push('\n');
                }
                None => eprintln!("# ocx: skipping invalid env-var key {:?} in global init", entry.key),
            }
        }
    }
    // When there is no global toolchain yet the file is still written
    // (header only) so the install-script `source` target always exists
    // and a later `install --global` + re-run of `shell init` populates it.

    let mut body = String::from(header);
    body.push_str(&project_guarded(shell, &export_lines));

    // Atomic write: tempfile in the same dir + rename (crash-safe; a
    // concurrent `shell init` cannot leave a half-written entrypoint that
    // a sourcing shell would partially execute). Blocking fs is wrapped in
    // `spawn_blocking` per the async-IO convention.
    let parent = init_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let target = init_path.clone();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        std::fs::create_dir_all(&parent)?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".init-")
            .suffix(".tmp")
            .tempfile_in(&parent)?;
        tmp.write_all(body.as_bytes())?;
        tmp.flush()?;
        tmp.persist(&target).map_err(|e| e.error)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("init-entrypoint writer task panicked: {e}"))??;

    Ok(())
}

impl ShellInit {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Materialize the static global PATH-prepend entrypoint so a
        // non-interactive sourced shell sees the global toolchain (the
        // per-prompt snippet below only layers dynamic project switching
        // on top).
        write_static_global_entrypoint(&context, self.shell).await?;

        let snippet = match self.shell {
            shell::Shell::Bash => BASH_INIT,
            shell::Shell::Zsh => ZSH_INIT,
            shell::Shell::Fish => FISH_INIT,
            shell::Shell::Nushell => NUSHELL_INIT,
            shell::Shell::Ash
            | shell::Shell::Ksh
            | shell::Shell::Dash
            | shell::Shell::Elvish
            | shell::Shell::PowerShell
            | shell::Shell::Batch => UNSUPPORTED_INIT,
        };
        print!("{snippet}");
        Ok(ExitCode::SUCCESS)
    }
}
