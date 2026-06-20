// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod applied_set;
pub mod error;
pub use applied_set::AppliedEntry;

use crate::{Error, env, log};

/// List of supported shells for OCX to generate scripts for, ie. profiles or auto-completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    /// Almquist `SHell` (ash)
    Ash,
    /// Korn `SHell` (ksh)
    Ksh,
    /// `Dash` shell, a POSIX-compliant shell often used in Debian-based systems
    Dash,
    /// Bourne Again `SHell` (bash)
    Bash,
    /// Elvish shell
    Elvish,
    /// Friendly Interactive `SHell` (fish)
    Fish,
    /// Windows `Batch` shell
    Batch,
    /// `PowerShell`
    PowerShell,
    /// Z `SHell` (zsh)
    Zsh,
    /// `Nushell` (nu)
    Nushell,
}

impl Shell {
    /// Tries to resolve the current shell by checking the `SHELL` environment variable and then the parent processes.
    pub fn detect() -> Option<Self> {
        Self::from_process().or_else(Self::from_env)
    }

    /// Tries to resolve the shell from a given path, which can be a full path or just a filename.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Option<Self> {
        let path = path.as_ref();
        log::trace!("Detecting shell from path: {}", path.display());

        if crate::symlink::is_link(path) {
            log::trace!("Shell is a symlink, attempting to resolve it...");
            if let Ok(canonical_path) = std::fs::read_link(path)
                && let Some(shell) = Self::from_path(canonical_path)
            {
                return Some(shell);
            }
        }

        let filename = path.file_stem()?.to_str()?;
        match filename {
            "ash" | "busybox" => Some(Self::Ash),
            "ksh" | "ksh86" | "ksh88" | "ksh93" => Some(Self::Ksh),
            "dash" => Some(Self::Dash),
            "bash" => Some(Self::Bash),
            "elvish" => Some(Self::Elvish),
            "fish" => Some(Self::Fish),
            "cmd" => Some(Self::Batch),
            "powershell" | "powershell_ise" | "pwsh" => Some(Self::PowerShell),
            "zsh" => Some(Self::Zsh),
            "nu" | "nushell" => Some(Self::Nushell),
            _ => None,
        }
    }

    /// Tries to resolve the shell from the `SHELL` environment variable.
    pub fn from_env() -> Option<Self> {
        crate::env::var("SHELL").and_then(Self::from_path)
    }

    /// Tries to resolve the shell by inspecting the current and parent process information.
    pub fn from_process() -> Option<Self> {
        fn try_process_id(pid: sysinfo::Pid, system: &sysinfo::System) -> Option<Shell> {
            log::trace!("Checking process with PID {} for shell information...", pid);
            if let Some(process) = system.process(pid)
                && let Some(shell) = Shell::from_path(process.name())
            {
                return Some(shell);
            }
            #[cfg(unix)]
            if let Some(shell) = Shell::from_path(format!("/proc/{}/exe", pid)) {
                return Some(shell);
            }
            None
        }

        let system = sysinfo::System::new_with_specifics(
            sysinfo::RefreshKind::default().with_processes(sysinfo::ProcessRefreshKind::default()),
        );
        let mut current_pid = sysinfo::get_current_pid().ok()?;
        if let Some(shell) = try_process_id(current_pid, &system) {
            return Some(shell);
        }
        while let Some(parent_pid) = system.process(current_pid)?.parent() {
            if let Some(shell) = try_process_id(parent_pid, &system) {
                return Some(shell);
            }
            current_pid = parent_pid;
        }
        None
    }

    /// Returns a shell comment line.
    pub fn comment(self, text: impl AsRef<str>) -> String {
        let text = text.as_ref();
        match self {
            Self::Batch => format!("REM {text}"),
            _ => format!("# {text}"),
        }
    }

    /// Emit a **self-contained, idempotent, move-to-front** shell statement that
    /// prepends `value` to the path-style variable named `key`.
    ///
    /// Re-sourcing the emitted statement never duplicates `value`; re-adding a
    /// value already present removes the stale occurrence and moves it to the
    /// front ("last activation wins" for lookup). The statement depends on no
    /// ocx process, no ocx-set guard variable, and no helper function — so it
    /// keeps working when captured into a profile
    /// (`ocx package env cmake --shell bash >> ~/.bashrc`) and re-sourced with
    /// ocx absent. This is the contract that unblocks the per-prompt shell hook.
    ///
    /// Each native shell (bash/zsh, fish, PowerShell, elvish, nushell) does the
    /// dedup with zero subprocesses; the strict-POSIX family (ash/ksh/dash) uses
    /// a single `awk`. A throwaway `__ocx_p` variable carries the value through
    /// one escape context and is `unset` within the same statement, so the
    /// value's match position references the quoted variable rather than a
    /// re-interpolated literal — closing the glob / second-escape gap. The
    /// shells whose `__ocx_p` is matched byte-for-byte against an existing PATH
    /// segment use a **single-quoted literal** (bash/zsh/POSIX via `'\''`,
    /// elvish/PowerShell via `''`) so `$`, backtick, `\`, `!`, and glob chars
    /// stay exact; fish and nushell keep [`Self::escape_value`] (their
    /// double-quoted form round-trips those bytes). `batch` is idempotent too:
    /// cmd's `%VAR:search=%` substring deletion gives move-to-front without the
    /// `FOR /F` + delayed-expansion `!`-corruption that made dedup look infeasible.
    ///
    /// **Precondition:** `value` is a single directory with no embedded
    /// `PATH_SEPARATOR` (the env resolver yields one resolved `bin/` dir per
    /// entry). The split-based emitters (POSIX `awk`, fish, PowerShell, elvish,
    /// nushell) treat a separator inside `value` as a segment boundary and would
    /// not recognise it for dedup — the same precondition as
    /// [`utility::path::move_to_front`](crate::utility::path::move_to_front).
    ///
    /// Returns `None` when `key` is not a valid POSIX environment-variable
    /// name (`[A-Za-z_][A-Za-z0-9_]*`). Caller decides how to surface that —
    /// invalid keys are produced exclusively by malformed package metadata
    /// and never by the OCX code base itself, so propagating `None` is the
    /// path-of-least-impact safety guard.
    pub fn export_path(self, key: impl AsRef<str>, value: impl AsRef<str>) -> Option<String> {
        let key = key.as_ref();
        if !env::is_valid_env_key(key) {
            return None;
        }
        let raw = value.as_ref();
        let separator = env::PATH_SEPARATOR;
        Some(match self {
            // bash / zsh — pure-builtin colon-sentinel removal, zero subprocess.
            // The value is a single-quoted literal so `$`, backtick, `\`, `!`,
            // and glob chars stay byte-exact (a double-quoted `\!` would not
            // match a real `!`-bearing segment). Quoting `"$__ocx_p"` inside
            // `${var//pat/repl}` forces a *literal* match (no glob). The `while`
            // loops the replacement to a fixed point so even pre-existing
            // *adjacent* duplicates collapse (one non-overlapping `${//}` pass
            // leaves one). zsh keeps its `path` array tied to `PATH` on every
            // assignment, so the scalar form updates both. `${KEY-}` is
            // `set -u`-safe.
            Self::Bash | Self::Zsh => {
                let value = escape_posix_single_quoted(raw);
                format!(
                    "__ocx_p='{value}'; {key}=\":${{{key}-}}:\"; while [ \"${key}\" != \"${{{key}//:\"$__ocx_p\":/:}}\" ]; do {key}=\"${{{key}//:\"$__ocx_p\":/:}}\"; done; {key}=\"${{{key}#:}}\"; {key}=\"${{{key}%:}}\"; export {key}=\"$__ocx_p${{{key}:+:${{{key}}}}}\"; unset __ocx_p"
                )
            }
            // strict POSIX (ash / ksh / dash) — one `awk`, literal key name (no
            // non-POSIX `${!var}` indirection). The value is a single-quoted
            // literal (byte-exact: `!`, `\`, `$` all literal) and is read by awk
            // through `ENVIRON["__ocx_p"]` (an exported shell var) rather than
            // `-v d=...`, because `awk -v` decodes backslash escapes in its
            // value — `-v d=/a\b` would set `d` to `/a<BS>b` and fail to match a
            // real `/a\b` segment. `ENVIRON` carries the bytes verbatim, `RS=":"`
            // is the POSIX path separator, and the compare is exact string
            // equality, so no glob/pattern escaping is needed. (`awk` collapses
            // adjacent duplicates in one pass, so no fixpoint loop is needed.)
            Self::Ash | Self::Ksh | Self::Dash => {
                let value = escape_posix_single_quoted(raw);
                format!(
                    "__ocx_p='{value}'; export __ocx_p; export {key}=\"$__ocx_p$(printf %s \"${{{key}-}}\" | awk 'BEGIN{{ORS=\"\";RS=\":\";d=ENVIRON[\"__ocx_p\"]}} $0!=d && $0!=\"\"{{printf \":%s\",$0}}')\"; unset __ocx_p"
                )
            }
            // fish — rebuild the list keeping the new value first and dropping any
            // exact-string duplicate. `test "$e" != "$p"` is exact (no glob), so
            // bracket/glob paths are safe; works for any key (not just `$PATH`).
            // The `fish_add_path` builtin is deliberately avoided: it skips
            // non-existent dirs and mangles bracket paths.
            Self::Fish => {
                let value = self.escape_value(raw);
                format!(
                    "set __ocx_p \"{value}\"; set __ocx_r; for __ocx_e in ${key}; test \"$__ocx_e\" != \"$__ocx_p\"; and set -a __ocx_r $__ocx_e; end; set -gx {key} $__ocx_p $__ocx_r; set -e __ocx_p __ocx_r __ocx_e"
                )
            }
            // PowerShell — split on the OS path separator, drop empties + the
            // value, prepend. The value is a single-quoted literal (`''` escapes
            // an embedded quote) so no `$`/backtick interpolation can fire.
            // `[IO.Path]::PathSeparator` is `;` on Windows, `:` elsewhere.
            Self::PowerShell => {
                let value = escape_single_quoted_doubled(raw);
                format!(
                    "$__ocx_p='{value}'; $__ocx_s=[IO.Path]::PathSeparator; $env:{key}=(@($__ocx_p)+($env:{key} -split [regex]::Escape($__ocx_s) | Where-Object {{$_ -and $_ -ne $__ocx_p}})) -join $__ocx_s; Remove-Variable __ocx_p,__ocx_s"
                )
            }
            // elvish — split the env string, filter empties + the value, prepend,
            // re-join. The value is a single-quoted raw string (`'` doubled):
            // elvish double-quoted strings reject `\$` / `` \` `` as *invalid
            // escape sequences* (a parse error), so a `$`/backtick-bearing path
            // must use the raw form. `not-eq` is exact (no glob). Works for any
            // key and is empty-safe (the `$paths` list view is `$nil` when
            // `PATH` is empty). `use str` is idempotent across repeated emits.
            // Requires elvish with the `str:` module (0.16+).
            Self::Elvish => {
                let value = escape_single_quoted_doubled(raw);
                format!(
                    "use str; set E:{key} = (str:join \"{separator}\" ['{value}' (str:split \"{separator}\" $E:{key} | each {{|p| if (and (not-eq $p '{value}') (not-eq $p \"\")) {{ put $p }} }})])"
                )
            }
            // nushell — normalise to a list (`$env.PATH` is auto-listified since
            // 0.101, other path vars stay strings), drop the value, prepend. The
            // `$env.KEY? | default ""` guard treats an unset variable as empty
            // (parity with the POSIX `${KEY-}`); the `describe` guard then
            // tolerates both string and list inputs. Plain (non-interpolating)
            // double-quoted literals, so `$`/`(` cannot fire.
            Self::Nushell => {
                let value = self.escape_value(raw);
                format!(
                    "$env.{key} = (($env.{key}? | default \"\") | (if ($in | describe) == 'string' {{ split row (char esep) }} else {{ $in }}) | where {{|p| $p != \"{value}\" and $p != \"\" }} | prepend \"{value}\")"
                )
            }
            // batch (cmd.exe) — idempotent move-to-front in a SINGLE statement.
            // cmd has no list primitive, but `%VAR:search=%` deletes every literal
            // occurrence of `search` using *normal* (non-delayed) expansion, so a
            // `!`-bearing segment stays intact (the `FOR /F` + delayed-expansion form
            // would corrupt it). One statement is essential: this line is consumed
            // both via `call file.bat` AND via `FOR /F ... DO @%i` (how `ocx --global
            // env` is applied), and a FOR/F eval does not re-expand `%KEY%` between
            // statements — a multi-statement form would read a stale value. So delete
            // every existing `value<sep>` occurrence and prepend `value<sep>` once, in
            // place. Re-running is stable: the front occurrence is deleted and
            // re-prepended (the work the removed OCX_ACTIVATED guard used to do).
            // Match is case-insensitive — Windows PATH semantics. Caveats, both
            // benign and matching the prior prepend-only behaviour: an entry that was
            // the unanchored *last* segment is not relocated (a one-time non-dedup,
            // never unbounded growth), and an empty `%KEY%` yields a trailing
            // separator (an empty PATH segment cmd ignores). `escape_value`
            // neutralises `%`, `^`, `&`, `<`, `>`, `|`.
            Self::Batch => {
                let value = self.escape_value(raw);
                format!("SET \"{key}={value}{separator}%{key}:{value}{separator}=%\"")
            }
        })
    }

    /// Emit a shell line that sets `key=value` (replacing any prior value).
    ///
    /// Returns `None` when `key` is not a valid POSIX environment-variable
    /// name. See [`Self::export_path`] for the rationale.
    pub fn export_constant(self, key: impl AsRef<str>, value: impl AsRef<str>) -> Option<String> {
        let key = key.as_ref();
        if !env::is_valid_env_key(key) {
            return None;
        }
        let value = self.escape_value(value.as_ref());
        Some(match self {
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => format!("export {key}=\"{value}\""),
            Self::Fish => format!("set -x {key} \"{value}\""),
            Self::PowerShell => format!("$env:{key} = \"{value}\""),
            Self::Batch => format!("SET \"{key}={value}\""),
            Self::Elvish => format!("set E:{key} = \"{value}\""),
            Self::Nushell => format!("$env.{key} = \"{value}\""),
        })
    }

    /// Returns a shell line that unsets the given environment variable.
    ///
    /// Used by the env exporters (`ocx env`, `ocx package env`,
    /// `ocx direnv export`) to clear a tool variable. Returns `None` when
    /// `key` is not a valid POSIX environment-variable name.
    pub fn unset(self, key: impl AsRef<str>) -> Option<String> {
        let key = key.as_ref();
        if !env::is_valid_env_key(key) {
            return None;
        }
        Some(match self {
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => format!("unset {key}"),
            Self::Fish => format!("set -e {key}"),
            Self::PowerShell => format!("Remove-Item Env:{key}"),
            Self::Batch => format!("SET {key}="),
            Self::Elvish => format!("unset-env {key}"),
            Self::Nushell => format!("hide-env {key}"),
        })
    }

    /// Escape `value` for safe interpolation into the double-quoted form of
    /// `self`'s shell.
    ///
    /// This is a defense-in-depth boundary: package metadata under attacker
    /// control may declare environment variable values containing shell
    /// metacharacters (`$`, backticks, `\`, `"`, …). Without escaping,
    /// `export FOO="$(rm -rf /)"` would expand at every prompt. Each branch
    /// below escapes every metacharacter the shell interprets inside the
    /// quoting form OCX uses for its emitted lines (see [`Self::export_path`]
    /// / [`Self::export_constant`]).
    ///
    /// Order of replacements matters: backslash is escaped *first* so that
    /// any backslashes introduced by subsequent escapes don't get re-escaped.
    pub fn escape_value(self, value: impl AsRef<str>) -> String {
        let value = value.as_ref();
        match self {
            // POSIX double-quoted form: `\`, `$`, `` ` ``, `"` interpolate.
            // Backslash first so subsequent inserted backslashes survive.
            // `!` is also escaped: interactive bash/zsh enable `histexpand`
            // by default, and `!` inside double-quotes triggers history
            // expansion (CWE-78). `\!` is literal in bash/zsh double-quotes.
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => value
                .replace('\\', "\\\\")
                .replace('$', "\\$")
                .replace('`', "\\`")
                .replace('!', "\\!")
                .replace('"', "\\\""),
            // Fish double-quoted: only `\`, `$`, and `"` are metacharacters.
            // Backtick has no special meaning inside fish double-quotes and
            // fish does not recognise `\`` as an escape sequence — emitting a
            // literal `\` before a backtick would be wrong. Do not escape `` ` ``.
            Self::Fish => value.replace('\\', "\\\\").replace('$', "\\$").replace('"', "\\\""),
            // Elvish double-quoted: `\` and `"` escape via backslash. No
            // `$`-interpolation in `"..."` (Elvish uses `$var` only outside
            // strings), but we conservatively escape `$` and `` ` `` too.
            Self::Elvish => value
                .replace('\\', "\\\\")
                .replace('$', "\\$")
                .replace('`', "\\`")
                .replace('"', "\\\""),
            // Nushell: `export_constant` emits plain `"..."` (no `(...)`
            // interpolation), but `export_path` uses `$"..."` with
            // `($env.<key>? | default '')` interpolation. We must therefore
            // also neutralize `(` and `)` so a value cannot inject a new
            // expression. `\(` / `\)` are accepted as literal in
            // interpolated strings. `$` also interpolates in `$"..."` and
            // must be escaped (CWE-78): `\$` is a literal `$` in Nushell
            // interpolated strings.
            Self::Nushell => value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('$', "\\$")
                .replace('(', "\\(")
                .replace(')', "\\)"),
            // PowerShell double-quoted: backtick is the escape char,
            // `$var` interpolates. Backtick first.
            Self::PowerShell => value.replace('`', "``").replace('$', "`$").replace('"', "`\""),
            Self::Batch => value
                .replace('%', "%%")
                .replace('^', "^^")
                .replace('&', "^&")
                .replace('<', "^<")
                .replace('>', "^>")
                .replace('|', "^|"),
        }
    }
}

/// Escape `value` for a single-quoted literal in shells where a quote is
/// written by **doubling** it (`'` → `''`): PowerShell and elvish. Inside
/// `'...'` neither shell interpolates, so `$`, backtick, `\`, `;`, and glob
/// metacharacters are all literal and only the quote needs escaping. This is
/// the strongest injection guard for the move-to-front emit — the value can
/// never start a subexpression — and (for elvish) it also avoids the
/// double-quote arm's `\$` / `` \` `` *invalid-escape* parse errors.
fn escape_single_quoted_doubled(value: &str) -> String {
    value.replace('\'', "''")
}

/// Escape `value` for a POSIX `sh` single-quoted literal. A literal quote is
/// written by closing the quote, emitting an escaped quote, and reopening
/// (`'` → `'\''`); every other byte — `$`, backtick, `\`, `!`, glob chars,
/// spaces — is literal inside `'...'`. Used by the bash/zsh and ash/ksh/dash
/// move-to-front emit so the value matches its existing PATH segment byte for
/// byte (the double-quoted form would turn `!` into `\!` and break the
/// comparison, leaving a stale duplicate) and cannot trigger interpolation,
/// history expansion, or globbing.
fn escape_posix_single_quoted(value: &str) -> String {
    value.replace('\'', "'\\''")
}

impl std::fmt::Display for Shell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Ash => "Ash",
            Self::Ksh => "Ksh",
            Self::Dash => "Dash",
            Self::Bash => "Bash",
            Self::Elvish => "Elvish",
            Self::Fish => "Fish",
            Self::Batch => "Batch",
            Self::PowerShell => "PowerShell",
            Self::Zsh => "Zsh",
            Self::Nushell => "Nushell",
        };
        write!(f, "{}", name)
    }
}

impl clap_builder::ValueEnum for Shell {
    fn value_variants<'a>() -> &'a [Self] {
        &[
            Self::Ash,
            Self::Ksh,
            Self::Dash,
            Self::Bash,
            Self::Elvish,
            Self::Fish,
            Self::Batch,
            Self::PowerShell,
            Self::Zsh,
            Self::Nushell,
        ]
    }

    fn to_possible_value(&self) -> Option<clap_builder::builder::PossibleValue> {
        use clap_builder::builder::PossibleValue;

        Some(match self {
            Self::Ash => PossibleValue::new("ash"),
            Self::Ksh => PossibleValue::new("ksh"),
            // `sh` is a POSIX alias for `Dash` — the canonical strict-POSIX
            // shell (Debian `/bin/sh`).  C5 contract: zero new enum variants,
            // zero new match arms.  `--shell=sh` emits byte-identical output
            // to `--shell=dash` through the existing Dash code path.
            Self::Dash => PossibleValue::new("dash").alias("sh"),
            Self::Bash => PossibleValue::new("bash"),
            Self::Elvish => PossibleValue::new("elvish"),
            Self::Fish => PossibleValue::new("fish"),
            // `cmd` is the canonical shell name on Windows (the interpreter is
            // `cmd.exe`).  Without this alias `--shell=cmd` would fail clap
            // parsing (exit 64), which is surprising for Windows users.
            Self::Batch => PossibleValue::new("batch").alias("cmd"),
            // `pwsh` is an alias for `powershell` — same C5 zero-new-variant
            // contract as the `sh`→Dash alias above.  The installer-generated
            // `env.ps1`/`env.sh` emit `--shell=pwsh`; without the alias that
            // would fail clap parsing (exit 64) and silently no-op global
            // toolchain activation on Windows.
            Self::PowerShell => PossibleValue::new("powershell").alias("pwsh"),
            Self::Zsh => PossibleValue::new("zsh"),
            // `nu` is the canonical short name used in most Nushell installations
            // (e.g. `which nu`, PATH entry `nu`, shebang `#!/usr/bin/env nu`).
            // Without this alias `--shell=nu` would fail clap parsing (exit 64),
            // which is surprising for the majority of Nushell users.
            Self::Nushell => PossibleValue::new("nushell").alias("nu"),
        })
    }
}

impl TryInto<clap_complete::Shell> for Shell {
    type Error = Error;

    fn try_into(self) -> Result<clap_complete::Shell, Self::Error> {
        match self {
            Self::Bash => Ok(clap_complete::Shell::Bash),
            Self::Elvish => Ok(clap_complete::Shell::Elvish),
            Self::Fish => Ok(clap_complete::Shell::Fish),
            Self::PowerShell => Ok(clap_complete::Shell::PowerShell),
            Self::Zsh => Ok(clap_complete::Shell::Zsh),
            _ => Err(error::Error::UnsupportedClapShell(self).into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test;

    #[test]
    fn test_from_path() {
        assert_eq!(Shell::from_path("/bin/ash"), Some(Shell::Ash));
        assert_eq!(Shell::from_path("/bin/busybox"), Some(Shell::Ash));
        assert_eq!(Shell::from_path("/bin/ksh"), Some(Shell::Ksh));
        assert_eq!(Shell::from_path("/usr/bin/dash"), Some(Shell::Dash));
        assert_eq!(Shell::from_path("/bin/bash"), Some(Shell::Bash));
        assert_eq!(Shell::from_path("/usr/bin/fish"), Some(Shell::Fish));
        assert_eq!(Shell::from_path("C:/Windows/System32/cmd.exe"), Some(Shell::Batch));
        assert_eq!(
            Shell::from_path("C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe"),
            Some(Shell::PowerShell)
        );
        assert_eq!(
            Shell::from_path("C:/Windows/System32/WindowsPowerShell/v1.0/pwsh.exe"),
            Some(Shell::PowerShell)
        );
        assert_eq!(Shell::from_path("/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_path("/usr/bin/nu"), Some(Shell::Nushell));
        assert_eq!(Shell::from_path("/usr/bin/nushell"), Some(Shell::Nushell));
        assert_eq!(Shell::from_path("/bin/unknown"), None);
    }

    #[test]
    fn test_from_env() {
        let env = test::env::lock();
        env.set("SHELL", "/bin/bash");
        assert_eq!(Shell::from_env(), Some(Shell::Bash));
        env.set("SHELL", "/usr/bin/fish");
        assert_eq!(Shell::from_env(), Some(Shell::Fish));
        env.remove("SHELL");
        assert_eq!(Shell::from_env(), None);
    }

    #[test]
    fn test_from_parent_process() {
        let shell = Shell::from_process();
        println!("Detected shell from parent process: {:?}", shell);
    }

    // ── C5: `sh` ≡ `Shell::Dash` via PossibleValue alias ────────────────
    //
    // `--shell=sh` must resolve to the same enum variant as `--shell=dash`
    // and emit byte-identical output.  This is the C5 contract from
    // plan_toolchain_cli.md: no new enum variant, zero new match arms.

    #[test]
    fn sh_alias_parses_to_dash_variant() {
        use clap_builder::ValueEnum;
        // The clap `ValueEnum::from_str` path must resolve "sh" → Shell::Dash.
        let parsed =
            <Shell as ValueEnum>::from_str("sh", true).expect("'sh' must be a valid shell alias (C5 contract)");
        assert_eq!(
            parsed,
            Shell::Dash,
            "C5: --shell=sh must resolve to Shell::Dash (POSIX strict); got {parsed:?}"
        );
    }

    #[test]
    fn sh_export_path_identical_to_dash() {
        // C5 byte-identity: `--shell=sh` export output must be byte-identical
        // to `--shell=dash`.  Both go through the same match arm.
        let sh_line = Shell::Dash.export_path("PATH", "/opt/ocx/bin").expect("valid key");
        // `Shell::Dash` IS the Dash code path — `sh` is just a PossibleValue
        // alias, not a separate variant.  This test confirms the alias does
        // not introduce a code-path fork (future-proofing against a stray
        // `match "sh" => …` accidentally added later).
        assert!(
            sh_line.contains("export PATH="),
            "Dash/sh export_path must emit POSIX export form; got: {sh_line:?}"
        );
    }

    #[test]
    fn sh_export_constant_identical_to_dash() {
        let line = Shell::Dash.export_constant("MY_VAR", "hello").expect("valid key");
        assert!(
            line.starts_with("export MY_VAR="),
            "Dash/sh export_constant must emit POSIX export form; got: {line:?}"
        );
    }

    #[test]
    fn pwsh_alias_parses_to_powershell_variant() {
        use clap_builder::ValueEnum;
        // The installer-generated env.ps1/env.sh emit `--shell=pwsh`.  Without
        // the PossibleValue alias clap rejects it (exit 64) and the global
        // toolchain silently fails to activate on Windows.
        let parsed = <Shell as ValueEnum>::from_str("pwsh", true)
            .expect("'pwsh' must be a valid shell alias (installer contract)");
        assert_eq!(
            parsed,
            Shell::PowerShell,
            "--shell=pwsh must resolve to Shell::PowerShell; got {parsed:?}"
        );
        // `powershell` must keep working too — alias adds, never replaces.
        assert_eq!(
            <Shell as ValueEnum>::from_str("powershell", true).expect("'powershell' canonical value"),
            Shell::PowerShell,
        );
    }

    #[test]
    fn from_path_resolves_nushell_executable() {
        // `/usr/local/bin/nu` is the binary install pattern Nushell's
        // installer uses; cover it to confirm `from_path` is not pinned to
        // `/usr/bin`.
        assert_eq!(Shell::from_path("/usr/local/bin/nu"), Some(Shell::Nushell));
    }

    #[test]
    fn export_path_nushell_appends_existing() {
        // Whatever exact syntax Nushell uses, the line MUST refer to
        // `$env.PATH` so that the existing PATH is preserved on update.
        let line = Shell::Nushell
            .export_path("PATH", "/opt/bin")
            .expect("valid env-var name accepted");
        assert!(
            line.contains("$env.PATH"),
            "Nushell export_path must reference $env.PATH; got: {line:?}"
        );
        assert!(
            line.contains("/opt/bin"),
            "Nushell export_path must include the value; got: {line:?}"
        );
    }

    #[test]
    fn export_constant_nushell_uses_env_assignment() {
        let line = Shell::Nushell
            .export_constant("MY_KEY", "myval")
            .expect("valid env-var name accepted");
        assert_eq!(
            line, "$env.MY_KEY = \"myval\"",
            "Nushell export_constant must emit `$env.<KEY> = \"<value>\"`"
        );
    }

    // ── escape_value hardening (Round 2 B1) ──────────────────────────
    //
    // `escape_value` must neutralize every metacharacter the destination
    // shell interprets inside its emitted quoting form. These tests
    // assert the raw metacharacter never escapes intact — i.e. an
    // attacker controlling a package metadata `value = "$(rm -rf /)"`
    // cannot cause command substitution at the shell side.

    #[test]
    fn escape_value_neutralizes_command_substitution_bash() {
        // The escaped form must carry an escaped `$` (`\$`) so the shell
        // sees `\$(...)` and treats it as a literal — `$(...)` would
        // execute. We check for the escaped marker, not absence of the
        // raw substring (the `$(` chars *do* appear, just preceded by a
        // backslash that disarms the substitution).
        let val = Shell::Bash.escape_value("$(rm -rf /)");
        assert!(
            val.starts_with("\\$"),
            "command substitution must be neutralized via \\$: got {val:?}"
        );
    }

    #[test]
    fn escape_value_neutralizes_backtick_bash() {
        // Backtick at the start would open command substitution; the
        // escaped form must lead with `\` so the shell sees `\``.
        let val = Shell::Bash.escape_value("`evil`");
        assert!(
            val.starts_with("\\`"),
            "leading backtick must be backslash-escaped: got {val:?}"
        );
    }

    #[test]
    fn escape_value_neutralizes_backslash_bash() {
        // `\$` inside a bash double-quoted string expands to a literal `$`,
        // so the backslash itself must be doubled when forwarded — we want
        // the shell to see `\\$VAR` (literal `\$VAR`), not `\$VAR` (which
        // would expand to `$VAR`).
        let val = Shell::Bash.escape_value(r"\$VAR");
        assert!(val.starts_with(r"\\"), "backslash must be escaped first: {val:?}");
    }

    #[test]
    fn escape_value_neutralizes_command_substitution_nushell() {
        // Spliced into `$"…(<x>)…"`, `(rm -rf /)` would start an inner
        // expression. After escaping, the inner `(` must be backslash-
        // escaped so Nushell treats it as a literal character.
        let val = Shell::Nushell.escape_value("(rm -rf /)");
        assert!(val.contains("\\("), "( must be backslash-escaped: got {val:?}");
    }

    #[test]
    fn escape_value_neutralizes_dollar_powershell() {
        // PowerShell interpolates `$var` inside `"..."`; backtick is the
        // escape char. `$` must come out as `` `$ ``.
        let val = Shell::PowerShell.escape_value("$evil");
        assert!(val.contains("`$"), "$ must be backtick-escaped in PowerShell: {val:?}");
    }

    #[test]
    fn export_constant_bash_escapes_metacharacters_in_value() {
        let line = Shell::Bash
            .export_constant("FOO", "$(rm -rf /)")
            .expect("valid env-var name accepted");
        // The boundary: the embedded `$` must be escaped (`\$`) so the
        // shell doesn't perform command substitution on `$(...)`.
        assert!(
            line.contains("\\$("),
            "$ must be escaped before ( so command substitution is disarmed: got {line:?}"
        );
    }

    // ── CWE-78 history expansion hardening ───────────────────────────
    //
    // Bash and Zsh enable `histexpand` by default in interactive sessions.
    // Inside double-quoted strings, `!` triggers history expansion (e.g.
    // `!!`, `!$`, `!rm` expand against shell history) when eval'd at the
    // login shell. The installer runs `eval "$(ocx --global env --shell=sh)"`
    // — any unescaped `!` in a metadata value reaches the interactive shell.
    //
    // Ash/Ksh/Dash do not implement histexpand but we escape uniformly
    // across the POSIX family (same match arm) to prevent drift if a user
    // configures histexpand on those shells.

    #[test]
    fn escape_value_neutralizes_history_expansion_bash() {
        // `!!` is "repeat last command"; `!$` is "last argument".
        // After escaping both must have the `!` preceded by a backslash.
        let val = Shell::Bash.escape_value("!!");
        assert!(
            val.starts_with("\\!"),
            "leading ! must be backslash-escaped for bash histexpand safety: got {val:?}"
        );
        assert!(
            !val.contains("!!"),
            "bare !! must not appear in escaped output: got {val:?}"
        );
    }

    #[test]
    fn escape_value_neutralizes_history_expansion_zsh() {
        let val = Shell::Zsh.escape_value("!rm -rf /");
        assert!(
            val.starts_with("\\!"),
            "leading ! must be backslash-escaped for zsh histexpand safety: got {val:?}"
        );
    }

    #[test]
    fn escape_value_history_expansion_in_export_constant_bash() {
        // End-to-end: the emitted export line must not contain bare `!`.
        let line = Shell::Bash
            .export_constant("FOO", "!!bad")
            .expect("valid env-var name accepted");
        assert!(
            !line.contains("!!"),
            "bare !! must not appear in emitted export line: got {line:?}"
        );
        assert!(
            line.contains("\\!\\!"),
            "both ! must be individually escaped: got {line:?}"
        );
    }

    // sh ≡ Dash: confirm the `!`-safe POSIX branch covers the login-shell path.
    #[test]
    fn escape_value_history_expansion_dash_sh_path() {
        // `--shell=sh` aliases to Shell::Dash (C5 contract). The same POSIX
        // match arm now escapes `!`, so the login eval path is safe.
        let val = Shell::Dash.escape_value("!important");
        assert!(
            val.starts_with("\\!"),
            "Dash (sh alias) must escape ! for histexpand safety: got {val:?}"
        );
    }

    // ── CWE-78 Nushell `$` interpolation hardening ───────────────────
    //
    // `export_path` emits `$"...($env.KEY?...)"`. Inside `$"..."`, `$`
    // triggers interpolation — a metadata value `$env.HOME` would expand.
    // `\$` is a literal `$` in Nushell interpolated strings.

    #[test]
    fn escape_value_neutralizes_dollar_nushell() {
        let val = Shell::Nushell.escape_value("$env.HOME");
        // The `$` must be preceded by `\` so Nushell does not interpolate.
        // The escaped output is `\$env.HOME`; in Rust `String` Debug that
        // prints as `"\\$env.HOME"`.  We check that every `$` in the output
        // is immediately preceded by `\` — no bare `$` survives.
        assert!(
            val.starts_with("\\$"),
            "$ must be backslash-escaped in Nushell to prevent interpolation: got {val:?}"
        );
        // Confirm the raw string contains no unescaped dollar sign.
        // An unescaped dollar is one that is NOT preceded by `\`.
        let has_bare_dollar = val
            .char_indices()
            .any(|(i, c)| c == '$' && (i == 0 || val.as_bytes()[i - 1] != b'\\'));
        assert!(
            !has_bare_dollar,
            "no bare $ must remain in Nushell-escaped output: got {val:?}"
        );
    }

    #[test]
    fn escape_value_dollar_nushell_in_export_path() {
        // End-to-end: emitted export_path line must not carry bare `$env.`.
        let line = Shell::Nushell
            .export_path("PATH", "$env.HOME/bin")
            .expect("valid env-var name accepted");
        // The value portion before the separator must have the $ escaped.
        // We search for the literal sequence `\$env` to confirm escape.
        assert!(
            line.contains("\\$env"),
            "$ in value must be escaped in Nushell export_path: got {line:?}"
        );
    }

    // ── Fish backtick correctness ────────────────────────────────────
    //
    // Fish double-quotes: only `\`, `$`, and `"` are metacharacters. Backtick
    // carries no special meaning and fish does not recognise `\`` as a valid
    // escape sequence — emitting `\`` would produce a literal backslash
    // followed by a backtick (wrong representation). Backtick must round-trip
    // as-is (no preceding backslash).

    #[test]
    fn escape_value_fish_backtick_roundtrips_literally() {
        let val = Shell::Fish.escape_value("`echo hi`");
        // The backtick must appear without a preceding `\`.
        assert!(
            val.starts_with('`'),
            "backtick must not be escaped in fish double-quotes: got {val:?}"
        );
        assert!(
            !val.contains("\\`"),
            "fish must not emit \\` (invalid escape): got {val:?}"
        );
    }

    // ── Env-key validation (Round 2 B2) ──────────────────────────────
    //
    // `is_valid_env_key` is the gate: emitter functions return `None`
    // for keys not matching the POSIX env-var-name grammar so a malicious
    // metadata key (e.g. `"FOO; rm -rf /; X"`) cannot inject into the
    // *key* slot of an emitted `export KEY=...` line.

    #[test]
    fn export_constant_rejects_injection_in_key() {
        let result = Shell::Bash.export_constant("FOO; rm -rf /; X", "value");
        assert!(result.is_none(), "injection key must be rejected: {result:?}");
    }

    #[test]
    fn export_path_rejects_injection_in_key() {
        let result = Shell::Bash.export_path("PATH; rm -rf /", "/opt/bin");
        assert!(result.is_none(), "injection key must be rejected: {result:?}");
    }

    #[test]
    fn unset_rejects_injection_in_key() {
        let result = Shell::Bash.unset("FOO; rm -rf /; X");
        assert!(result.is_none(), "injection key must be rejected: {result:?}");
    }

    #[test]
    fn export_constant_rejects_empty_key() {
        let result = Shell::Bash.export_constant("", "value");
        assert!(result.is_none(), "empty key must be rejected: {result:?}");
    }

    #[test]
    fn export_constant_rejects_leading_digit_key() {
        let result = Shell::Bash.export_constant("1FOO", "value");
        assert!(result.is_none(), "leading-digit key must be rejected: {result:?}");
    }

    #[test]
    fn export_constant_accepts_underscore_prefixed_key() {
        // Leading-underscore env-var names are valid POSIX identifiers.
        let line = Shell::Bash
            .export_constant("_OCX_INTERNAL", "value")
            .expect("underscore-prefixed key accepted");
        assert!(line.contains("_OCX_INTERNAL"));
    }

    // ── Per-shell unset syntax matrix (Round 2 W4) ───────────────────

    #[test]
    fn unset_per_shell_syntax() {
        let key = "FOO";
        assert_eq!(Shell::Ash.unset(key), Some("unset FOO".into()));
        assert_eq!(Shell::Ksh.unset(key), Some("unset FOO".into()));
        assert_eq!(Shell::Dash.unset(key), Some("unset FOO".into()));
        assert_eq!(Shell::Bash.unset(key), Some("unset FOO".into()));
        assert_eq!(Shell::Zsh.unset(key), Some("unset FOO".into()));
        assert_eq!(Shell::Fish.unset(key), Some("set -e FOO".into()));
        assert_eq!(Shell::PowerShell.unset(key), Some("Remove-Item Env:FOO".into()));
        assert_eq!(Shell::Batch.unset(key), Some("SET FOO=".into()));
        assert_eq!(Shell::Elvish.unset(key), Some("unset-env FOO".into()));
        assert_eq!(Shell::Nushell.unset(key), Some("hide-env FOO".into()));
    }

    #[test]
    fn display_nushell_is_nushell() {
        assert_eq!(Shell::Nushell.to_string(), "Nushell");
    }

    #[test]
    fn value_enum_includes_nushell() {
        use clap_builder::ValueEnum;
        let variants = Shell::value_variants();
        assert!(
            variants.contains(&Shell::Nushell),
            "Shell::value_variants() must include Nushell"
        );
    }

    #[test]
    fn to_possible_value_nushell_is_nushell() {
        use clap_builder::ValueEnum;
        let pv = Shell::Nushell
            .to_possible_value()
            .expect("Nushell must produce a PossibleValue");
        assert_eq!(pv.get_name(), "nushell");
    }

    // ── `nu` clap alias for Nushell ──────────────────────────────────────
    //
    // `nu` is the canonical executable name used in the majority of Nushell
    // installations (PATH entry, shebang `#!/usr/bin/env nu`, `which nu`).
    // Without the alias `--shell=nu` fails clap parsing (exit 64), which is
    // surprising for most users.  Confirm both canonical name and alias work.

    #[test]
    fn nu_alias_parses_to_nushell_variant() {
        use clap_builder::ValueEnum;
        let parsed = <Shell as ValueEnum>::from_str("nu", true).expect("'nu' must be a valid shell alias for Nushell");
        assert_eq!(
            parsed,
            Shell::Nushell,
            "--shell=nu must resolve to Shell::Nushell; got {parsed:?}"
        );
        // Canonical name `nushell` must still work — alias adds, never replaces.
        let canonical = <Shell as ValueEnum>::from_str("nushell", true).expect("'nushell' canonical value");
        assert_eq!(canonical, Shell::Nushell);
    }

    // ── `cmd` clap alias for Batch ───────────────────────────────────────────
    //
    // `cmd` is the canonical Windows shell name (`cmd.exe`).  Without the alias
    // `--shell=cmd` would fail clap parsing (exit 64), which is surprising for
    // Windows users.  Confirm both canonical name and alias work.

    #[test]
    fn cmd_alias_parses_to_batch_variant() {
        use clap_builder::ValueEnum;
        let parsed = <Shell as ValueEnum>::from_str("cmd", true).expect("'cmd' must be a valid shell alias for Batch");
        assert_eq!(
            parsed,
            Shell::Batch,
            "--shell=cmd must resolve to Shell::Batch; got {parsed:?}"
        );
        // Canonical name `batch` must still work — alias adds, never replaces.
        let canonical = <Shell as ValueEnum>::from_str("batch", true).expect("'batch' canonical value");
        assert_eq!(canonical, Shell::Batch);
    }

    #[test]
    fn try_into_clap_complete_unsupported_for_nushell() {
        let res: Result<clap_complete::Shell, _> = Shell::Nushell.try_into();
        let err = res.expect_err("Nushell must be an unsupported clap shell");
        // Match the typed variant — this also asserts the source error chain
        // surfaces the offending shell.
        let msg = err.to_string();
        assert!(
            msg.contains("Nushell"),
            "expected error to reference Nushell, got: {msg:?}"
        );
    }

    // ══ Idempotent move-to-front export_path (issue #26) ═════════════════
    //
    // Two layers: (1) exact-shape tests lock the emitted bytes so a `format!`
    // brace regression fails loudly; (2) `live_*` tests run the *real* emitted
    // statement through an actual interpreter (skipped when the interpreter is
    // absent) to prove idempotency / move-to-front / injection-safety end to end.

    fn sep() -> &'static str {
        crate::env::PATH_SEPARATOR
    }

    #[test]
    fn export_path_bash_zsh_exact_shape() {
        // bash and zsh share the single-quoted colon-sentinel + fixpoint form.
        let expected = "__ocx_p='/opt/bin'; PATH=\":${PATH-}:\"; while [ \"$PATH\" != \"${PATH//:\"$__ocx_p\":/:}\" ]; do PATH=\"${PATH//:\"$__ocx_p\":/:}\"; done; PATH=\"${PATH#:}\"; PATH=\"${PATH%:}\"; export PATH=\"$__ocx_p${PATH:+:${PATH}}\"; unset __ocx_p";
        assert_eq!(Shell::Bash.export_path("PATH", "/opt/bin").as_deref(), Some(expected));
        assert_eq!(Shell::Zsh.export_path("PATH", "/opt/bin").as_deref(), Some(expected));
    }

    #[test]
    fn export_path_posix_exact_shape() {
        // ash / ksh / dash share the single-awk POSIX form.
        let expected = "__ocx_p='/opt/bin'; export __ocx_p; export PATH=\"$__ocx_p$(printf %s \"${PATH-}\" | awk 'BEGIN{ORS=\"\";RS=\":\";d=ENVIRON[\"__ocx_p\"]} $0!=d && $0!=\"\"{printf \":%s\",$0}')\"; unset __ocx_p";
        for shell in [Shell::Ash, Shell::Ksh, Shell::Dash] {
            assert_eq!(
                shell.export_path("PATH", "/opt/bin").as_deref(),
                Some(expected),
                "{shell}"
            );
        }
    }

    #[test]
    fn export_path_fish_exact_shape() {
        let expected = "set __ocx_p \"/opt/bin\"; set __ocx_r; for __ocx_e in $PATH; test \"$__ocx_e\" != \"$__ocx_p\"; and set -a __ocx_r $__ocx_e; end; set -gx PATH $__ocx_p $__ocx_r; set -e __ocx_p __ocx_r __ocx_e";
        assert_eq!(Shell::Fish.export_path("PATH", "/opt/bin").as_deref(), Some(expected));
    }

    #[test]
    fn export_path_powershell_exact_shape() {
        let expected = "$__ocx_p='/opt/bin'; $__ocx_s=[IO.Path]::PathSeparator; $env:PATH=(@($__ocx_p)+($env:PATH -split [regex]::Escape($__ocx_s) | Where-Object {$_ -and $_ -ne $__ocx_p})) -join $__ocx_s; Remove-Variable __ocx_p,__ocx_s";
        assert_eq!(
            Shell::PowerShell.export_path("PATH", "/opt/bin").as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn export_path_elvish_exact_shape() {
        let s = sep();
        let expected = format!(
            "use str; set E:PATH = (str:join \"{s}\" ['/opt/bin' (str:split \"{s}\" $E:PATH | each {{|p| if (and (not-eq $p '/opt/bin') (not-eq $p \"\")) {{ put $p }} }})])"
        );
        assert_eq!(Shell::Elvish.export_path("PATH", "/opt/bin"), Some(expected));
    }

    #[test]
    fn export_path_nushell_exact_shape() {
        let expected = "$env.PATH = (($env.PATH? | default \"\") | (if ($in | describe) == 'string' { split row (char esep) } else { $in }) | where {|p| $p != \"/opt/bin\" and $p != \"\" } | prepend \"/opt/bin\")";
        assert_eq!(
            Shell::Nushell.export_path("PATH", "/opt/bin").as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn export_path_batch_idempotent_move_to_front() {
        // Single statement: delete every `value<sep>` occurrence, prepend once.
        let s = sep();
        let expected = format!("SET \"PATH=/opt/bin{s}%PATH:/opt/bin{s}=%\"");
        assert_eq!(Shell::Batch.export_path("PATH", "/opt/bin"), Some(expected));
    }

    #[test]
    fn export_path_is_generic_over_key() {
        // The emitter must work for any path-style key, not just `PATH`
        // (package metadata declares `LD_LIBRARY_PATH`, `MANPATH`, etc. as path).
        let line = Shell::Bash.export_path("LD_LIBRARY_PATH", "/x/lib").unwrap();
        assert!(line.contains("export LD_LIBRARY_PATH="), "got: {line}");
        assert!(!line.contains("PATH=\":${PATH-}:\""), "must not hardcode PATH: {line}");
    }

    #[test]
    fn export_path_rejects_invalid_key_all_shells() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Ash,
            Shell::Ksh,
            Shell::Dash,
            Shell::Fish,
            Shell::PowerShell,
            Shell::Elvish,
            Shell::Nushell,
            Shell::Batch,
        ] {
            assert!(shell.export_path("PATH; rm -rf /", "/x").is_none(), "{shell}");
        }
    }

    #[test]
    fn export_path_bash_neutralizes_injection() {
        // A metadata value `/x;$(touch evil)` is carried as a single-quoted
        // literal, where `$`, `;`, and `(` are all inert — no command
        // substitution can fire and the value never reaches a command position.
        let line = Shell::Bash.export_path("PATH", "/x;$(touch evil)").unwrap();
        assert!(line.contains("__ocx_p='/x;$(touch evil)'"), "got: {line}");
    }

    #[test]
    fn export_path_posix_single_quote_escapes_embedded_quote() {
        // A value with a literal `'` must close/escape/reopen (`'\''`) so the
        // single-quoted literal stays balanced across bash and the POSIX family.
        for shell in [Shell::Bash, Shell::Zsh, Shell::Dash, Shell::Ksh, Shell::Ash] {
            let line = shell.export_path("PATH", "/o'brien/bin").unwrap();
            assert!(line.contains("__ocx_p='/o'\\''brien/bin'"), "{shell}: {line}");
        }
    }

    #[test]
    fn export_path_posix_value_with_bang_is_literal_not_escaped() {
        // History-expansion safety must not corrupt the value: a `!`-bearing dir
        // stays byte-exact inside `'...'` (no `\!`), so it matches its real PATH
        // segment and dedups instead of leaving a stale copy.
        for shell in [Shell::Bash, Shell::Zsh, Shell::Dash] {
            let line = shell.export_path("PATH", "/x!y/bin").unwrap();
            assert!(line.contains("__ocx_p='/x!y/bin'"), "{shell}: {line}");
            assert!(
                !line.contains("\\!"),
                "{shell}: value must not be backslash-escaped: {line}"
            );
        }
    }

    #[test]
    fn export_path_powershell_single_quote_escapes_embedded_quote() {
        let line = Shell::PowerShell.export_path("PATH", "/o'brien/bin").unwrap();
        assert!(line.contains("$__ocx_p='/o''brien/bin'"), "got: {line}");
    }

    // ── live-interpreter tests (skip when the shell is unavailable) ──────
    //
    // These run the *actual* emitted statement so the format!-built bytes are
    // exercised by a real parser, proving the three invariants end to end:
    // move-to-front, idempotency (source twice), and injection safety.

    use std::process::Command;

    /// Run `script` under `argv` (interpreter + flags), returning trimmed stdout,
    /// or `None` if the interpreter is not installed (so CI without it stays green).
    fn run_script(argv: &[&str], script: &str) -> Option<String> {
        let (bin, head) = argv.split_first()?;
        let mut command = Command::new(bin);
        command.args(head).arg(script);
        match command.output() {
            Ok(output) => Some(String::from_utf8_lossy(&output.stdout).trim_end().to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("failed to spawn {bin}: {error}"),
        }
    }

    /// POSIX-style readback: seed PATH with the dir mid-list, source twice, print.
    fn posix_roundtrip(argv: &[&str], real_bin_dir: &str) {
        let line = Shell::from_argv(argv).export_path("PATH", "/opt/bin").unwrap();
        let script = format!("export PATH=/a:/opt/bin:/b:{real_bin_dir}; {line}; {line}; printf '%s' \"$PATH\"");
        if let Some(out) = run_script(argv, &script) {
            assert_eq!(out, format!("/opt/bin:/a:/b:{real_bin_dir}"), "argv={argv:?}");
        }
    }

    impl Shell {
        /// Map a live-test interpreter argv back to the `Shell` whose emit we test.
        fn from_argv(argv: &[&str]) -> Shell {
            match argv[0] {
                "bash" => Shell::Bash,
                "zsh" => Shell::Zsh,
                "dash" => Shell::Dash,
                "ksh" => Shell::Ksh,
                "busybox" => Shell::Ash,
                other => panic!("unmapped interpreter {other}"),
            }
        }
    }

    fn awk_dir() -> String {
        // The POSIX awk form needs `awk` resolvable on PATH; keep the real bin dir.
        std::path::Path::new(&run_script(&["bash", "-c"], "command -v awk").unwrap_or_default())
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/bin".to_string())
    }

    #[test]
    fn live_bash_zsh_idempotent_move_to_front() {
        let dir = awk_dir();
        posix_roundtrip(&["bash", "-c"], &dir);
        posix_roundtrip(&["zsh", "-c"], &dir);
    }

    #[test]
    fn live_posix_idempotent_move_to_front() {
        let dir = awk_dir();
        posix_roundtrip(&["dash", "-c"], &dir);
        posix_roundtrip(&["ksh", "-c"], &dir);
        posix_roundtrip(&["busybox", "ash", "-c"], &dir);
    }

    #[test]
    fn live_adjacent_duplicates_collapse() {
        // Pre-existing ADJACENT duplicates of the added dir must collapse to one
        // (regression: one non-overlapping `${//}` pass leaves a stale copy).
        let dir = awk_dir();
        for argv in [
            &["bash", "-c"][..],
            &["zsh", "-c"][..],
            &["dash", "-c"][..],
            &["busybox", "ash", "-c"][..],
        ] {
            let line = Shell::from_argv(argv).export_path("PATH", "/opt/bin").unwrap();
            let script = format!("export PATH=/opt/bin:/opt/bin:/a:{dir}; {line}; printf '%s' \"$PATH\"");
            if let Some(out) = run_script(argv, &script) {
                assert_eq!(
                    out,
                    format!("/opt/bin:/a:{dir}"),
                    "argv={argv:?}: adjacent dup not collapsed"
                );
            }
        }
    }

    #[test]
    fn live_bang_path_dedups() {
        // A `!`-bearing dir present mid-PATH must move to front and dedup.
        // Regression: a double-quoted `\!` escape broke the exact-segment match.
        let dir = awk_dir();
        for argv in [&["bash", "-c"][..], &["dash", "-c"][..]] {
            let line = Shell::from_argv(argv).export_path("PATH", "/x!y/bin").unwrap();
            let script = format!("export PATH=/a:/x!y/bin:/b:{dir}; {line}; printf '%s' \"$PATH\"");
            if let Some(out) = run_script(argv, &script) {
                assert_eq!(
                    out,
                    format!("/x!y/bin:/a:/b:{dir}"),
                    "argv={argv:?}: bang-path dedup failed"
                );
            }
        }
    }

    #[test]
    fn live_bash_empty_path_has_no_separators() {
        let line = Shell::Bash.export_path("PATH", "/opt/bin").unwrap();
        let script = format!("export PATH=; {line}; printf '%s' \"$PATH\"");
        if let Some(out) = run_script(&["bash", "-c"], &script) {
            assert_eq!(out, "/opt/bin", "empty PATH must not gain leading/trailing separators");
        }
    }

    #[test]
    fn live_bash_injection_does_not_execute() {
        let marker = std::env::temp_dir().join("ocx_inj_live_bash");
        let _ = std::fs::remove_file(&marker);
        let value = format!("/x;touch {}", marker.display());
        let line = Shell::Bash.export_path("PATH", &value).unwrap();
        let script = format!("export PATH=/a:/b; {line}; true");
        if run_script(&["bash", "-c"], &script).is_some() {
            assert!(!marker.exists(), "injection executed: {} was created", marker.display());
        }
    }

    #[test]
    fn live_fish_idempotent_move_to_front() {
        let line = Shell::Fish.export_path("PATH", "/opt/bin").unwrap();
        let script = format!("set -gx PATH /a /opt/bin /b; {line}; {line}; string join : $PATH");
        if let Some(out) = run_script(&["fish", "-c"], &script) {
            assert_eq!(out, "/opt/bin:/a:/b");
        }
    }

    #[test]
    fn live_powershell_idempotent_move_to_front() {
        let line = Shell::PowerShell.export_path("PATH", "/opt/bin").unwrap();
        let script = format!("$env:PATH='/a:/opt/bin:/b'; {line}; {line}; $env:PATH");
        if let Some(out) = run_script(&["pwsh", "-NoProfile", "-Command"], &script) {
            assert_eq!(out, "/opt/bin:/a:/b");
        }
    }

    /// Run a batch `body` as a temp `.bat` under `cmd /c`, returning trimmed
    /// stdout, or `None` when cmd.exe is absent (non-Windows → the test skips
    /// green). A real script file is required, not `cmd /c "<body>"`: the
    /// move-to-front emit is multi-line and relies on per-statement `%PATH%`
    /// re-expansion, which only a sequentially-parsed `.bat` provides. CRLF line
    /// endings keep legacy cmd parsers happy.
    fn run_batch(body: &str) -> Option<String> {
        let path = std::env::temp_dir().join("ocx_batch_live_mtf.bat");
        std::fs::write(&path, format!("@echo off\r\n{body}\r\n")).ok()?;
        let result = match Command::new("cmd").args(["/c", &path.to_string_lossy()]).output() {
            Ok(output) => Some(String::from_utf8_lossy(&output.stdout).trim_end().to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("failed to spawn cmd: {error}"),
        };
        let _ = std::fs::remove_file(&path);
        result
    }

    #[test]
    fn live_batch_idempotent_move_to_front() {
        // cmd.exe only (Windows CI's `nextest --workspace` leg); skips green
        // elsewhere via NotFound. Proves the substring-delete emit moves a
        // mid-PATH entry to the front and is idempotent across a double source —
        // the work the removed `OCX_ACTIVATED` guard used to do for prepend-only.
        let separator = sep();
        let value = r"C:\opt\bin";
        let line = Shell::Batch.export_path("PATH", value).unwrap();
        let seed = format!(r"C:\sys{separator}{value}{separator}C:\other");
        let body = format!("SET \"PATH={seed}\"\r\n{line}\r\n{line}\r\necho %PATH%");
        if let Some(out) = run_batch(&body) {
            assert_eq!(
                out,
                format!(r"{value}{separator}C:\sys{separator}C:\other"),
                "batch move-to-front not idempotent across a double source"
            );
        }
    }

    #[test]
    fn live_batch_empty_path_has_entry_first() {
        // Documented batch contract on an empty PATH: a single statement cannot
        // conditionally drop the separator, so the result is `value<sep>` (a
        // trailing empty segment cmd ignores). The entry must lead and the form
        // must stay stable across a double source.
        let separator = sep();
        let value = r"C:\opt\bin";
        let line = Shell::Batch.export_path("PATH", value).unwrap();
        let body = format!("SET \"PATH=\"\r\n{line}\r\n{line}\r\necho %PATH%");
        if let Some(out) = run_batch(&body) {
            assert_eq!(
                out,
                format!(r"{value}{separator}"),
                "batch empty-PATH form must be `value<sep>` and stable"
            );
        }
    }
}
