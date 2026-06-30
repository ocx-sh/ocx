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
use crate::utility::fs::persist_temp_file;

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
if [ -x "$_ocx_bin" ]; then
    eval "$("$_ocx_bin" self activate --shell="$_ocx_shell" --no-completion 2>/dev/null)" || true
fi

# Shell completions: re-inject on EVERY interactive source, NOT once. zsh's
# `compinit` rebuilds its completion table from scratch; oh-my-zsh (and many
# frameworks) run it from `.zshrc`, AFTER a login shell has already sourced
# this file from `.zprofile`. A one-shot guard would register completions
# pre-`compinit` only to have them wiped with no second chance; re-running the
# generator on the post-`compinit` source re-registers them. `$-` carries `i`
# for an interactive shell; `sh`/Dash has no completion backend so it is
# skipped. Output is completion-only (no PATH/env), so repeating it is cheap
# and side-effect-free.
case "$-" in
    *i*)
        if [ -x "$_ocx_bin" ] && [ "$_ocx_shell" != sh ]; then
            eval "$("$_ocx_bin" shell completion --shell="$_ocx_shell" 2>/dev/null)" || true
        fi
        ;;
esac
unset _ocx_bin _ocx_shell
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
# Decide completions here via `status is-interactive` and pass the explicit
# --completion/--no-completion flag. stderr is still redirected (2>/dev/null) to
# suppress startup diagnostics, but the gate no longer depends on isatty(2).
if test -x "$_ocx_bin"
    if status is-interactive
        "$_ocx_bin" self activate --shell=fish --completion 2>/dev/null | source
    else
        "$_ocx_bin" self activate --shell=fish --no-completion 2>/dev/null | source
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
    # Request completions only on an interactive PowerShell 5.0+ session: legacy
    # Windows PowerShell <5.0 cannot run clap's `using namespace` /
    # `Register-ArgumentCompleter -Native` completion output, so it opts out with
    # --no-completion while still emitting PATH + global env.
    $_ocxArgs = @('self', 'activate', '--shell=powershell')
    if ([Environment]::UserInteractive -and $PSVersionTable.PSVersion.Major -ge 5) {
        $_ocxArgs += '--completion'
    } else {
        $_ocxArgs += '--no-completion'
    }
    $_ocxActivate = (& $_ocxBin @_ocxArgs 2>$null) | Out-String
    # Guard $null/empty: Out-String of empty/failed output yields $null, and
    # `Invoke-Expression $null` throws "Cannot bind argument ... is null".
    if ($_ocxActivate) { Invoke-Expression $_ocxActivate }
}
Remove-Variable _ocxBase, _ocxExe, _ocxBin, _ocxArgs, _ocxActivate -ErrorAction SilentlyContinue
"#;

/// The Nushell loop that applies one `ocx --format json --global env` document to
/// `$env`, dispatching on each entry's modifier **type** exactly like the shell
/// emitters (`Shell::Nushell::export_path` / `export_constant`):
///
/// - `type == "path"` — prepend the value's segment(s) to the named key,
///   move-to-front (drop the existing occurrence + empties via `uniq`), and store
///   the result as a separator-joined **string**. Strings (not lists) are
///   required because Nushell silently drops a LIST-valued non-`PATH` env var when
///   it spawns an external; `PATH` itself round-trips fine as a string.
/// - `type == "constant"` — replace any existing value with `load-env`.
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
pub const NU_ENV_APPLY_LOOP: &str = "for _ocx_e in ($_ocx_json.entries? | default []) { if $_ocx_e.type == \"path\" { let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { \"\" }); load-env {($_ocx_e.key): (($_ocx_e.value | split row (char esep)) ++ ($_ocx_cur | split row (char esep)) | where {|p| $p != \"\" } | uniq | str join (char esep))} } else { load-env {($_ocx_e.key): $_ocx_e.value} } }";

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
        for _ocx_e in ($_ocx_json.entries? | default []) { if $_ocx_e.type == "path" { let _ocx_cur = (if ($_ocx_e.key in ($env | columns)) { $env | get $_ocx_e.key } else { "" }); load-env {($_ocx_e.key): (($_ocx_e.value | split row (char esep)) ++ ($_ocx_cur | split row (char esep)) | where {|p| $p != "" } | uniq | str join (char esep))} } else { load-env {($_ocx_e.key): $_ocx_e.value} } }
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
# PATH + global toolchain env run in their OWN eval unit, kept apart from the
# completion below. Elvish compiles an eval unit as a whole, so coupling the two
# lets a completion that fails to compile void the PATH prepend too: the
# clap_complete elvish completer sets edit:completion:arg-completer, and the
# edit: namespace is bound only when an interactive line editor is active (a real
# TTY). A non-TTY interactive shell has no edit:, so the completion unit raises a
# compile error - and if PATH shared that unit it would be lost with it. Keeping
# them separate makes PATH activation independent of completion succeeding.
if ?(test -x $_ocx_bin) {
    eval ($_ocx_bin self activate --shell=elvish --no-completion 2>/dev/null | slurp)
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
/// Windows-retry-aware atomic-publish primitive
/// [`persist_temp_file`](crate::utility::fs::persist_temp_file). A file whose
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
            write_atomic(ocx_home, &target, body)?;
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

/// Write `body` to `target` atomically: a `NamedTempFile` in `dir` is filled
/// then published through [`persist_temp_file`].
fn write_atomic(dir: &Path, target: &Path, body: &str) -> Result<(), Error> {
    use std::io::Write as _;

    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(|source| Error::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    tmp.write_all(body.as_bytes()).map_err(|source| Error::Io {
        path: target.to_path_buf(),
        source,
    })?;
    persist_temp_file(tmp, target).map_err(|source| Error::Io {
        path: target.to_path_buf(),
        source,
    })
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
