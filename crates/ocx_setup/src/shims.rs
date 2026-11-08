// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-shell env shims OCX writes into `$OCX_HOME`.
//!
//! Bodies are byte-identical across users: each shim resolves `OCX_HOME` at
//! runtime, never at install time.

use std::path::{Path, PathBuf};

use crate::error::Error;
use ocx_util::fs::write_bytes_atomic;

/// `$OCX_HOME/env.sh` — POSIX (sh/bash/zsh) shim.
pub const ENV_SH: &str = r#"#!/bin/sh
# Managed by ocx installer - do not edit.

# OCX_HOME env-var-with-fallback. Assigns and exports only when unset or empty.
: "${OCX_HOME:=$HOME/.ocx}"
export OCX_HOME

_ocx_bin="$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"

# Detect the real sourcing shell so the right completion backend is chosen.
# This file is sourced by bash AND zsh (not just /bin/sh); `sh` resolves to
# Shell::Dash, which has no clap completion backend - so bash/zsh users would
# get no completions if we hardcoded `--shell=sh`. PATH and global-env-eval
# output are identical across the POSIX arms, so this only changes the
# completion backend.
_ocx_shell=sh
if [ -n "${BASH_VERSION:-}" ]; then
    _ocx_shell=bash
elif [ -n "${ZSH_VERSION:-}" ]; then
    _ocx_shell=zsh
fi

# Interactivity, decided ONCE here and passed explicitly to the binary. `$-`
# carries `i` for an interactive shell and is the only source that answers
# correctly: the binary sees a redirected stderr and, under `ssh -t host 'bash
# -lc ...'`, a stdin that is a terminal for a shell that never renders a prompt.
# Both the hook and the completions below read this one answer.
_ocx_interactive=--no-interactive
case "$-" in
    *i*) _ocx_interactive=--interactive ;;
esac

# PATH + global toolchain env. The emitted activation is idempotent
# move-to-front by construction, so it is safe to run on EVERY source: a
# re-source (or a snippet captured into a profile) never duplicates a PATH
# entry. There is deliberately no double-source guard variable — an exported
# one-shot guard leaks into child processes (e.g. a VS Code Remote server whose
# terminals inherit it) and would wrongly suppress activation in a shell that
# needs it, while a clean SSH login would still work. Running unconditionally
# also lets a re-source pick up a changed global toolchain. `--no-completion`
# keeps completions out of this block; they are handled separately below so they
# survive a completion system that initializes later.
#
# This eval is ALSO the shim-side hook registration: whatever the running binary
# emits - the per-prompt reconcile hook, the `ocx` wrapper - rides this very
# stream, so registering it takes no shim change and no logic here. Which of
# those arrive depends on the shell detected above: bash and zsh have an
# append-safe prompt seam and get both, plain `sh` has neither and gets just the
# PATH and env lines. That is the whole point of a thin dispatcher - what gets
# exported can change without rewriting this file, and the change reaches every
# new shell immediately.
if [ -x "$_ocx_bin" ]; then
    eval "$("$_ocx_bin" self activate --shell="$_ocx_shell" --no-completion "$_ocx_interactive" 2>/dev/null)" || true
fi

# Shell completions: re-inject on EVERY interactive source, NOT once. zsh's
# `compinit` rebuilds its completion table from scratch; oh-my-zsh (and many
# frameworks) run it from `.zshrc`, AFTER a login shell has already sourced
# this file from `.zprofile`. A one-shot guard would register completions
# pre-`compinit` only to have them wiped with no second chance; re-running the
# generator on the post-`compinit` source re-registers them. It reuses the one
# interactivity answer decided above; `sh`/Dash has no completion backend so it
# is skipped. Output is completion-only (no PATH/env), so repeating it is cheap
# and side-effect-free. `--if-enabled` is what makes this second call consult
# the same policy the activation stream above does - without it the generator
# prints unconditionally and OCX_NO_COMPLETION never reaches an installed
# shell. The interactivity answer rides along for that policy's last rung.
if [ "$_ocx_interactive" = --interactive ] && [ -x "$_ocx_bin" ] && [ "$_ocx_shell" != sh ]; then
    eval "$("$_ocx_bin" shell completion --shell="$_ocx_shell" --if-enabled "$_ocx_interactive" 2>/dev/null)" || true
fi
unset _ocx_bin _ocx_shell _ocx_interactive
"#;

/// `$OCX_HOME/env.fish` — fish shim.
pub const ENV_FISH: &str = r#"# Managed by ocx installer - do not edit.
# No double-source guard: the emitted activation is idempotent move-to-front, so
# re-sourcing never duplicates a PATH entry. An exported guard would leak into
# child shells (e.g. VS Code Remote terminals) and wrongly suppress activation.

if not set -q OCX_HOME
    set -gx OCX_HOME "$HOME/.ocx"
end

set -l _ocx_bin "$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx"
# Decide interactivity here via `status is-interactive` and pass it explicitly,
# for the completions gate and the per-prompt hook alike. stderr is still
# redirected (2>/dev/null) to suppress startup diagnostics, but neither gate
# depends any longer on what the binary can see of its own descriptors.
if test -x "$_ocx_bin"
    if status is-interactive
        "$_ocx_bin" self activate --shell=fish --interactive 2>/dev/null | source
    else
        "$_ocx_bin" self activate --shell=fish --no-interactive 2>/dev/null | source
    end
end
"#;

/// `$OCX_HOME/env.ps1` — PowerShell shim.
pub const ENV_PS1: &str = r#"# Managed by ocx installer - do not edit.
# No double-source guard: the emitted activation is idempotent move-to-front, so
# re-sourcing never duplicates a PATH entry. An exported guard would leak into
# child shells (e.g. VS Code Remote terminals) and wrongly suppress activation.

if (-not $env:OCX_HOME) {
    # $env:USERPROFILE is null on Linux/macOS pwsh; fall back to $HOME so this
    # shim works for PowerShell 7 on every platform, not just Windows.
    $_ocxBase = if ($env:USERPROFILE) { $env:USERPROFILE } else { $HOME }
    $env:OCX_HOME = Join-Path $_ocxBase '.ocx'
}

# Binary name is platform-specific. $env:OS is 'Windows_NT' on every Windows
# PowerShell (Desktop 5.1 + Core 7) and unset on Linux/macOS pwsh; reading an
# unset $env: var is StrictMode-safe (yields $null). Forward slashes are
# accepted by PowerShell on every platform.
$_ocxExe = if ($env:OS -eq 'Windows_NT') { 'ocx.exe' } else { 'ocx' }
$_ocxBin = Join-Path $env:OCX_HOME "symlinks/ocx.sh/ocx/cli/current/content/bin/$_ocxExe"
if ([System.IO.File]::Exists($_ocxBin)) {
    # Build args as an array so the completion flag is appended cleanly - never
    # a $null/empty positional that clap would reject (Windows PowerShell 5.1
    # passes a bare $null arg as an empty string).
    # Interactivity is stated by the session that knows it, not probed by the
    # binary, whose stderr this shim redirects. [Console]::IsInputRedirected is
    # the discriminating check: [Environment]::UserInteractive is HARDCODED true
    # on .NET for Unix, so it answers 'interactive' for a script, a CI step and
    # `ssh host pwsh -Command ...` alike. Written as `-eq $false` rather than
    # `-not`, so the one host that lacks the property (.NET < 4.5, i.e. Windows
    # PowerShell below 5.1) yields $null and degrades to --no-interactive; `-not
    # $null` would degrade the other way and force the hook on. Kept separate
    # from the completions gate below: that one ALSO requires PowerShell 5.0+,
    # and the per-prompt hook has no such version floor.
    #
    # Completions are NOT requested with --completion: that flag outranks
    # OCX_NO_COMPLETION and `[shell] completions`, so a shim spelling its answer
    # there would revoke both opt-outs for every session it starts. The
    # interactivity above is the input the policy's last rung wants; the policy
    # itself lives in the binary.
    #
    # The one opt-out that stays is a capability floor, not a policy override:
    # legacy Windows PowerShell <5.0 cannot run clap's `using namespace` /
    # `Register-ArgumentCompleter -Native` output at all, so it passes
    # --no-completion while still emitting PATH + global env.
    $_ocxArgs = @('self', 'activate', '--shell=powershell')
    $_ocxInter = [Console]::IsInputRedirected -eq $false
    $_ocxArgs += if ($_ocxInter) { '--interactive' } else { '--no-interactive' }
    if ($PSVersionTable.PSVersion.Major -lt 5) {
        $_ocxArgs += '--no-completion'
    }
    $_ocxActivate = (& $_ocxBin @_ocxArgs 2>$null) | Out-String
    # Guard $null/empty: Out-String of empty/failed output yields $null, and
    # `Invoke-Expression $null` throws "Cannot bind argument ... is null".
    if ($_ocxActivate) { Invoke-Expression $_ocxActivate }
}
Remove-Variable _ocxBase, _ocxExe, _ocxBin, _ocxInter, _ocxArgs, _ocxActivate -ErrorAction SilentlyContinue
"#;

/// The Nushell loop applying the `ocx --format json --global env` document bound to
/// `$_ocx_json` to `$env`, as `self activate --shell=nushell` emits it.
// Dispatches on entry `kind`, never `key == "PATH"`, since a package may declare any key as `path`.
// `path` stores a joined string: Nushell drops a list-valued non-`PATH` var when spawning an external.
// `list` must agree byte for byte with `Shell::export_list`.
// No `else` arm: routing an unknown type to `constant` clobbers the caller's value.
// No `get --optional`: it postdates Nushell 0.101.0 and its parse error voids the whole autoload.
pub const NU_ENV_APPLY_LOOP: &str = "for _ocx_e in ($_ocx_json.items? | default []) { if $_ocx_e.kind == \"path\" { let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { \"\" }); let _ocx_v = ($_ocx_e.value | split row (char esep)); load-env {($_ocx_e.key): ($_ocx_v ++ ($_ocx_cur | split row (char esep) | where {|p| $p != \"\" and not ($p in $_ocx_v) }) | where {|p| $p != \"\" } | str join (char esep))} } else if $_ocx_e.kind == \"list\" { let _ocx_s = ($_ocx_e.separator? | default \" \"); let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { \"\" }); let _ocx_p = ($_ocx_s + $_ocx_e.value + $_ocx_s); mut _ocx_l = (if $_ocx_cur == \"\" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_e.key): (($_ocx_l | str replace $_ocx_s \"\") + $_ocx_e.value)} } else if $_ocx_e.kind == \"constant\" { load-env {($_ocx_e.key): $_ocx_e.value} } }";

/// The Nushell applier for the `ocx --format json self activate --reconcile --shell=nushell` plan bound to
/// `$_ocx_plan`: `sets`, then `restores`, then `removes`, the order `plan_lines` renders for every other shell.
// A plan whose `v` is not 1 (or is absent) applies nothing that prompt.
// Dispatches on the entry `type`, never `key == "PATH"`; a list-valued `$env.PATH` is joined first.
// `list` must agree byte for byte with `Shell::export_list` and a removal with `Shell::remove_list_element`: a null separator selects path-kind.
// No `else` arm after `constant`: an unknown type applies nothing instead of clobbering the caller's value.
// No `get --optional`: it postdates Nushell 0.101.0 and its parse error voids the whole autoload.
pub const NU_PLAN_APPLY: &str = r#"if ($_ocx_plan.v? | default 0) == 1 { for _ocx_e in ($_ocx_plan.sets? | default []) { let _ocx_k = $_ocx_e.key; let _ocx_c = (if ($_ocx_k in ($env | columns)) { $env | get $_ocx_k } else { "" }); let _ocx_cur = (if ($_ocx_c | describe) == "string" { $_ocx_c } else { $_ocx_c | str join (char esep) }); if $_ocx_e.type == "path" { let _ocx_v = ($_ocx_e.value | split row (char esep)); load-env {($_ocx_k): ($_ocx_v ++ ($_ocx_cur | split row (char esep) | where {|p| $p != "" and not ($p in $_ocx_v) }) | where {|p| $p != "" } | str join (char esep))} } else if $_ocx_e.type == "list" { let _ocx_s = ($_ocx_e.separator? | default " "); let _ocx_p = ($_ocx_s + $_ocx_e.value + $_ocx_s); mut _ocx_l = (if $_ocx_cur == "" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_k): (($_ocx_l | str replace $_ocx_s "") + $_ocx_e.value)} } else if $_ocx_e.type == "constant" { load-env {($_ocx_k): $_ocx_e.value} } }; for _ocx_r in ($_ocx_plan.restores? | default []) { if $_ocx_r.1 == null { hide-env -i $_ocx_r.0 } else { load-env {($_ocx_r.0): $_ocx_r.1} } }; for _ocx_r in ($_ocx_plan.removes? | default []) { let _ocx_k = $_ocx_r.0; if ($_ocx_k in ($env | columns)) { let _ocx_c = ($env | get $_ocx_k); let _ocx_cur = (if ($_ocx_c | describe) == "string" { $_ocx_c } else { $_ocx_c | str join (char esep) }); if $_ocx_r.2 == null { load-env {($_ocx_k): ($_ocx_cur | split row (char esep) | where {|p| $p != $_ocx_r.1 and $p != "" } | str join (char esep))} } else { let _ocx_s = $_ocx_r.2; let _ocx_p = ($_ocx_s + $_ocx_r.1 + $_ocx_s); mut _ocx_l = (if $_ocx_cur == "" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_k): ($_ocx_l | split row $_ocx_s | skip 1 | drop 1 | str join $_ocx_s)} } } } }"#;

/// `$OCX_HOME/env.nu` — Nushell shim, applying the reconcile plan as data because Nushell has
/// no string `eval` and `source` needs a parse-time-constant path.
// The `try` keeps a failed reconcile from voiding the bin-on-PATH step.
// Must run after the user's `config.nu`, which the vendor-autoload slot guarantees.
pub const ENV_NU: &str = r#"# Managed by ocx installer - do not edit.
# No double-source guard: activation is idempotent move-to-front, so re-sourcing
# never duplicates a PATH entry. An exported guard would leak into child shells
# (e.g. VS Code Remote terminals) and wrongly suppress activation.

$env.OCX_HOME = ($env.OCX_HOME? | default ($env.HOME | path join '.ocx'))

let _ocx_bin = ($env.OCX_HOME | path join 'symlinks/ocx.sh/ocx/cli/current/content/bin')
if (($_ocx_bin | path join 'ocx') | path exists) {
    # ocx bin on PATH, move-to-front. $env.PATH is a list at startup; normalize to
    # segments, drop only ocx's own prior entry, prepend, and store as a
    # separator-joined string so the plan apply below operates uniformly on
    # string-valued path vars.
    $env.PATH = ([$_ocx_bin] ++ ($env.PATH | (if ($in | describe) == 'string' { split row (char esep) } else { $in }) | where {|p| $p != "" and $p != $_ocx_bin }) | str join (char esep))
    try {
        let _ocx_plan = (^($_ocx_bin | path join 'ocx') --offline --format json self activate --reconcile --shell=nushell --shell-pid $nu.pid | from json)
        if ($_ocx_plan.v? | default 0) == 1 { for _ocx_e in ($_ocx_plan.sets? | default []) { let _ocx_k = $_ocx_e.key; let _ocx_c = (if ($_ocx_k in ($env | columns)) { $env | get $_ocx_k } else { "" }); let _ocx_cur = (if ($_ocx_c | describe) == "string" { $_ocx_c } else { $_ocx_c | str join (char esep) }); if $_ocx_e.type == "path" { let _ocx_v = ($_ocx_e.value | split row (char esep)); load-env {($_ocx_k): ($_ocx_v ++ ($_ocx_cur | split row (char esep) | where {|p| $p != "" and not ($p in $_ocx_v) }) | where {|p| $p != "" } | str join (char esep))} } else if $_ocx_e.type == "list" { let _ocx_s = ($_ocx_e.separator? | default " "); let _ocx_p = ($_ocx_s + $_ocx_e.value + $_ocx_s); mut _ocx_l = (if $_ocx_cur == "" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_k): (($_ocx_l | str replace $_ocx_s "") + $_ocx_e.value)} } else if $_ocx_e.type == "constant" { load-env {($_ocx_k): $_ocx_e.value} } }; for _ocx_r in ($_ocx_plan.restores? | default []) { if $_ocx_r.1 == null { hide-env -i $_ocx_r.0 } else { load-env {($_ocx_r.0): $_ocx_r.1} } }; for _ocx_r in ($_ocx_plan.removes? | default []) { let _ocx_k = $_ocx_r.0; if ($_ocx_k in ($env | columns)) { let _ocx_c = ($env | get $_ocx_k); let _ocx_cur = (if ($_ocx_c | describe) == "string" { $_ocx_c } else { $_ocx_c | str join (char esep) }); if $_ocx_r.2 == null { load-env {($_ocx_k): ($_ocx_cur | split row (char esep) | where {|p| $p != $_ocx_r.1 and $p != "" } | str join (char esep))} } else { let _ocx_s = $_ocx_r.2; let _ocx_p = ($_ocx_s + $_ocx_r.1 + $_ocx_s); mut _ocx_l = (if $_ocx_cur == "" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_k): ($_ocx_l | split row $_ocx_s | skip 1 | drop 1 | str join $_ocx_s)} } } } }
    } catch { }
    # Per-prompt hook. APPENDED onto the existing list and assigned back, never a
    # bare assignment and never a wholesale `$env.config.hooks = { ... }`:
    # starship's nushell integration owns this same slot, and overwriting it
    # silently stops the user's prompt updating. Every intermediate level is
    # defaulted, because a `nu -n` session's `$env.config` may carry no `hooks`
    # key at all and nushell parses a whole file before running any of it - one
    # erroring expression would void this entire autoload, taking the
    # `$env.PATH` prepend above down with it. The whole registration sits in its
    # own try/catch for the same reason: a hostile config shape must cost the
    # hook, never the PATH.
    #
    # `.pre_prompt` may legitimately hold a BARE CLOSURE rather than a list, and
    # `++` against a closure is a type error the try/catch would swallow,
    # dropping this hook with no diagnostic. A non-list value is wrapped into a
    # one-element list before the append. `str starts-with` on `describe` is
    # used rather than `=~` or `like`: both spellings have moved across nu
    # versions, and an unknown operator is a PARSE error that voids the whole
    # file.
    #
    # The closure carries the marker `__ocx_prompt_hook`, and a re-source drops
    # any closure holding it before appending, so a sourced-twice file registers
    # once.
    #
    # The closure runs the SAME apply the startup path above runs, written out a
    # second time rather than factored into a `def`: a `def` body is not a
    # closure and cannot see `$_ocx_bin`, so factoring would buy one shared copy
    # at the price of a new parse-time construct on the one shell where a parse
    # error costs the entire autoload. A test pins the two copies together
    # instead. The in-shell gate runs the binary only when the directory or a
    # signal variable changed since the last reconcile, and never while the
    # state it cannot read is already stood down; it mirrors the elvish guard
    # field for field, since nushell cannot ask the binary for it.
    try {
        $env.config.hooks = ($env.config.hooks? | default {})
        let _ocx_pre = ($env.config.hooks.pre_prompt? | default [])
        $env.config.hooks.pre_prompt = (((if ($_ocx_pre | describe | str starts-with 'list') { $_ocx_pre } else { [$_ocx_pre] }) | where {|h| not (($h | describe) == 'closure' and (view source $h | str contains '__ocx_prompt_hook')) }) ++ [{|| let __ocx_prompt_hook = 1; let _ocx_state = ($env.__OCX_ENV_STATE? | default ""); let _ocx_stood = ($env.__OCX_ENV_STOOD_DOWN? | default ""); let _ocx_gate = ([$nu.pid $env.PWD ($env.DIRENV_DIR? | default "") ($env.MISE_SHELL? | default "") ($env.__MISE_ORIG_PATH? | default "") ($env.OCX_CONSENT_PATHS? | default "") ($env.OCX_CONSENT_NAMESPACES? | default "")] | str join " "); if (($_ocx_stood == "") or ($_ocx_state != $_ocx_stood)) and (($_ocx_state == "") or (($env.__OCX_ENV_PWD? | default "") != $_ocx_gate)) { try { let _ocx_plan = (^($_ocx_bin | path join 'ocx') --offline --format json self activate --reconcile --shell=nushell --shell-pid $nu.pid | from json); if ($_ocx_plan.v? | default 0) == 1 { for _ocx_e in ($_ocx_plan.sets? | default []) { let _ocx_k = $_ocx_e.key; let _ocx_c = (if ($_ocx_k in ($env | columns)) { $env | get $_ocx_k } else { "" }); let _ocx_cur = (if ($_ocx_c | describe) == "string" { $_ocx_c } else { $_ocx_c | str join (char esep) }); if $_ocx_e.type == "path" { let _ocx_v = ($_ocx_e.value | split row (char esep)); load-env {($_ocx_k): ($_ocx_v ++ ($_ocx_cur | split row (char esep) | where {|p| $p != "" and not ($p in $_ocx_v) }) | where {|p| $p != "" } | str join (char esep))} } else if $_ocx_e.type == "list" { let _ocx_s = ($_ocx_e.separator? | default " "); let _ocx_p = ($_ocx_s + $_ocx_e.value + $_ocx_s); mut _ocx_l = (if $_ocx_cur == "" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_k): (($_ocx_l | str replace $_ocx_s "") + $_ocx_e.value)} } else if $_ocx_e.type == "constant" { load-env {($_ocx_k): $_ocx_e.value} } }; for _ocx_r in ($_ocx_plan.restores? | default []) { if $_ocx_r.1 == null { hide-env -i $_ocx_r.0 } else { load-env {($_ocx_r.0): $_ocx_r.1} } }; for _ocx_r in ($_ocx_plan.removes? | default []) { let _ocx_k = $_ocx_r.0; if ($_ocx_k in ($env | columns)) { let _ocx_c = ($env | get $_ocx_k); let _ocx_cur = (if ($_ocx_c | describe) == "string" { $_ocx_c } else { $_ocx_c | str join (char esep) }); if $_ocx_r.2 == null { load-env {($_ocx_k): ($_ocx_cur | split row (char esep) | where {|p| $p != $_ocx_r.1 and $p != "" } | str join (char esep))} } else { let _ocx_s = $_ocx_r.2; let _ocx_p = ($_ocx_s + $_ocx_r.1 + $_ocx_s); mut _ocx_l = (if $_ocx_cur == "" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_k): ($_ocx_l | split row $_ocx_s | skip 1 | drop 1 | str join $_ocx_s)} } } } } } catch { } } }])
    } catch { }
}
"#;

/// `$OCX_HOME/env.elv` — Elvish shim.
pub const ENV_ELV: &str = r#"# Managed by ocx installer - do not edit.
# No double-source guard: the emitted activation is idempotent move-to-front, so
# re-sourcing never duplicates a PATH entry. An exported guard would leak into
# child shells (e.g. VS Code Remote terminals) and wrongly suppress activation.

if (not (has-env OCX_HOME)) {
    set-env OCX_HOME $E:HOME/.ocx
}

var _ocx_bin = $E:OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx
# Interactivity, stated rather than probed: this shim redirects the binary's
# stderr, and elvish has no in-language isatty. `test -t 0` is the same kind of
# external path test the binary check below uses, and stdin is the descriptor
# nothing here redirects. `?(...)` turns a non-zero exit into a falsey value.
var _ocx_interactive = '--no-interactive'
if ?(test -t 0) { set _ocx_interactive = '--interactive' }
# PATH + global toolchain env run in their OWN eval unit, kept apart from the
# completion below. Elvish compiles an eval unit as a whole, so coupling the two
# lets a completion that fails to compile void the PATH prepend too: the
# clap_complete elvish completer sets edit:completion:arg-completer, and the
# edit: namespace is bound only when an interactive line editor is active (a real
# TTY). A non-TTY interactive shell has no edit:, so the completion unit raises a
# compile error - and if PATH shared that unit it would be lost with it. Keeping
# them separate makes PATH activation independent of completion succeeding.
if ?(test -x $_ocx_bin) {
    eval ($_ocx_bin self activate --shell=elvish --no-completion $_ocx_interactive 2>/dev/null | slurp)
    # Shell completions: best-effort, isolated. try/catch swallows the compile
    # error raised when edit: is absent (a non-TTY interactive shell), so a failed
    # completion can never disturb the PATH activation above. Sourced via
    # `ocx shell completion --if-enabled` - the same generator, and the same
    # policy consultation, the POSIX shim uses.
    try {
        eval ($_ocx_bin shell completion --shell=elvish --if-enabled $_ocx_interactive 2>/dev/null | slurp)
    } catch _ { }
}
"#;

/// Body of the fish `conf.d/ocx.fish` autoload file.
const FISH_CONF: &str = r#"# OCX shell environment - managed by ocx installer.
# Sources $OCX_HOME/env.fish which evaluates the global toolchain env.
set -l _ocx_env (string join '' (set -q OCX_HOME; and echo $OCX_HOME; or echo $HOME/.ocx) '/env.fish')
if test -f "$_ocx_env"
    source "$_ocx_env"
end
"#;

/// The shim whose presence under `$OCX_HOME` witnesses that `ocx self setup` has run.
// A valid witness only while `write_shims` writes every shim on every platform.
pub const WITNESS_SHIM: &str = "env.sh";

/// The five `(filename, body)` shims written into `$OCX_HOME`.
const SHIMS: [(&str, &str); 5] = [
    (WITNESS_SHIM, ENV_SH),
    ("env.fish", ENV_FISH),
    ("env.ps1", ENV_PS1),
    ("env.nu", ENV_NU),
    ("env.elv", ENV_ELV),
];

/// The Nushell vendor-autoload body: the full [`ENV_NU`] activation, since `source`
/// cannot take the runtime `$OCX_HOME/env.nu` path.
#[must_use]
pub fn nu_autoload_body() -> &'static str {
    ENV_NU
}

/// The fish `conf.d/ocx.fish` body that sources `$OCX_HOME/env.fish` at startup.
///
/// The orchestrator writes this to
/// `${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/ocx.fish`.
#[must_use]
pub fn fish_conf_body() -> &'static str {
    FISH_CONF
}

/// Write all five `env.*` shims into `ocx_home`, atomically, skipping any already current.
///
/// Returns the paths (re)written, or with `dry_run` the would-write set.
///
/// # Errors
///
/// Returns [`Error::Io`] if the directory cannot be created or a shim cannot be
/// read or written.
pub fn write_shims(ocx_home: &Path, dry_run: bool) -> Result<Vec<PathBuf>, Error> {
    if !dry_run {
        std::fs::create_dir_all(ocx_home).map_err(|source| Error::Io {
            path: ocx_home.to_path_buf(),
            source,
        })?;
    }

    let mut written = Vec::new();
    for (name, body) in SHIMS {
        let target = ocx_home.join(name);
        if !needs_write(&target, body)? {
            continue;
        }
        if !dry_run {
            write_bytes_atomic(&target, body.as_bytes()).map_err(|source| Error::Io {
                path: target.to_path_buf(),
                source,
            })?;
        }
        written.push(target);
    }
    Ok(written)
}

/// Refresh the ocx-owned `env.*` shims: [`write_shims`] without `dry_run`.
///
/// # Errors
///
/// Returns [`Error::Io`] on a directory or shim read/write failure.
pub fn refresh_shims(ocx_home: &Path) -> Result<Vec<PathBuf>, Error> {
    write_shims(ocx_home, false)
}

/// Whether `target` is absent or holds other bytes than `body`; any other read error propagates.
fn needs_write(target: &Path, body: &str) -> Result<bool, Error> {
    match std::fs::read(target) {
        Ok(existing) => Ok(existing != body.as_bytes()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(source) => Err(Error::Io {
            path: target.to_path_buf(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Test-Path` globs `[`/`]` in `OCX_HOME`, so the PowerShell shim probes with .NET.
    #[test]
    fn the_powershell_shim_uses_no_test_path() {
        assert!(!ENV_PS1.contains("Test-Path"), "env.ps1 must not use Test-Path");
        assert!(ENV_PS1.contains("[System.IO.File]::Exists($_ocxBin)"));
    }

    /// The `# Managed by ocx installer - do not edit.` header must survive in
    /// every shim const (contract 3 invariant — preserved verbatim).
    #[test]
    fn every_shim_carries_the_managed_header() {
        for (name, body) in SHIMS {
            assert!(
                body.contains("# Managed by ocx installer - do not edit."),
                "{name} must carry the managed-by-ocx header verbatim"
            );
        }
    }

    /// Each shim must compute `OCX_HOME` at runtime via an env-var-with-fallback
    /// form — never embed a literal install path (the byte-identical-across-users
    /// invariant). This is the structural equivalent of the install.sh golden
    /// tests that asserted the runtime fallback line is present.
    #[test]
    fn each_shim_uses_a_runtime_ocx_home_fallback() {
        assert!(
            ENV_SH.contains(r#": "${OCX_HOME:=$HOME/.ocx}""#),
            "env.sh must assign-if-unset OCX_HOME at runtime"
        );
        assert!(
            ENV_FISH.contains("if not set -q OCX_HOME") && ENV_FISH.contains(r#"set -gx OCX_HOME "$HOME/.ocx""#),
            "env.fish must fall back to $HOME/.ocx at runtime"
        );
        assert!(
            ENV_PS1.contains("if (-not $env:OCX_HOME)") && ENV_PS1.contains("$env:OCX_HOME = Join-Path"),
            "env.ps1 must fall back to a computed OCX_HOME at runtime"
        );
        assert!(
            ENV_NU.contains(r#"$env.OCX_HOME = ($env.OCX_HOME? | default"#),
            "env.nu must fall back to a computed OCX_HOME at runtime"
        );
        assert!(
            ENV_ELV.contains("(not (has-env OCX_HOME))") && ENV_ELV.contains("set-env OCX_HOME $E:HOME/.ocx"),
            "env.elv must fall back to $HOME/.ocx at runtime"
        );
    }

    /// No shim may carry an install-time substitution placeholder: the bodies
    /// are byte-identical across users, so there is nothing to interpolate.
    #[test]
    fn no_shim_contains_a_substitution_placeholder() {
        for (name, body) in SHIMS {
            assert!(
                !body.contains("{OCX_HOME}") && !body.contains("{{") && !body.contains("@OCX_HOME@"),
                "{name} must not contain a substitution placeholder"
            );
        }
    }

    /// No shim may carry a `_OCX_ENV_LOADED`-style activation guard. An exported
    /// one-shot guard leaks into child processes (e.g. a VS Code Remote server
    /// whose terminals inherit it) and suppresses activation in a shell that
    /// needs it, while a clean SSH login still works — the exact divergence this
    /// removal fixes. Idempotent move-to-front makes re-running activation safe,
    /// so no guard is needed for correctness.
    #[test]
    fn no_shim_carries_an_activation_guard() {
        for (name, body) in SHIMS {
            assert!(
                !body.contains("_OCX_ENV_LOADED"),
                "{name} must not carry a _OCX_ENV_LOADED activation guard (it leaks across \
                 process boundaries; idempotency makes it unnecessary)"
            );
        }
    }

    /// Each shim invokes the running binary, resolving it through the `current`
    /// install symlink — never a literal path.
    #[test]
    fn each_shim_resolves_the_binary_through_the_current_symlink() {
        // The shared prefix is literal in every shim. Most append `/ocx`
        // (or `/$_ocxExe`) directly; the nushell shim joins the bin DIR and the
        // `ocx` basename in separate `path join` steps, so assert the prefix
        // through `content/bin` (no trailing slash) which all shims share.
        for (name, body) in SHIMS {
            assert!(
                body.contains("symlinks/ocx.sh/ocx/cli/current/content/bin"),
                "{name} must resolve ocx through the current install symlink"
            );
            // POSIX-family + elvish shims write `self activate` contiguously; the PowerShell
            // shim builds the args as a clap-safe array. The nushell shim cannot `eval`/`source`
            // that output (no string eval; `source` needs a parse-time const path), so it
            // invokes the binary for the reconcile plan as STRUCTURED DATA and applies it with
            // `load-env` — see the ENV_NU doc comment.
            let invokes_binary = body.contains("self activate")
                || body.contains("@('self', 'activate', '--shell=powershell')")
                || body.contains("--format json --global env");
            assert!(invokes_binary, "{name} must invoke the ocx binary to activate");
        }
    }

    /// Shim content is independent of the chosen `OCX_HOME` (no substitution):
    /// writing into two different homes produces byte-identical files.
    #[test]
    fn shim_content_identical_across_ocx_home_values() {
        let home_a = tempfile::tempdir().unwrap();
        let home_b = tempfile::tempdir().unwrap();

        write_shims(home_a.path(), false).unwrap();
        write_shims(home_b.path(), false).unwrap();

        for (name, _) in SHIMS {
            let bytes_a = std::fs::read(home_a.path().join(name)).unwrap();
            let bytes_b = std::fs::read(home_b.path().join(name)).unwrap();
            assert_eq!(bytes_a, bytes_b, "{name} must be byte-identical across OCX_HOME values");
        }
    }

    /// `write_shims` writes each shim with the canonical bytes on a fresh home.
    #[test]
    fn write_shims_writes_all_five_with_canonical_bytes() {
        let home = tempfile::tempdir().unwrap();

        let written = write_shims(home.path(), false).unwrap();

        assert_eq!(
            written.len(),
            SHIMS.len(),
            "all five shims must be written on a fresh home"
        );
        for (name, body) in SHIMS {
            let on_disk = std::fs::read_to_string(home.path().join(name)).unwrap();
            assert_eq!(on_disk, body, "{name} on disk must equal the canonical const body");
        }
    }

    /// `write_shims` creates a missing `OCX_HOME` directory (`mkdir -p`).
    #[test]
    fn write_shims_creates_missing_dir() {
        let parent = tempfile::tempdir().unwrap();
        let home = parent.path().join("nested").join(".ocx");
        assert!(!home.exists(), "precondition: home dir absent");

        let written = write_shims(&home, false).unwrap();

        assert!(home.is_dir(), "write_shims must create the OCX_HOME directory");
        assert_eq!(written.len(), SHIMS.len());
    }

    /// The diff-gate skips a shim whose on-disk bytes already match: a second
    /// `write_shims` returns an empty written set.
    #[test]
    fn write_shims_diff_gate_skips_identical_files() {
        let home = tempfile::tempdir().unwrap();

        let first = write_shims(home.path(), false).unwrap();
        assert_eq!(first.len(), SHIMS.len(), "first run writes everything");

        let second = write_shims(home.path(), false).unwrap();
        assert!(second.is_empty(), "diff-gate must skip unchanged shims on re-run");
    }

    /// The diff-gate rewrites only the shims that drifted: tamper with one file,
    /// re-run, and exactly that one is rewritten.
    #[test]
    fn write_shims_rewrites_only_drifted_files() {
        let home = tempfile::tempdir().unwrap();
        write_shims(home.path(), false).unwrap();

        std::fs::write(home.path().join("env.sh"), b"tampered").unwrap();

        let rewritten = write_shims(home.path(), false).unwrap();

        assert_eq!(
            rewritten,
            vec![home.path().join("env.sh")],
            "only the drifted shim is rewritten"
        );
        let restored = std::fs::read_to_string(home.path().join("env.sh")).unwrap();
        assert_eq!(restored, ENV_SH, "the drifted shim is restored to canonical bytes");
    }

    /// `dry_run` writes no byte: it reports the would-write set without touching
    /// the filesystem.
    #[test]
    fn write_shims_dry_run_writes_nothing() {
        let home = tempfile::tempdir().unwrap();

        let would_write = write_shims(home.path(), true).unwrap();

        assert_eq!(
            would_write.len(),
            SHIMS.len(),
            "dry-run reports all five as would-write on a fresh home"
        );
        for (name, _) in SHIMS {
            assert!(
                !home.path().join(name).exists(),
                "{name} must NOT be written on dry-run"
            );
        }
    }

    /// `refresh_shims` is `write_shims(.., false)`: it writes a fresh home and
    /// is diff-gated on re-run, never touching anything but the env.* files.
    #[test]
    fn refresh_shims_writes_then_diff_gates() {
        let home = tempfile::tempdir().unwrap();

        let first = refresh_shims(home.path()).unwrap();
        assert_eq!(first.len(), SHIMS.len(), "refresh writes everything on a fresh home");

        let second = refresh_shims(home.path()).unwrap();
        assert!(second.is_empty(), "refresh is diff-gated on re-run");
    }

    /// The dedicated-file bodies carry their own managed-by-ocx header and the
    /// runtime OCX_HOME fallback (contract 5 — ported from install.sh).
    #[test]
    fn dedicated_file_bodies_use_runtime_ocx_home() {
        // The nu autoload is the full activation (it cannot `source` a runtime
        // `$OCX_HOME/env.nu` path — Nushell `source` needs a parse-time const),
        // so it equals ENV_NU and resolves OCX_HOME at runtime itself.
        assert_eq!(
            nu_autoload_body(),
            ENV_NU,
            "nu autoload must inline the full activation (ENV_NU)"
        );
        assert!(nu_autoload_body().contains(r#"$env.OCX_HOME = ($env.OCX_HOME? | default"#));

        assert!(fish_conf_body().contains("# OCX shell environment - managed by ocx installer."));
        assert!(fish_conf_body().contains("set -q OCX_HOME"));
        assert!(fish_conf_body().contains(r#"source "$_ocx_env""#));
    }

    /// Regression: the Nushell shim must apply activation as DATA (`load-env`
    /// from the `--reconcile` plan), never via the parse-time-broken
    /// constructs that left ocx off `PATH` entirely — a runtime `source (expr)`
    /// (rejected `not_a_constant`), the non-existent `$nu.temp-path` field, or a
    /// subprocess `nu -c` (which mutates only a child's env). See the ENV_NU doc
    /// comment for the full rationale.
    #[test]
    fn env_nu_applies_activation_as_data_not_via_source() {
        // Must use the data-application primitives.
        assert!(ENV_NU.contains("from json"), "env.nu must read the plan as JSON");
        assert!(ENV_NU.contains("load-env"), "env.nu must apply constants via load-env");
        assert!(
            ENV_NU.contains("--offline --format json self activate --reconcile --shell=nushell --shell-pid $nu.pid"),
            "env.nu must resolve the session plan as structured data, carrying its own pid"
        );
        assert!(
            ENV_NU.contains("[$_ocx_bin] ++"),
            "env.nu must prepend the ocx bin dir to PATH directly"
        );
        // The apply must dispatch on the entry MODIFIER TYPE (`type == "path"`),
        // not on `key == "PATH"`: a non-PATH path var such as LD_LIBRARY_PATH
        // must prepend (move-to-front), not be overwritten.
        assert!(
            ENV_NU.contains(NU_PLAN_APPLY),
            "env.nu must embed the shared plan applier verbatim (drift guard)"
        );
        assert!(
            NU_PLAN_APPLY.contains(r#"$_ocx_e.type == "path""#),
            "the applier must dispatch on the entry modifier type"
        );
        assert!(
            !NU_PLAN_APPLY.contains(r#"$_ocx_e.key == "PATH""#),
            "the applier must NOT branch on the key name (LD_LIBRARY_PATH etc. are type:path too)"
        );
        // The retired `--global env` document is not what the shim resolves any more.
        assert!(
            !ENV_NU.contains("--global env"),
            "env.nu must apply the reconcile plan, not the global-only document"
        );
        // Must NOT carry any of the parse-time-broken constructs.
        assert!(
            !ENV_NU.contains("$nu.temp-path"),
            "env.nu must not use the non-existent $nu.temp-path"
        );
        assert!(
            !ENV_NU.contains("| save "),
            "env.nu must not write a temp activation file"
        );
        assert!(
            !ENV_NU.contains("source ("),
            "env.nu must not `source` a runtime path (Nushell source needs a parse-time const)"
        );
        assert!(
            !ENV_NU.contains("nu -c"),
            "env.nu must not shell out to a child `nu -c` (no parent env effect)"
        );
    }

    /// Every other shell's `export_path` removes only the entries equal to the value it places, so unrelated
    /// duplicates in the caller's own PATH survive; `uniq` would collapse them.
    #[test]
    fn nu_plan_apply_path_arm_filters_only_the_placed_value() {
        let start = NU_PLAN_APPLY
            .find(r#"$_ocx_e.type == "path""#)
            .expect("the applier has a path arm");
        let end = NU_PLAN_APPLY
            .find(r#"$_ocx_e.type == "list""#)
            .expect("the applier has a list arm");
        let arm = &NU_PLAN_APPLY[start..end];
        assert!(
            !arm.contains("uniq"),
            "an unrelated duplicate ambient entry must survive: {arm}"
        );
        assert!(
            arm.contains("where {|p| $p != \"\" and not ($p in $_ocx_v) }"),
            "only entries equal to the placed value leave the ambient list: {arm}"
        );
    }

    /// The startup loop and env.nu's move-to-front obey the same rule: only the placed value leaves the
    /// caller's PATH, so unrelated duplicates survive.
    #[test]
    fn nu_startup_path_placement_filters_only_the_placed_value() {
        let start = NU_ENV_APPLY_LOOP
            .find(r#"kind == "path""#)
            .expect("the loop has a path arm");
        let end = NU_ENV_APPLY_LOOP
            .find(r#"kind == "list""#)
            .expect("the loop has a list arm");
        let arm = &NU_ENV_APPLY_LOOP[start..end];
        assert!(
            !arm.contains("uniq"),
            "loop path arm collapses caller duplicates: {arm}"
        );
        assert!(
            arm.contains("not ($p in $_ocx_v)"),
            "loop path arm must filter only the placed value: {arm}"
        );
        assert!(
            !ENV_NU.contains("uniq"),
            "env.nu move-to-front collapses caller duplicates"
        );
        assert!(
            ENV_NU.contains("$p != $_ocx_bin"),
            "env.nu must drop only ocx's own bin entry"
        );
    }

    /// The plan applier mirrors `plan_lines`: sets, then restores, then removes.
    ///
    /// A restore before a remove, because both can name one key whose kind the
    /// scopes disagreed on, and removing first lets the restore write the
    /// retired element straight back.
    #[test]
    fn nu_plan_apply_orders_sets_then_restores_then_removes() {
        let at = |needle: &str| {
            NU_PLAN_APPLY
                .find(needle)
                .unwrap_or_else(|| panic!("the applier lost `{needle}`"))
        };
        let version = at("$_ocx_plan.v?");
        let sets = at("$_ocx_plan.sets?");
        let restores = at("$_ocx_plan.restores?");
        let removes = at("$_ocx_plan.removes?");
        assert!(
            version < sets && sets < restores && restores < removes,
            "the version gate comes first, then sets, restores, removes"
        );
        assert!(
            NU_PLAN_APPLY.starts_with("if ($_ocx_plan.v? | default 0) == 1 { "),
            "a plan of another version, or none, must apply nothing"
        );
        assert!(
            NU_PLAN_APPLY.contains("hide-env -i $_ocx_r.0"),
            "a restore-to-unset must hide the variable, tolerating one already absent"
        );
        assert!(
            NU_PLAN_APPLY.contains("$_ocx_r.2 == null"),
            "a null separator selects path-kind removal, a present one list-kind"
        );
    }

    /// The drift guard and the call sites: the applier appears at startup and inside the hook, and both
    /// calls pass the shell's own pid so the session token names this shell.
    #[test]
    fn env_nu_reconciles_at_startup_and_from_the_prompt_hook_with_its_own_pid() {
        let call = "--offline --format json self activate --reconcile --shell=nushell --shell-pid $nu.pid";
        assert_eq!(
            ENV_NU.matches(call).count(),
            2,
            "one reconcile at startup, one per prompt"
        );
        assert_eq!(
            ENV_NU.matches(NU_PLAN_APPLY).count(),
            2,
            "the startup apply and the hook closure must embed the shared applier verbatim"
        );
        assert!(
            ENV_NU
                .contains("let _ocx_bin = ($env.OCX_HOME | path join 'symlinks/ocx.sh/ocx/cli/current/content/bin')\n"),
            "`$_ocx_bin` names the bin directory; the call joins the executable name itself"
        );
        assert_eq!(
            ENV_NU.matches("^($_ocx_bin | path join 'ocx')").count(),
            2,
            "both calls must name the resolved binary, never a bare `ocx` a user could shadow"
        );
    }

    /// The per-prompt gate skips the exec while nothing changed, and while the stood-down state is unchanged.
    ///
    /// Its two clauses and the `__OCX_ENV_PWD` string must agree with the binary's gate value
    /// (`hook::gate_value`) field for field, or the shim execs every prompt or never.
    #[test]
    fn env_nu_gates_the_prompt_exec_on_the_gate_value_and_the_stood_down_copy() {
        let code = code_only(ENV_NU);
        assert!(
            code.contains(
                "if (($_ocx_stood == \"\") or ($_ocx_state != $_ocx_stood)) and (($_ocx_state == \"\") or \
                 (($env.__OCX_ENV_PWD? | default \"\") != $_ocx_gate))"
            ),
            "the hook must run only while the stood-down copy differs from the state and the gate value moved"
        );
        assert!(
            code.contains(
                "let _ocx_gate = ([$nu.pid $env.PWD ($env.DIRENV_DIR? | default \"\") ($env.MISE_SHELL? | default \"\") \
                 ($env.__MISE_ORIG_PATH? | default \"\") ($env.OCX_CONSENT_PATHS? | default \"\") \
                 ($env.OCX_CONSENT_NAMESPACES? | default \"\")] | str join \" \")"
            ),
            "the gate string must be built as the binary builds it: pid, directory, then each signal"
        );
    }

    /// A re-source replaces its own closure instead of stacking another, and keeps the user's hooks.
    #[test]
    fn env_nu_registers_one_marked_prompt_hook() {
        let code = code_only(ENV_NU);
        assert_eq!(
            code.matches("let __ocx_prompt_hook = 1;").count(),
            1,
            "the registered closure carries the marker"
        );
        assert!(
            code.contains("view source $h | str contains '__ocx_prompt_hook'"),
            "a re-source must drop the closure that carries the marker before appending"
        );
        assert!(
            code.contains("$env.config.hooks.pre_prompt = (("),
            "the hook list is rebuilt from the existing value, never assigned a literal"
        );
    }

    /// The apply loop dispatches four ways, not two.
    ///
    /// `path`, `list` and `constant` each get their own arm and an entry of any other
    /// `kind` applies **nothing**. A two-way branch sent a `list` entry down the `else`
    /// arm as a constant, so a global `CFLAGS = { type = "list", … }` CLOBBERED the
    /// caller's `CFLAGS` instead of folding into it, and would mis-apply any later kind.
    ///
    /// Structural: no `nu` is installed on any cargo leg. The behavioural half is
    /// the shell-zoo acceptance test.
    #[test]
    fn nu_apply_loop_dispatches_on_all_three_modifier_kinds() {
        for kind in ["path", "list", "constant"] {
            assert!(
                NU_ENV_APPLY_LOOP.contains(&format!(r#"$_ocx_e.kind == "{kind}""#)),
                "the apply loop must carry its own arm for `{kind}`"
            );
        }
        // The list arm is the `export_list` fold, on the entry's OWN separator —
        // not `char esep`, and not an overwrite.
        assert!(
            NU_ENV_APPLY_LOOP.contains("$_ocx_e.separator?"),
            "the list arm must fold on the entry's effective separator"
        );
        assert!(
            NU_ENV_APPLY_LOOP.contains("str replace --all"),
            "the list arm must remove every prior occurrence before appending (unique-append fold)"
        );
        // Four-way means the fourth way is "apply nothing": an unrecognised
        // `kind` must not fall through to a `load-env` that overwrites.
        let tail = NU_ENV_APPLY_LOOP
            .rsplit_once(r#"$_ocx_e.kind == "constant""#)
            .expect("the constant arm anchors the tail")
            .1;
        assert!(
            !tail.contains("else"),
            "an unrecognised modifier type must apply nothing, not fall through: {tail}"
        );
    }

    /// Regression: the Nushell apply loop reads an existing env value with a flag-free
    /// construct, never `get --optional`.
    ///
    /// `get --optional` postdates nu 0.101.0, and nu parses a whole file before running it, so
    /// on an older-but-supported nu the unknown flag is a PARSE error that voids the whole
    /// vendor-autoload, taking the `$env.PATH` prepend with it. The replacement is
    /// `($_ocx_e.key in ($env | columns))` plus a dynamic `get`. Pinned on both the shared
    /// const and its inlined copy in [`ENV_NU`] (drift guard).
    #[test]
    fn nu_apply_loop_reads_env_without_the_get_optional_flag() {
        for (name, body) in [
            ("NU_ENV_APPLY_LOOP", NU_ENV_APPLY_LOOP),
            ("NU_PLAN_APPLY", NU_PLAN_APPLY),
            ("ENV_NU", ENV_NU),
        ] {
            assert!(
                !body.contains("get --optional"),
                "{name} must not use `get --optional` (absent on nu < the version that added it; \
                 its unknown flag voids the whole autoload at parse time, dropping the PATH prepend)"
            );
        }
        for (name, body, needle) in [
            (
                "NU_ENV_APPLY_LOOP",
                NU_ENV_APPLY_LOOP,
                "$_ocx_e.key in ($env | columns)",
            ),
            ("NU_PLAN_APPLY", NU_PLAN_APPLY, "$_ocx_k in ($env | columns)"),
        ] {
            assert!(
                body.contains(needle),
                "{name} must read an existing value via the flag-free `key in ($env | columns)` form"
            );
        }
    }

    /// The plan applier dispatches on `path`, `list` and `constant` and applies nothing for any other type.
    #[test]
    fn nu_plan_apply_dispatches_on_all_three_modifier_kinds() {
        for kind in ["path", "list", "constant"] {
            assert!(
                NU_PLAN_APPLY.contains(&format!(r#"$_ocx_e.type == "{kind}""#)),
                "the applier must carry its own arm for `{kind}`"
            );
        }
        assert!(
            NU_PLAN_APPLY.contains("$_ocx_e.separator?"),
            "the list arm must fold on the entry's effective separator"
        );
        let tail = NU_PLAN_APPLY
            .split_once(r#"$_ocx_e.type == "constant""#)
            .expect("the constant arm anchors the tail")
            .1;
        let sets_arm = tail
            .split_once("; for _ocx_r")
            .expect("the restores loop follows the sets loop")
            .0;
        assert!(
            !sets_arm.contains("else"),
            "an unrecognised type must apply nothing, not fall through: {sets_arm}"
        );
    }

    // ── how the hook reaches each family ─────────────

    /// The four families whose shim hands the output of `ocx self activate` back to the
    /// **sourcing** shell — `eval`, `| source`, `Invoke-Expression`, `eval (… | slurp)`.
    /// Whatever `shell/hook.rs` emits rides that stream, so registering a hook needs no shim
    /// change and the body stays a pure dispatcher.
    ///
    /// The name says eval-capable, not hooked: `hook::registration` returns `None` for ash,
    /// ksh and dash, so `env.sh` delivers a hook only when the sourcing shell is bash or zsh.
    /// Nushell is absent: it has no string `eval` and `source` needs a parse-time-constant
    /// path, so its hook is inlined in [`ENV_NU`] and it is exempt from the denylist below.
    const EVAL_CAPABLE: [(&str, &str); 4] = [
        ("env.sh", ENV_SH),
        ("env.fish", ENV_FISH),
        ("env.ps1", ENV_PS1),
        ("env.elv", ENV_ELV),
    ];

    /// Each eval-capable shim must evaluate the activation stream **in the sourcing
    /// shell's own scope** and must not opt out of the hook.
    ///
    /// Capturing the stream into a variable, a file or a subshell still "invokes the binary"
    /// while silently dropping every hook and wrapper definition the stream carries.
    /// Scanned comment-stripped: against the raw const, a commented-out activation line
    /// still matches its needle and the guard stays green (`code_only`).
    #[test]
    fn each_eval_capable_shim_evaluates_the_activation_stream_in_its_own_scope() {
        let sh = code_only(ENV_SH);
        let fish = code_only(ENV_FISH);
        let power_shell = code_only(ENV_PS1);
        let elvish = code_only(ENV_ELV);
        // The eval channel, per arm.
        assert!(
            sh.contains(r#"eval "$("$_ocx_bin" self activate"#),
            "env.sh must eval the activation stream in the sourcing shell"
        );
        assert!(
            fish.contains("self activate --shell=fish --interactive 2>/dev/null | source")
                && fish.contains("self activate --shell=fish --no-interactive 2>/dev/null | source"),
            "env.fish must pipe both activation arms into `source`"
        );
        assert!(
            power_shell.contains("Invoke-Expression $_ocxActivate"),
            "env.ps1 must Invoke-Expression the activation stream in the session scope"
        );
        assert!(
            elvish.contains("eval ($_ocx_bin self activate --shell=elvish --no-completion"),
            "env.elv must eval the activation stream in the sourcing shell"
        );
        // No arm opts out of the hook: the flag would suppress exactly the
        // registration this stream exists to carry.
        for (name, body) in EVAL_CAPABLE {
            assert!(
                !code_only(body).contains("--no-hook"),
                "{name} must not suppress the hook the activation stream carries"
            );
        }
    }

    /// No shim forces completions ON.
    ///
    /// `--completion` is rung 2 of the completions ladder, above `OCX_NO_COMPLETION` and
    /// `[shell] completions`, so a shim that spells its answer there revokes both opt-outs;
    /// no shim passes `--hook` either. Scans for the forcing spelling only: the remaining
    /// `--no-completion` uses are not policy overrides (the POSIX and elvish streams carry no
    /// completions by construction, and Windows PowerShell below 5.0 cannot run clap's
    /// `using namespace` output).
    #[test]
    fn no_shim_forces_completions_on() {
        for (name, body) in SHIMS {
            assert!(
                !code_only(body).contains("--completion"),
                "{name} must not pass --completion: it is rung 2 of the ladder and outranks both \
                 OCX_NO_COMPLETION and `[shell] completions`"
            );
        }
    }

    /// The separate completion injection consults the ladder.
    ///
    /// `env.sh` and `env.elv` re-run `ocx shell completion` on every interactive source
    /// (zsh's `compinit` runs after `.zprofile` and would wipe a one-shot registration).
    /// That generator prints unconditionally by default, so the shim must pass
    /// `--if-enabled` plus its own measured interactivity, or every installed POSIX and
    /// elvish shell loads completions whatever `OCX_NO_COMPLETION` and
    /// `[shell] completions` say. Positive and per-arm: a denylist cannot prove a flag present.
    #[test]
    fn separate_completion_injection_consults_the_ladder() {
        assert!(
            code_only(ENV_SH).contains(r#"shell completion --shell="$_ocx_shell" --if-enabled "$_ocx_interactive""#),
            "env.sh must ask the ladder, and state its interactivity, on its separate completion call"
        );
        assert!(
            code_only(ENV_ELV).contains("shell completion --shell=elvish --if-enabled $_ocx_interactive"),
            "env.elv must ask the ladder, and state its interactivity, on its separate completion call"
        );
    }

    /// Every eval-capable shim decides its own
    /// interactivity and passes it explicitly.
    ///
    /// The binary cannot answer this for itself. Each body below runs the
    /// activation with its stderr redirected to `/dev/null`, so a stderr probe
    /// reads `false` in every real shell; and stdin is no better in the other
    /// direction, because `ssh -t host 'bash -lc …'` hands a terminal to a shell
    /// that reads the login profile and exits without ever rendering a prompt.
    ///
    /// Per-arm and **positive**: each language spells its own interactivity test
    /// differently, and a denylist can only prove a flag absent, never present.
    ///
    /// The answer must ride `--interactive`/`--no-interactive` and never
    /// `--hook`. That one is rung 2 of the ladder, above `OCX_NO_HOOK` and
    /// `[shell] hook`, so a shim spelling its answer there would revoke both
    /// opt-outs for every shell it starts.
    #[test]
    fn each_eval_capable_shim_states_its_own_interactivity() {
        let sh = code_only(ENV_SH);
        assert!(
            sh.contains(r#"case "$-" in"#) && sh.contains("_ocx_interactive=--interactive"),
            "env.sh must derive interactivity from `$-`"
        );
        assert!(
            sh.contains(r#"self activate --shell="$_ocx_shell" --no-completion "$_ocx_interactive""#),
            "env.sh must pass its own answer to `self activate`"
        );

        let fish = code_only(ENV_FISH);
        assert!(
            fish.contains("status is-interactive")
                && fish.contains("--shell=fish --interactive")
                && fish.contains("--shell=fish --no-interactive"),
            "env.fish must carry `status is-interactive` through BOTH activation arms"
        );

        let power_shell = code_only(ENV_PS1);
        assert!(
            power_shell.contains("$_ocxInter = [Console]::IsInputRedirected -eq $false")
                && power_shell.contains("$_ocxArgs += if ($_ocxInter) { '--interactive' } else { '--no-interactive' }"),
            "env.ps1 must derive interactivity from [Console]::IsInputRedirected"
        );
        // The trap this arm shipped through once: on .NET for Unix
        // `[Environment]::UserInteractive` is HARDCODED true, so it declares a
        // script, a CI step and `ssh host pwsh -Command ...` interactive — the
        // mirror image of the outage the explicit flag exists to close.
        // Measured on Linux pwsh 7.6: non-tty UserInteractive=True
        // IsInputRedirected=True; pty UserInteractive=True IsInputRedirected=False.
        assert!(
            !power_shell.contains("[Environment]::UserInteractive"),
            "env.ps1 must not probe [Environment]::UserInteractive: it is hardcoded true on .NET for Unix, so \
             every non-interactive pwsh would register the per-prompt hook"
        );

        let elvish = code_only(ENV_ELV);
        assert!(
            elvish.contains("if ?(test -t 0) { set _ocx_interactive = '--interactive' }")
                && elvish.contains("--shell=elvish --no-completion $_ocx_interactive"),
            "env.elv must probe stdin with `test -t 0` and pass the answer through"
        );

        for (name, body) in EVAL_CAPABLE {
            assert!(
                !code_only(body).contains("--hook"),
                "{name} must state interactivity, not force the hook on: `--hook` is rung 2 and \
                 outranks OCX_NO_HOOK and `[shell] hook`"
            );
        }
    }

    /// The nushell prompt hook is **appended** onto a fully
    /// `default`-ed path, never assigned over.
    ///
    /// starship's nushell integration owns the same slot, so an assignment
    /// silently stops the user's prompt updating; and nushell parses a whole
    /// file before running it, so an `append` against an absent `hooks` key in a
    /// `nu -n` session would void this entire autoload — taking the `$env.PATH`
    /// prepend above it down too.
    #[test]
    fn env_nu_appends_its_prompt_hook_onto_a_fully_defaulted_path() {
        // Comment-stripped for the same reason the thin-dispatcher denylist is: the body's
        // own comment names both forms it forbids, which is what a comment
        // documenting a hazard is *for*.
        let code = code_only(ENV_NU);
        // The append-and-assign-back form: existing value first, filtered of
        // its own previous registration, `++`, then a one-element list whose
        // element opens a CLOSURE (`{|`).
        assert!(
            code.contains("let _ocx_pre = ($env.config.hooks.pre_prompt? | default [])")
                && code.contains(
                    "$env.config.hooks.pre_prompt = (((if ($_ocx_pre | describe | str starts-with 'list') \
                     { $_ocx_pre } else { [$_ocx_pre] })"
                )
                && code.contains(") ++ [{||"),
            "env.nu must append its prompt hook closure onto the existing value, never replace it"
        );
        // A present-but-scalar `.pre_prompt` (a bare closure, which nu accepts)
        // must be normalised to a list before the append: `++` against a
        // closure is a type error the surrounding try/catch would swallow,
        // dropping the hook with no diagnostic. Defaulting an absent path does
        // not cover this.
        assert!(
            code.contains("else { [$_ocx_pre] }"),
            "env.nu must wrap a non-list prompt hook value into a list before appending"
        );
        // Every intermediate level defaulted, so an absent `hooks` key cannot error.
        assert!(
            code.contains("$env.config.hooks = ($env.config.hooks? | default {})"),
            "env.nu must default $env.config.hooks before reaching into it"
        );
        // The two forms that overwrite the user's hooks.
        assert!(
            !code.contains("$env.config.hooks = {"),
            "env.nu must never assign a wholesale hooks record over the user's"
        );
        assert!(
            !code.contains("$env.config.hooks.pre_prompt = [") && !code.contains("$env.config.hooks.pre_prompt = ({"),
            "env.nu must never assign `.pre_prompt` without appending the existing value first"
        );
        // The directory-change hook is not registered: the prompt gate already
        // compares the directory, and a second registration would reconcile twice per `cd`.
        assert!(
            !code.contains("env_change"),
            "env.nu must not register an env_change hook beside the prompt hook"
        );
        // The dedicated nushell autoload is ENV_NU, so it carries the hook too.
        assert!(
            code_only(nu_autoload_body()).contains("$env.config.hooks.pre_prompt = (("),
            "the nushell vendor-autoload body must carry the prompt hook"
        );
    }

    // ── the thin-dispatcher guard ────────────────────────────────

    /// Business-logic tokens that must not appear in an eval-capable dispatcher
    /// body. The shims resolve `$OCX_HOME`, find `ocx` through the
    /// `current` symlink and hand the binary's output to the shell; consent,
    /// trust, the ledger, reconciliation and the activation whitelist are the
    /// binary's job, and inlining any of them is what turns a dispatcher into a
    /// second implementation that only heals one `self update` later.
    ///
    /// A tripwire for the likely accident, not the contract itself — a denylist
    /// cannot enumerate every way to write the forbidden shape. The ceiling
    /// below is its blunt backstop.
    const BUSINESS_LOGIC_TOKENS: [&str; 8] = [
        "consent",
        "trust",
        "ledger",
        "reconcile",
        "priors",
        "__OCX_ENV_STATE",
        "OCX_CONSENT_PATHS",
        "OCX_CONSENT_NAMESPACES",
    ];

    /// Drop blank lines and whole-line `#` comments — the comment syntax all
    /// five families share — leaving only the lines that actually execute.
    ///
    /// **The stripping is load-bearing, not tidiness.** A denylist that names
    /// the forms it forbids matches its own comment: documenting *why* a shim
    /// carries no reconciliation logic necessarily writes the word down, and a
    /// guard whose needle is a literal in the file it scans is measuring itself.
    /// `the_business_logic_scan_strips_comments_before_matching` proves this
    /// function is doing work rather than being decorative.
    ///
    /// Line-oriented on purpose: a trailing-`#` stripper would have to know each
    /// family's string-literal rules to avoid eating a `#` inside a quoted value.
    fn code_only(body: &str) -> String {
        body.lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
            .fold(String::new(), |mut accumulated, line| {
                accumulated.push_str(line);
                accumulated.push('\n');
                accumulated
            })
    }

    /// No eval-capable shim body carries ocx business logic.
    ///
    /// Nushell is **exempt**: it has no string `eval`, so it
    /// inlines the apply by necessity. Scanning it here would force the guard
    /// green-by-exception on the one family that genuinely violates the spirit.
    ///
    /// EC-EMIT-009, the denylist half of the thin-dispatcher invariant. The test once cited
    /// for the invariant, `each_shim_resolves_the_binary_through_the_current_symlink`,
    /// does not enforce it: a body that *invokes* the binary can carry any
    /// amount of inlined reconciliation beside the invocation and still pass.
    /// This is the check that fails when it does. The blunt backstop — a
    /// per-family byte ceiling, which also covers the nushell body the denylist
    /// exempts — is [`each_shim_body_stays_under_its_dispatcher_ceiling`].
    #[test]
    fn eval_capable_shim_bodies_carry_no_business_logic_token() {
        for (name, body) in EVAL_CAPABLE {
            let code = code_only(body).to_ascii_lowercase();
            for token in BUSINESS_LOGIC_TOKENS {
                assert!(
                    !code.contains(&token.to_ascii_lowercase()),
                    "{name} must stay a pure dispatcher: `{token}` is ocx business logic and \
                     belongs in the binary, not in a shim body that heals one `self update` later"
                );
            }
        }
    }

    /// The comment strip is load-bearing: `env.sh` documents the hook
    /// registration that rides its activation eval, and that sentence contains a
    /// denylisted token. Delete [`code_only`]'s filter and the denylist reds on
    /// its own documentation.
    #[test]
    fn the_business_logic_scan_strips_comments_before_matching() {
        assert!(
            ENV_SH.to_ascii_lowercase().contains("reconcile"),
            "env.sh's comments must keep naming the reconcile hook they register — this is the \
             canary that keeps the comment strip honest"
        );
        assert!(
            !code_only(ENV_SH).to_ascii_lowercase().contains("reconcile"),
            "code_only must strip comments before the denylist runs"
        );
        // Same trap, second needle: `env.ps1`'s interactivity denylist forbids
        // `[Environment]::UserInteractive`, and the comment that explains WHY
        // has to write it down. Without the strip that guard would match its own
        // documentation — a detector measuring itself, green in every state.
        assert!(
            ENV_PS1.contains("[Environment]::UserInteractive"),
            "env.ps1's comment must keep naming the probe it refuses — the canary for the strip below"
        );
        assert!(
            !code_only(ENV_PS1).contains("[Environment]::UserInteractive"),
            "code_only must strip env.ps1's comments before the interactivity denylist runs"
        );
    }

    // Per-family ceilings on the **comment-stripped** body, in bytes.
    //
    // Derivation — measured from the shipped bodies, then a headroom factor,
    // rounded up to the next 50 bytes:
    //
    // | family      | code bytes | factor | ceiling | headroom |
    // |-------------|-----------:|-------:|--------:|---------:|
    // | `env.sh`    |        715 |  ×1.5  |     950 |      235 |
    // | `env.fish`  |        406 |  ×1.5  |     600 |      194 |
    // | `env.ps1`   |        940 |  ×1.5  |    1250 |      310 |
    // | `env.nu`    |       5406 | ×1.00  |    5450 |       44 |
    // | `env.elv`   |        479 |  ×1.5  |     550 |       71 |
    // | fish loader |        153 |  ×1.5  |     250 |       97 |
    //
    // The four eval-capable bodies grew when each shim began stating its own
    // interactivity (`--interactive`/`--no-interactive`) instead of leaving the
    // binary to probe a descriptor the shim redirects. `env.ps1` grew again,
    // 912 -> 940, when that arm swapped `[Environment]::UserInteractive` (which
    // is hardcoded true on .NET for Unix, so it declared every script and CI
    // step interactive) for `[Console]::IsInputRedirected`. No ceiling moved:
    // the growth spends headroom, which is the direction that makes a ceiling
    // tighter, not looser.
    //
    // **Bytes, not lines.** A line ceiling is defeated by a single 2 KB
    // one-liner — precisely the shape nushell's inlined apply already has, at
    // 460 bytes on one line. Bytes catch both a long one-liner and many lines.
    //
    // **The headroom column is the number that matters, and it is asserted, not
    // claimed.** A ceiling is only a guard if the gap between it and the shipped
    // body is smaller than the thing it is meant to refuse, so
    // [`INLINED_LOGIC_FLOOR`] pins that: every headroom must stay under it, and
    // the ceiling test checks the whole table on every run. Raising a constant
    // without re-measuring reds. `env.nu` takes no headroom factor for exactly this
    // reason — a ×1.25 gap would be 1352 bytes, wide enough to admit the very
    // reconciler the ceiling exists to refuse, and nushell is the one family that
    // is exempt from the denylist, so here the ceiling is the *only*
    // mechanical guard there is.
    //
    // Nushell is also the family that legitimately grows. It cannot `eval` what
    // the binary prints, so it applies the reconcile plan as data, and the apply
    // has to run inside the session: the unique-append fold and the element
    // removal need the AMBIENT value, which only a statement running in the nu
    // session can read. The plan applier is written out twice (startup and the
    // prompt hook), so raising the constant is a deliberate act in the commit
    // that grows the body, with the new measurement written into this table:
    // 2900 -> 5450 when the shim began applying the plan in-shell, 5450 -> 5550 for
    // the exact-match path arm. Measured at 5545.
    const CEILING_ENV_SH: usize = 950;
    const CEILING_ENV_FISH: usize = 600;
    const CEILING_ENV_PS1: usize = 1250;
    const CEILING_ENV_NU: usize = 5550;
    const CEILING_ENV_ELV: usize = 550;
    const CEILING_FISH_CONF: usize = 250;

    /// The smallest inlined-reconciler-shaped body a ceiling has to refuse.
    ///
    /// Measured against the real thing: nushell's apply loop — one `for` with a
    /// two-way branch and two `load-env` calls, the least a reconciler can be —
    /// is 460 bytes. A headroom wider than this makes its ceiling decorative,
    /// because the forbidden shape fits underneath it.
    const INLINED_LOGIC_FLOOR: usize = 500;

    const BODY_CEILINGS: [(&str, &str, usize); 6] = [
        ("env.sh", ENV_SH, CEILING_ENV_SH),
        ("env.fish", ENV_FISH, CEILING_ENV_FISH),
        ("env.ps1", ENV_PS1, CEILING_ENV_PS1),
        ("env.nu", ENV_NU, CEILING_ENV_NU),
        ("env.elv", ENV_ELV, CEILING_ENV_ELV),
        ("conf.d/ocx.fish", FISH_CONF, CEILING_FISH_CONF),
    ];

    /// Every emitted body stays under its per-family ceiling.
    ///
    /// EC-EMIT-009, the size half of the thin-dispatcher invariant, and the ONLY mechanical
    /// guard on `env.nu`, which is exempt from the denylist in
    /// [`eval_capable_shim_bodies_carry_no_business_logic_token`]. A denylist
    /// catches the likely accident by name; the ceiling catches an inlining
    /// that happens to avoid every listed word.
    #[test]
    fn each_shim_body_stays_under_its_dispatcher_ceiling() {
        for (name, body, ceiling) in BODY_CEILINGS {
            let measured = code_only(body).len();
            assert!(
                measured <= ceiling,
                "{name} is {measured} comment-stripped bytes, over its {ceiling}-byte dispatcher \
                 ceiling — a shim that grew this much is carrying logic that belongs in the binary; \
                 raise the constant only in the commit that deliberately grows the body"
            );
            // The ceiling must also still be tight enough to mean something. A
            // constant raised without re-measuring drifts away from its body
            // until the forbidden shape fits underneath it, at which point the
            // check is green for every input and indistinguishable from absent.
            assert!(
                ceiling - measured < INLINED_LOGIC_FLOOR,
                "{name}'s ceiling leaves {headroom} bytes of headroom, at or over the \
                 {INLINED_LOGIC_FLOOR}-byte floor an inlined reconciler occupies — the ceiling no \
                 longer refuses what it exists to refuse. Re-measure the body and tighten the \
                 constant, and update the derivation table above with both numbers",
                headroom = ceiling - measured
            );
        }
    }

    /// Every family resolves the binary through the `current` symlink
    /// **unconditionally**: `OCX_BINARY_PIN` has three consumers (the Windows
    /// `.exe` shim, the script host's `ocx` module, generated Unix launchers),
    /// all of them re-entrant downstream invocations. An interactive shell's own
    /// top-level resolution is upstream of that mechanism and must not consult it.
    #[test]
    fn no_shim_body_reads_the_binary_pin() {
        for (name, body) in SHIMS {
            assert!(
                !body.contains("OCX_BINARY_PIN"),
                "{name} must resolve ocx through the `current` symlink unconditionally; the pin \
                 must never reach a shell's own top-level resolution"
            );
        }
    }

    /// Regression: the elvish shim must DECOUPLE shell completion from PATH
    /// activation.
    ///
    /// clap_complete's elvish completer sets `edit:completion:arg-completer`, and
    /// the `edit:` namespace is bound only when an interactive line editor (a real
    /// TTY) is active; a non-TTY interactive shell has no `edit:`, so that block
    /// raises a compile error. Elvish compiles an `eval` unit as a whole, so the
    /// previous form — completion and the PATH prepend in ONE
    /// `eval (… --completion | slurp)` — let that compile error void the PATH
    /// prepend, leaving ocx off PATH. PATH must run in its own `--no-completion`
    /// eval unit, and completion in a separate `try`/`catch`-guarded unit so its
    /// failure cannot break PATH.
    #[test]
    fn env_elv_decouples_completion_from_path_activation() {
        // PATH activation runs with --no-completion in its own eval unit.
        assert!(
            ENV_ELV.contains("self activate --shell=elvish --no-completion"),
            "env.elv must activate PATH with --no-completion so a completion error cannot void it"
        );
        // The coupled single-unit form (completion in the PATH activation call)
        // must be gone.
        assert!(
            !ENV_ELV.contains("self activate --shell=elvish --completion"),
            "env.elv must NOT request completion in the PATH activation eval unit (the coupled form)"
        );
        // Completion loads via a separate `shell completion` invocation, guarded
        // by try/catch so a compile error (no edit: in a non-TTY shell) is caught.
        assert!(
            ENV_ELV.contains("shell completion --shell=elvish"),
            "env.elv must load completion via a separate `shell completion` invocation"
        );
        assert!(
            ENV_ELV.contains("try {") && ENV_ELV.contains("} catch"),
            "the completion eval must be wrapped in try/catch so its failure cannot break PATH"
        );
    }
}
