// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::shell;

/// Prints a shell-specific init snippet for the user to paste into their shell rc.
///
/// The snippet wires `ocx hook-env` into the shell's prompt cycle so that
/// every prompt re-evaluates the project toolchain. The exact mechanism
/// differs per shell: Bash inserts into `PROMPT_COMMAND`, Zsh uses
/// `add-zsh-hook precmd`, Fish defines `function __ocx_hook --on-event
/// fish_prompt`, and Nushell writes to `$env.NU_VENDOR_AUTOLOAD_DIR/ocx.nu`.
///
/// This is a pure code generator — it never reads the project, never
/// touches disk, and never contacts the network. The user runs it once
/// (typically piping into `>>` or copying the output) and then the shell
/// drives `ocx hook-env` on every subsequent prompt.
#[derive(Parser)]
pub struct ShellInit {
    /// The shell to print the init snippet for.
    #[arg(value_name = "SHELL")]
    shell: shell::Shell,
}

/// Bash init: hook into `PROMPT_COMMAND` (idempotent — only inserted if
/// the function name is not already present). Wraps the eval in a function
/// so the saved exit status of the user's prompt commands is preserved
/// across the hook (mise/direnv convention).
const BASH_INIT: &str = r#"_ocx_hook() {
  local previous_exit_status=$?
  eval "$(ocx hook-env --shell bash)"
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
  eval "$(ocx hook-env --shell zsh)"
}
add-zsh-hook precmd _ocx_hook
"#;

/// Fish init: a function attached to the prompt event. Fires before every
/// prompt (including `cd` and other state changes), matching the cadence
/// of mise/direnv. `--on-variable PWD` was the original choice but only
/// fires on `cd`; `--on-event fish_prompt` is the right granularity for
/// per-prompt re-evaluation.
const FISH_INIT: &str = r#"function __ocx_hook --on-event fish_prompt
  ocx hook-env --shell fish | source
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
    ocx hook-env --shell nushell | save --force $tmp
    source $tmp
    rm $tmp
  }
]
"#;

/// Init snippets for shells that lack a documented prompt-hook convention
/// (Ash, Ksh, Dash, Elvish, PowerShell, Batch). Phase 7 ships v1 with
/// targeted support for Bash, Zsh, Fish, and Nushell only; other shells
/// can still call `ocx hook-env` manually.
const UNSUPPORTED_INIT: &str = "# `ocx shell init` does not yet ship a prompt-hook snippet for this shell.\n\
     # Call `ocx hook-env --shell <name>` from your prompt manually until support lands.\n";

impl ShellInit {
    pub async fn execute(&self, _context: crate::app::Context) -> anyhow::Result<ExitCode> {
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
