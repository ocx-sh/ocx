// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-shell env shims OCX writes into `$OCX_HOME` (contract 3).
//!
//! Most shims are thin loaders that delegate to `ocx self activate` at shell
//! startup (the POSIX/fish/elvish/PowerShell families `eval`/`source`/`slurp`
//! its output). Nushell is the exception: it has no string `eval` and `source`
//! needs a parse-time-constant path, so [`ENV_NU`] applies activation as
//! structured DATA (`load-env` from `ocx --format json --global env`) — see its
//! doc comment. The bodies are **byte-identical across users** — there is NO
//! install-time substitution; `OCX_HOME` is resolved at runtime by the shim
//! itself (`: "${OCX_HOME:=$HOME/.ocx}"` and the per-shell equivalents). The
//! consts below are the single source of truth.
//!
//! [`write_shims`] writes all five `env.*` files atomically with a diff-gate
//! (a file whose bytes already match is left untouched). [`refresh_shims`] is
//! the same operation under an intent-revealing name for the `ocx self update`
//! post-swap hook (Decision 4C) — the shims are ocx-owned, so a refresh never
//! consults user edits.

use std::path::{Path, PathBuf};

use crate::setup::error::Error;
use crate::utility::fs::write_bytes_atomic;

/// `$OCX_HOME/env.sh` — POSIX (sh/bash/zsh) shim.
///
/// Byte-identical to the former `install.sh` `env.sh` heredoc body.
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
# and side-effect-free.
if [ "$_ocx_interactive" = --interactive ] && [ -x "$_ocx_bin" ] && [ "$_ocx_shell" != sh ]; then
    eval "$("$_ocx_bin" shell completion --shell="$_ocx_shell" 2>/dev/null)" || true
fi
unset _ocx_bin _ocx_shell _ocx_interactive
"#;

/// `$OCX_HOME/env.fish` — fish shim.
///
/// Byte-identical to the former `install.sh` `env.fish` heredoc body.
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
        "$_ocx_bin" self activate --shell=fish --completion --interactive 2>/dev/null | source
    else
        "$_ocx_bin" self activate --shell=fish --no-completion --no-interactive 2>/dev/null | source
    end
end
"#;

/// `$OCX_HOME/env.ps1` — PowerShell shim.
///
/// Byte-identical to the former `install.sh` `env.ps1` heredoc body, which in
/// turn matched the `install.ps1` `Create-EnvFile` here-string line-for-line.
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
if (Test-Path $_ocxBin -PathType Leaf) {
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
    # Request completions only on an interactive PowerShell 5.0+ session: legacy
    # Windows PowerShell <5.0 cannot run clap's `using namespace` /
    # `Register-ArgumentCompleter -Native` completion output, so it opts out with
    # --no-completion while still emitting PATH + global env.
    $_ocxArgs = @('self', 'activate', '--shell=powershell')
    $_ocxInter = [Console]::IsInputRedirected -eq $false
    $_ocxArgs += if ($_ocxInter) { '--interactive' } else { '--no-interactive' }
    if ($_ocxInter -and $PSVersionTable.PSVersion.Major -ge 5) {
        $_ocxArgs += '--completion'
    } else {
        $_ocxArgs += '--no-completion'
    }
    $_ocxActivate = (& $_ocxBin @_ocxArgs 2>$null) | Out-String
    # Guard $null/empty: Out-String of empty/failed output yields $null, and
    # `Invoke-Expression $null` throws "Cannot bind argument ... is null".
    if ($_ocxActivate) { Invoke-Expression $_ocxActivate }
}
Remove-Variable _ocxBase, _ocxExe, _ocxBin, _ocxInter, _ocxArgs, _ocxActivate -ErrorAction SilentlyContinue
"#;

/// The Nushell loop that applies one `ocx --format json --global env` document to
/// `$env`, dispatching **four ways** on each entry's modifier `type`, exactly like
/// the shell emitters (`Shell::Nushell::export_path` / `export_list` /
/// `export_constant`):
///
/// - `type == "path"` — prepend the value's segment(s) to the named key,
///   move-to-front (drop the existing occurrence + empties via `uniq`), and store
///   the result as a separator-joined **string**. Strings (not lists) are
///   required because Nushell silently drops a LIST-valued non-`PATH` env var when
///   it spawns an external; `PATH` itself round-trips fine as a string.
/// - `type == "list"` — the unique-append fold on the entry's **effective
///   separator** (`separator`, defaulting to `" "`): wrap the ambient in the
///   separator, delete every `sep + value + sep` to a fixpoint, strip the leading
///   separator, append the value. The same function
///   [`export_list`](crate::shell::Shell::export_list) computes, so an entry
///   applied here and one applied through an emitted stream agree byte for byte.
/// - `type == "constant"` — replace any existing value with `load-env`.
/// - **anything else — apply nothing.** There is no `else`. A two-way branch sent
///   a `list` entry down the constant arm and CLOBBERED the caller's value where
///   every other arm appends; leaving the fall-through in place would do the same
///   to whatever modifier kind is added next. Refusing an entry no arm
///   understands is the only safe answer a shim can give.
///
/// Dispatching on `type` (not `key == "PATH"`) is essential: a package may declare
/// any key (`LD_LIBRARY_PATH`, `PKG_CONFIG_PATH`, …) as `type: path`, and that must
/// prepend, not overwrite. The caller must bind `$_ocx_json` to the parsed record
/// first. Shared **verbatim** between [`ENV_NU`] and the
/// `self activate --shell=nushell` line so the two cannot drift.
///
/// The existing-value read uses `($_ocx_e.key in ($env | columns))` + a dynamic
/// `get` rather than the terser `get --optional`: `get --optional` was added to
/// Nushell *after* 0.101.0, and Nushell parses an entire file before running it,
/// so on an older-but-supported nu the unknown flag is a PARSE error that voids
/// the whole vendor-autoload — dropping the PATH prepend with it. The
/// flag-free form uses only pre-0.101 stable features. Do not "simplify" it back
/// to `get --optional` (the `nu_apply_loop_reads_env_without_the_get_optional_flag`
/// test guards this).
pub const NU_ENV_APPLY_LOOP: &str = "for _ocx_e in ($_ocx_json.entries? | default []) { if $_ocx_e.type == \"path\" { let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { \"\" }); load-env {($_ocx_e.key): (($_ocx_e.value | split row (char esep)) ++ ($_ocx_cur | split row (char esep)) | where {|p| $p != \"\" } | uniq | str join (char esep))} } else if $_ocx_e.type == \"list\" { let _ocx_s = ($_ocx_e.separator? | default \" \"); let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { \"\" }); let _ocx_p = ($_ocx_s + $_ocx_e.value + $_ocx_s); mut _ocx_l = (if $_ocx_cur == \"\" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_e.key): (($_ocx_l | str replace $_ocx_s \"\") + $_ocx_e.value)} } else if $_ocx_e.type == \"constant\" { load-env {($_ocx_e.key): $_ocx_e.value} } }";

/// `$OCX_HOME/env.nu` — Nushell shim.
///
/// Unlike the other shims, Nushell activation cannot delegate to
/// `ocx self activate` via an `eval`/`source`: Nushell has no string `eval`, and
/// `source` requires a **parse-time-constant** path AND reads the file at PARSE
/// time — so the older "write `self activate` output to a temp file, then
/// `source` it" form failed two ways (a runtime `source (expr)` is rejected as
/// `not_a_constant`, and even with a constant path the file does not exist when
/// `source` parses). Nushell therefore applies activation as **data**: the ocx
/// bin dir is prepended to `$env.PATH` directly (a fixed path under `$OCX_HOME`),
/// and the global toolchain env is read from `ocx --format json --global env` and
/// applied by [`NU_ENV_APPLY_LOOP`] (which dispatches on each entry's modifier
/// type). No temp file, no `source`, no subprocess `nu -c` (which would mutate
/// only a child's env). Idempotent: every path apply is a move-to-front
/// (`uniq`), so a re-source never duplicates a segment; the `try/catch` keeps a
/// malformed global lock from aborting the (already-applied) bin-on-PATH step.
///
/// The same limitation makes this the only shim that carries its own
/// per-directory-change hook: the other four families receive theirs inside the
/// activation stream they `eval`. It is **appended** with `++` onto
/// `($env.config.hooks?.env_change?.PWD? | default [])`, never assigned over,
/// and every intermediate level is defaulted — starship owns the same slot, and
/// an absent `hooks` key in a `nu -n` session would otherwise error and void the
/// whole autoload. The body must run **after** the user's `config.nu`, which the
/// `$nu.vendor-autoload-dirs` slot [`nu_autoload_body`] targets provides.
pub const ENV_NU: &str = r#"# Managed by ocx installer - do not edit.
# No double-source guard: activation is idempotent move-to-front, so re-sourcing
# never duplicates a PATH entry. An exported guard would leak into child shells
# (e.g. VS Code Remote terminals) and wrongly suppress activation.

$env.OCX_HOME = ($env.OCX_HOME? | default ($env.HOME | path join '.ocx'))

let _ocx_bin = ($env.OCX_HOME | path join 'symlinks/ocx.sh/ocx/cli/current/content/bin')
if (($_ocx_bin | path join 'ocx') | path exists) {
    # ocx bin on PATH, move-to-front. $env.PATH is a list at startup; normalize to
    # segments, prepend, dedup, and store as a separator-joined string so the
    # global-env apply below operates uniformly on string-valued path vars.
    $env.PATH = ([$_ocx_bin] ++ ($env.PATH | (if ($in | describe) == 'string' { split row (char esep) } else { $in })) | where {|p| $p != "" } | uniq | str join (char esep))
    try {
        let _ocx_json = (^($_ocx_bin | path join 'ocx') --format json --global env | from json)
        for _ocx_e in ($_ocx_json.entries? | default []) { if $_ocx_e.type == "path" { let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { "" }); load-env {($_ocx_e.key): (($_ocx_e.value | split row (char esep)) ++ ($_ocx_cur | split row (char esep)) | where {|p| $p != "" } | uniq | str join (char esep))} } else if $_ocx_e.type == "list" { let _ocx_s = ($_ocx_e.separator? | default " "); let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { "" }); let _ocx_p = ($_ocx_s + $_ocx_e.value + $_ocx_s); mut _ocx_l = (if $_ocx_cur == "" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_e.key): (($_ocx_l | str replace $_ocx_s "") + $_ocx_e.value)} } else if $_ocx_e.type == "constant" { load-env {($_ocx_e.key): $_ocx_e.value} } }
    } catch { }
    # Per-directory-change hook. APPENDED with `++` onto the existing list and
    # assigned back - never a bare assignment to `.PWD` and never a wholesale
    # `$env.config.hooks = { ... }`: starship's nushell integration owns this
    # same slot, and overwriting it silently stops the user's prompt updating.
    # Every intermediate level is defaulted, because a `nu -n` session's
    # `$env.config` may carry no `hooks` key at all and nushell parses a whole
    # file before running any of it - one erroring expression would void this
    # entire autoload, taking the `$env.PATH` prepend above down with it. The
    # whole registration sits in its own try/catch for the same reason: a hostile
    # config shape must cost the hook, never the PATH.
    #
    # `.PWD` may legitimately hold a BARE CLOSURE rather than a list - nu accepts
    # either, and a user who wrote `$env.config.hooks.env_change.PWD = {|before,
    # after| ... }` in config.nu has one. `++` against a closure is a type error,
    # which the try/catch would swallow, silently dropping this hook with no
    # diagnostic. Defaulting an ABSENT path is not the same as normalising a
    # PRESENT scalar, so a non-list value is wrapped into a one-element list
    # before the append. `str starts-with` on `describe` is used rather than `=~`
    # or `like`: both spellings have moved across nu versions, and an unknown
    # operator is a PARSE error that voids the whole file.
    #
    # The closure re-runs the SAME apply the startup path above runs, written out
    # a second time rather than factored into a `def`: a `def` body is not a
    # closure and cannot see `$_ocx_bin`, so factoring would buy one shared copy
    # at the price of a new parse-time construct on the one shell where a parse
    # error costs the entire autoload. A test pins the two copies together
    # instead. It fires on `cd`, not on every prompt, so the external call it
    # costs is per-directory-change - nushell has no builtin newer-than test and
    # is budgeted separately for exactly this.
    try {
        $env.config.hooks = ($env.config.hooks? | default {})
        $env.config.hooks.env_change = ($env.config.hooks.env_change? | default {})
        let _ocx_pwd = ($env.config.hooks.env_change.PWD? | default [])
        $env.config.hooks.env_change.PWD = ((if ($_ocx_pwd | describe | str starts-with 'list') { $_ocx_pwd } else { [$_ocx_pwd] }) ++ [{|_ocx_before, _ocx_after| try { let _ocx_json = (^($_ocx_bin | path join 'ocx') --format json --global env | from json); for _ocx_e in ($_ocx_json.entries? | default []) { if $_ocx_e.type == "path" { let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { "" }); load-env {($_ocx_e.key): (($_ocx_e.value | split row (char esep)) ++ ($_ocx_cur | split row (char esep)) | where {|p| $p != "" } | uniq | str join (char esep))} } else if $_ocx_e.type == "list" { let _ocx_s = ($_ocx_e.separator? | default " "); let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { "" }); let _ocx_p = ($_ocx_s + $_ocx_e.value + $_ocx_s); mut _ocx_l = (if $_ocx_cur == "" { $_ocx_s } else { $_ocx_s + $_ocx_cur + $_ocx_s }); while ($_ocx_l | str contains $_ocx_p) { $_ocx_l = ($_ocx_l | str replace --all $_ocx_p $_ocx_s) }; load-env {($_ocx_e.key): (($_ocx_l | str replace $_ocx_s "") + $_ocx_e.value)} } else if $_ocx_e.type == "constant" { load-env {($_ocx_e.key): $_ocx_e.value} } } } catch { } }])
    } catch { }
}
"#;

/// `$OCX_HOME/env.elv` — Elvish shim.
///
/// Byte-identical to the former `install.sh` `env.elv` heredoc body.
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
    # `ocx shell completion` - the same generator the POSIX shim uses.
    try {
        eval ($_ocx_bin shell completion --shell=elvish 2>/dev/null | slurp)
    } catch _ { }
}
"#;

/// Body of the fish `conf.d/ocx.fish` autoload file.
///
/// Ported verbatim from the former `install.sh` `create_fish_config` heredoc.
/// Sources `$OCX_HOME/env.fish` after resolving `OCX_HOME` at runtime.
const FISH_CONF: &str = r#"# OCX shell environment - managed by ocx installer.
# Sources $OCX_HOME/env.fish which evaluates the global toolchain env.
set -l _ocx_env (string join '' (set -q OCX_HOME; and echo $OCX_HOME; or echo $HOME/.ocx) '/env.fish')
if test -f "$_ocx_env"
    source "$_ocx_env"
end
"#;

/// The five `(filename, body)` shims written into `$OCX_HOME`.
const SHIMS: [(&str, &str); 5] = [
    ("env.sh", ENV_SH),
    ("env.fish", ENV_FISH),
    ("env.ps1", ENV_PS1),
    ("env.nu", ENV_NU),
    ("env.elv", ENV_ELV),
];

/// The Nushell vendor-autoload body that activates ocx at startup.
///
/// The orchestrator writes this to
/// `${XDG_DATA_HOME:-$HOME/.local/share}/nushell/vendor/autoload/ocx.nu`
/// (contract 5, `DedicatedFile`). The body takes no substitution.
///
/// It is the **full activation** ([`ENV_NU`]) rather than a one-line loader that
/// sources `$OCX_HOME/env.nu`: fish's `conf.d/ocx.fish` can `source "$_ocx_env"`
/// because POSIX/fish `source` accepts a runtime path, but Nushell `source`
/// requires a parse-time-constant path — it cannot source `$OCX_HOME/env.nu`
/// where `OCX_HOME` is only known at runtime. Inlining the activation sidesteps
/// that limitation. The update hook (`refresh_profiles`) re-applies this body, so
/// it stays in sync with the binary just like the `env.*` shims.
#[must_use]
pub fn nu_autoload_body() -> &'static str {
    ENV_NU
}

/// The fish `conf.d/ocx.fish` body that sources `$OCX_HOME/env.fish` at startup.
///
/// The orchestrator writes this to
/// `${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/ocx.fish`
/// (contract 5, `DedicatedFile`). The body takes no substitution.
#[must_use]
pub fn fish_conf_body() -> &'static str {
    FISH_CONF
}

/// Write all five `env.*` shims into `ocx_home`, atomically and diff-gated.
///
/// Creates `ocx_home` (`mkdir -p`) if absent, then writes each shim through the
/// private-file atomic-write helper
/// [`write_bytes_atomic`](crate::utility::fs::write_bytes_atomic) (temp-in-parent
/// then Windows-retry-aware publish). A file whose
/// on-disk bytes already equal the canonical body is skipped (diff-gate), so a
/// re-run is a no-op. Returns the paths actually (re)written, in shim order.
///
/// When `dry_run` is set, nothing is written: the returned vec is the
/// would-write set (every shim whose bytes are absent or differ).
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

/// Refresh the ocx-owned `env.*` shims after a `self update` binary swap
/// (Decision 4C).
///
/// Identical behavior to [`write_shims`] with `dry_run = false`; it exists as a
/// named entry point so the call from the update hook reads intentfully and can
/// never accidentally touch user RC. Always diff-gated — only shims whose bytes
/// drifted are rewritten.
///
/// # Errors
///
/// Returns [`Error::Io`] on a directory or shim read/write failure.
pub fn refresh_shims(ocx_home: &Path) -> Result<Vec<PathBuf>, Error> {
    write_shims(ocx_home, false)
}

/// Whether `target` must be (re)written to hold `body` — true if it is absent
/// or its bytes differ (diff-gate). A read error other than "not found"
/// propagates so a permission problem is not silently treated as "rewrite".
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
            // POSIX-family + elvish shims write `self activate` contiguously; the
            // PowerShell shim builds the args as a clap-safe array. The nushell
            // shim cannot `eval`/`source` `self activate` output (no string eval;
            // `source` needs a parse-time const path), so it invokes the binary
            // for the global env as STRUCTURED DATA (`--format json --global env`)
            // and applies it with `load-env` — see the ENV_NU doc comment.
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
    /// from `ocx --format json --global env`), never via the parse-time-broken
    /// constructs that left ocx off `PATH` entirely — a runtime `source (expr)`
    /// (rejected `not_a_constant`), the non-existent `$nu.temp-path` field, or a
    /// subprocess `nu -c` (which mutates only a child's env). See the ENV_NU doc
    /// comment for the full rationale.
    #[test]
    fn env_nu_applies_activation_as_data_not_via_source() {
        // Must use the data-application primitives.
        assert!(ENV_NU.contains("from json"), "env.nu must read the global env as JSON");
        assert!(ENV_NU.contains("load-env"), "env.nu must apply constants via load-env");
        assert!(
            ENV_NU.contains("--format json --global env"),
            "env.nu must resolve the global toolchain env as structured data"
        );
        assert!(
            ENV_NU.contains("[$_ocx_bin] ++"),
            "env.nu must prepend the ocx bin dir to PATH directly"
        );
        // The global-env apply must dispatch on the entry MODIFIER TYPE
        // (`type == "path"`), not on `key == "PATH"`: a non-PATH path var such as
        // LD_LIBRARY_PATH must prepend (move-to-front), not be overwritten.
        assert!(
            ENV_NU.contains(NU_ENV_APPLY_LOOP),
            "env.nu must embed the shared apply loop verbatim (drift guard)"
        );
        assert!(
            NU_ENV_APPLY_LOOP.contains(r#"$_ocx_e.type == "path""#),
            "the apply loop must dispatch on the entry modifier type"
        );
        assert!(
            !NU_ENV_APPLY_LOOP.contains(r#"$_ocx_e.key == "PATH""#),
            "the apply loop must NOT branch on the key name (LD_LIBRARY_PATH etc. are type:path too)"
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

    /// A-23(2): the apply loop dispatches four ways, not two.
    ///
    /// `path`, `list` and `constant` each get their own arm, and an entry whose
    /// `type` is none of the three applies **nothing**. A two-way branch sent a
    /// `list` entry down the `else` arm as a constant, so a global
    /// `CFLAGS = { type = "list", … }` CLOBBERED the caller's `CFLAGS` instead of
    /// folding into it — every emitted-stream arm appends. `list` has shipped on
    /// the wire since `EnvEntry` gained `type` + `separator`, so the `else` arm
    /// was reachable in production, and the same fall-through would silently
    /// mis-apply any modifier kind added later.
    ///
    /// Structural: no `nu` is installed on any cargo leg. The behavioural half is
    /// the shell-zoo row `EC-NU-006`.
    #[test]
    fn nu_apply_loop_dispatches_on_all_three_modifier_kinds() {
        for kind in ["path", "list", "constant"] {
            assert!(
                NU_ENV_APPLY_LOOP.contains(&format!(r#"$_ocx_e.type == "{kind}""#)),
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
        // `type` must not fall through to a `load-env` that overwrites.
        let tail = NU_ENV_APPLY_LOOP
            .rsplit_once(r#"$_ocx_e.type == "constant""#)
            .expect("the constant arm anchors the tail")
            .1;
        assert!(
            !tail.contains("else"),
            "an unrecognised modifier type must apply nothing, not fall through: {tail}"
        );
    }

    /// Regression: the Nushell apply loop must read an existing env value with a
    /// FLAG-FREE construct, never `get --optional`.
    ///
    /// `get --optional` was added to nu *after* 0.101.0. Nushell parses an entire
    /// file before running it, so on an older-but-supported nu (e.g. 0.101.0) the
    /// unknown flag is a PARSE error that voids the whole vendor-autoload — taking
    /// down the `$env.PATH` prepend that precedes it, so `ocx` never lands on
    /// PATH. The replacement reads the value via `($_ocx_e.key in ($env |
    /// columns))` + a dynamic `get`, all pre-0.101 stable features, and emits no
    /// deprecation warning on a newer nu. Pinned on both the shared const and its
    /// inlined copy in [`ENV_NU`] (drift guard).
    #[test]
    fn nu_apply_loop_reads_env_without_the_get_optional_flag() {
        for (name, body) in [("NU_ENV_APPLY_LOOP", NU_ENV_APPLY_LOOP), ("ENV_NU", ENV_NU)] {
            assert!(
                !body.contains("get --optional"),
                "{name} must not use `get --optional` (absent on nu < the version that added it; \
                 its unknown flag voids the whole autoload at parse time, dropping the PATH prepend)"
            );
        }
        assert!(
            NU_ENV_APPLY_LOOP.contains("$_ocx_e.key in ($env | columns)"),
            "the apply loop must read an existing value via the flag-free `key in ($env | columns)` form"
        );
    }

    // ── C-043 (shim half) — how the hook reaches each family ─────────────

    /// The four families whose shim hands the output of `ocx self activate`
    /// back to the **sourcing** shell — `eval`, `| source`, `Invoke-Expression`,
    /// `eval (… | slurp)`. That stream is the shim-side hook registration
    /// (C-043): whatever `shell/hook.rs` emits rides it, so registering a hook
    /// needs no shim change and no reconciliation logic in the shim — the body
    /// stays a pure dispatcher (C-047).
    ///
    /// **The name says eval-capable, not hooked, and the difference is real.**
    /// `hook::registration` returns a body for bash, zsh, fish, PowerShell and
    /// elvish; for ash, ksh and dash it returns `None`, because none of those
    /// has an append-safe per-prompt seam. So `env.sh` delivers a hook only when
    /// the sourcing shell turns out to be bash or zsh — never under plain `sh` —
    /// while every other family here is hooked. This list exists to scope the
    /// denylist and the ceilings, and it would still be these four if no family
    /// were hooked at all.
    ///
    /// Nushell is deliberately absent: it has no string `eval` and `source`
    /// needs a parse-time-constant path, so its hook is inlined in [`ENV_NU`]
    /// instead (A-24) — and it is the family Decision 6(b) exempts from the
    /// denylist below.
    const EVAL_CAPABLE: [(&str, &str); 4] = [
        ("env.sh", ENV_SH),
        ("env.fish", ENV_FISH),
        ("env.ps1", ENV_PS1),
        ("env.elv", ENV_ELV),
    ];

    /// Each eval-capable shim must evaluate the activation stream **in the
    /// sourcing shell's own scope**, and must not opt out of the hook.
    ///
    /// A shim that captured the stream into a variable, piped it to a file, or
    /// ran it in a subshell would still "invoke the binary" — the property
    /// `each_shim_resolves_the_binary_through_the_current_symlink` proves — while
    /// silently dropping every hook and wrapper definition the stream carries.
    ///
    /// **Scanned comment-stripped, and that is load-bearing here.** Against the
    /// raw const, commenting a shim's activation lines out leaves the needle
    /// matching inside the dead comment: the shim is inert and the guard stays
    /// green. `code_only` is what makes a disabled channel visible.
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
            fish.contains("self activate --shell=fish --completion --interactive 2>/dev/null | source")
                && fish.contains("self activate --shell=fish --no-completion --no-interactive 2>/dev/null | source"),
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

    /// C-038 rung 5 (shim half) — every eval-capable shim decides its own
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
                && fish.contains("--shell=fish --completion --interactive")
                && fish.contains("--shell=fish --no-completion --no-interactive"),
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

    /// A-24 / C-043 — the nushell PWD hook is **appended** onto a fully
    /// `default`-ed path, never assigned over.
    ///
    /// starship's nushell integration owns the same slot, so an assignment
    /// silently stops the user's prompt updating; and nushell parses a whole
    /// file before running it, so an `append` against an absent `hooks` key in a
    /// `nu -n` session would void this entire autoload — taking the `$env.PATH`
    /// prepend above it down too.
    #[test]
    fn env_nu_appends_its_pwd_hook_onto_a_fully_defaulted_path() {
        // Comment-stripped for the same reason C-047's denylist is: the body's
        // own comment names both forms A-24 forbids, which is what a comment
        // documenting a hazard is *for*.
        let code = code_only(ENV_NU);
        // The append-and-assign-back form, in one needle: existing value first,
        // `++`, then a one-element list whose element opens a CLOSURE (`{|`).
        assert!(
            code.contains("let _ocx_pwd = ($env.config.hooks.env_change.PWD? | default [])")
                && code.contains(
                    "$env.config.hooks.env_change.PWD = ((if ($_ocx_pwd | describe | str starts-with 'list') \
                     { $_ocx_pwd } else { [$_ocx_pwd] }) ++ [{|"
                ),
            "env.nu must append its PWD hook closure onto the existing value, never replace it"
        );
        // A present-but-scalar `.PWD` (a bare closure, which nu accepts) must be
        // normalised to a list before the append: `++` against a closure is a
        // type error the surrounding try/catch would swallow, dropping the hook
        // with no diagnostic. Defaulting an absent path does not cover this.
        assert!(
            code.contains("else { [$_ocx_pwd] }"),
            "env.nu must wrap a non-list PWD hook value into a list before appending"
        );
        // Every intermediate level defaulted, so an absent `hooks` key cannot error.
        assert!(
            code.contains("$env.config.hooks = ($env.config.hooks? | default {})"),
            "env.nu must default $env.config.hooks before reaching into it"
        );
        assert!(
            code.contains("$env.config.hooks.env_change = ($env.config.hooks.env_change? | default {})"),
            "env.nu must default $env.config.hooks.env_change before reaching into it"
        );
        // The two forms A-24 forbids outright.
        assert!(
            !code.contains("$env.config.hooks = {"),
            "env.nu must never assign a wholesale hooks record over the user's"
        );
        assert!(
            !code.contains("$env.config.hooks.env_change.PWD = [")
                && !code.contains("$env.config.hooks.env_change.PWD = ({"),
            "env.nu must never assign `.PWD` without appending the existing value first"
        );
        // The hook applies through the SAME shared loop the startup path uses:
        // once inline at startup, once inside the closure (drift guard).
        assert_eq!(
            ENV_NU.matches(NU_ENV_APPLY_LOOP).count(),
            2,
            "the startup apply and the PWD-hook closure must embed the shared loop verbatim"
        );
        // The dedicated nushell autoload is ENV_NU, so it carries the hook too.
        assert!(
            code_only(nu_autoload_body()).contains("$env.config.hooks.env_change.PWD = ((if "),
            "the nushell vendor-autoload body must carry the PWD hook (A-24 ordering slot)"
        );
    }

    // ── C-047 — the thin-dispatcher guard ────────────────────────────────

    /// Business-logic tokens that must not appear in an eval-capable dispatcher
    /// body (C-047). The shims resolve `$OCX_HOME`, find `ocx` through the
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

    /// C-047 — no eval-capable shim body carries ocx business logic.
    ///
    /// Nushell is **exempt by Decision 6(b)**: it has no string `eval`, so it
    /// inlines the apply by necessity. Scanning it here would force the guard
    /// green-by-exception on the one family that genuinely violates the spirit.
    ///
    /// EC-EMIT-009, half one of two — the denylist half of the thin-dispatcher
    /// invariant. The row's point is that the test previously cited for the
    /// invariant, `each_shim_resolves_the_binary_through_the_current_symlink`,
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
    // | `env.nu`    |       2858 | ×1.25  |    2900 |       42 |
    // | `env.elv`   |        479 |  ×1.5  |     550 |       71 |
    // | fish loader |        153 |  ×1.5  |     250 |       97 |
    //
    // The four eval-capable bodies grew when C-038 made each state its own
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
    // without re-measuring reds. `env.nu` takes a tighter ×1.25 for exactly this
    // reason — at ×1.5 its headroom would be 938 bytes, wide enough to admit the
    // very reconciler the ceiling exists to refuse, and nushell is the one family
    // Decision 6(b) exempts from the denylist, so here the ceiling is the *only*
    // mechanical guard there is.
    //
    // Nushell is also the family that legitimately grows: C-048 replaces its
    // apply with a `Plan` applier. That tension is resolved by review, not by a
    // number — a nushell body that outgrows the constant raises it deliberately,
    // in the commit that grows it, with the new measurement written into this
    // table. Raised 2300 -> 2900 when A-23(2) gave the apply loop its `list`
    // arm: the unique-append fold needs the AMBIENT value, which only a
    // statement running inside the nu session can read, so it cannot move into
    // the binary the way a resolved value can. Measured at 2858.
    const CEILING_ENV_SH: usize = 950;
    const CEILING_ENV_FISH: usize = 600;
    const CEILING_ENV_PS1: usize = 1250;
    const CEILING_ENV_NU: usize = 2900;
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

    /// C-047 — every emitted body stays under its per-family ceiling.
    ///
    /// EC-EMIT-009, half two of two — the size half of the thin-dispatcher
    /// invariant, and the ONLY mechanical guard on `env.nu`, which Decision
    /// 6(b) exempts from the denylist in
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

    /// A-34 — every family resolves the binary through the `current` symlink
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
