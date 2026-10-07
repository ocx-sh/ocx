// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-shell prompt-hook and wrapper emission.
//!
//! Every registration appends to its hook point, never clobbers it. Never emit a bare `ocx`: the
//! wrapper owns that name, so a command substitution would re-enter it.
//! Never suppress the deferred messages: they tell a user why a project is inert.

use std::path::{Path, PathBuf};

use super::Shell;
use super::escape::{fish_single_quoted, posix_single_quoted, single_quoted_doubled};
use ocx_config::shell::GRANT_SIGNALS;

/// The temp-file template the emitted body hands `mktemp` for its stamp.
///
/// Never a fixed path: the checkpoint truncates the stamp, so a pre-planted symlink would get its target truncated.
const STAMP_TEMPLATE: &str = "ocx-env-stamp.XXXXXXXX";

/// The project file the guard tests for at the shell's **live** `$PWD`.
///
/// Without it, an `ocx.toml` created in a project-less directory stays inert until the next `cd`.
/// Ceiling: one appearing in an *ancestor* of `$PWD` still waits for the next `cd`.
const PROJECT_FILE: &str = "ocx.toml";

/// The bash/zsh shell-exit cleanup, or one stamp file per shell leaks where nothing reaps `$TMPDIR`.
///
/// `command rm`, not `rm`: the emitted stream never calls a name a user function could own.
const POSIX_STAMP_CLEANUP: &str = "command rm -f \"${__ocx_stamp-}\" 2>/dev/null";

/// The fish form of [`POSIX_STAMP_CLEANUP`].
const FISH_STAMP_CLEANUP: &str = "command rm -f \"$__ocx_stamp\" 2>/dev/null";

/// The offline switch every emitted reconcile call carries, sparing a `reqwest` client per index source (22 vs 58 ms).
///
/// It must precede the subcommand: `--offline` is a root flag, and after it the call exits 64.
const OFFLINE: &str = "--offline";

/// The live-session sentinels every arm records in its checkpoint and re-reads in its guard.
///
/// Must mirror what [`super::coexistence::detect`] reads, or a sentinel it honours never triggers a yield.
const YIELD_SIGNALS: [&str; 3] = [
    ocx_env::DIRENV_DIR.name,
    ocx_env::MISE_SHELL.name,
    ocx_env::MISE_ORIG_PATH.name,
];

// The guard compares `GRANT_SIGNALS` too, or an `OCX_CONSENT_*` export mid-session never runs ocx.

/// `keys` as one bash/zsh word, `set -u`-safe since most sentinels are unset.
fn posix_signal(keys: &[&str]) -> String {
    keys.iter()
        .map(|key| format!("${{{key}-}}"))
        .collect::<Vec<_>>()
        .join("|")
}

/// The fish form of [`posix_signal`]; both uses are quoted, so fish's `:` re-join of a `*PATH` value round-trips.
fn fish_signal(keys: &[&str]) -> String {
    keys.iter().map(|key| format!("${key}")).collect::<Vec<_>>().join("|")
}

/// The PowerShell form of [`posix_signal`].
fn power_shell_signal(keys: &[&str]) -> String {
    keys.iter()
        .map(|key| format!("$($env:{key})"))
        .collect::<Vec<_>>()
        .join("|")
}

/// The elvish form of [`posix_signal`], as the tail of [`elvish_pwd_value`].
fn elvish_signal(keys: &[&str]) -> String {
    keys.iter().map(|key| format!("' '$E:{key}")).collect()
}

/// How every wrapper hands its post-command reconcile to the guarded prompt hook, so an idle `ocx` execs nothing.
///
/// `typeset -f`, not `command -v`, which also finds a `$PATH` file of that name that the next line would run.
const POSIX_HOOK_CALL: &str = "if typeset -f __ocx_prompt_hook >/dev/null 2>&1; then __ocx_prompt_hook; fi";

/// The PowerShell form of [`POSIX_HOOK_CALL`]; the check has its own name because calling `prompt` would print one.
const PWSH_HOOK_CALL: &str = "if (Test-Path function:global:__ocxReconcile) { __ocxReconcile }";

/// Emit the idempotent per-prompt hook registration for `shell`, or `None` where it has no hook point.
///
/// `binary` is the resolved absolute path; a restricted shell (`rbash`) no-ops.
/// Every "already registered?" probe stays scoped to the namespace it asks about, or the shell runs unhooked for life.
pub fn registration(shell: Shell, binary: &Path, watch_paths: &[PathBuf]) -> Option<String> {
    #[cfg(any(test, feature = "__testing"))]
    inject_latency_fault();
    let binary = binary.to_string_lossy();
    match shell {
        Shell::Bash => Some(bash_registration(&binary, watch_paths)),
        Shell::Zsh => Some(zsh_registration(&binary, watch_paths)),
        Shell::Fish => Some(fish_registration(&binary, watch_paths)),
        Shell::PowerShell => Some(power_shell_registration(&binary, watch_paths)),
        Shell::Elvish => Some(elvish_registration(&binary)),
        // No append-safe hook point; nushell's hook is inlined in its shim.
        Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Nushell | Shell::Batch => None,
    }
}

/// Re-emit only the guarded function for `shell` with a **new** watch set, never the one-time setup.
///
/// Without it a project entered mid-session is never watched. `None` where the guard has no watch-set term.
pub fn redefinition(shell: Shell, binary: &Path, watch_paths: &[PathBuf]) -> Option<String> {
    let binary = binary.to_string_lossy();
    match shell {
        Shell::Bash => Some(posix_redefinition(&binary, "bash", watch_paths)),
        Shell::Zsh => Some(posix_redefinition(&binary, "zsh", watch_paths)),
        Shell::Fish => Some(fish_hook_function(&fish_single_quoted(&binary), watch_paths)),
        Shell::PowerShell => Some(power_shell_reconcile_function(
            &single_quoted_doubled(&binary),
            watch_paths,
        )),
        Shell::Elvish | Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Nushell | Shell::Batch => None,
    }
}

/// The bash/zsh form of [`redefinition`]; keeps the unreachable restricted-shell arm so every POSIX
/// emit stands down in `rbash`.
fn posix_redefinition(binary: &str, shell_name: &str, watch_paths: &[PathBuf]) -> String {
    let quoted = posix_single_quoted(binary);
    format!(
        "case $- in\n\
         *r*) : ;;\n\
         *)\n\
         {body}\n\
         ;;\n\
         esac",
        body = posix_hook_body(&quoted, shell_name, watch_paths)
    )
}

/// Testing-only latency fault for the shell-latency gate, in milliseconds from `__OCX_TESTING_LATENCY_INJECT_MS`.
///
/// In [`registration`], not `main`, so a gate aimed at a command that emits no hook cannot fake its red.
#[cfg(any(test, feature = "__testing"))]
fn inject_latency_fault() {
    let Some(raw) = ocx_env::__OCX_TESTING_LATENCY_INJECT_MS.get_os() else {
        return;
    };
    let Some(milliseconds) = raw.to_str().and_then(|text| text.trim().parse::<f64>().ok()) else {
        return;
    };
    if milliseconds > 0.0 {
        std::thread::sleep(std::time::Duration::from_secs_f64(milliseconds / 1000.0));
    }
}

/// The freshness checkpoint a **successful** reconcile emits, or `None` for arms with no hook.
///
/// Emitted by the reconcile, never the hook body, or a failed run latches the shell into a stale environment.
/// Carries a second latency seam: the two budgets are asserted separately and one seam cannot red both.
pub fn checkpoint(shell: Shell) -> Option<String> {
    #[cfg(any(test, feature = "__testing"))]
    inject_latency_fault();
    Some(match shell {
        // `>|` overrides a user's `noclobber`.
        Shell::Bash | Shell::Zsh => format!(
            "if [ -n \"${{__ocx_stamp-}}\" ]; then : >| \"${{__ocx_stamp-}}\" 2>/dev/null || true; fi\n\
             __ocx_pwd=$PWD\n\
             __ocx_yield=\"{yielded}\"\n\
             __ocx_grant=\"{granted}\"",
            yielded = posix_signal(&YIELD_SIGNALS),
            granted = posix_signal(&GRANT_SIGNALS)
        ),
        // `true` is a fish builtin, so the refresh costs no exec.
        Shell::Fish => format!(
            "if test -n \"$__ocx_stamp\"; true >\"$__ocx_stamp\" 2>/dev/null; end\n\
             set -g __ocx_pwd $PWD\n\
             set -g __ocx_yield \"{yielded}\"\n\
             set -g __ocx_grant \"{granted}\"",
            yielded = fish_signal(&YIELD_SIGNALS),
            granted = fish_signal(&GRANT_SIGNALS)
        ),
        Shell::PowerShell => format!(
            "$global:__ocxStamp = [datetime]::UtcNow\n\
             $global:__ocxPwd = $PWD.Path\n\
             $global:__ocxYield = \"{yielded}\"\n\
             $global:__ocxGrant = \"{granted}\"",
            yielded = power_shell_signal(&YIELD_SIGNALS),
            granted = power_shell_signal(&GRANT_SIGNALS)
        ),
        Shell::Elvish => format!("set-env {ELVISH_PWD_KEY} {value}", value = elvish_pwd_value()),
        Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Nushell | Shell::Batch => return None,
    })
}

/// Emit the `ocx` wrapper function for `shell`, or `None` wherever [`registration`] is.
///
/// A latency optimization, never the correctness floor. It must return the binary's own status,
/// captured before the reconcile, or `ocx add … && next` breaks.
pub fn wrapper(shell: Shell, binary: &Path) -> Option<String> {
    let binary = binary.to_string_lossy();
    match shell {
        Shell::Bash | Shell::Zsh => Some(posix_wrapper(&binary)),
        Shell::Fish => Some(fish_wrapper(&binary)),
        Shell::PowerShell => Some(power_shell_wrapper(&binary)),
        Shell::Elvish => Some(elvish_wrapper(&binary)),
        Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Nushell | Shell::Batch => None,
    }
}

// ── POSIX arms (bash, zsh) ───────────────────────────────────────────────

fn bash_registration(binary: &str, watch_paths: &[PathBuf]) -> String {
    // `PROMPT_COMMAND` is probed apart from the function guard: an owner's overwrite drops our call
    // while the function stays defined.
    // ponytail: substring, not element equality, so a function named `…__ocx_prompt_hook…` suppresses it.
    let quoted = posix_single_quoted(binary);
    // `EXIT` is trapped only when free: `trap` replaces, so taking it would drop the user's handler.
    let body = posix_hook_body(&quoted, "bash", watch_paths);
    // `declare -p` is the only way to tell an array `PROMPT_COMMAND` from the string form.
    // Trailing `;` and whitespace are stripped first, or the append yields a `;;` syntax error.
    format!(
        "case $- in\n\
         *r*) : ;;\n\
         *)\n\
         if ! typeset -f __ocx_prompt_hook >/dev/null 2>&1; then\n\
         __ocx_stamp=\"$(command mktemp -t {STAMP_TEMPLATE} 2>/dev/null)\" || __ocx_stamp=''\n\
         if [ -n \"${{__ocx_stamp-}}\" ] && [ -z \"$(trap -p EXIT 2>/dev/null)\" ]; then trap '{POSIX_STAMP_CLEANUP}' EXIT; fi\n\
         {body}\n\
         fi\n\
         case \"$(declare -p PROMPT_COMMAND 2>/dev/null)\" in\n\
         *__ocx_prompt_hook*) : ;;\n\
         'declare -a'*) PROMPT_COMMAND+=(__ocx_prompt_hook) ;;\n\
         *)\n\
         __ocx_pc=\"${{PROMPT_COMMAND-}}\"\n\
         while :; do case \"$__ocx_pc\" in *';'|*' '|*'\t') __ocx_pc=\"${{__ocx_pc%?}}\" ;; *) break ;; esac; done\n\
         PROMPT_COMMAND=\"${{__ocx_pc:+$__ocx_pc;}}__ocx_prompt_hook\"\n\
         unset __ocx_pc\n\
         ;;\n\
         esac\n\
         ;;\n\
         esac"
    )
}

fn zsh_registration(binary: &str, watch_paths: &[PathBuf]) -> String {
    // `add-zsh-hook` is duplicate-safe for `precmd` and `zshexit` alike, so no probe is needed.
    let quoted = posix_single_quoted(binary);
    let body = posix_hook_body(&quoted, "zsh", watch_paths);
    // Outside the function guard, or a wholesale `precmd_functions=(…)` drops our entry for good.
    format!(
        "case $- in\n\
         *r*) : ;;\n\
         *)\n\
         if ! typeset -f __ocx_prompt_hook >/dev/null 2>&1; then\n\
         __ocx_stamp=\"$(command mktemp -t {STAMP_TEMPLATE} 2>/dev/null)\" || __ocx_stamp=''\n\
         {body}\n\
         __ocx_stamp_cleanup() {{ {POSIX_STAMP_CLEANUP}; }}\n\
         fi\n\
         autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook precmd __ocx_prompt_hook 2>/dev/null || true\n\
         add-zsh-hook zshexit __ocx_stamp_cleanup 2>/dev/null || true\n\
         ;;\n\
         esac"
    )
}

fn posix_hook_body(quoted_binary: &str, shell_name: &str, watch_paths: &[PathBuf]) -> String {
    // `local __ocx_status=$?` must stay first, or the prompt's inherited `$?` is lost.
    format!(
        "__ocx_prompt_hook() {{\n\
         local __ocx_status=$?\n\
         {check}\n\
         return $__ocx_status\n\
         }}",
        check = posix_reconcile(quoted_binary, shell_name, watch_paths)
    )
}

fn posix_reconcile(quoted_binary: &str, shell_name: &str, watch_paths: &[PathBuf]) -> String {
    // Builtins only, so an unchanged prompt execs nothing; `-` defaults keep every read `set -u`-safe.
    let newer: String = watch_paths
        .iter()
        .map(|path| {
            format!(
                " || [ '{path}' -nt \"${{__ocx_stamp-}}\" ]",
                path = posix_single_quoted(&path.to_string_lossy())
            )
        })
        .collect();
    // The `$PWD` term makes `cd` reconcile: the baked watch paths are the previous project's.
    format!(
        "if [ -x '{quoted_binary}' ] && {{ [ -z \"${{__OCX_ENV_STATE-}}\" ] || [ \"${{__ocx_pwd-}}\" != \"$PWD\" ] || [ \"${{__ocx_yield-}}\" != \"{yielded}\" ] || [ \"${{__ocx_grant-}}\" != \"{granted}\" ] || [ -z \"${{__ocx_stamp-}}\" ] || [ ! -f \"${{__ocx_stamp-}}\" ]{newer} || [ \"$PWD/{PROJECT_FILE}\" -nt \"${{__ocx_stamp-}}\" ]; }}; then\n\
         {apply}\n\
         fi",
        yielded = posix_signal(&YIELD_SIGNALS),
        granted = posix_signal(&GRANT_SIGNALS),
        apply = posix_apply(quoted_binary, shell_name)
    )
}

fn posix_apply(quoted_binary: &str, shell_name: &str) -> String {
    // Stderr and status discarded, so a binary predating `--reconcile` breaks no prompt.
    // Never refresh the stamp here: a failed run must leave it stale so the next prompt retries.
    format!(
        "eval \"$('{quoted_binary}' {OFFLINE} self activate --reconcile --shell={shell_name} 2>/dev/null)\" || true"
    )
}

fn posix_wrapper(binary: &str) -> String {
    let quoted = posix_single_quoted(binary);
    format!(
        "case $- in\n\
         *r*) : ;;\n\
         *)\n\
         ocx() {{\n\
         '{quoted}' \"$@\"\n\
         local __ocx_status=$?\n\
         {check}\n\
         return $__ocx_status\n\
         }}\n\
         ;;\n\
         esac",
        check = POSIX_HOOK_CALL
    )
}

// ── fish ─────────────────────────────────────────────────────────────────

fn fish_registration(binary: &str, watch_paths: &[PathBuf]) -> String {
    let quoted = fish_single_quoted(binary);
    // A named `--on-event fish_exit` handler displaces no other module's.
    format!(
        "if not functions -q __ocx_prompt_hook\n\
         set -g __ocx_stamp (command mktemp -t {STAMP_TEMPLATE} 2>/dev/null)\n\
         function __ocx_stamp_cleanup --on-event fish_exit\n\
         if test -n \"$__ocx_stamp\"; {FISH_STAMP_CLEANUP}; end\n\
         end\n\
         {function}\n\
         end",
        function = fish_hook_function(&quoted, watch_paths)
    )
}

/// The `__ocx_prompt_hook` function alone, shared by [`redefinition`] and [`fish_registration`] so they cannot drift.
fn fish_hook_function(quoted_binary: &str, watch_paths: &[PathBuf]) -> String {
    format!(
        "function __ocx_prompt_hook --on-event fish_prompt\n\
         set -l __ocx_status $status\n\
         {check}\n\
         return $__ocx_status\n\
         end",
        check = fish_reconcile(quoted_binary, watch_paths)
    )
}

fn fish_reconcile(quoted_binary: &str, watch_paths: &[PathBuf]) -> String {
    let newer: String = watch_paths
        .iter()
        .map(|path| {
            format!(
                "; or test '{path}' -nt \"$__ocx_stamp\"",
                path = fish_single_quoted(&path.to_string_lossy())
            )
        })
        .collect();
    format!(
        "if test -x '{quoted_binary}'; and begin; test -z \"$__OCX_ENV_STATE\"; or test \"$__ocx_pwd\" != \"$PWD\"; or test \"$__ocx_yield\" != \"{yielded}\"; or test \"$__ocx_grant\" != \"{granted}\"; or test -z \"$__ocx_stamp\"; or not test -f \"$__ocx_stamp\"{newer}; or test \"$PWD/{PROJECT_FILE}\" -nt \"$__ocx_stamp\"; end\n\
         {apply}\n\
         end",
        yielded = fish_signal(&YIELD_SIGNALS),
        granted = fish_signal(&GRANT_SIGNALS),
        apply = fish_apply(quoted_binary)
    )
}

fn fish_apply(quoted_binary: &str) -> String {
    format!("'{quoted_binary}' {OFFLINE} self activate --reconcile --shell=fish 2>/dev/null | source")
}

fn fish_wrapper(binary: &str) -> String {
    let quoted = fish_single_quoted(binary);
    format!(
        "function ocx\n\
         '{quoted}' $argv\n\
         set -l __ocx_status $status\n\
         if functions -q __ocx_prompt_hook\n\
         __ocx_prompt_hook\n\
         end\n\
         return $__ocx_status\n\
         end"
    )
}

// ── PowerShell ───────────────────────────────────────────────────────────

fn power_shell_registration(binary: &str, watch_paths: &[PathBuf]) -> String {
    // `Continue` preferences in the hook's own scope plus the `catch`, so a hook error never fails a prompt.
    // `Remove-Variable -Scope Local` restores them; reading the PS 5.1-absent preference throws under StrictMode.
    // `$?` and `$LASTEXITCODE` are captured on entry and restored, `$?` by the final statement's outcome.
    let quoted = single_quoted_doubled(binary);
    // Probe the live `prompt` body, not a marker variable a clobber leaves behind, so a clobber is re-wrapped.
    // `"$(…)"`, not `.ToString()`, which throws on `$null` under StrictMode.
    // ponytail: an owner that *wraps* ours makes a re-source re-wrap, so reconcile runs twice.
    format!(
        "if (\"$($function:prompt)\" -notmatch '__ocxReconcile') {{\n\
         $global:__ocxPrevPrompt = $function:prompt\n\
         $global:__ocxStamp = [datetime]::MinValue\n\
         $global:__ocxPwd = ''\n\
         $global:__ocxYield = ''\n\
         $global:__ocxGrant = ''\n\
         {function}\n\
         function global:prompt {{\n\
         $__ocxOk = $?\n\
         $__ocxLast = $global:LASTEXITCODE\n\
         try {{\n\
         try {{\n\
         $ErrorActionPreference = 'Continue'\n\
         $PSNativeCommandUseErrorActionPreference = $false\n\
         {call}\n\
         }} finally {{\n\
         Remove-Variable -Name ErrorActionPreference,PSNativeCommandUseErrorActionPreference -Scope Local -ErrorAction SilentlyContinue\n\
         }}\n\
         }} catch {{ }}\n\
         $__ocxOut = if ($global:__ocxPrevPrompt) {{ & $global:__ocxPrevPrompt }} else {{ \"PS $($ExecutionContext.SessionState.Path.CurrentLocation)$('>' * ($NestedPromptLevel + 1)) \" }}\n\
         $global:LASTEXITCODE = $__ocxLast\n\
         $__ocxOut\n\
         if ($__ocxOk) {{ $null = $true }} else {{ Write-Error -Message 'ocx' -ErrorAction Ignore }}\n\
         }}\n\
         }}",
        function = power_shell_reconcile_function(&quoted, watch_paths),
        call = PWSH_HOOK_CALL
    )
}

/// The `__ocxReconcile` function alone, for [`redefinition`].
///
/// Never reseeds the `$global:` state, or the reset stamp makes the next prompt exec unconditionally.
fn power_shell_reconcile_function(quoted_binary: &str, watch_paths: &[PathBuf]) -> String {
    format!(
        "function global:__ocxReconcile {{\n\
         {check}\n\
         }}",
        check = power_shell_reconcile(quoted_binary, watch_paths)
    )
}

fn power_shell_reconcile(quoted_binary: &str, watch_paths: &[PathBuf]) -> String {
    // An absent path reads as 1601-01-01 rather than throwing, so it counts once created.
    let newer: String = watch_paths
        .iter()
        .map(|path| {
            format!(
                " -or [System.IO.File]::GetLastWriteTimeUtc('{path}') -gt $global:__ocxStamp",
                path = single_quoted_doubled(&path.to_string_lossy())
            )
        })
        .collect();
    // `CurrentFileSystemLocation`, not `$PWD`: under `HKLM:` PS 5.1's `Combine` throws and kills the reconcile.
    // `[System.IO.Path]::Combine`, not `Join-Path`, which a user function could shadow.
    format!(
        "if (Test-Path -LiteralPath '{quoted_binary}' -PathType Leaf) {{\n\
         if ([string]::IsNullOrEmpty($env:__OCX_ENV_STATE) -or $global:__ocxPwd -ne $PWD.Path -or $global:__ocxYield -ne \"{yielded}\" -or $global:__ocxGrant -ne \"{granted}\"{newer} -or [System.IO.File]::GetLastWriteTimeUtc([System.IO.Path]::Combine($ExecutionContext.SessionState.Path.CurrentFileSystemLocation.Path, '{PROJECT_FILE}')) -gt $global:__ocxStamp) {{\n\
         {apply}\n\
         }}\n\
         }}",
        yielded = power_shell_signal(&YIELD_SIGNALS),
        granted = power_shell_signal(&GRANT_SIGNALS),
        apply = power_shell_apply(quoted_binary)
    )
}

fn power_shell_apply(quoted_binary: &str) -> String {
    format!(
        "& '{quoted_binary}' {OFFLINE} self activate --reconcile --shell=powershell 2>$null | Out-String | Invoke-Expression"
    )
}

fn power_shell_wrapper(binary: &str) -> String {
    // `$?` is replayed with `Write-Error -ErrorAction Ignore`, or it reads true after a failed `ocx`.
    // The `finally` reconciles even when the caller's `Stop` preferences make the call throw.
    let quoted = single_quoted_doubled(binary);
    // Workaround for PowerShell/PowerShell#21208: the binder eats a function call's first bare `--`.
    // Re-insert it from `$MyInvocation.Line` (PS 5.1 has no `.Statement`), matching this call by column,
    // or a nested or `;`-joined `ocx` is taken for it; `-x:1` binds as two `$args`.
    // A `--` on a continuation line or after a splat stays unrepaired; the `catch` keeps `$args` as bound.
    format!(
        "function global:ocx {{\n\
         $__ocxSt = $null\n\
         $__ocxOk = $true\n\
         $__ocxArgs = [System.Collections.Generic.List[object]]::new()\n\
         foreach ($__ocxArg in $args) {{ $__ocxArgs.Add($__ocxArg) }}\n\
         try {{\n\
         if ($MyInvocation.Line -match '(^|\\s)--(\\s|$)') {{\n\
         $__ocxColumn = $MyInvocation.OffsetInLine\n\
         $__ocxCommand = [System.Management.Automation.Language.Parser]::ParseInput($MyInvocation.Line, [ref]$null, [ref]$null).Find({{ param($node) $node -is [System.Management.Automation.Language.CommandAst] -and $node.Extent.StartLineNumber -eq 1 -and $node.Extent.StartColumnNumber -eq $__ocxColumn }}, $true)\n\
         if ($null -ne $__ocxCommand) {{\n\
         $__ocxAt = 0\n\
         $__ocxElements = $__ocxCommand.CommandElements\n\
         for ($__ocxIndex = 1; $__ocxIndex -lt $__ocxElements.Count; $__ocxIndex++) {{\n\
         $__ocxElement = $__ocxElements[$__ocxIndex]\n\
         if ($__ocxElement -is [System.Management.Automation.Language.CommandParameterAst]) {{\n\
         if ($__ocxElement.Extent.Text -eq '--') {{\n\
         if ($__ocxAt -le $__ocxArgs.Count) {{ $__ocxArgs.Insert($__ocxAt, '--') }}\n\
         break\n\
         }}\n\
         if ($null -ne $__ocxElement.Argument) {{ $__ocxAt += 2 }} else {{ $__ocxAt += 1 }}\n\
         }} elseif ($__ocxElement -is [System.Management.Automation.Language.VariableExpressionAst] -and $__ocxElement.Splatted) {{\n\
         break\n\
         }} else {{\n\
         $__ocxAt += 1\n\
         }}\n\
         }}\n\
         }}\n\
         }}\n\
         }} catch {{ }}\n\
         try {{\n\
         & '{quoted}' @__ocxArgs\n\
         $__ocxOk = $?\n\
         $__ocxSt = $LASTEXITCODE\n\
         }} finally {{\n\
         if ($null -eq $__ocxSt) {{ $__ocxSt = $LASTEXITCODE }}\n\
         try {{\n\
         $ErrorActionPreference = 'Continue'\n\
         $PSNativeCommandUseErrorActionPreference = $false\n\
         {call}\n\
         }} catch {{ }} finally {{\n\
         Remove-Variable -Name ErrorActionPreference,PSNativeCommandUseErrorActionPreference -Scope Local -ErrorAction SilentlyContinue\n\
         }}\n\
         $global:LASTEXITCODE = $__ocxSt\n\
         if ($__ocxOk) {{ $null = $true }} else {{ Write-Error -Message 'ocx' -ErrorAction Ignore }}\n\
         }}\n\
         }}",
        call = PWSH_HOOK_CALL
    )
}

// ── elvish ───────────────────────────────────────────────────────────────

/// The directory the last successful reconcile ran for, in the **process environment**.
///
/// Not a shell variable: an `eval` unit's `set` may not reach the caller's scope, nor the wrapper's.
const ELVISH_PWD_KEY: &str = "__OCX_ENV_PWD";

/// What [`ELVISH_PWD_KEY`] holds: the recording shell's pid, its `$pwd`, then the signal tails.
///
/// The pid keeps a child elvish, which inherits the variable, from reading as already reconciled.
fn elvish_pwd_value() -> String {
    format!(
        "(to-string $pid)' '$pwd{yielded}{granted}",
        yielded = elvish_signal(&YIELD_SIGNALS),
        granted = elvish_signal(&GRANT_SIGNALS)
    )
}

/// The "already registered" marker, carried as the registered closure's **rest-argument name**.
///
/// A declaration, so no user comment or string can forge it; `@` because elvish may pass hook arguments later.
const ELVISH_HOOK_MARKER: &str = "__ocx-prompt-hook";

/// The elvish expression that answers "does `list` already carry our closure?".
///
/// Structural over `arg-names`: a substring probe matches a user hook's comment and silently leaves the shell unhooked.
/// Only a `fn` is indexed with `[arg-names]` (indexing a string raises), so no user hook can abort registration.
fn elvish_already_registered(list: &str) -> String {
    format!(
        "(has-value [(all {list} | each {{|__ocx_candidate| \
         if (==s (kind-of $__ocx_candidate) fn) {{ all $__ocx_candidate[arg-names] }} }})] '{ELVISH_HOOK_MARKER}')"
    )
}

/// The closure [`elvish_registration`] appends; `quoted_binary` is already escaped for a single-quoted string.
fn elvish_hook_closure(quoted_binary: &str) -> String {
    format!(
        "{{|@{ELVISH_HOOK_MARKER}|\n\
         if (or (==s $E:{carrier} '') (!=s $E:{ELVISH_PWD_KEY} {value})) {{\n\
         try {{ eval ('{quoted_binary}' {OFFLINE} self activate --reconcile --shell=elvish 2>/dev/null | slurp) }} catch e {{ }}\n\
         }}\n\
         }}",
        carrier = super::reconcile::CARRIER_KEY,
        value = elvish_pwd_value(),
    )
}

// The body rides in `eval`: a direct `$edit:` is a compile error in non-interactive elvish that no `try` catches.
fn elvish_registration(binary: &str) -> String {
    // No watch-set term: elvish has no in-shell mtime, and an external `test -nt` would exec every prompt.
    // So an in-place `ocx.toml` edit reconciles only at the next `cd` or `ocx` call.
    let quoted = single_quoted_doubled(binary);
    // The marker lives in `$edit:before-readline`, never the env: `exec elvish` keeps both and would skip registering.
    let body = format!(
        "if (not {probe}) {{\n\
         set edit:before-readline = [$@edit:before-readline {closure}]\n\
         }}",
        probe = elvish_already_registered("$edit:before-readline"),
        closure = elvish_hook_closure(&quoted),
    );
    format!("try {{ eval '{}' }} catch e {{ }}", single_quoted_doubled(&body))
}

// Clears the recorded directory instead of reconciling: with no watch term a call would reconcile after every `ocx`.
// So elvish gives up same-line chaining; `ocx add --global foo && foo` sees the change a prompt later.
fn elvish_wrapper(binary: &str) -> String {
    // `edit:add-var`, not `fn`: a `fn` defined inside `eval` never escapes it.
    let quoted = single_quoted_doubled(binary);
    // `defer` clears even when the binary raises, and the exception propagates unchanged.
    let body = format!(
        "edit:add-var ocx~ {{|@__ocx_args| defer {{ set-env {ELVISH_PWD_KEY} '' }}; '{quoted}' $@__ocx_args }}"
    );
    format!("try {{ eval '{}' }} catch e {{ }}", single_quoted_doubled(&body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap_builder::ValueEnum as _;

    fn binary() -> PathBuf {
        PathBuf::from("/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx")
    }

    fn watch() -> Vec<PathBuf> {
        vec![PathBuf::from("/w/ocx.toml"), PathBuf::from("/w/ocx.lock")]
    }

    /// What an arm's per-prompt guard is made of.
    ///
    /// One exhaustive match ([`guard_kind`]) is the single source for every
    /// per-arm list in this module, so a newly added `Shell` variant is a
    /// compile error here rather than a variant that quietly lands on the
    /// permissive side of a hand-maintained array. The two hand-maintained
    /// arrays this replaces were unrelated to each other: an arm added to the
    /// hooked list alone escaped the watch-path test with nothing in its place.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum GuardKind {
        /// Carrier, `$PWD` and a stamp compared against every watch member.
        WatchGuarded,
        /// Carrier and `$pwd` only — elvish, which exposes no file timestamp.
        CarrierAndPwd,
        /// No prompt hook at all, so no guard and no wrapper.
        None,
    }

    /// The kind of guard `shell` emits. Deliberately spelled out rather than
    /// derived from the emitted text: a test that read the answer off the
    /// emission could not fail when the emission is what regressed.
    fn guard_kind(shell: Shell) -> GuardKind {
        match shell {
            Shell::Bash | Shell::Zsh | Shell::Fish | Shell::PowerShell => GuardKind::WatchGuarded,
            Shell::Elvish => GuardKind::CarrierAndPwd,
            Shell::Ash | Shell::Ksh | Shell::Dash | Shell::Nushell | Shell::Batch => GuardKind::None,
        }
    }

    fn shells_where(kind: GuardKind) -> Vec<Shell> {
        Shell::value_variants()
            .iter()
            .copied()
            .filter(|shell| guard_kind(*shell) == kind)
            .collect()
    }

    /// The arms that host a prompt hook, and therefore a wrapper.
    fn hooked() -> Vec<Shell> {
        Shell::value_variants()
            .iter()
            .copied()
            .filter(|shell| guard_kind(*shell) != GuardKind::None)
            .collect()
    }

    fn every_emitted_body() -> Vec<(Shell, String)> {
        let mut bodies = Vec::new();
        for shell in Shell::value_variants() {
            if let Some(text) = registration(*shell, &binary(), &watch()) {
                bodies.push((*shell, text));
            }
            if let Some(text) = wrapper(*shell, &binary()) {
                bodies.push((*shell, text));
            }
        }
        bodies
    }

    // ── exact emitted text, per arm ──────────────────────────────────────

    #[test]
    fn bash_registration_emits_exactly() {
        assert_eq!(
            registration(Shell::Bash, &binary(), &watch()).as_deref(),
            Some(
                "case $- in\n\
                 *r*) : ;;\n\
                 *)\n\
                 if ! typeset -f __ocx_prompt_hook >/dev/null 2>&1; then\n\
                 __ocx_stamp=\"$(command mktemp -t ocx-env-stamp.XXXXXXXX 2>/dev/null)\" || __ocx_stamp=''\n\
                 if [ -n \"${__ocx_stamp-}\" ] && [ -z \"$(trap -p EXIT 2>/dev/null)\" ]; then trap 'command rm -f \"${__ocx_stamp-}\" 2>/dev/null' EXIT; fi\n\
                 __ocx_prompt_hook() {\n\
                 local __ocx_status=$?\n\
                 if [ -x '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' ] && { [ -z \"${__OCX_ENV_STATE-}\" ] || [ \"${__ocx_pwd-}\" != \"$PWD\" ] || [ \"${__ocx_yield-}\" != \"${DIRENV_DIR-}|${MISE_SHELL-}|${__MISE_ORIG_PATH-}\" ] || [ \"${__ocx_grant-}\" != \"${OCX_CONSENT_PATHS-}|${OCX_CONSENT_NAMESPACES-}\" ] || [ -z \"${__ocx_stamp-}\" ] || [ ! -f \"${__ocx_stamp-}\" ] || [ '/w/ocx.toml' -nt \"${__ocx_stamp-}\" ] || [ '/w/ocx.lock' -nt \"${__ocx_stamp-}\" ] || [ \"$PWD/ocx.toml\" -nt \"${__ocx_stamp-}\" ]; }; then\n\
                 eval \"$('/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' --offline self activate --reconcile --shell=bash 2>/dev/null)\" || true\n\
                 fi\n\
                 return $__ocx_status\n\
                 }\n\
                 fi\n\
                 case \"$(declare -p PROMPT_COMMAND 2>/dev/null)\" in\n\
                 *__ocx_prompt_hook*) : ;;\n\
                 'declare -a'*) PROMPT_COMMAND+=(__ocx_prompt_hook) ;;\n\
                 *)\n\
                 __ocx_pc=\"${PROMPT_COMMAND-}\"\n\
                 while :; do case \"$__ocx_pc\" in *';'|*' '|*'\t') __ocx_pc=\"${__ocx_pc%?}\" ;; *) break ;; esac; done\n\
                 PROMPT_COMMAND=\"${__ocx_pc:+$__ocx_pc;}__ocx_prompt_hook\"\n\
                 unset __ocx_pc\n\
                 ;;\n\
                 esac\n\
                 ;;\n\
                 esac"
            )
        );
    }

    #[test]
    fn zsh_registration_emits_exactly() {
        assert_eq!(
            registration(Shell::Zsh, &binary(), &watch()).as_deref(),
            Some(
                "case $- in\n\
                 *r*) : ;;\n\
                 *)\n\
                 if ! typeset -f __ocx_prompt_hook >/dev/null 2>&1; then\n\
                 __ocx_stamp=\"$(command mktemp -t ocx-env-stamp.XXXXXXXX 2>/dev/null)\" || __ocx_stamp=''\n\
                 __ocx_prompt_hook() {\n\
                 local __ocx_status=$?\n\
                 if [ -x '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' ] && { [ -z \"${__OCX_ENV_STATE-}\" ] || [ \"${__ocx_pwd-}\" != \"$PWD\" ] || [ \"${__ocx_yield-}\" != \"${DIRENV_DIR-}|${MISE_SHELL-}|${__MISE_ORIG_PATH-}\" ] || [ \"${__ocx_grant-}\" != \"${OCX_CONSENT_PATHS-}|${OCX_CONSENT_NAMESPACES-}\" ] || [ -z \"${__ocx_stamp-}\" ] || [ ! -f \"${__ocx_stamp-}\" ] || [ '/w/ocx.toml' -nt \"${__ocx_stamp-}\" ] || [ '/w/ocx.lock' -nt \"${__ocx_stamp-}\" ] || [ \"$PWD/ocx.toml\" -nt \"${__ocx_stamp-}\" ]; }; then\n\
                 eval \"$('/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' --offline self activate --reconcile --shell=zsh 2>/dev/null)\" || true\n\
                 fi\n\
                 return $__ocx_status\n\
                 }\n\
                 __ocx_stamp_cleanup() { command rm -f \"${__ocx_stamp-}\" 2>/dev/null; }\n\
                 fi\n\
                 autoload -Uz add-zsh-hook 2>/dev/null && add-zsh-hook precmd __ocx_prompt_hook 2>/dev/null || true\n\
                 add-zsh-hook zshexit __ocx_stamp_cleanup 2>/dev/null || true\n\
                 ;;\n\
                 esac"
            )
        );
    }

    #[test]
    fn fish_registration_emits_exactly() {
        assert_eq!(
            registration(Shell::Fish, &binary(), &watch()).as_deref(),
            Some(
                "if not functions -q __ocx_prompt_hook\n\
                 set -g __ocx_stamp (command mktemp -t ocx-env-stamp.XXXXXXXX 2>/dev/null)\n\
                 function __ocx_stamp_cleanup --on-event fish_exit\n\
                 if test -n \"$__ocx_stamp\"; command rm -f \"$__ocx_stamp\" 2>/dev/null; end\n\
                 end\n\
                 function __ocx_prompt_hook --on-event fish_prompt\n\
                 set -l __ocx_status $status\n\
                 if test -x '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx'; and begin; test -z \"$__OCX_ENV_STATE\"; or test \"$__ocx_pwd\" != \"$PWD\"; or test \"$__ocx_yield\" != \"$DIRENV_DIR|$MISE_SHELL|$__MISE_ORIG_PATH\"; or test \"$__ocx_grant\" != \"$OCX_CONSENT_PATHS|$OCX_CONSENT_NAMESPACES\"; or test -z \"$__ocx_stamp\"; or not test -f \"$__ocx_stamp\"; or test '/w/ocx.toml' -nt \"$__ocx_stamp\"; or test '/w/ocx.lock' -nt \"$__ocx_stamp\"; or test \"$PWD/ocx.toml\" -nt \"$__ocx_stamp\"; end\n\
                 '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' --offline self activate --reconcile --shell=fish 2>/dev/null | source\n\
                 end\n\
                 return $__ocx_status\n\
                 end\n\
                 end"
            )
        );
    }

    #[test]
    fn power_shell_registration_emits_exactly() {
        assert_eq!(
            registration(Shell::PowerShell, &binary(), &watch()).as_deref(),
            Some(
                "if (\"$($function:prompt)\" -notmatch '__ocxReconcile') {\n\
                 $global:__ocxPrevPrompt = $function:prompt\n\
                 $global:__ocxStamp = [datetime]::MinValue\n\
                 $global:__ocxPwd = ''\n\
                 $global:__ocxYield = ''\n\
                 $global:__ocxGrant = ''\n\
                 function global:__ocxReconcile {\n\
                 if (Test-Path -LiteralPath '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' -PathType Leaf) {\n\
                 if ([string]::IsNullOrEmpty($env:__OCX_ENV_STATE) -or $global:__ocxPwd -ne $PWD.Path -or $global:__ocxYield -ne \"$($env:DIRENV_DIR)|$($env:MISE_SHELL)|$($env:__MISE_ORIG_PATH)\" -or $global:__ocxGrant -ne \"$($env:OCX_CONSENT_PATHS)|$($env:OCX_CONSENT_NAMESPACES)\" -or [System.IO.File]::GetLastWriteTimeUtc('/w/ocx.toml') -gt $global:__ocxStamp -or [System.IO.File]::GetLastWriteTimeUtc('/w/ocx.lock') -gt $global:__ocxStamp -or [System.IO.File]::GetLastWriteTimeUtc([System.IO.Path]::Combine($ExecutionContext.SessionState.Path.CurrentFileSystemLocation.Path, 'ocx.toml')) -gt $global:__ocxStamp) {\n\
                 & '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' --offline self activate --reconcile --shell=powershell 2>$null | Out-String | Invoke-Expression\n\
                 }\n\
                 }\n\
                 }\n\
                 function global:prompt {\n\
                 $__ocxOk = $?\n\
                 $__ocxLast = $global:LASTEXITCODE\n\
                 try {\n\
                 try {\n\
                 $ErrorActionPreference = 'Continue'\n\
                 $PSNativeCommandUseErrorActionPreference = $false\n\
                 if (Test-Path function:global:__ocxReconcile) { __ocxReconcile }\n\
                 } finally {\n\
                 Remove-Variable -Name ErrorActionPreference,PSNativeCommandUseErrorActionPreference -Scope Local -ErrorAction SilentlyContinue\n\
                 }\n\
                 } catch { }\n\
                 $__ocxOut = if ($global:__ocxPrevPrompt) { & $global:__ocxPrevPrompt } else { \"PS $($ExecutionContext.SessionState.Path.CurrentLocation)$('>' * ($NestedPromptLevel + 1)) \" }\n\
                 $global:LASTEXITCODE = $__ocxLast\n\
                 $__ocxOut\n\
                 if ($__ocxOk) { $null = $true } else { Write-Error -Message 'ocx' -ErrorAction Ignore }\n\
                 }\n\
                 }"
            )
        );
    }

    #[test]
    fn bash_wrapper_emits_exactly() {
        assert_eq!(
            wrapper(Shell::Bash, &binary()).as_deref(),
            Some(
                "case $- in\n\
                 *r*) : ;;\n\
                 *)\n\
                 ocx() {\n\
                 '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' \"$@\"\n\
                 local __ocx_status=$?\n\
                 if typeset -f __ocx_prompt_hook >/dev/null 2>&1; then __ocx_prompt_hook; fi\n\
                 return $__ocx_status\n\
                 }\n\
                 ;;\n\
                 esac"
            )
        );
    }

    #[test]
    fn zsh_wrapper_emits_exactly() {
        assert_eq!(
            wrapper(Shell::Zsh, &binary()).as_deref(),
            Some(
                "case $- in\n\
                 *r*) : ;;\n\
                 *)\n\
                 ocx() {\n\
                 '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' \"$@\"\n\
                 local __ocx_status=$?\n\
                 if typeset -f __ocx_prompt_hook >/dev/null 2>&1; then __ocx_prompt_hook; fi\n\
                 return $__ocx_status\n\
                 }\n\
                 ;;\n\
                 esac"
            )
        );
    }

    #[test]
    fn fish_wrapper_emits_exactly() {
        assert_eq!(
            wrapper(Shell::Fish, &binary()).as_deref(),
            Some(
                "function ocx\n\
                 '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' $argv\n\
                 set -l __ocx_status $status\n\
                 if functions -q __ocx_prompt_hook\n\
                 __ocx_prompt_hook\n\
                 end\n\
                 return $__ocx_status\n\
                 end"
            )
        );
    }

    #[test]
    fn power_shell_wrapper_emits_exactly() {
        assert_eq!(
            wrapper(Shell::PowerShell, &binary()).as_deref(),
            Some(
                "function global:ocx {\n\
                 $__ocxSt = $null\n\
                 $__ocxOk = $true\n\
                 $__ocxArgs = [System.Collections.Generic.List[object]]::new()\n\
                 foreach ($__ocxArg in $args) { $__ocxArgs.Add($__ocxArg) }\n\
                 try {\n\
                 if ($MyInvocation.Line -match '(^|\\s)--(\\s|$)') {\n\
                 $__ocxColumn = $MyInvocation.OffsetInLine\n\
                 $__ocxCommand = [System.Management.Automation.Language.Parser]::ParseInput($MyInvocation.Line, [ref]$null, [ref]$null).Find({ param($node) $node -is [System.Management.Automation.Language.CommandAst] -and $node.Extent.StartLineNumber -eq 1 -and $node.Extent.StartColumnNumber -eq $__ocxColumn }, $true)\n\
                 if ($null -ne $__ocxCommand) {\n\
                 $__ocxAt = 0\n\
                 $__ocxElements = $__ocxCommand.CommandElements\n\
                 for ($__ocxIndex = 1; $__ocxIndex -lt $__ocxElements.Count; $__ocxIndex++) {\n\
                 $__ocxElement = $__ocxElements[$__ocxIndex]\n\
                 if ($__ocxElement -is [System.Management.Automation.Language.CommandParameterAst]) {\n\
                 if ($__ocxElement.Extent.Text -eq '--') {\n\
                 if ($__ocxAt -le $__ocxArgs.Count) { $__ocxArgs.Insert($__ocxAt, '--') }\n\
                 break\n\
                 }\n\
                 if ($null -ne $__ocxElement.Argument) { $__ocxAt += 2 } else { $__ocxAt += 1 }\n\
                 } elseif ($__ocxElement -is [System.Management.Automation.Language.VariableExpressionAst] -and $__ocxElement.Splatted) {\n\
                 break\n\
                 } else {\n\
                 $__ocxAt += 1\n\
                 }\n\
                 }\n\
                 }\n\
                 }\n\
                 } catch { }\n\
                 try {\n\
                 & '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' @__ocxArgs\n\
                 $__ocxOk = $?\n\
                 $__ocxSt = $LASTEXITCODE\n\
                 } finally {\n\
                 if ($null -eq $__ocxSt) { $__ocxSt = $LASTEXITCODE }\n\
                 try {\n\
                 $ErrorActionPreference = 'Continue'\n\
                 $PSNativeCommandUseErrorActionPreference = $false\n\
                 if (Test-Path function:global:__ocxReconcile) { __ocxReconcile }\n\
                 } catch { } finally {\n\
                 Remove-Variable -Name ErrorActionPreference,PSNativeCommandUseErrorActionPreference -Scope Local -ErrorAction SilentlyContinue\n\
                 }\n\
                 $global:LASTEXITCODE = $__ocxSt\n\
                 if ($__ocxOk) { $null = $true } else { Write-Error -Message 'ocx' -ErrorAction Ignore }\n\
                 }\n\
                 }"
            )
        );
    }

    #[test]
    fn elvish_registration_emits_exactly() {
        // The one arm at quoting depth 2: the body is a single-quoted elvish
        // string inside another, so every quote it carries is doubled twice. A
        // `contains` needle cannot see a mis-nesting one layer out, which is why
        // this arm needs the pin more than the four that already have one.
        assert_eq!(
            registration(Shell::Elvish, &binary(), &watch()).as_deref(),
            Some(
                "try { eval 'if (not (has-value [(all $edit:before-readline | each {|__ocx_candidate| \
                 if (==s (kind-of $__ocx_candidate) fn) { all $__ocx_candidate[arg-names] } })] \
                 ''__ocx-prompt-hook'')) {\n\
                 set edit:before-readline = [$@edit:before-readline {|@__ocx-prompt-hook|\n\
                 if (or (==s $E:__OCX_ENV_STATE '''') (!=s $E:__OCX_ENV_PWD (to-string $pid)'' ''$pwd'' ''$E:DIRENV_DIR'' ''$E:MISE_SHELL'' ''$E:__MISE_ORIG_PATH'' ''$E:OCX_CONSENT_PATHS'' ''$E:OCX_CONSENT_NAMESPACES)) {\n\
                 try { eval (''/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx'' --offline self activate --reconcile --shell=elvish 2>/dev/null | slurp) } catch e { }\n\
                 }\n\
                 }]\n\
                 }' } catch e { }"
            )
        );
    }

    #[test]
    fn elvish_wrapper_emits_exactly() {
        assert_eq!(
            wrapper(Shell::Elvish, &binary()).as_deref(),
            Some(
                "try { eval 'edit:add-var ocx~ {|@__ocx_args| defer { set-env __OCX_ENV_PWD '''' }; \
                 ''/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx'' $@__ocx_args }' } catch e { }"
            )
        );
    }

    /// A-35 for the elvish arm, as far as emitted text can carry it.
    ///
    /// The live rig runs `elvish -c`, which binds no `edit:` namespace, so the
    /// emitted wrapper's `edit:add-var` raises there and `ocx` is never defined —
    /// the rig cannot exercise this arm at all, and a pty would need a dependency
    /// the test suite does not have. Verified by hand instead, in a real pty
    /// against elvish 0.21: `ocx` resolved to the wrapper, ran the real binary
    /// through a path containing a quote, and the cleared directory made the next
    /// prompt reconcile.
    ///
    /// What is pinned here is the shape the claim rests on: the real binary's
    /// invocation is the wrapper's **last** statement, so no statement of ours
    /// runs after it to swallow or rewrite the exception a non-zero exit raises,
    /// and the `defer` that clears the recorded directory is registered *before*
    /// the call so it still runs on the raising path.
    #[test]
    fn the_elvish_wrapper_leaves_the_real_binarys_failure_alone() {
        let text = wrapper(Shell::Elvish, &binary()).expect("elvish wraps");
        let call = format!("''{}'' $@__ocx_args }}' }} catch e {{ }}", binary().display());
        assert!(
            text.ends_with(&call),
            "the wrapped call must be the wrapper's last statement: {text}"
        );
        let defer = "defer { set-env __OCX_ENV_PWD '''' }";
        let (Some(defer_at), Some(call_at)) = (text.find(defer), text.find(&call)) else {
            panic!("wrapper lost either its defer or its wrapped call: {text}");
        };
        assert!(
            defer_at < call_at,
            "the defer must be registered before the call, or a raising call skips it: {text}"
        );
    }

    #[test]
    fn elvish_checkpoint_emits_exactly() {
        assert_eq!(
            checkpoint(Shell::Elvish).as_deref(),
            Some(
                "set-env __OCX_ENV_PWD (to-string $pid)' '$pwd' '$E:DIRENV_DIR' '$E:MISE_SHELL' '$E:__MISE_ORIG_PATH' '$E:OCX_CONSENT_PATHS' '$E:OCX_CONSENT_NAMESPACES"
            )
        );
    }

    // ── the arms that emit nothing ───────────────────────────────────────

    #[test]
    fn arms_without_a_prompt_hook_emit_nothing() {
        let unhooked = shells_where(GuardKind::None);
        assert!(!unhooked.is_empty(), "the unhooked list must not be vacuous");
        for shell in &unhooked {
            assert_eq!(
                registration(*shell, &binary(), &watch()),
                None,
                "{shell} must host no hook"
            );
            assert_eq!(wrapper(*shell, &binary()), None, "{shell} must host no wrapper");
        }
        // Exhaustive by construction: both lists come from the one `guard_kind`
        // match, so a newly added `Shell` variant fails to compile there rather
        // than landing silently on whichever side no test covers. The sum is
        // asserted anyway — it is what makes a future third hooked kind visible
        // here instead of quietly dropping out of both lists.
        assert_eq!(
            Shell::value_variants().len(),
            hooked().len() + unhooked.len(),
            "every variant must be classified exactly once"
        );
    }

    /// The parity `guard_kind` exists to enforce: a `WatchGuarded` arm carries
    /// every watch-path literal, a `CarrierAndPwd` arm carries none.
    ///
    /// The two halves are one test because either alone is satisfiable by an arm
    /// that emits nothing at all — the emptiness assertion is what rules that
    /// out.
    #[test]
    fn every_arm_guards_the_way_its_kind_says_it_does() {
        for shell in hooked() {
            let text = registration(shell, &binary(), &watch()).unwrap_or_default();
            assert!(!text.is_empty(), "{shell} must register something");
            for path in watch() {
                let literal = path.to_string_lossy().to_string();
                match guard_kind(shell) {
                    GuardKind::WatchGuarded => assert!(
                        text.contains(&literal),
                        "{shell} is watch-guarded but never names {literal}: {text}"
                    ),
                    GuardKind::CarrierAndPwd => assert!(
                        !text.contains(&literal),
                        "{shell} has no in-shell mtime, so {literal} in its guard is dead weight: {text}"
                    ),
                    GuardKind::None => unreachable!("hooked() excludes it"),
                }
            }
        }
    }

    // ── A-36: the guard sees the same sentinels the detector reads ───────

    /// Every guarded arm folds [`YIELD_SIGNALS`] into both halves of its
    /// checkpoint/guard pair.
    ///
    /// Both halves, because either alone is satisfiable by a broken arm: an arm
    /// that records the sentinels but never compares them is quiet forever, and
    /// an arm that compares them against something it never records reconciles on
    /// every single prompt — C-044's exec storm. Their agreement is what makes
    /// the term fire exactly on a change, in either direction.
    ///
    /// [`GRANT_SIGNALS`] rides the same assertion: the fingerprint fold reads
    /// exactly that list ([`the_fingerprint_fold_reads_exactly_the_grant_signals`]),
    /// so a name in it the guard cannot see is a grant the fold never gets to
    /// notice — the binary is not invoked (EC-HOOK-019, ocx-sh/ocx#442).
    #[test]
    fn every_guard_reads_the_sentinels_its_checkpoint_records() {
        for shell in hooked() {
            let guard = registration(shell, &binary(), &watch()).unwrap_or_default();
            let checkpoint = checkpoint(shell).unwrap_or_default();
            for key in YIELD_SIGNALS.iter().chain(&GRANT_SIGNALS) {
                assert!(
                    guard.contains(key),
                    "{shell}'s guard cannot see {key}, so a value exported mid-session never reaches \
                     the reconciler (A-36, #442): {guard}"
                );
                assert!(
                    checkpoint.contains(key),
                    "{shell} records no {key}, so its guard compares against a value that never \
                     moves: {checkpoint}"
                );
            }
        }
    }

    /// The EC-HOOK-019 (#442) tripwire, fold side: `current_fingerprint` reads
    /// its environment through `GRANT_SIGNAL_VARS` and nowhere else.
    ///
    /// The guard test above proves every name in the list is a guard term; this
    /// proves the list is the fold's whole env read set, so the two layers of
    /// the staleness check cannot come apart the way they did when the fold
    /// spelled `OCX_CONSENT_PATHS` by hand and the guard never heard of it. Same
    /// source-scan shape as [`the_yield_sentinels_are_exactly_what_the_detector_reads`].
    ///
    /// Scoped to the two fold functions: `watch_paths` in the same file reads
    /// `OCX_CONFIG` to *name a member*, which is a different question (the
    /// path list is recorded in the ledger and re-derived from it).
    #[test]
    fn the_fingerprint_fold_reads_exactly_the_grant_signals() {
        let source = include_str!("reconcile/fingerprint.rs");
        let body = |name: &str| {
            let start = source
                .find(&format!("pub fn {name}("))
                .expect("the fold function exists");
            let end = source[start..].find("\n}\n").expect("the fold function ends") + start;
            &source[start..end]
        };
        // `.get` covers every registry read (`get`, `get_os`, `get_raw`, `get_slot`); `env::var`
        // covers a raw `std::env::var`/`var_os` that bypasses the registry.
        let reads = |text: &str| {
            text.matches(".get").count() + text.matches("ocx_env::dynamic").count() + text.matches("env::var").count()
        };
        assert_eq!(
            reads(body("fingerprint")),
            0,
            "the pure fold read the environment; its inputs are parameters (A-13)"
        );
        let current = body("current_fingerprint");
        assert_eq!(
            reads(current),
            1,
            "current_fingerprint reads the environment somewhere other than the GRANT_SIGNAL_VARS map; \
             a variable folded there that the guard does not watch is #442 again"
        );
        assert!(
            current.contains("GRANT_SIGNAL_VARS.map("),
            "current_fingerprint must read its names through GRANT_SIGNAL_VARS so the guard and the \
             fold share one list"
        );
    }

    // ── #397: a project that appears under an unchanged $PWD ─────────────

    /// EC-HOOK-018 — every watch-guarded arm tests the **live** `$PWD` for
    /// [`PROJECT_FILE`].
    ///
    /// The baked watch set names a project's files only when a project resolved,
    /// so without this term `ocx init` in a project-free directory moves nothing
    /// the guard can see and the shell stays inert until the next `cd`
    /// (ocx-sh/ocx#397).
    ///
    /// What is asserted is the **live** spelling, not the file name: `watch()`
    /// already puts `/w/ocx.toml` in every guarded body, so a `contains("ocx.toml")`
    /// would pass against the very emission this term exists to repair. A term
    /// baked at emission time answers for the directory the *last* reconcile ran
    /// in — the value that is already stale in this case.
    #[test]
    fn every_watch_guarded_arm_tests_the_live_pwd_for_a_project_file() {
        for shell in shells_where(GuardKind::WatchGuarded) {
            let guard = registration(shell, &binary(), &watch()).expect("a guarded arm registers");
            // pwsh reads the provider-independent filesystem location; the POSIX
            // family and fish read `$PWD` directly.
            let term = match shell {
                Shell::PowerShell => format!("CurrentFileSystemLocation.Path, '{PROJECT_FILE}')"),
                _ => format!("\"$PWD/{PROJECT_FILE}\""),
            };
            assert!(
                guard.contains(&term),
                "{shell}'s guard never tests the live $PWD for {PROJECT_FILE}, so a project created \
                 in place stays inert until the next `cd` (#397); looked for `{term}` in: {guard}"
            );
        }
    }

    /// The tripwire against the two lists coming apart.
    ///
    /// [`YIELD_SIGNALS`] is a second spelling of what
    /// [`super::super::coexistence::detect`] reads. Both directions matter: a
    /// name here that the detector does not read costs a pointless reconcile,
    /// and a name the detector reads that is missing here is the A-36 defect
    /// all over again, silently, for that one tool.
    ///
    /// The first half sets each sentinel alone and expects a yield. The count
    /// half is a tripwire, not a contract — it keys on the number of env reads
    /// `detect` performs, which is the cheapest observable that moves when a
    /// fourth sentinel is added.
    #[test]
    fn the_yield_sentinels_are_exactly_what_the_detector_reads() {
        fn declared(name: &str) -> &'static ocx_env::EnvVar {
            ocx_env::all()
                .find(|var| var.name == name)
                .expect("a yield sentinel is a declared variable")
        }
        let project = tempfile::TempDir::new().expect("tempdir");
        let project_dir = project.path().to_str().expect("utf8 tempdir path");
        for key in YIELD_SIGNALS {
            let env = ocx_env::overrides::lock();
            for other in YIELD_SIGNALS {
                env.remove(declared(other));
            }
            env.set(declared(key), format!("-{project_dir}"));
            assert!(
                !super::super::coexistence::detect(project.path()).observed.is_empty(),
                "detect ignores {key}, so the guard reconciles on a sentinel nothing yields for"
            );
        }
        let detector = include_str!("coexistence.rs");
        assert_eq!(
            detector.matches("verbatim(&").count(),
            YIELD_SIGNALS.len(),
            "`detect` reads a different number of environment variables than the guard watches; \
             a sentinel it honours and the guard cannot see is a yield that never happens (A-36)"
        );
    }

    // ── C-046: `set -u` discipline ───────────────────────────────────────

    #[test]
    fn posix_ledger_reads_use_default_expansion() {
        // The carrier is unset on the first prompt by construction, so every
        // read of it must default-expand or `set -u` aborts the prompt. The
        // count assertion is what keeps this from passing vacuously if the
        // read is ever renamed away.
        for shell in [Shell::Bash, Shell::Zsh] {
            let text = registration(shell, &binary(), &watch()).unwrap_or_default();
            assert_eq!(
                text.matches("${__OCX_ENV_STATE-}").count(),
                1,
                "{shell} must read the carrier exactly once, default-expanded"
            );
            assert!(
                !text.contains("${__OCX_ENV_STATE}") && !text.contains("$__OCX_ENV_STATE\""),
                "{shell} must never read the carrier without a default: {text}"
            );
        }
    }

    #[test]
    fn posix_stamp_reads_use_default_expansion() {
        for shell in [Shell::Bash, Shell::Zsh] {
            for text in [
                registration(shell, &binary(), &watch()).unwrap_or_default(),
                wrapper(shell, &binary()).unwrap_or_default(),
            ] {
                assert!(
                    !text.contains("${__ocx_stamp}") && !text.contains("\"$__ocx_stamp\""),
                    "{shell} must default-expand every stamp read: {text}"
                );
            }
        }
    }

    // ── C-045: no emitted snippet may ever call bare `ocx` ───────────────

    #[test]
    fn every_ocx_invocation_uses_the_absolute_path_and_refuses_the_network() {
        // Positive form: each reconcile occurrence must be preceded by the
        // quoted absolute path, so the wrapper function named `ocx` can never
        // shadow the call (C-045). A pure denylist would miss the forms it
        // failed to enumerate.
        //
        // The needle carries `--offline` because the flag is worth 36 ms of
        // registry-client and TLS-root construction per prompt fire on a path
        // that is forbidden to use either. The per-body count comparison is
        // what keeps a call site that lost the flag from silently dropping out
        // of the match set instead of failing.
        let mut seen = 0;
        for (shell, text) in every_emitted_body() {
            // Elvish's whole body rides inside a single-quoted `eval` string, so
            // the path's own quotes are doubled a second time. The rule is the
            // same — an absolute path in quotes, never a bare `ocx` — and
            // spelling the doubled form out here is what keeps the assertion
            // from being weakened to a substring search that would also accept
            // an unquoted path.
            let quoted = if shell == Shell::Elvish {
                format!("''{}'' ", binary().display())
            } else {
                format!("'{}' ", binary().display())
            };
            for (offset, _) in text.match_indices("--offline self activate --reconcile") {
                seen += 1;
                assert!(
                    text[..offset].ends_with(&quoted),
                    "{shell} invokes the reconciler without the absolute path at {offset}: {text}"
                );
            }
            assert_eq!(
                text.matches("self activate --reconcile").count(),
                text.matches("--offline self activate --reconcile").count(),
                "{shell} reaches for the network on a prompt path that cannot use it: {text}"
            );
            // The wrapped invocation in the wrapper is the same rule.
            assert!(
                !text.contains("\nocx "),
                "{shell} must never start a command with a bare `ocx`: {text}"
            );
        }
        // One call site per hooked registration and nowhere else: the wrappers
        // call the registration's guarded function instead of carrying a second
        // copy of the invocation.
        assert_eq!(
            seen,
            hooked().len(),
            "expected one reconcile call site per hooked registration, saw {seen}"
        );
    }

    // ── P3: the wrapper reuses the prompt hook's guard ───────────────────

    #[test]
    fn every_wrapper_defers_to_the_prompt_hooks_own_guard() {
        // The shipped wrapper ran an unconditional reconcile after *every* ocx
        // invocation — 61.3 ms on `ocx version` against 4.3 ms direct. It now
        // calls the registration's guarded check, so a command that moved no
        // watch member execs nothing. Both halves are asserted: the call is
        // present, and no wrapper carries an invocation of its own.
        for shell in hooked() {
            let text = wrapper(shell, &binary()).unwrap_or_default();
            // Universal half: no wrapper carries a reconcile invocation of its
            // own, whatever it does with the hook's guard.
            assert!(
                !text.contains("self activate --reconcile"),
                "{shell} must not carry a second, unguarded reconcile: {text}"
            );
            let registration = registration(shell, &binary(), &watch()).unwrap_or_default();
            if shell == Shell::Elvish {
                // Elvish hands over by INVALIDATING the guard rather than
                // calling it: its guard has no watch-set term to consult (it has
                // no in-shell mtime), so a call could only reconcile
                // unconditionally — the tax this test exists to prevent.
                // Clearing the recorded directory makes the next prompt's own
                // guard fire instead, at no cost to the ocx command.
                assert!(
                    text.contains("defer { set-env __OCX_ENV_PWD '''' }"),
                    "elvish must invalidate the recorded directory on the way out: {text}"
                );
                assert!(
                    registration.contains("(!=s $E:__OCX_ENV_PWD (to-string $pid)'' ''$pwd'' ''$E:DIRENV_DIR'' ''$E:MISE_SHELL'' ''$E:__MISE_ORIG_PATH'' ''$E:OCX_CONSENT_PATHS'' ''$E:OCX_CONSENT_NAMESPACES)"),
                    "elvish's guard must read the variable its wrapper clears: {registration}"
                );
                continue;
            }
            let call = match shell {
                Shell::PowerShell => PWSH_HOOK_CALL,
                Shell::Fish => "if functions -q __ocx_prompt_hook\n__ocx_prompt_hook",
                _ => POSIX_HOOK_CALL,
            };
            assert!(text.contains(call), "{shell} must call the guarded hook: {text}");
            // The guarded function has to exist for the call to mean anything.
            let name = if shell == Shell::PowerShell {
                "function global:__ocxReconcile"
            } else {
                "__ocx_prompt_hook"
            };
            assert!(
                registration.contains(name),
                "{shell} registers no {name} for its wrapper to call: {registration}"
            );
        }
    }

    /// Every existence probe resolves in the namespace it is asking about, and
    /// only there.
    ///
    /// The defect class this closes is one probe answering a *wider* question
    /// than the one being asked. It arrived twice in this module, in two guises:
    ///
    /// - elvish searched a rendering of `$edit:before-readline` for a marker
    ///   *substring*, so any text mentioning it counted (see
    ///   [`the_elvish_probe_reads_a_parsed_declaration_not_rendered_source`]);
    /// - bash and zsh asked `command -v __ocx_prompt_hook`, which resolves
    ///   aliases, builtins and `$PATH` executables besides functions. An
    ///   executable file named `__ocx_prompt_hook` on `$PATH` therefore read as
    ///   "already registered" and the shell ran unhooked — and in
    ///   [`POSIX_HOOK_CALL`] it is worse than that, because the next word *runs*
    ///   what was found.
    ///
    /// `typeset -f` is zero exactly for a shell function (verified in bash 5 and
    /// zsh 5 against a `$PATH` executable, an alias, and a real function).
    /// fish's `functions -q` is already function-scoped; it is asserted here so a
    /// future widening reds.
    ///
    /// pwsh's probe reads the live `prompt` function's own body rather than the
    /// `$global:__ocxPrevPrompt` variable it used to (#347). That is a *narrowing*
    /// in the same direction as the rest of this test: the old probe asked about a
    /// variable that is not the registration and therefore outlived it, so a
    /// framework assigning `function global:prompt` read as "already registered".
    /// `"$($function:prompt)"` resolves in `function:` and nowhere else — string
    /// interpolation rather than `.ToString()`, which throws on a `$null` prompt
    /// under `Set-StrictMode`.
    #[test]
    fn every_existence_probe_is_scoped_to_the_namespace_it_asks_about() {
        for (shell, probes) in [
            (
                Shell::Bash,
                vec!["if ! typeset -f __ocx_prompt_hook >/dev/null 2>&1; then"],
            ),
            (
                Shell::Zsh,
                vec!["if ! typeset -f __ocx_prompt_hook >/dev/null 2>&1; then"],
            ),
            (Shell::Fish, vec!["if not functions -q __ocx_prompt_hook"]),
            (
                Shell::PowerShell,
                vec!["if (\"$($function:prompt)\" -notmatch '__ocxReconcile')"],
            ),
        ] {
            let text = registration(shell, &binary(), &watch()).expect("a hooked arm");
            for probe in probes {
                assert!(text.contains(probe), "{shell} must probe with `{probe}`: {text}");
            }
        }
        for shell in [Shell::Bash, Shell::Zsh] {
            let text = wrapper(shell, &binary()).expect("a hooked arm");
            assert!(
                text.contains("if typeset -f __ocx_prompt_hook >/dev/null 2>&1; then __ocx_prompt_hook; fi"),
                "{shell}'s wrapper must not call through a $PATH lookup: {text}"
            );
        }
        // The wider form must be gone from every emission, registration and
        // wrapper alike — fixing one call site and leaving a sibling is exactly
        // how this class survived the first pass.
        for (shell, text) in every_emitted_body() {
            assert!(
                !text.contains("command -v __ocx_prompt_hook"),
                "{shell} still resolves the hook name through $PATH: {text}"
            );
        }
    }

    #[test]
    fn no_emitted_body_reads_the_binary_pin() {
        // A-34 — the hook always resolves through `current`; the pin is a
        // downstream, re-entrant mechanism and must not reach this stream.
        for (shell, text) in every_emitted_body() {
            assert!(
                !text.contains("OCX_BINARY_PIN"),
                "{shell} must not read the pin: {text}"
            );
        }
    }

    // ── A-35: the wrapper returns the wrapped command's status ───────────

    #[test]
    fn wrapper_captures_the_wrapped_status_before_anything_else() {
        let path = binary();
        let path = path.display();
        for (shell, invocation, capture) in [
            (Shell::Bash, format!("'{path}' \"$@\"\n"), "local __ocx_status=$?"),
            (Shell::Zsh, format!("'{path}' \"$@\"\n"), "local __ocx_status=$?"),
            (Shell::Fish, format!("'{path}' $argv\n"), "set -l __ocx_status $status"),
            // Both captures, in order: `$?` first, because assigning it would
            // otherwise overwrite `$?` with the assignment's own success.
            // `$LASTEXITCODE` is untouched by assignments, so it can follow.
            (
                Shell::PowerShell,
                format!("& '{path}' @__ocxArgs\n"),
                "$__ocxOk = $?\n$__ocxSt = $LASTEXITCODE",
            ),
        ] {
            let text = wrapper(shell, &binary()).unwrap_or_default();
            let expected = format!("{invocation}{capture}");
            assert!(
                text.contains(&expected),
                "{shell} must capture the status immediately after the wrapped call: {text}"
            );
        }
    }

    #[test]
    fn wrapper_returns_the_captured_status_last() {
        for shell in [Shell::Bash, Shell::Zsh] {
            let text = wrapper(shell, &binary()).unwrap_or_default();
            assert!(
                text.contains("return $__ocx_status\n}"),
                "{shell} must return the captured status: {text}"
            );
        }
        let fish = wrapper(Shell::Fish, &binary()).unwrap_or_default();
        assert!(
            fish.contains("return $__ocx_status\nend"),
            "fish must return it: {fish}"
        );
        let pwsh = wrapper(Shell::PowerShell, &binary()).unwrap_or_default();
        assert!(
            pwsh.contains("$global:LASTEXITCODE = $__ocxSt\n"),
            "pwsh must restore the captured exit code: {pwsh}"
        );
        // A35-1 — `$?` is the second observable, and an assignment always
        // succeeds, so the exit-code restore cannot be the last statement.
        assert!(
            pwsh.contains("if ($__ocxOk) { $null = $true } else { Write-Error -Message 'ocx' -ErrorAction Ignore }\n}"),
            "pwsh must replay `$?` as its final statement, or `ocx nope; if ($?)` reads true: {pwsh}"
        );
        assert!(
            pwsh.contains("& '/home/u/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx' @__ocxArgs\n$__ocxOk = $?"),
            "pwsh must capture `$?` immediately after the wrapped call: {pwsh}"
        );
    }

    // ── restricted shells (plan §5 correction 12) ────────────────────────

    #[test]
    fn restricted_shells_are_a_silent_no_op() {
        // `rbash` / `rksh` forbid setting `PATH` and forbid invoking any
        // command containing `/`. `$-` carries `r` in both, and in zsh under
        // `set -r` — a builtin `case`, so the guard itself costs no exec and
        // prints nothing.
        for shell in [Shell::Bash, Shell::Zsh] {
            for text in [
                registration(shell, &binary(), &watch()).unwrap_or_default(),
                wrapper(shell, &binary()).unwrap_or_default(),
            ] {
                assert!(
                    text.starts_with("case $- in\n*r*) : ;;\n*)\n") && text.ends_with("\n;;\nesac"),
                    "{shell} must wrap the whole body in the restricted-shell guard: {text}"
                );
            }
        }
    }

    // ── C-043: append-only registration ──────────────────────────────────

    #[test]
    fn bash_appends_to_prompt_command_in_both_forms() {
        let text = registration(Shell::Bash, &binary(), &watch()).unwrap_or_default();
        assert!(
            text.contains("PROMPT_COMMAND+=(__ocx_prompt_hook)"),
            "array form: {text}"
        );
        assert!(
            text.contains("PROMPT_COMMAND=\"${__ocx_pc:+$__ocx_pc;}__ocx_prompt_hook\""),
            "string form must carry the previous value: {text}"
        );
        assert!(
            text.contains("*';'|*' '|*'\t'"),
            "a trailing separator must be stripped before concatenating (Warp#5219): {text}"
        );
    }

    #[test]
    fn zsh_registers_through_add_zsh_hook_and_never_defines_precmd() {
        let text = registration(Shell::Zsh, &binary(), &watch()).unwrap_or_default();
        assert!(text.contains("add-zsh-hook precmd __ocx_prompt_hook"), "{text}");
        assert!(
            !text.contains("precmd()"),
            "defining precmd() clobbers its owner: {text}"
        );
        assert!(
            !text.contains("precmd ()"),
            "defining precmd() clobbers its owner: {text}"
        );
    }

    #[test]
    fn power_shell_wraps_the_previous_prompt() {
        let text = registration(Shell::PowerShell, &binary(), &watch()).unwrap_or_default();
        assert!(
            text.contains("$global:__ocxPrevPrompt = $function:prompt"),
            "capture: {text}"
        );
        assert!(text.contains("& $global:__ocxPrevPrompt"), "call through: {text}");
    }

    #[test]
    fn fish_binds_a_named_prompt_event_handler() {
        let text = registration(Shell::Fish, &binary(), &watch()).unwrap_or_default();
        assert!(
            text.contains("function __ocx_prompt_hook --on-event fish_prompt"),
            "{text}"
        );
    }

    // ── A-22: pwsh runs under its own preferences and restores `$?` ──────

    #[test]
    fn power_shell_hook_scopes_its_preferences_and_restores_the_status() {
        let text = registration(Shell::PowerShell, &binary(), &watch()).unwrap_or_default();
        for needle in [
            "$__ocxOk = $?",
            "$__ocxLast = $global:LASTEXITCODE",
            "$ErrorActionPreference = 'Continue'",
            "$PSNativeCommandUseErrorActionPreference = $false",
            "} finally {",
            "Remove-Variable -Name ErrorActionPreference,PSNativeCommandUseErrorActionPreference -Scope Local",
            "} catch { }",
            "$global:LASTEXITCODE = $__ocxLast",
            "if ($__ocxOk) { $null = $true } else { Write-Error -Message 'ocx' -ErrorAction Ignore }",
        ] {
            assert!(text.contains(needle), "pwsh hook must carry {needle:?}: {text}");
        }
    }

    // ── C-044: the watch set reaches the emitted body ────────────────────

    #[test]
    fn every_watch_path_reaches_every_hooked_arm() {
        for shell in shells_where(GuardKind::WatchGuarded) {
            let text = registration(shell, &binary(), &watch()).unwrap_or_default();
            for path in watch() {
                assert!(
                    text.contains(&path.to_string_lossy().to_string()),
                    "{shell} must carry {path:?}: {text}"
                );
            }
        }
    }

    #[test]
    fn elvish_guard_says_what_it_has_instead_of_a_watch_term() {
        // `every_arm_guards_the_way_its_kind_says_it_does` owns the negative
        // half — no watch path may appear. That half alone would pass for an arm
        // that lost its guard entirely, so the two terms elvish *does* evaluate
        // are asserted positively here.
        let text = registration(Shell::Elvish, &binary(), &watch()).expect("elvish registers");
        assert!(
            text.contains("(==s $E:__OCX_ENV_STATE '''')"),
            "elvish must still reconcile on an empty carrier (C-012's repair gesture): {text}"
        );
        assert!(
            text.contains("(!=s $E:__OCX_ENV_PWD (to-string $pid)'' ''$pwd'' ''$E:DIRENV_DIR'' ''$E:MISE_SHELL'' ''$E:__MISE_ORIG_PATH'' ''$E:OCX_CONSENT_PATHS'' ''$E:OCX_CONSENT_NAMESPACES)"),
            "elvish must still reconcile on a directory change (C-019 member 7): {text}"
        );
        // The pid half is what a child elvish's first prompt depends on: the key
        // is exported, so a bare `$pwd` recording would already match in a child
        // standing in the same directory, and A-21's deferred messages would
        // never be printed there.
        let checkpoint = checkpoint(Shell::Elvish).expect("elvish checkpoints");
        assert!(
            checkpoint.contains("(to-string $pid)' '$pwd"),
            "the checkpoint must record the pid the guard compares against: {checkpoint}"
        );
        // A-36's third term, on the arm with the fewest of them: without it a
        // direnv session appearing mid-session moves nothing elvish can see.
        assert!(
            text.contains("$E:DIRENV_DIR") && checkpoint.contains("$E:DIRENV_DIR"),
            "elvish must reconcile when a coexisting tool goes live (A-36): {text}"
        );
    }

    /// Idempotency keys on the shell, not the process — the `exec elvish` defect.
    ///
    /// `exec` keeps the pid and inherits the environment, so a marker in either
    /// reads as "already registered" in a shell that has registered nothing. The
    /// probe therefore has to read a store a new process image starts empty, and
    /// `$edit:before-readline` is the only one an `eval` unit can both write and
    /// read. Verified in a real pty against elvish 0.21: with the pid marker the
    /// post-`exec` shell registered nothing and `cd` never fired; with this probe
    /// it registers and fires.
    #[test]
    fn elvish_registration_probes_the_shells_own_hook_list_not_the_environment() {
        let text = registration(Shell::Elvish, &binary(), &watch()).expect("elvish registers");
        assert!(
            text.contains("(all $edit:before-readline | each {|__ocx_candidate|"),
            "the idempotency probe must read $edit:before-readline: {text}"
        );
        // The marker has to ride inside the registered closure, or the probe can
        // never find what it just registered.
        assert!(
            text.contains("[$@edit:before-readline {|@__ocx-prompt-hook|\n"),
            "the marker must ride inside the appended closure: {text}"
        );
        // No environment-carried registration marker may come back: it is exactly
        // what `exec` defeats, and a second marker would be a second answer.
        assert!(
            !text.contains("__OCX_ENV_HOOK"),
            "an exported registration marker cannot distinguish an exec'd shell: {text}"
        );
    }

    /// The elvish probe reads a *parsed declaration*, never rendered source.
    ///
    /// The shipped form was `str:contains (to-string $edit:before-readline)
    /// '__ocx-prompt-hook'`. `to-string` renders every closure in the list with
    /// its `&def` (its literal body, comments included) and its `&src` (the whole
    /// source of the `eval` unit that defined it), and the marker was searched as
    /// an undifferentiated substring — so a user's own pre-existing hook that
    /// merely *mentioned* the marker suppressed ocx's registration entirely, for
    /// that shell's whole life. `elvish_probe_ignores_a_user_hook_that_mentions_the_marker`
    /// is the behavioural half of this; here is the shape that makes it possible.
    ///
    /// Every assertion is positive except the two that name the retired form,
    /// because a denylist alone would pass for an arm that emitted no probe at
    /// all.
    #[test]
    fn the_elvish_probe_reads_a_parsed_declaration_not_rendered_source() {
        let text = registration(Shell::Elvish, &binary(), &watch()).expect("elvish registers");
        assert!(
            text.contains("all $__ocx_candidate[arg-names]"),
            "the probe must read the parsed argument names, which no text can forge: {text}"
        );
        assert!(
            text.contains("(==s (kind-of $__ocx_candidate) fn)"),
            "the probe must skip a non-closure before indexing it, or a stray list element raises \
             and aborts the registration (C-051): {text}"
        );
        // `to-string` over the list, in any form, is the defect: it folds every
        // closure's body and defining source into one string the marker is then
        // searched for.
        assert!(
            !text.contains("to-string $edit:before-readline"),
            "the probe must never stringify the hook list: {text}"
        );
        assert!(
            !text.contains("str:contains"),
            "a substring search over shell source cannot tell our closure from text about it: {text}"
        );
    }

    #[test]
    fn an_empty_watch_set_still_emits_a_usable_guard() {
        for shell in hooked() {
            let text = registration(shell, &binary(), &[]).unwrap_or_default();
            assert!(!text.is_empty(), "{shell} must still register");
            assert!(text.contains("--reconcile"), "{shell} must still reconcile: {text}");
        }
        // Elvish carries no watch-set term at all, so this case cannot tell an
        // empty set from a full one — the two emissions are byte-identical. That
        // is asserted rather than left as an unexplained blank: it is the same
        // contract `every_arm_guards_the_way_its_kind_says_it_does` states, seen
        // from the other side.
        assert_eq!(
            registration(Shell::Elvish, &binary(), &[]),
            registration(Shell::Elvish, &binary(), &watch()),
            "elvish's registration must not vary with a watch set it cannot read"
        );
        // …and every other hooked arm must vary, or the assertion above would be
        // a statement about all of them.
        for shell in shells_where(GuardKind::WatchGuarded) {
            assert_ne!(
                registration(shell, &binary(), &[]),
                registration(shell, &binary(), &watch()),
                "{shell} is watch-guarded, so its emission must depend on the watch set"
            );
        }
    }

    // ── #347: the baked gate is refreshable ──────────────────────────────

    /// A second watch set, disjoint from [`watch`], so "carries the new list"
    /// and "no longer carries the old" are two separate assertions.
    fn regrown_watch() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/w/ocx.toml"),
            PathBuf::from("/w/ocx.lock"),
            PathBuf::from("/w/sub/ocx.toml"),
            PathBuf::from("/w/sub/ocx.lock"),
        ]
    }

    #[test]
    fn exactly_the_watch_guarded_arms_can_redefine_their_gate() {
        for shell in shells_where(GuardKind::WatchGuarded) {
            assert!(
                redefinition(shell, &binary(), &watch()).is_some(),
                "{shell} bakes a watch set into its gate, so it must be able to re-emit one"
            );
        }
        for kind in [GuardKind::CarrierAndPwd, GuardKind::None] {
            for shell in shells_where(kind) {
                assert_eq!(
                    redefinition(shell, &binary(), &watch()),
                    None,
                    "{shell} has no watch-set term in its gate, so there is nothing to refresh"
                );
            }
        }
    }

    /// The whole point of the emission: the new members are in it and the
    /// retired ones are gone. A redefinition that merely *added* would leave a
    /// left project watched for the shell's whole life.
    #[test]
    fn a_redefinition_carries_the_new_watch_set_and_drops_the_old() {
        for shell in shells_where(GuardKind::WatchGuarded) {
            let text = redefinition(shell, &binary(), &regrown_watch()).unwrap_or_default();
            for path in regrown_watch() {
                assert!(
                    text.contains(&*path.to_string_lossy()),
                    "{shell}: the redefinition must carry the new member {path:?}: {text}"
                );
            }
            let shrunk = redefinition(shell, &binary(), &watch()).unwrap_or_default();
            assert!(
                !shrunk.contains("/w/sub/ocx.lock"),
                "{shell}: a member the new set dropped must not survive the redefinition: {shrunk}"
            );
        }
    }

    /// The gate the redefinition installs must be the gate the registration
    /// bakes, byte for byte.
    ///
    /// Two spellings of one guard is how a re-emission silently installs a
    /// weaker test than shell start does — and only the *second* prompt of a
    /// session would ever run it, which is the hardest place to notice.
    #[test]
    fn a_redefinition_installs_the_same_guard_the_registration_bakes() {
        for shell in shells_where(GuardKind::WatchGuarded) {
            let redefined = redefinition(shell, &binary(), &regrown_watch()).unwrap_or_default();
            let registered = registration(shell, &binary(), &regrown_watch()).unwrap_or_default();
            // The guard's first line, which is the whole conditional in every
            // arm: one `if` carrying every term and every watch path.
            let guard = redefined
                .lines()
                .find(|line| line.trim_start().starts_with("if "))
                .unwrap_or_else(|| panic!("{shell}: a redefinition must carry a guard: {redefined}"));
            assert!(
                registered.contains(guard),
                "{shell}: the redefined guard is not the one the registration bakes.\n                 redefined: {guard}\nregistration: {registered}"
            );
        }
    }

    /// A redefinition must carry the guarded function and **nothing else**.
    ///
    /// Every arm's registration is idempotent on purpose (C-043), so re-emitting
    /// one would change nothing at all — and the parts it guards are the
    /// one-time setup: the `mktemp` stamp, bash's `EXIT` trap, pwsh's three
    /// `$global:` seeds. Re-running those would leak a temp file per prompt and,
    /// in pwsh, reset the stamp to `MinValue` so the next prompt execs
    /// unconditionally — the exact cost C-044 exists to remove.
    #[test]
    fn a_redefinition_repeats_no_one_time_setup() {
        for shell in shells_where(GuardKind::WatchGuarded) {
            let text = redefinition(shell, &binary(), &watch()).unwrap_or_default();
            for forbidden in [
                "mktemp",
                "trap ",
                "add-zsh-hook",
                "PROMPT_COMMAND",
                "__ocxPrevPrompt",
                "[datetime]::MinValue",
                "function global:prompt",
                "--on-event fish_exit",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "{shell}: a redefinition must not repeat the one-time setup ({forbidden:?}): {text}"
                );
            }
        }
    }

    /// The emitted stream is `eval`'d (A-21), so a redefinition is bound by the
    /// same two module-wide rules as every other emission.
    #[test]
    fn a_redefinition_is_silent_and_never_calls_bare_ocx() {
        for shell in shells_where(GuardKind::WatchGuarded) {
            let text = redefinition(shell, &binary(), &watch()).unwrap_or_default();
            for forbidden in ["printf ", "echo ", "Write-Host", "Write-Output", ">&2"] {
                assert!(
                    !text.contains(forbidden),
                    "{shell}: a redefinition must print nothing ({forbidden:?}): {text}"
                );
            }
            // C-045 — every call site is the resolved absolute path.
            assert!(
                !text.contains("'ocx' ") && !text.contains(" ocx "),
                "{shell}: a redefinition must never name bare `ocx`: {text}"
            );
        }
    }

    // ── A-21: no diagnostic is emitted on the startup path ───────────────

    #[test]
    fn no_emitted_body_prints_a_startup_diagnostic() {
        for (shell, text) in every_emitted_body() {
            for forbidden in ["printf ", "echo ", "Write-Host", "Write-Output", ">&2"] {
                assert!(
                    !text.contains(forbidden),
                    "{shell} must emit no startup diagnostic ({forbidden:?}): {text}"
                );
            }
        }
    }

    // ── C-042 / S-041: enablement is decided once, at startup ────────────

    #[test]
    fn no_emitted_body_re_evaluates_enablement() {
        for (shell, text) in every_emitted_body() {
            assert!(
                !text.contains("OCX_NO_HOOK"),
                "{shell} must not re-read enablement per prompt: {text}"
            );
        }
    }

    // ── escaping ─────────────────────────────────────────────────────────

    #[test]
    fn a_quote_in_a_path_is_escaped_per_arm() {
        let nasty = PathBuf::from("/home/it's work/bin/ocx");
        let watch = [PathBuf::from("/home/it's work/ocx.toml")];
        let bash = registration(Shell::Bash, &nasty, &watch).unwrap_or_default();
        assert!(bash.contains("'/home/it'\\''s work/bin/ocx'"), "{bash}");
        assert!(bash.contains("'/home/it'\\''s work/ocx.toml'"), "{bash}");
        let fish = registration(Shell::Fish, &nasty, &watch).unwrap_or_default();
        assert!(fish.contains("'/home/it\\'s work/bin/ocx'"), "{fish}");
        let pwsh = registration(Shell::PowerShell, &nasty, &watch).unwrap_or_default();
        assert!(pwsh.contains("'/home/it''s work/bin/ocx'"), "{pwsh}");
        // Elvish is the one arm at quoting depth 2: the path is doubled once for
        // its own literal and again for the `eval` string that carries it, so a
        // quote comes out QUADRUPLED. Both emissions are checked — they are
        // separate call sites, and a fix applied to one would leave the other
        // able to close the eval string and have its remainder parsed as elvish
        // source. Round-tripped against elvish 0.21 in a pty: with the binary
        // genuinely living at this path, both the hook and the wrapper found and
        // ran it.
        let elvish = registration(Shell::Elvish, &nasty, &watch).unwrap_or_default();
        assert!(elvish.contains("''/home/it''''s work/bin/ocx''"), "{elvish}");
        let elvish_wrapper = wrapper(Shell::Elvish, &nasty).unwrap_or_default();
        assert!(
            elvish_wrapper.contains("''/home/it''''s work/bin/ocx''"),
            "{elvish_wrapper}"
        );
    }

    #[test]
    fn a_backslash_in_a_path_is_escaped_for_fish_only() {
        // fish is the one arm whose single-quoted form treats `\` as an
        // escape; POSIX and PowerShell single quotes carry it verbatim, and
        // doubling it there would corrupt a Windows path.
        let windows = PathBuf::from(r"C:\Users\u\ocx.exe");
        let fish = wrapper(Shell::Fish, &windows).unwrap_or_default();
        assert!(fish.contains(r"'C:\\Users\\u\\ocx.exe'"), "{fish}");
        let bash = wrapper(Shell::Bash, &windows).unwrap_or_default();
        assert!(bash.contains(r"'C:\Users\u\ocx.exe'"), "{bash}");
        let pwsh = wrapper(Shell::PowerShell, &windows).unwrap_or_default();
        assert!(pwsh.contains(r"'C:\Users\u\ocx.exe'"), "{pwsh}");
    }
}

/// Is an absent `name` allowed to skip, or does the environment promise it?
///
/// `__OCX_TESTING_REQUIRE_LIVE_SHELLS` (`1` / `all`, or a comma-separated
/// list of interpreter binaries) is the same seam `shell.rs`'s live tests
/// read, spelled again here because that module's copy lives inside its own
/// private `#[cfg(test)]` mod. At file level so both live modules below — one
/// `unix`-only, one not — share it. Off by default: no `cargo nextest` leg
/// installs elvish, and a hard default would red every runner rather than the
/// emit. Set it where the interpreter does exist and a skip becomes a
/// failure — because a skip and a pass otherwise carry the same evidence.
#[cfg(test)]
fn absence_may_skip(name: &str) -> bool {
    let Some(raw) = ocx_env::__OCX_TESTING_REQUIRE_LIVE_SHELLS.get() else {
        return true;
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return true;
    }
    if raw == "1" || raw.eq_ignore_ascii_case("all") {
        return false;
    }
    !raw.split(',').any(|want| want.trim() == name)
}

/// Live-shell coverage for what only a real shell can prove about an emitted
/// body: that the guard's `$PWD` term fires (C-019 member 7), and that the
/// reconcile call's two streams land where A-21 says they do.
///
/// Everything else in this module's unit tests asserts over emitted *text*;
/// these run it.
///
/// On the `$PWD` term specifically:
///
/// Unit-level string assertions prove the term is *emitted*; only a real shell
/// proves it *fires*. Each case runs three prompts against one registration,
/// because two cannot discriminate: a hook that fires unconditionally passes a
/// "fires after `cd`" assertion while being a per-prompt exec storm, and a hook
/// that never fires passes a "quiet when nothing changed" assertion. The
/// discriminating shape is **fire, quiet, fire** on one stamp and one
/// unchanging watch set, where only `$PWD` moved for the third.
///
/// The rig is built so the **first** prompt fires for a reason the `$PWD` term
/// is not responsible for — an empty carrier, which the fake binary then seeds.
/// That is deliberate: it keeps the mutation's blast radius on the third count
/// alone, so a red cannot be mistaken for a rig that stopped working.
#[cfg(all(test, unix))]
mod live_shell_tests {
    #[expect(
        clippy::disallowed_types,
        reason = "test-only live-shell harness proving the per-prompt hook fires"
    )]
    use std::process::Command;

    use super::*;

    /// Run `script` under `argv`, or `None` when the interpreter is absent.
    ///
    /// Mirrors `shell.rs`'s own live-test runner: a missing interpreter skips,
    /// a failing one that can still run `echo ok` is our bug and panics.
    ///
    /// **The environment is controlled, and that is not hygiene — it is the
    /// difference between measuring the emission and measuring the developer.**
    /// `fish -c` reads `~/.config/fish/conf.d/*`, and on a machine that
    /// dogfoods ocx's own fish integration that already sources
    /// `$OCX_HOME/env.fish` and defines the real `__ocx_prompt_hook`. The
    /// emitted registration is idempotent by construction (C-043 —
    /// `if not functions -q __ocx_prompt_hook`), so it correctly stands down,
    /// the fixture's definition never happens, and the script then calls the
    /// **host's** hook against the host's cwd: `functions --details` reads `-`
    /// instead of the fixture's path, and the real binary answers where the
    /// fake was expected. Nothing was wrong with the emission; the test was
    /// asking the wrong shell. CI stayed green only because CI has no fish
    /// config, which is the "unchecked green" shape inverted — a result that
    /// depends on ambient state the test does not control.
    ///
    /// The carrier is dropped for the same reason one shell over: it is
    /// exported, so a developer running these tests from inside a live ocx
    /// session leaks their own ledger into every fixture, and the arms that
    /// seed it do so from inside their own script.
    ///
    /// `XDG_CONFIG_HOME` and not `--no-config`: the flag is per-shell, has to be
    /// spelled four ways, and silently turns "unsupported flag" into a skip via
    /// the `echo ok` probe below. One empty directory covers every arm that
    /// looks for user config, whatever it is called.
    //
    // ponytail: `ZDOTDIR` is deliberately not set. `zsh -c` does read
    // `$HOME/.zshenv`, but ocx's zsh activation writes to `.zshrc`, which `-c`
    // never reads — so there is no leak to close yet. Add it the day one shows up.
    #[expect(
        clippy::disallowed_types,
        reason = "test-only live-shell harness proving the per-prompt hook fires"
    )]
    fn run(argv: &[&str], script: &str) -> Option<String> {
        let (bin, head) = argv.split_first()?;
        let config = tempfile::TempDir::new().expect("an empty config home");
        let go = |body: &str| match Command::new(bin)
            .args(head)
            .arg(body)
            .env("XDG_CONFIG_HOME", config.path())
            .env_remove(super::super::reconcile::CARRIER_KEY)
            .env_remove(ELVISH_PWD_KEY)
            .output()
        {
            Ok(output) => Some(output),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("failed to spawn {bin}: {error}"),
        };
        let output = go(script)?;
        if !output.status.success() {
            let probe = go("echo ok")?;
            if !probe.status.success() {
                return None;
            }
            panic!(
                "{bin} exited {} on:\n{script}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Some(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
    }

    /// A stand-in ocx binary plus an unchanging watch set.
    ///
    /// The fake records each invocation as one line in `counter` — counting in
    /// the filesystem rather than in an emitted assignment keeps the fixture
    /// free of per-shell syntax — and emits `carrier_assignment`, which the
    /// hook's own `eval` / `source` applies. Seeding the carrier from inside the
    /// apply is what makes prompt 2 quiet.
    struct Rig {
        _home: tempfile::TempDir,
        binary: PathBuf,
        counter: PathBuf,
        watch: Vec<PathBuf>,
        alpha: PathBuf,
        beta: PathBuf,
    }

    fn rig(shell: Shell, carrier_assignment: &str) -> Rig {
        use std::os::unix::fs::PermissionsExt as _;

        let home = tempfile::TempDir::new().expect("tempdir");
        let counter = home.path().join("fired");
        std::fs::write(&counter, "").expect("seed the counter");

        let binary = home.path().join("ocx");
        // The fake emits what a real successful reconcile emits: the carrier and
        // the freshness checkpoint. The checkpoint riding the *output* rather
        // than the hook body is the contract under test here too — a run that
        // emitted nothing would leave the stamp stale.
        let checkpoint = checkpoint(shell).expect("every arm with a hook has a checkpoint");
        std::fs::write(
            &binary,
            format!(
                "#!/bin/sh\necho fired >> '{counter}'\ncat <<'__OCX_EOF'\n{carrier_assignment}\n{checkpoint}\n__OCX_EOF\n",
                counter = counter.display()
            ),
        )
        .expect("write the fake ocx");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod the fake ocx");

        // One watch member, written before the registration takes its stamp, so
        // it is always older than the stamp and no `-nt` term can account for a
        // firing. Only `$PWD` can.
        let watch = vec![home.path().join("ocx.lock")];
        std::fs::write(&watch[0], "version = 3\n").expect("write the watch member");

        let alpha = home.path().join("alpha");
        let beta = home.path().join("beta");
        std::fs::create_dir_all(&alpha).expect("mkdir alpha");
        std::fs::create_dir_all(&beta).expect("mkdir beta");

        Rig {
            _home: home,
            binary,
            counter,
            watch,
            alpha,
            beta,
        }
    }

    /// C-019 member 7 — `cd` into a different project reconciles (bash, zsh).
    ///
    /// Red state: drop ` || [ "${__ocx_pwd-}" != "$PWD" ]` from the guard and
    /// the observed counts go `1 1 1` instead of `1 1 2` — the third prompt
    /// stops firing. That is the shipped bug: entering a different project never
    /// applies its environment.
    #[test]
    fn cd_into_another_project_fires_the_posix_hook_and_a_still_prompt_does_not() {
        for argv in [["bash", "-c"], ["zsh", "-c"]] {
            let shell = Shell::from_live_argv(&argv);
            let rig = rig(shell, "__OCX_ENV_STATE=seeded");
            let registration = registration(shell, &rig.binary, &rig.watch).expect("a hook arm");
            let script = format!(
                "{registration}\n\
                 count() {{ grep -c fired '{counter}'; }}\n\
                 cd '{alpha}'; __ocx_prompt_hook >/dev/null 2>&1; first=$(count)\n\
                 __ocx_prompt_hook >/dev/null 2>&1; still=$(count)\n\
                 cd '{beta}'; __ocx_prompt_hook >/dev/null 2>&1; moved=$(count)\n\
                 printf '%s %s %s' \"$first\" \"$still\" \"$moved\"",
                counter = rig.counter.display(),
                alpha = rig.alpha.display(),
                beta = rig.beta.display(),
            );
            let Some(observed) = run(&argv, &script) else {
                continue;
            };
            assert_eq!(
                observed, "1 1 2",
                "{argv:?}: prompt 1 fires on the empty carrier, prompt 2 must be quiet, and \
                 `cd` must fire prompt 3 (C-019 member 7)"
            );
        }
    }

    /// EC-HOOK-019 (ocx-sh/ocx#442) — a consent grant exported mid-session fires
    /// the next prompt, with nothing else moved (bash, zsh).
    ///
    /// The exact shipped defect, replayed against the real emitted guard: the
    /// fingerprint folded `OCX_CONSENT_PATHS` (A-13), but the guard that decides
    /// whether the binary runs at all had no term for it, so an `export` in a
    /// still prompt moved nothing the shell could see. The acceptance row
    /// `test_a_grant_arriving_mid_session_activates_at_the_next_prompt` drives
    /// `--reconcile` unconditionally and therefore never reached this guard.
    ///
    /// Red state: drop ` || [ "${__ocx_grant-}" != … ]` from the guard and the
    /// counts go `1 1 1` instead of `1 1 2`.
    #[test]
    fn a_grant_exported_mid_session_fires_the_posix_hook() {
        for argv in [["bash", "-c"], ["zsh", "-c"]] {
            let shell = Shell::from_live_argv(&argv);
            let rig = rig(shell, "__OCX_ENV_STATE=seeded");
            let registration = registration(shell, &rig.binary, &rig.watch).expect("a hook arm");
            let script = format!(
                "{registration}\n\
                 count() {{ grep -c fired '{counter}'; }}\n\
                 cd '{alpha}'; __ocx_prompt_hook >/dev/null 2>&1; first=$(count)\n\
                 __ocx_prompt_hook >/dev/null 2>&1; still=$(count)\n\
                 export OCX_CONSENT_PATHS='{alpha}'; __ocx_prompt_hook >/dev/null 2>&1; granted=$(count)\n\
                 printf '%s %s %s' \"$first\" \"$still\" \"$granted\"",
                counter = rig.counter.display(),
                alpha = rig.alpha.display(),
            );
            let Some(observed) = run(&argv, &script) else {
                continue;
            };
            assert_eq!(
                observed, "1 1 2",
                "{argv:?}: prompt 1 fires on the empty carrier, prompt 2 must be quiet, and an \
                 exported OCX_CONSENT_PATHS must fire prompt 3 (#442)"
            );
        }
    }

    /// The fish arm of the same contract. Its guard is a separate emission, and
    /// a `; or` term added to the wrong `begin` block is silently inert.
    ///
    /// `--on-event fish_prompt` only fires on a real prompt, so the function is
    /// invoked directly: the guard, not fish's event loop, is under test.
    #[test]
    fn cd_into_another_project_fires_the_fish_hook() {
        let rig = rig(Shell::Fish, "set -gx __OCX_ENV_STATE seeded");
        let registration = registration(Shell::Fish, &rig.binary, &rig.watch).expect("fish hosts a hook");
        let script = format!(
            "{registration}\n\
             function count; grep -c fired '{counter}'; end\n\
             cd '{alpha}'; __ocx_prompt_hook >/dev/null 2>&1; set first (count)\n\
             __ocx_prompt_hook >/dev/null 2>&1; set still (count)\n\
             cd '{beta}'; __ocx_prompt_hook >/dev/null 2>&1; set moved (count)\n\
             printf '%s %s %s' $first $still $moved",
            counter = rig.counter.display(),
            alpha = rig.alpha.display(),
            beta = rig.beta.display(),
        );
        let Some(observed) = run(&["fish", "-c"], &script) else {
            return;
        };
        assert_eq!(
            observed, "1 1 2",
            "fish: prompt 1 fires on the empty carrier, prompt 2 must be quiet, and `cd` must \
             fire prompt 3 (C-019 member 7)"
        );
    }

    /// The fish arm of EC-HOOK-019 (#442) — the `; or` grant term sits in the
    /// guard's `begin` block, where a term in the wrong block is silently inert.
    #[test]
    fn a_grant_exported_mid_session_fires_the_fish_hook() {
        let rig = rig(Shell::Fish, "set -gx __OCX_ENV_STATE seeded");
        let registration = registration(Shell::Fish, &rig.binary, &rig.watch).expect("fish hosts a hook");
        let script = format!(
            "{registration}\n\
             function count; grep -c fired '{counter}'; end\n\
             cd '{alpha}'; __ocx_prompt_hook >/dev/null 2>&1; set first (count)\n\
             __ocx_prompt_hook >/dev/null 2>&1; set still (count)\n\
             set -gx OCX_CONSENT_PATHS '{alpha}'; __ocx_prompt_hook >/dev/null 2>&1; set granted (count)\n\
             printf '%s %s %s' $first $still $granted",
            counter = rig.counter.display(),
            alpha = rig.alpha.display(),
        );
        let Some(observed) = run(&["fish", "-c"], &script) else {
            return;
        };
        assert_eq!(
            observed, "1 1 2",
            "fish: prompt 1 fires on the empty carrier, prompt 2 must be quiet, and an exported \
             OCX_CONSENT_PATHS must fire prompt 3 (#442)"
        );
    }

    /// D2 / C-044 — a reconcile that emitted nothing does **not** quiet the next
    /// prompt.
    ///
    /// The freshness checkpoint rides the reconcile's own output, so a degraded
    /// run — an unparseable `ocx.lock`, a `config.toml` caught mid-rename, an
    /// indeterminate walk — leaves the stamp stale and the shell retries. With
    /// the refresh written unconditionally into the hook body instead, prompt 1
    /// would bump the stamp past every watch member and prompts 2..n would never
    /// exec again: the environment stays stale until some watched file's mtime
    /// happens to move, and D2's "every prompt re-converges" is quietly false.
    ///
    /// Red state: put `if [ -n "${__ocx_stamp-}" ]; then : >| … fi` back into
    /// `posix_apply` and the observed counts go `1 2 3` -> `1 1 1`.
    #[test]
    fn a_reconcile_that_emitted_nothing_leaves_the_next_prompt_to_retry() {
        for argv in [["bash", "-c"], ["zsh", "-c"]] {
            let shell = Shell::from_live_argv(&argv);
            // A binary that seeds the carrier but emits **no checkpoint** —
            // the shape of a run whose reconcile degraded after the carrier was
            // already in place. Seeding the carrier is what makes this test
            // measure the *stamp*: with an empty carrier the guard's first term
            // fires every prompt on its own and the assertion would pass for any
            // build, which is the vacuous-check failure mode.
            let rig = rig(shell, "");
            std::fs::write(
                &rig.binary,
                format!(
                    "#!/bin/sh\necho fired >> '{}'\necho '__OCX_ENV_STATE=seeded'\n",
                    rig.counter.display()
                ),
            )
            .expect("rewrite the fake ocx as a degraded run");

            let registration = registration(shell, &rig.binary, &rig.watch).expect("a hook arm");
            let script = format!(
                "{registration}\n\
                 count() {{ grep -c fired '{counter}'; }}\n\
                 cd '{alpha}'; __ocx_prompt_hook >/dev/null 2>&1; first=$(count)\n\
                 __ocx_prompt_hook >/dev/null 2>&1; second=$(count)\n\
                 __ocx_prompt_hook >/dev/null 2>&1; third=$(count)\n\
                 printf '%s %s %s' \"$first\" \"$second\" \"$third\"",
                counter = rig.counter.display(),
                alpha = rig.alpha.display(),
            );
            let Some(observed) = run(&argv, &script) else {
                continue;
            };
            assert_eq!(
                observed, "1 2 3",
                "{argv:?}: a run that emitted no checkpoint must leave the stamp stale so every \
                 following prompt retries - a body-side refresh would latch this at `1 1 1`"
            );
        }
    }

    /// The `mktemp` stamp is removed when the shell exits.
    ///
    /// The registration creates one temp file per shell start and nothing used to
    /// take it away: `ocx clean` walks `$OCX_HOME`, not `$TMPDIR`, so a
    /// tmux-heavy desktop or a container with no reaper accumulated them without
    /// bound. Each arm uses its own append-safe exit registry — bash an `EXIT`
    /// trap installed only when the slot is free, zsh `add-zsh-hook zshexit`,
    /// fish `--on-event fish_exit`.
    ///
    /// The check is the file's absence *after the interpreter has exited*, read
    /// from Rust, because a shell cannot observe its own exit hook. Both states
    /// are reachable: with the cleanup line deleted from its arm the stamp
    /// survives and this reds.
    ///
    /// A missing interpreter skips its arm, so the run is gated on at least one
    /// having been exercised — three `continue`s would otherwise be a green
    /// indistinguishable from the test never having run.
    #[test]
    fn the_stamp_file_is_removed_when_the_shell_exits() {
        let mut exercised: Vec<&str> = Vec::new();
        for argv in [["bash", "-c"], ["zsh", "-c"], ["fish", "-c"]] {
            let shell = Shell::from_live_argv(&argv);
            // The binary need not exist: the guard's `-x` test is inside the
            // hook function, and this test never fires a prompt.
            let registration = registration(
                shell,
                Path::new("/nonexistent/ocx"),
                &[PathBuf::from("/nonexistent/ocx.lock")],
            )
            .expect("a hook arm");
            // Printing the path is the only way to learn which name `mktemp`
            // picked; the shell exits immediately after, firing its exit hook.
            let script = match shell {
                Shell::Fish => format!("{registration}\nprintf '%s' \"$__ocx_stamp\""),
                _ => format!("{registration}\nprintf '%s' \"${{__ocx_stamp-}}\""),
            };
            let Some(stamp) = run(&argv, &script) else {
                continue;
            };
            assert!(
                !stamp.is_empty(),
                "{argv:?}: mktemp produced no stamp, so this proves nothing about its removal"
            );
            let stamp = PathBuf::from(stamp);
            assert!(!stamp.exists(), "{argv:?}: {stamp:?} outlived the shell that made it");
            exercised.push(argv[0]);
        }
        assert!(
            !exercised.is_empty(),
            "no interpreter was available, so nothing about the stamp's lifetime was proven"
        );
    }

    /// Re-sourcing the activation registers the hook exactly once (#347).
    ///
    /// The repair for #347 moved each POSIX arm's registration out from under
    /// the `typeset -f __ocx_prompt_hook` guard, so a slot a prompt framework
    /// assigned over is re-registered on the next source. That is only safe
    /// because the registration carries a membership test of its **own**: bash's
    /// `*__ocx_prompt_hook*` arm over the one `declare -p` capture, and
    /// `add-zsh-hook`'s built-in one. Without it the same change turns every
    /// re-source into an append and the hook runs N times per prompt.
    ///
    /// This is a different property from the one `EC-HOOK-017` covers, and the
    /// two are defended by different lines — deleting bash's `*__ocx_prompt_hook*`
    /// arm leaves `EC-HOOK-017` green (the repair still happens, it just also
    /// duplicates), which is how this gap was found. Observed red state: with
    /// that arm deleted the counts go `1 1` -> `3 3`.
    ///
    /// The seeded foreign value is what keeps the two branches apart — the bash
    /// array case would otherwise never be reached, and the string case would not
    /// prove a foreign owner survives.
    #[test]
    fn re_sourcing_the_registration_registers_the_hook_exactly_once() {
        let mut exercised: Vec<&str> = Vec::new();
        for (argv, seed, count, foreign) in [
            (
                ["bash", "-c"],
                "PROMPT_COMMAND='__foreign'",
                "declare -p PROMPT_COMMAND 2>/dev/null | grep -o __ocx_prompt_hook | wc -l",
                "declare -p PROMPT_COMMAND 2>/dev/null | grep -o __foreign | wc -l",
            ),
            (
                ["bash", "-c"],
                "PROMPT_COMMAND=(__foreign)",
                "declare -p PROMPT_COMMAND 2>/dev/null | grep -o __ocx_prompt_hook | wc -l",
                "declare -p PROMPT_COMMAND 2>/dev/null | grep -o __foreign | wc -l",
            ),
            (
                ["zsh", "-c"],
                "precmd_functions=(__foreign)",
                "print -l -- $precmd_functions | grep -c '^__ocx_prompt_hook$'",
                "print -l -- $precmd_functions | grep -c '^__foreign$'",
            ),
        ] {
            let shell = Shell::from_live_argv(&argv);
            let rig = rig(shell, "__OCX_ENV_STATE=seeded");
            let registration = registration(shell, &rig.binary, &rig.watch).expect("a hook arm");
            let script = format!(
                "{seed}\n{registration}\n{registration}\n{registration}\n\
                 printf '%s %s' \"$({count} | tr -d ' ')\" \"$({foreign} | tr -d ' ')\""
            );
            let Some(observed) = run(&argv, &script) else {
                continue;
            };
            assert_eq!(
                observed, "1 1",
                "{argv:?} after `{seed}`: three sources must leave exactly one hook entry and \
                 leave the foreign owner's entry intact"
            );
            exercised.push(argv[0]);
        }
        assert!(
            !exercised.is_empty(),
            "no POSIX interpreter was available, so nothing about re-source idempotency was proven"
        );
    }

    /// pwsh's form of the same property: re-sourcing must not wrap our own
    /// wrapper (#347).
    ///
    /// The pwsh guard re-captures `$global:__ocxPrevPrompt` from whatever owns
    /// the prompt, which is what repairs a clobber — and what would make the
    /// wrapper call itself for ever if the guard ever fired while our wrapper was
    /// still installed. `__ocxPrevPrompt` naming a body that mentions
    /// `__ocxReconcile` is that state, so the count must stay `0` across any
    /// number of sources. Red state: change the guard back to
    /// `-not (Test-Path variable:global:__ocxPrevPrompt)` and it stays 0 (that
    /// guard never re-captures) — change it to an unconditional re-capture and it
    /// goes to 1 and the prompt recurses.
    #[test]
    fn re_sourcing_the_pwsh_registration_never_wraps_our_own_wrapper() {
        let argv = ["pwsh", "-NoProfile", "-Command"];
        let rig = rig(Shell::PowerShell, "$env:__OCX_ENV_STATE = 'seeded'");
        let registration = registration(Shell::PowerShell, &rig.binary, &rig.watch).expect("pwsh hosts a hook");
        let script = format!(
            "{registration}\n{registration}\n{registration}\n\
             $prev = \"$($global:__ocxPrevPrompt)\"\n\
             Write-Output ([regex]::Matches($prev, '__ocxReconcile').Count)\n\
             $null = prompt"
        );
        let Some(observed) = run(&argv, &script) else {
            return;
        };
        assert_eq!(
            observed, "0",
            "pwsh: $global:__ocxPrevPrompt must never hold our own wrapper — a self-wrap makes \
             every prompt recurse until the stack blows"
        );
    }

    /// The change summary A-21 defers to the first `--reconcile` run.
    const SUMMARY: &str = "ocx: +JAVA_HOME ~PATH -PYENV_ROOT (acme, lock a1b2c3)";

    /// What a diagnostic written on the binary's OWN stderr looks like — the
    /// `log::info!` shape A-21 rules out. The hook applies `2>/dev/null` at
    /// invocation time, so this never reaches a terminal.
    ///
    /// Apostrophe-free on purpose: [`fake_reconcile`] puts it inside a
    /// single-quoted `echo` in a `/bin/sh` script, and a quote here makes that
    /// script a syntax error — which produces an empty stdout and would read as
    /// "the channel is broken" instead of "the fixture is broken".
    const DECOY: &str = "ocx: DECOY - this line went to the fake binary own stderr";

    /// Write a fake ocx that prints `stdout_body` on stdout and [`DECOY`] on
    /// its own stderr, and return its path.
    fn fake_reconcile(home: &Path, stdout_body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let binary = home.join("ocx");
        std::fs::write(
            &binary,
            format!("#!/bin/sh\necho '{DECOY}' >&2\ncat <<'__OCX_EOF'\n{stdout_body}\n__OCX_EOF\n"),
        )
        .expect("write the fake ocx");
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod the fake ocx");
        binary
    }

    /// EC-REC-003 — the summary line's surviving channel, end to end.
    ///
    /// A-21 settles a contradiction the register read as fatal: the hook
    /// `eval`s the reconcile call's **stdout** and discards its **stderr**
    /// unconditionally, so a message written the ordinary way (a log line on
    /// the process's stderr) is thrown away, while a message written as *shell
    /// code that prints* rides the eval'd stream and lands on the user's
    /// stderr. The channel already existed; only its use for the summary was
    /// unstated.
    ///
    /// Both halves are asserted from one run, because either alone is half a
    /// proof: a build that dropped the `2>/dev/null` would still show the
    /// summary, and a build that eval'd nothing would still hide the decoy.
    ///
    /// The positive control below is the red state named in A-21's test hook —
    /// the SAME summary text, moved onto the binary's own stderr, must not
    /// arrive. Without it, "the summary arrived" cannot be told apart from a
    /// rig that simply leaks every byte the fake writes.
    ///
    /// The statement itself comes from [`Shell::emit_message`], not a
    /// hand-written `printf`: the escaper and the format-argument placement are
    /// that primitive's contract (pinned in `shell.rs`), and re-spelling them
    /// here would let this test pass against an emitter that had stopped
    /// producing them.
    ///
    /// A missing interpreter skips its arm, so the run is gated on at least one
    /// having been exercised: three `continue`s in a row would otherwise be a
    /// green indistinguishable from the test never having run.
    #[test]
    fn the_deferred_summary_rides_the_evald_stream_and_the_binary_stderr_is_discarded() {
        let mut exercised: Vec<&str> = Vec::new();
        for argv in [["bash", "-c"], ["zsh", "-c"], ["fish", "-c"]] {
            let shell = Shell::from_live_argv(&argv);
            let statement = shell.emit_message(SUMMARY).expect("every hooked arm emits a message");
            let home = tempfile::TempDir::new().expect("tempdir");
            let binary = fake_reconcile(home.path(), &statement);
            let registration = registration(shell, &binary, &[]).expect("a hook arm");
            // `2>&1 >/dev/null`, in that order: the shell's stderr is dup'd onto
            // the pipe first, then its stdout is dropped. What the runner reads
            // back is therefore exactly what a user would see on a prompt.
            let script = format!("{registration}\n__ocx_prompt_hook 2>&1 >/dev/null\ntrue");
            let Some(observed) = run(&argv, &script) else {
                continue;
            };
            assert!(
                observed.contains(SUMMARY),
                "{argv:?}: the summary must reach the user's stderr through the eval'd stream; got \
                 {observed:?}"
            );
            assert!(
                !observed.contains(DECOY),
                "{argv:?}: the reconcile call's own stderr is discarded unconditionally; got \
                 {observed:?}"
            );

            // Positive control: the same text, written the way A-21 rules out.
            let control_home = tempfile::TempDir::new().expect("tempdir");
            let control_binary = control_home.path().join("ocx");
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::write(&control_binary, format!("#!/bin/sh\necho '{SUMMARY}' >&2\n"))
                    .expect("write the control ocx");
                std::fs::set_permissions(&control_binary, std::fs::Permissions::from_mode(0o755))
                    .expect("chmod the control ocx");
            }
            let control_registration = super::registration(shell, &control_binary, &[]).expect("a hook arm");
            let control_script = format!("{control_registration}\n__ocx_prompt_hook 2>&1 >/dev/null\ntrue");
            let control = run(&argv, &control_script).expect("the interpreter ran a moment ago");
            assert!(
                !control.contains(SUMMARY),
                "{argv:?}: a summary written on the binary's own stderr must be discarded - if it \
                 arrives, this rig proves nothing about the eval'd channel; got {control:?}"
            );
            exercised.push(argv[0]);
        }
        assert!(
            !exercised.is_empty(),
            "no interpreter was available, so nothing about the diagnostic channel was proven"
        );
    }

    /// The elvish probe ignores a user hook whose *body mentions* the marker.
    ///
    /// This is the shipped bug, in a real elvish. The guard used to be
    /// `str:contains (to-string $edit:before-readline) '__ocx-prompt-hook'`, and
    /// `to-string` folds every closure in the list together with its `&def` (its
    /// literal body, comments included) and its `&src` (the whole source of the
    /// `eval` unit that defined it). So a user who already had a
    /// `$edit:before-readline` hook that merely *named* ocx's — in a comment, in
    /// a string, anywhere — made the probe true, and **no ocx hook was registered
    /// at all**, silently, for that shell's whole life.
    ///
    /// Both halves come from the emission's own seams: the predicate from
    /// [`elvish_already_registered`] and the registered closure from
    /// [`elvish_hook_closure`], so this cannot pass against a build that stopped
    /// producing either. Only the list they are applied to is synthetic —
    /// `$edit:before-readline` is bound only in an interactive elvish, and a pty
    /// is a dependency this suite does not have. What that leaves unproven (the
    /// registration reaching the real hook list) is covered by
    /// [`elvish_registration_emits_exactly`] plus a by-hand pty run against
    /// elvish 0.21, which reproduced the suppression before this change and the
    /// firing after it, including across `exec elvish`.
    ///
    /// Both outcomes are asserted from one run: the hostile list alone must read
    /// `$false` and the same list with our closure appended `$true`. Either half
    /// alone is satisfiable by a predicate stuck on one answer.
    #[test]
    fn elvish_probe_ignores_a_user_hook_that_mentions_the_marker() {
        // The `ocx` path is quote-free on purpose. `elvish_hook_closure` takes a
        // path already escaped for the single-quoted `eval` string the
        // registration wraps it in; here the closure is written straight into a
        // script, one quoting layer shallower. A path with no quote is identical
        // under both, so this fixture exercises the shipped bytes without
        // re-implementing the escape — which `a_quote_in_a_path_is_escaped_per_arm`
        // and `elvish_registration_emits_exactly` own.
        let closure = elvish_hook_closure("/opt/ocx/bin/ocx");
        let script = format!(
            "var hostile = [ {{\n\
             # my own prompt hook. ocx appends {ELVISH_HOOK_MARKER} after it.\n\
             nop\n\
             }} ]\n\
             var ours = [ $@hostile {closure} ]\n\
             echo {hostile_probe} {ours_probe}",
            hostile_probe = elvish_already_registered("$hostile"),
            ours_probe = elvish_already_registered("$ours"),
        );
        let Some(observed) = run(&["elvish", "-c"], &script) else {
            assert!(
                absence_may_skip("elvish"),
                "elvish is absent, so nothing about the idempotency probe was proven \
                 (__OCX_TESTING_REQUIRE_LIVE_SHELLS names it); install it"
            );
            eprintln!("# ocx: UNPROVEN — elvish is not installed, so this live test asserted nothing");
            return;
        };
        assert_eq!(
            observed, "$false $true",
            "a user hook that only mentions the marker must not read as ours, and our own closure \
             must: {script}"
        );
    }

    impl Shell {
        /// Map a live-test interpreter argv back to the `Shell` it emits for.
        fn from_live_argv(argv: &[&str; 2]) -> Shell {
            match argv[0] {
                "bash" => Shell::Bash,
                "zsh" => Shell::Zsh,
                "fish" => Shell::Fish,
                other => panic!("unmapped live interpreter {other}"),
            }
        }
    }
}

/// The PowerShell wrapper against a real interpreter, judged by the one oracle
/// that cannot drift: the same line typed at a native program directly.
///
/// PowerShell's parameter binder claims the first bare `--` of a *function*
/// call as its own end-of-parameters token and drops it before `$args` exists
/// (ocx#524); a native call keeps it. So every case runs twice in one script —
/// once through the emitted wrapper, once as `& <echo>` — and the two argv
/// dumps must agree. A case the wrapper deliberately does not repair names its
/// oracle line explicitly, so a change on that side is a red too, not a
/// silent improvement.
///
/// Not `unix`-gated, unlike the module above: the argv echo is a `.cmd` on
/// Windows, and there both `pwsh` and the built-in Windows PowerShell 5.1 run.
/// 5.1 is the interpreter this repair exists for as much as 7.x — it has no
/// `$MyInvocation.Statement` — and the Windows nextest leg is the only place
/// it runs.
#[cfg(test)]
mod live_power_shell_wrapper_tests {
    #[expect(
        clippy::disallowed_types,
        reason = "test-only live-shell harness proving the per-prompt hook fires"
    )]
    use std::process::Command;

    use super::*;

    /// `(name, wrapper line, oracle line)`. `{call}` expands to `ocx` in the
    /// wrapper line and to `& $echo` in the oracle line; `{name}` to `ocx` and
    /// `$echo`, for the one case that spells the call operator itself.
    const CASES: &[(&str, &str, &str)] = &[
        ("plain", "{call} pkg -- grant -h", "{call} pkg -- grant -h"),
        ("no separator", "{call} pkg grant -h", "{call} pkg grant -h"),
        (
            "quoted separator",
            "{call} pkg '--' grant -h",
            "{call} pkg '--' grant -h",
        ),
        (
            "second separator",
            "{call} -g pkg -- grant -- -h",
            "{call} -g pkg -- grant -- -h",
        ),
        ("leading separator", "{call} -- x", "{call} -- x"),
        ("trailing separator", "{call} x --", "{call} x --"),
        (
            "variables",
            "{call} pkg -- $v $arr 'a b' -h",
            "{call} pkg -- $v $arr 'a b' -h",
        ),
        ("splat after", "{call} pkg -- @h -h", "{call} pkg -- @h -h"),
        (
            "pipeline",
            "'in' | {call} pkg -- grant -h",
            "'in' | {call} pkg -- grant -h",
        ),
        (
            "two statements",
            "{call} a b -- c; {call} d -- e",
            "{call} a b -- c; {call} d -- e",
        ),
        (
            "subexpression",
            "{call} pkg -- $(1+1) tail",
            "{call} pkg -- $(1+1) tail",
        ),
        (
            "nested call",
            "{call} pkg -- $({call} inner -- deep) tail",
            "{call} pkg -- $({call} inner -- deep) tail",
        ),
        ("call operator", "& {name} pkg -- grant", "& {name} pkg -- grant"),
        (
            "continued line",
            "{call} pkg -- grant `\n  -h",
            "{call} pkg -- grant `\n  -h",
        ),
        // A colon-form parameter before the separator is two `$args` entries;
        // counted as one, the separator lands a slot early. The oracle spells
        // the pair the way the wrapper forwards it, since `-x:1` and `-x: 1`
        // are one native argument and two respectively.
        (
            "colon parameter",
            "{call} -x:1 pkg -- grant",
            "{call} -x: 1 pkg -- grant",
        ),
        // With no separator at all the list copy still matters: `@args` splats a
        // colon pair back as a named parameter, which a native call drops on
        // pwsh 7 (`-x:1 b` arrived as `1 b`); the copied list forwards both.
        ("colon parameter, no separator", "{call} -x:1 b", "{call} -x: 1 b"),
        // Not repaired, by design: the separator sits on a continuation line
        // `$MyInvocation.Line` does not carry, or behind a splat whose width
        // the line cannot tell. Both keep the pre-repair argv.
        (
            "separator on continuation",
            "{call} pkg `\n  -- grant",
            "{call} pkg `\n  grant",
        ),
        ("splat before", "{call} @h pkg -- grant", "{call} @h pkg grant"),
    ];

    fn expand(line: &str, call: &str, name: &str) -> String {
        line.replace("{call}", call).replace("{name}", name)
    }

    /// A native program that prints its argv, one bracketed item each.
    fn argv_echo(dir: &Path) -> PathBuf {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let path = dir.join("argv-echo");
            std::fs::write(
                &path,
                "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]' \"$a\"; done\necho\n",
            )
            .expect("write the argv echo");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod the argv echo");
            path
        }
        #[cfg(windows)]
        {
            // `%*` is the raw argument tail PowerShell built, which is exactly
            // the thing both sides of the comparison produce.
            let path = dir.join("argv-echo.cmd");
            std::fs::write(&path, "@echo [%*]\r\n").expect("write the argv echo");
            path
        }
    }

    /// Run `script` as a file under `interpreter`, or `None` when it is absent.
    ///
    /// A file rather than `-Command`, because the repair reads
    /// `$MyInvocation.Line` and a file is what a profile or a user script is.
    #[expect(
        clippy::disallowed_types,
        reason = "test-only live-shell harness proving the per-prompt hook fires"
    )]
    fn run(interpreter: &str, dir: &Path, script: &str) -> Option<String> {
        let file = dir.join(format!("{interpreter}-cases.ps1"));
        std::fs::write(&file, script).expect("write the case script");
        let mut command = Command::new(interpreter);
        command.args(["-NoProfile", "-NonInteractive"]);
        if cfg!(windows) {
            command.args(["-ExecutionPolicy", "Bypass"]);
        }
        let output = match command.arg("-File").arg(&file).output() {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => panic!("failed to spawn {interpreter}: {error}"),
        };
        assert!(
            output.status.success(),
            "{interpreter} exited {} on:\n{script}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        Some(String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n"))
    }

    /// Split `@@ <label>` sections out of `stdout`.
    fn sections(stdout: &str) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        let mut current: Option<(String, Vec<&str>)> = None;
        for line in stdout.lines() {
            if let Some(label) = line.strip_prefix("@@ ") {
                if let Some((label, body)) = current.take() {
                    out.insert(label, body.join("\n"));
                }
                current = Some((label.to_string(), Vec::new()));
            } else if let Some((_, body)) = current.as_mut() {
                body.push(line.trim_end());
            }
        }
        if let Some((label, body)) = current {
            out.insert(label, body.join("\n"));
        }
        out
    }

    #[test]
    fn the_wrapper_forwards_the_argv_a_native_call_would_receive() {
        let interpreters: &[&str] = if cfg!(windows) {
            &["pwsh", "powershell"]
        } else {
            &["pwsh"]
        };
        let dir = tempfile::TempDir::new().expect("tempdir");
        let echo = argv_echo(dir.path());
        let wrapper = wrapper(Shell::PowerShell, &echo).expect("pwsh has a wrapper");

        let mut script = format!(
            "{wrapper}\n$echo = '{echo}'\n$v = 'VAL'; $arr = @('p', 'q'); $h = @('s1', 's2')\n",
            echo = single_quoted_doubled(&echo.to_string_lossy())
        );
        for (name, wrapped, oracle) in CASES {
            script.push_str(&format!(
                "Write-Output '@@ {name} wrapped'\n{}\n",
                expand(wrapped, "ocx", "ocx")
            ));
            script.push_str(&format!(
                "Write-Output '@@ {name} oracle'\n{}\n",
                expand(oracle, "& $echo", "$echo")
            ));
        }

        for interpreter in interpreters {
            let Some(stdout) = run(interpreter, dir.path(), &script) else {
                assert!(
                    absence_may_skip(interpreter),
                    "{interpreter} is absent, so nothing about the separator repair was proven \
                     (__OCX_TESTING_REQUIRE_LIVE_SHELLS names it); install it"
                );
                eprintln!("# ocx: UNPROVEN — {interpreter} is not installed, so this live arm asserted nothing");
                continue;
            };
            let sections = sections(&stdout);
            for (name, _, _) in CASES {
                let wrapped = &sections[&format!("{name} wrapped")];
                let oracle = &sections[&format!("{name} oracle")];
                assert!(!oracle.is_empty(), "{interpreter}: `{name}` printed nothing natively");
                assert_eq!(
                    wrapped, oracle,
                    "{interpreter}: `{name}` reached the program differently through the wrapper"
                );
            }
        }
    }
}
