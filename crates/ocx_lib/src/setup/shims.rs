// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The per-shell env shims OCX writes into `$OCX_HOME` (contract 3).
//!
//! Each shim is a thin loader that delegates to `ocx self activate` at shell
//! startup. The bodies are **byte-identical across users** — there is NO
//! install-time substitution; `OCX_HOME` is resolved at runtime by the shim
//! itself (`: "${OCX_HOME:=$HOME/.ocx}"` and the per-shell equivalents). The
//! consts below are the single source of truth, lifted verbatim from the
//! former `install.sh` / `install.ps1` heredocs.
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

# PATH + global toolchain env: apply once per shell. The `_OCX_ENV_LOADED`
# guard keeps the PATH prepend and the (subprocess-spawning) global-env eval
# from repeating when the user re-sources their profile. `--no-completion`
# keeps completions out of this one-shot block; they are handled separately
# below so they survive a completion system that initializes later.
if [ -z "${_OCX_ENV_LOADED:-}" ]; then
    _OCX_ENV_LOADED=1
    export _OCX_ENV_LOADED
    if [ -x "$_ocx_bin" ]; then
        eval "$("$_ocx_bin" self activate --shell="$_ocx_shell" --no-completion 2>/dev/null)" || true
    fi
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
# Double-source guard - prevents PATH duplication on re-source.
# Set before any side effects so re-source after partial failure also short-circuits.
if set -q _OCX_ENV_LOADED
    return
end
set -gx _OCX_ENV_LOADED 1

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
# Double-source guard - prevents PATH duplication on re-source.
# Set before any side effects so re-source after partial failure also short-circuits.
if ($env:_OCX_ENV_LOADED) { return }
$env:_OCX_ENV_LOADED = '1'

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

/// `$OCX_HOME/env.nu` — Nushell shim.
///
/// Byte-identical to the former `install.sh` `env.nu` heredoc body.
pub const ENV_NU: &str = r#"# Managed by ocx installer - do not edit.
# Double-source guard - prevents PATH duplication on re-source.
# Set before any side effects so re-source after partial failure also short-circuits.
if ($env._OCX_ENV_LOADED? | default '') != '' { return }
$env._OCX_ENV_LOADED = '1'

$env.OCX_HOME = ($env.OCX_HOME? | default ($env.HOME | path join '.ocx'))

let _ocx_bin = ($env.OCX_HOME | path join 'symlinks/ocx.sh/ocx/cli/current/content/bin/ocx')
if ($_ocx_bin | path exists) {
    ^$_ocx_bin self activate --shell=nushell 2>/dev/null | save --force ($nu.temp-path | path join 'ocx_activate.nu')
    source ($nu.temp-path | path join 'ocx_activate.nu')
}
"#;

/// `$OCX_HOME/env.elv` — Elvish shim.
///
/// Byte-identical to the former `install.sh` `env.elv` heredoc body.
pub const ENV_ELV: &str = r#"# Managed by ocx installer - do not edit.
# Double-source guard - prevents PATH duplication on re-source.
# Set before any side effects so re-source after partial failure also short-circuits.
if (has-env _OCX_ENV_LOADED) {
    return
}
set-env _OCX_ENV_LOADED 1

if (not (has-env OCX_HOME)) {
    set-env OCX_HOME (path:join $E:HOME .ocx)
}

var _ocx_bin = (path:join $E:OCX_HOME symlinks/ocx.sh/ocx/cli/current/content/bin/ocx)
# rc.elv is sourced only for interactive Elvish sessions, so --completion is
# unconditional here. The hook redirects stderr (2>/dev/null), so the flag -
# not an isatty(2) probe - is what gates completion work.
if ?(test -x $_ocx_bin) {
    eval (e:$_ocx_bin self activate --shell=elvish --completion 2>/dev/null | slurp)
}
"#;

/// Body of the Nushell vendor-autoload file (`vendor/autoload/ocx.nu`).
///
/// Ported verbatim from the former `install.sh` `create_nu_autoload` heredoc.
/// Sources `$OCX_HOME/env.nu` after resolving `OCX_HOME` at runtime.
const NU_AUTOLOAD: &str = r#"# OCX shell environment - managed by ocx installer.
$env.OCX_HOME = ($env.OCX_HOME? | default ($env.HOME | path join '.ocx'))
source ($env.OCX_HOME + '/env.nu')
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

/// The Nushell vendor-autoload body that sources `$OCX_HOME/env.nu` at startup.
///
/// The orchestrator writes this to
/// `${XDG_DATA_HOME:-$HOME/.local/share}/nushell/vendor/autoload/ocx.nu`
/// (contract 5, `DedicatedFile`). The body takes no substitution.
#[must_use]
pub fn nu_autoload_body() -> &'static str {
    NU_AUTOLOAD
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
            ENV_ELV.contains("(not (has-env OCX_HOME))")
                && ENV_ELV.contains("set-env OCX_HOME (path:join $E:HOME .ocx)"),
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

    /// Each shim delegates to the running binary via `self activate`, resolving
    /// it through the `current` install symlink — never a literal path.
    #[test]
    fn each_shim_resolves_the_binary_through_the_current_symlink() {
        // The shared prefix is literal in every shim; the binary basename is
        // `ocx` for the four POSIX-family shims and `$_ocxExe` (ocx / ocx.exe)
        // for the PowerShell shim, so assert only the `current` symlink prefix.
        for (name, body) in SHIMS {
            assert!(
                body.contains("symlinks/ocx.sh/ocx/cli/current/content/bin/"),
                "{name} must resolve ocx through the current install symlink"
            );
            // POSIX-family shims write `self activate` contiguously; the
            // PowerShell shim builds the args as a clap-safe array.
            let delegates =
                body.contains("self activate") || body.contains("@('self', 'activate', '--shell=powershell')");
            assert!(delegates, "{name} must delegate to `self activate`");
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
        assert!(nu_autoload_body().contains("# OCX shell environment - managed by ocx installer."));
        assert!(nu_autoload_body().contains(r#"$env.OCX_HOME = ($env.OCX_HOME? | default"#));
        assert!(nu_autoload_body().contains("source ($env.OCX_HOME + '/env.nu')"));

        assert!(fish_conf_body().contains("# OCX shell environment - managed by ocx installer."));
        assert!(fish_conf_body().contains("set -q OCX_HOME"));
        assert!(fish_conf_body().contains(r#"source "$_ocx_env""#));
    }
}
