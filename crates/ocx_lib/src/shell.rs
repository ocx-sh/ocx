// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod applied_set;
pub mod error;
mod profile_builder;
pub use applied_set::{AppliedEntry, compute_fingerprint, parse_applied, render_applied};
pub use profile_builder::ProfileBuilder;

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

    pub fn profile_builder(&self, content: impl Into<std::path::PathBuf>) -> ProfileBuilder {
        ProfileBuilder::new(content.into(), *self)
    }

    /// Default basename for the per-shell init file under `$OCX_HOME`
    /// (`init.bash`, `init.zsh`, `init.fish`, `init.nu`, …).
    ///
    /// POSIX-family shells (`ash`, `ksh`, `dash`, `bash`) all share
    /// `init.bash` — they accept the same `export KEY="…"` syntax that
    /// `export_constant` / `export_path` emit, so a single generated file
    /// works for the whole family. Other shells get their own basename so
    /// the generated file's syntax matches the shell that will source it.
    ///
    /// Used by [`crate::ConfigLoader::home_init_path`] to compute the
    /// default `--output` target for `ocx shell profile generate` (see
    /// [`crates/ocx_cli/src/command/shell_profile_generate.rs`]).
    pub fn default_init_filename(self) -> &'static str {
        match self {
            Self::Bash | Self::Ash | Self::Ksh | Self::Dash => "init.bash",
            Self::Zsh => "init.zsh",
            Self::Fish => "init.fish",
            Self::Nushell => "init.nu",
            Self::PowerShell => "init.ps1",
            Self::Batch => "init.bat",
            Self::Elvish => "init.elv",
        }
    }

    /// Returns a shell comment line.
    pub fn comment(self, text: impl AsRef<str>) -> String {
        let text = text.as_ref();
        match self {
            Self::Batch => format!("REM {text}"),
            _ => format!("# {text}"),
        }
    }

    /// Emit a shell line that prepends `value` (via `PATH_SEPARATOR`) to the
    /// existing value of the environment variable named `key`.
    ///
    /// Returns `None` when `key` is not a valid POSIX environment-variable
    /// name (`[A-Za-z_][A-Za-z0-9_]*`). Caller decides how to surface that —
    /// invalid keys are produced exclusively by malformed package metadata
    /// and never by the OCX code base itself, so propagating `None` is the
    /// path-of-least-impact safety guard.
    pub fn export_path(self, key: impl AsRef<str>, value: impl AsRef<str>) -> Option<String> {
        let key = key.as_ref();
        if !is_valid_env_key(key) {
            return None;
        }
        let value = self.escape_value(value);
        let separator = env::PATH_SEPARATOR;
        Some(match self {
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => {
                format!("export {key}=\"{value}{separator}${{{key}}}\"")
            }
            Self::Fish => format!("set -x {key} \"{value}{separator}\" ${key}"),
            Self::PowerShell => format!("$env:{key} = \"{value}{separator}$env:{key}\""),
            Self::Batch => format!("SET \"{key}={value}{separator}%{key}%\""),
            Self::Elvish => format!("set E:{key} = \"{value}{separator}\"$E:{key}"),
            Self::Nushell => {
                format!("$env.{key} = $\"{value}{separator}($env.{key}? | default '')\"")
            }
        })
    }

    /// Emit a shell line that sets `key=value` (replacing any prior value).
    ///
    /// Returns `None` when `key` is not a valid POSIX environment-variable
    /// name. See [`Self::export_path`] for the rationale.
    pub fn export_constant(self, key: impl AsRef<str>, value: impl AsRef<str>) -> Option<String> {
        let key = key.as_ref();
        if !is_valid_env_key(key) {
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
    /// Used by `ocx hook-env` to clear previously-applied tool variables
    /// before re-emitting the current set when the `_OCX_APPLIED`
    /// fingerprint changes. Returns `None` when `key` is not a valid POSIX
    /// environment-variable name.
    pub fn unset(self, key: impl AsRef<str>) -> Option<String> {
        let key = key.as_ref();
        if !is_valid_env_key(key) {
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
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => value
                .replace('\\', "\\\\")
                .replace('$', "\\$")
                .replace('`', "\\`")
                .replace('"', "\\\""),
            // Fish double-quoted: same metacharacters as POSIX.
            Self::Fish => value
                .replace('\\', "\\\\")
                .replace('$', "\\$")
                .replace('`', "\\`")
                .replace('"', "\\\""),
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
            // interpolated strings.
            Self::Nushell => value
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
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

/// Validates that `key` matches the POSIX environment-variable name grammar
/// (`[A-Za-z_][A-Za-z0-9_]*`).
///
/// Reject anything else: malicious package metadata could otherwise inject
/// shell syntax via the *key* slot of an `export KEY=VAL` line (e.g.
/// `KEY="; rm -rf $HOME; X"`), which `escape_value` does not protect against.
/// Validation lives at the emitter layer (`export_path`, `export_constant`,
/// `unset`) so every consumer — shell-hook, hook-env, `shell env`,
/// `ci export` — gets the protection automatically.
fn is_valid_env_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let mut bytes = key.bytes();
    let Some(first) = bytes.next() else { return false };
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return false;
    }
    bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
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
            Self::Dash => PossibleValue::new("dash"),
            Self::Bash => PossibleValue::new("bash"),
            Self::Elvish => PossibleValue::new("elvish"),
            Self::Fish => PossibleValue::new("fish"),
            Self::Batch => PossibleValue::new("batch"),
            Self::PowerShell => PossibleValue::new("powershell"),
            Self::Zsh => PossibleValue::new("zsh"),
            Self::Nushell => PossibleValue::new("nushell"),
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

    // ── Nushell parity (Phase 7 — shell-hook trio) ───────────────────
    //
    // Brings Nushell coverage to parity with Bash/Zsh for the public surface
    // touched by `ocx hook-env`, `ocx shell-hook`, and `ocx shell init`.

    // ── default_init_filename (Round 2 B1) ───────────────────────────
    //
    // `default_init_filename` is the canonical shell-keyed basename for
    // the `$OCX_HOME/init.<shell>` default that `ocx shell profile
    // generate` writes. The mapping is a property of each shell, so the
    // table lives on `Shell` rather than in the CLI command.

    #[test]
    fn default_init_filename_per_shell_basenames() {
        // POSIX family share `init.bash` — they all accept the same
        // `export KEY="…"` syntax.
        assert_eq!(Shell::Bash.default_init_filename(), "init.bash");
        assert_eq!(Shell::Ash.default_init_filename(), "init.bash");
        assert_eq!(Shell::Ksh.default_init_filename(), "init.bash");
        assert_eq!(Shell::Dash.default_init_filename(), "init.bash");
        // Distinct shells get distinct basenames.
        assert_eq!(Shell::Zsh.default_init_filename(), "init.zsh");
        assert_eq!(Shell::Fish.default_init_filename(), "init.fish");
        assert_eq!(Shell::Nushell.default_init_filename(), "init.nu");
        assert_eq!(Shell::PowerShell.default_init_filename(), "init.ps1");
        assert_eq!(Shell::Batch.default_init_filename(), "init.bat");
        assert_eq!(Shell::Elvish.default_init_filename(), "init.elv");
    }

    #[test]
    fn default_init_filename_distinct_across_shell_families() {
        // Confirm the non-POSIX shells get distinct basenames so the
        // generated file's syntax matches the shell that will source it.
        // Skip the POSIX family (Ash/Ksh/Dash/Bash) which intentionally
        // share `init.bash`.
        let distinct_families = [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Nushell,
            Shell::PowerShell,
            Shell::Batch,
            Shell::Elvish,
        ];
        let mut names: Vec<&'static str> = distinct_families.iter().map(|s| s.default_init_filename()).collect();
        names.sort_unstable();
        let len_before = names.len();
        names.dedup();
        assert_eq!(
            names.len(),
            len_before,
            "non-POSIX shells must yield distinct basenames; got {names:?}"
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
        // `_OCX_APPLIED` is itself such a key — it must pass.
        let line = Shell::Bash
            .export_constant("_OCX_APPLIED", "v1:abc")
            .expect("underscore-prefixed key accepted");
        assert!(line.contains("_OCX_APPLIED"));
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
}
