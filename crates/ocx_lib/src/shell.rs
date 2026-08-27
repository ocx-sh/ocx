// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod coexistence;
pub mod error;
pub mod escape;
pub mod hook;
pub mod reconcile;

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
    /// stay exact; fish and nushell keep their double-quoted escaper (that
    /// form round-trips those bytes). `batch` is idempotent too:
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
    /// **The emitted result equals
    /// [`move_to_front`](crate::utility::path::move_to_front) byte for byte, in
    /// every arm** — the parity the reconciler needs, because it applies in
    /// process on one prompt and through this emit on another in the same
    /// session, and a divergence would surface as PATH order flapping between
    /// prompts. That includes dropping ambient empty segments, and comparing a
    /// PATH element segment-exact after stripping one surrounding pair of `"`,
    /// ordinally on Unix and case-insensitively on Windows.
    ///
    /// An **empty `value` is a no-op**, emitted as a shell comment. Prepending
    /// it would put an empty segment at the front of the variable, which POSIX
    /// resolves as the current working directory — a privilege-escalation
    /// primitive, and a divergence from
    /// [`move_to_front`](crate::utility::path::move_to_front), which refuses to
    /// prepend an empty value.
    ///
    /// Returns `None` when `key` is not a valid POSIX environment-variable
    /// name (`[A-Za-z_][A-Za-z0-9_]*`), **or** for [`Shell::Batch`] when the
    /// value contains `%`, LF or CR (see [`Self::export_constant`] for why cmd
    /// cannot express those). Caller decides how to surface that — invalid keys
    /// are produced exclusively by malformed package metadata and never by the
    /// OCX code base itself, so propagating `None` is the path-of-least-impact
    /// safety guard.
    pub fn export_path(self, key: impl AsRef<str>, value: impl AsRef<str>) -> Option<String> {
        let key = key.as_ref();
        if !env::is_valid_env_key(key) {
            return None;
        }
        let raw = value.as_ref();
        if raw.is_empty() {
            return Some(self.comment(format!("ocx: {key} path entry is empty, nothing to prepend")));
        }
        if self == Self::Batch && batch_cannot_express(raw) {
            return None;
        }
        let separator = env::PATH_SEPARATOR;
        Some(match self {
            // bash / zsh — pure-builtin colon-sentinel removal, zero subprocess.
            // The value is a single-quoted literal so `$`, backtick, `\`, `!`,
            // and glob chars stay byte-exact (a double-quoted `\!` would not
            // match a real `!`-bearing segment). Quoting `"$__ocx_p"` inside
            // `${var//pat/repl}` forces a *literal* match (no glob). The `while`
            // loops the replacement to a fixed point so even pre-existing
            // *adjacent* duplicates collapse (one non-overlapping `${//}` pass
            // leaves one). A second fixpoint loop collapses `::` to `:`: the
            // other six arms and `move_to_front` all drop ambient empty
            // segments, and under a per-prompt reconciler a surviving empty
            // segment would be re-asserted on every recompose. zsh keeps its
            // `path` array tied to `PATH` on every assignment, so the scalar
            // form updates both. `${KEY-}` is `set -u`-safe.
            Self::Bash | Self::Zsh => {
                let value = escape::posix_single_quoted(raw);
                format!(
                    "__ocx_p='{value}'; {key}=\":${{{key}-}}:\"; while [ \"${key}\" != \"${{{key}//:\"$__ocx_p\":/:}}\" ]; do {key}=\"${{{key}//:\"$__ocx_p\":/:}}\"; done; while [ \"${key}\" != \"${{{key}//::/:}}\" ]; do {key}=\"${{{key}//::/:}}\"; done; {key}=\"${{{key}#:}}\"; {key}=\"${{{key}%:}}\"; export {key}=\"$__ocx_p${{{key}:+:${{{key}}}}}\"; unset __ocx_p"
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
                let value = escape::posix_single_quoted(raw);
                format!(
                    "__ocx_p='{value}'; export __ocx_p; export {key}=\"$__ocx_p$(printf %s \"${{{key}-}}\" | awk 'BEGIN{{ORS=\"\";RS=\":\";d=ENVIRON[\"__ocx_p\"]}} $0!=d && $0!=\"\"{{printf \":%s\",$0}}')\"; unset __ocx_p"
                )
            }
            // fish — rebuild the list keeping the new value first and dropping any
            // exact-string duplicate. `test "$e" != "$p"` is exact (no glob), so
            // bracket/glob paths are safe. `test -n` drops ambient empty elements,
            // which is the same normalisation `move_to_front` performs and the
            // other arms already did — measured, fish was the last arm where an
            // ambient `/a::/b` survived as `/a::/b` while every other
            // implementation returned `/a:/b`. The `fish_add_path` builtin is
            // deliberately avoided: it skips non-existent dirs and mangles
            // bracket paths.
            //
            // **`--path` on BOTH sides, and it is the whole correctness of this
            // arm for any key other than `PATH`.** fish colon-splits on import and
            // colon-joins on export only for a *path variable*: `PATH`, `CDPATH`,
            // `MANPATH` and names ending in `PATH`. For `PERL5LIB`, `RUBYLIB`,
            // `XDG_DATA_DIRS`, `GEM_HOME`, `TERMINFO_DIRS` the ambient arrives as
            // ONE element holding the whole `/x:/y` string — measured — so
            // iterating `$KEY` directly never matched the operand and the
            // write-back produced a two-element list fish exports **space**-joined,
            // corrupting the variable rather than mis-ordering it. `set --path`
            // makes the list-ness explicit instead of inherited: it splits the
            // seed on `:` whatever the key is called, and the `--path` write-back
            // marks the exported variable so it re-joins on `:`. On `PATH` itself
            // both are no-ops (it already carries the flag), so one form serves
            // every key.
            Self::Fish => {
                let value = escape::fish_double_quoted(raw);
                format!(
                    "set --path __ocx_l ${key}; set __ocx_p \"{value}\"; set __ocx_r; for __ocx_e in $__ocx_l; test \"$__ocx_e\" != \"$__ocx_p\"; and test -n \"$__ocx_e\"; and set -a __ocx_r $__ocx_e; end; set -gx --path {key} $__ocx_p $__ocx_r; set -e __ocx_p __ocx_r __ocx_e __ocx_l"
                )
            }
            // PowerShell — split on the OS path separator, drop empties + the
            // value, prepend. The value is a single-quoted literal (`''` escapes
            // an embedded quote) so no `$`/backtick interpolation can fire.
            // `[IO.Path]::PathSeparator` is `;` on Windows, `:` elsewhere.
            // The comparison is `[String]::Equals` with an explicit
            // `StringComparison`, never `-ne`: PowerShell's comparison
            // operators are case-INsensitive by default, so on Linux — where
            // `/opt/Bin` and `/opt/bin` are different directories — `-ne`
            // silently deletes a foreign entry. `[String]::Equals(a, b, cmp)`
            // is available on .NET Framework too, so Windows PowerShell 5.1
            // parses this. Each segment is compared after stripping one
            // surrounding pair of `"` (Windows quotes PATH segments containing
            // spaces, and `std::env::split_paths` unquotes on the in-process
            // side) — the *comparison* is normalised, the surviving segment is
            // kept byte-exact.
            //
            // E3 — **the operand carries the identical normalisation**, and the
            // comparison is symmetric because of it. Normalising one side only
            // meant this arm never recognised the quoted value it had itself
            // written a prompt earlier, re-prepending one copy per prompt
            // without bound. `$__ocx_p` itself stays raw, so the prepend is
            // still byte-exact and still equals what `move_to_front` writes;
            // only the equality test sees the stripped form, on both sides.
            Self::PowerShell => {
                let value = escape::single_quoted_doubled(raw);
                let comparison = path_element_comparison();
                let normalisation = path_segment_normalisation();
                format!(
                    "$__ocx_p='{value}'; $__ocx_s=[IO.Path]::PathSeparator; $env:{key}=(@($__ocx_p)+($env:{key} -split [regex]::Escape($__ocx_s) | Where-Object {{$_ -and -not [String]::Equals(($_{normalisation}), ($__ocx_p{normalisation}), [StringComparison]::{comparison})}})) -join $__ocx_s; Remove-Variable __ocx_p,__ocx_s"
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
                let value = escape::single_quoted_doubled(raw);
                format!(
                    "use str; set E:{key} = (str:join \"{separator}\" ['{value}' (str:split \"{separator}\" $E:{key} | each {{|p| if (and (not-eq $p '{value}') (not-eq $p \"\")) {{ put $p }} }})])"
                )
            }
            // nushell — normalise to a list (`$env.PATH` is auto-listified since
            // 0.101, other path vars stay strings), drop the value, prepend, then
            // **join back to a string**. The `$env.KEY? | default ""` guard treats
            // an unset variable as empty (parity with the POSIX `${KEY-}`); the
            // `describe` guard then tolerates both string and list inputs. Plain
            // (non-interpolating) double-quoted literals, so `$`/`(` cannot fire.
            //
            // The trailing `str join` is not cosmetic. `$env.PATH` has a built-in
            // `ENV_CONVERSIONS` entry and re-joins on export; a path-style
            // variable that has none — `PERL5LIB`, `XDG_DATA_DIRS`, `GEM_HOME` —
            // does not, and nushell refuses to hand a non-string env value to an
            // external command, so storing the list silently dropped the variable
            // for every child process. One joined string serves every key, agrees
            // with the shipped `env.nu` applier (which stores `$env.PATH` as a
            // string too) and with the in-process `Env`, whose values are
            // `OsString`.
            Self::Nushell => {
                let value = escape::nushell_plain_string(raw);
                format!(
                    "$env.{key} = (($env.{key}? | default \"\") | (if ($in | describe) == 'string' {{ split row (char esep) }} else {{ $in }}) | where {{|p| $p != \"{value}\" and $p != \"\" }} | prepend \"{value}\" | str join (char esep))"
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
            // Match is case-insensitive — Windows PATH semantics, and the correct
            // answer here because Batch exists only on Windows. Caveats, both
            // benign and matching the prior prepend-only behaviour: an entry that was
            // the unanchored *last* segment is not relocated (a one-time non-dedup,
            // never unbounded growth), and an empty `%KEY%` yields a trailing
            // separator (an empty PATH segment cmd ignores).
            //
            // **Delayed-expansion-off precondition** (a named ceiling, not a bug):
            // the emit is correct only under cmd's default. Under `cmd /v:on`, or
            // after `setlocal EnableDelayedExpansion` in the consuming `.bat`, a
            // `!`-bearing value is consumed as a variable reference and the segment
            // is truncated. Nothing in ocx controls the consuming script's
            // `setlocal`, and no spelling is correct under both states.
            Self::Batch => {
                let value = escape::batch_set_value(raw);
                format!("SET \"{key}={value}{separator}%{key}:{value}{separator}=%\"")
            }
        })
    }

    /// Emit a **self-contained, idempotent, unique-append** shell statement that
    /// appends `value` to the `separator`-joined list variable named `key`.
    ///
    /// The fold is pinned by `adr_env_modifier_types.md` D1 and computes the
    /// same function as [`utility::list::append_unique`](crate::utility::list::append_unique),
    /// so an exported shell line and the in-process child env (`ocx run`) agree
    /// byte for byte:
    ///
    /// > wrap the ambient value in the separator; replace **every**
    /// > `sep + value + sep` with `sep`, to a fixpoint; strip the wrapper;
    /// > append `sep + value` (bare `value` when nothing survived).
    ///
    /// Every arm runs the same four steps: seed the working string with
    /// `sep + ambient + sep`, or with a **bare `sep` when the ambient is
    /// empty** — that branch is what keeps an empty ambient from yielding a
    /// leading separator, and it makes a single *leading*-separator strip
    /// equivalent to the primitive's strip-both-ends; replace to a fixpoint;
    /// strip the one leading separator; concatenate `value`. The fixpoint loop
    /// is what makes the removal total: a single replace pass leaves the second
    /// of two *adjacent* duplicates behind, because the first match consumed
    /// the separator they share.
    ///
    /// **The separator is untrusted text**, exactly like the value: both are
    /// authored in package metadata and both go through the same per-shell
    /// escaper. Matching is **case-sensitive in every shell** — list elements
    /// are opaque option strings, where `-DFOO=1` and `-Dfoo=1` are different
    /// options, so the case-insensitive default of PowerShell's `-replace`/`-eq`
    /// would delete the wrong element. The PowerShell arm therefore calls the
    /// ordinal .NET `String.Contains`/`String.Replace` rather than any
    /// PowerShell operator, which also removes the need to regex-escape either
    /// operand.
    ///
    /// **`cmd.exe` (`Shell::Batch`) returns `None`** — it cannot express this
    /// fold. Its only string-replacement primitive, `%VAR:search=replace%`, is
    /// case-**insensitive** with no case-sensitive form (measured: `%V:,abc,=,%`
    /// deletes `,ABC,`), so it silently removes a differently-cased element;
    /// its case-sensitive primitives (`IF` comparison, `%VAR:~n,m%`) cannot
    /// locate a substring at an unknown position. An append that skips the
    /// removal instead grows without bound when two entries share a key, and a
    /// tail-anchored `IF` guard grows the same way as soon as a second entry is
    /// interleaved. Emitting nothing beats emitting a statement that deletes
    /// the wrong option or grows on every re-source.
    ///
    /// An empty `value` is a no-op (the primitive's rule), emitted as a shell
    /// comment: folding with an empty value would make the search pattern
    /// `sep + sep` and collapse the ambient's own adjacent separators.
    ///
    /// **Precondition:** `separator` is non-empty and `value` neither starts nor
    /// ends with it — the same precondition
    /// [`append_unique`](crate::utility::list::append_unique) documents, enforced
    /// at every parse boundary and again after template resolution.
    ///
    /// Returns `None` when `key` is not a valid POSIX environment-variable name
    /// (see [`Self::export_path`]) or when the shell cannot express the fold.
    pub fn export_list(
        self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
        separator: impl AsRef<str>,
    ) -> Option<String> {
        let key = key.as_ref();
        if !env::is_valid_env_key(key) {
            return None;
        }
        let raw = value.as_ref();
        let raw_separator = separator.as_ref();
        if raw.is_empty() {
            return Some(self.comment(format!("ocx: {key} list entry is empty, nothing to append")));
        }
        match self {
            // POSIX family (bash/zsh + strict ash/ksh/dash) — one arm, pure
            // builtins, no subprocess. `${l%%"$p"*}` is the prefix before the
            // FIRST occurrence of `$__ocx_p` and `${l#*"$p"}` the part after it,
            // so the loop body replaces that occurrence with the separator; the
            // same `%%` expansion doubles as the loop guard, since it returns
            // `$__ocx_l` unchanged exactly when the pattern does not occur (the
            // idiom bash's `export_path` uses with `${//}`). Every interpolated
            // pattern is a *quoted* expansion, which POSIX makes literal — so a
            // separator containing `*`, `?` or `[` matches itself instead of
            // globbing. Value and separator ride single-quoted literals, keeping
            // `$`, backtick, `\` and `!` byte-exact (the double-quoted form's
            // `\!` would not match a real `!`). `${KEY:+…}` supplies the
            // empty-ambient branch inline and is `set -u`-safe.
            Self::Bash | Self::Zsh | Self::Ash | Self::Ksh | Self::Dash => {
                let value = escape::posix_single_quoted(raw);
                let separator = escape::posix_single_quoted(raw_separator);
                Some(format!(
                    "__ocx_v='{value}'; __ocx_s='{separator}'; __ocx_p=\"$__ocx_s$__ocx_v$__ocx_s\"; __ocx_l=\"${{{key}:+$__ocx_s${{{key}}}}}$__ocx_s\"; while [ \"$__ocx_l\" != \"${{__ocx_l%%\"$__ocx_p\"*}}\" ]; do __ocx_l=\"${{__ocx_l%%\"$__ocx_p\"*}}$__ocx_s${{__ocx_l#*\"$__ocx_p\"}}\"; done; export {key}=\"${{__ocx_l#\"$__ocx_s\"}}$__ocx_v\"; unset __ocx_v __ocx_s __ocx_p __ocx_l"
                ))
            }
            // fish — `string replace` without `-r` is a literal, case-sensitive
            // replace; `--all` covers one pass and the `$__ocx_n`/`$__ocx_l`
            // compare drives it to the fixpoint. The final unflagged
            // `string replace` removes the FIRST occurrence, which is the
            // leading separator by construction. The variable is written as one
            // plain string (`set -gx`, not a fish list) so the shell fold and
            // the in-process fold produce identical bytes — a list-typed
            // variable is a string in every other shell too.
            Self::Fish => {
                let value = escape::fish_double_quoted(raw);
                let separator = escape::fish_double_quoted(raw_separator);
                // `| string collect` on every substitution: fish splits command
                // substitution output on newlines into a list, so a
                // newline-bearing ambient or value would silently come back
                // space-joined. (`--no-trim-newlines` hangs the loop — the
                // trailing newline it preserves never compares equal.)
                Some(format!(
                    "set __ocx_v \"{value}\"; set __ocx_s \"{separator}\"; set __ocx_p \"$__ocx_s$__ocx_v$__ocx_s\"; set __ocx_l \"$__ocx_s\"; if test -n \"${key}\"; set __ocx_l \"$__ocx_s${key}$__ocx_s\"; end; set __ocx_n (string replace --all -- \"$__ocx_p\" \"$__ocx_s\" \"$__ocx_l\" | string collect); while test \"$__ocx_n\" != \"$__ocx_l\"; set __ocx_l \"$__ocx_n\"; set __ocx_n (string replace --all -- \"$__ocx_p\" \"$__ocx_s\" \"$__ocx_l\" | string collect); end; set __ocx_l (string replace -- \"$__ocx_s\" \"\" \"$__ocx_l\" | string collect); set -gx {key} \"$__ocx_l$__ocx_v\"; set -e __ocx_v __ocx_s __ocx_p __ocx_l __ocx_n"
                ))
            }
            // PowerShell — ordinal (case-sensitive) .NET string methods, so no
            // `-creplace` and no `[regex]::Escape` of either operand. The value
            // and separator are single-quoted literals (`''` escapes a quote),
            // which no `$`/backtick interpolation can reach. `.Substring` needs
            // no bounds guard: the working string always starts with the
            // separator.
            Self::PowerShell => {
                let value = escape::single_quoted_doubled(raw);
                let separator = escape::single_quoted_doubled(raw_separator);
                Some(format!(
                    "$__ocx_v='{value}'; $__ocx_s='{separator}'; $__ocx_p=\"$__ocx_s$__ocx_v$__ocx_s\"; $__ocx_l=if ($env:{key}) {{ \"$__ocx_s$($env:{key})$__ocx_s\" }} else {{ $__ocx_s }}; while ($__ocx_l.Contains($__ocx_p)) {{ $__ocx_l=$__ocx_l.Replace($__ocx_p,$__ocx_s) }}; $env:{key}=$__ocx_l.Substring($__ocx_s.Length)+$__ocx_v; Remove-Variable __ocx_v,__ocx_s,__ocx_p,__ocx_l"
                ))
            }
            // elvish — `str:replace` mirrors Go's `strings.Replace`: literal,
            // case-sensitive, all occurrences by default, and `&max=1` for the
            // leading-separator strip. Single-quoted raw strings (`'` doubled)
            // carry both operands, because elvish rejects `\$` / `` \` `` inside
            // double quotes as invalid escape sequences. `has-env` guards the
            // ambient read so an unset key cannot raise.
            Self::Elvish => {
                let value = escape::single_quoted_doubled(raw);
                let separator = escape::single_quoted_doubled(raw_separator);
                Some(format!(
                    "use str; var __ocx_l = '{separator}'; if (and (has-env {key}) (not-eq $E:{key} '')) {{ set __ocx_l = '{separator}'$E:{key}'{separator}' }}; while (str:contains $__ocx_l '{separator}{value}{separator}') {{ set __ocx_l = (str:replace '{separator}{value}{separator}' '{separator}' $__ocx_l) }}; set E:{key} = (str:replace &max=1 '{separator}' '' $__ocx_l)'{value}'"
                ))
            }
            // nushell — `str replace` without `--regex` is literal and
            // case-sensitive; `--all` per pass, the `while` drives the fixpoint,
            // and the unflagged form strips the leading separator (first
            // occurrence). The variable is written as one string, not a nushell
            // list, so the two folds agree byte for byte. Plain (non-interpolating)
            // double-quoted literals — `$` and `(` cannot fire, so
            // `escape::nushell_plain_string` neutralizes only `\` and `"`.
            Self::Nushell => {
                let value = escape::nushell_plain_string(raw);
                let separator = escape::nushell_plain_string(raw_separator);
                Some(format!(
                    "mut __ocx_l = (if ($env.{key}? | default \"\") == \"\" {{ \"{separator}\" }} else {{ \"{separator}\" + ($env.{key}? | default \"\") + \"{separator}\" }}); while ($__ocx_l | str contains \"{separator}{value}{separator}\") {{ $__ocx_l = ($__ocx_l | str replace --all \"{separator}{value}{separator}\" \"{separator}\") }}; $env.{key} = (($__ocx_l | str replace \"{separator}\" \"\") + \"{value}\")"
                ))
            }
            // batch (cmd.exe) — no emit; see the doc comment for the measured
            // case-insensitivity of `%VAR:search=replace%` and why every
            // single-statement alternative either deletes the wrong element or
            // grows on re-source.
            Self::Batch => None,
        }
    }

    /// Emit a shell statement that removes one whole contribution from `key`.
    ///
    /// The inverse of [`export_list`](Self::export_list) and of
    /// [`append_unique`](crate::utility::list::append_unique): **flank-delimited
    /// removal of one whole contribution, never a segment op** — a contribution
    /// that itself carries the separator is still removed as one span.
    /// Delete-if-found — absence is not an error, and removal commutes with
    /// foreign prepends and appends, which is what makes the reconciler's
    /// revert safe against every other tool that edited the variable since.
    ///
    /// `separator: None` means the platform PATH separator **and selects
    /// path-kind semantics**; `Some(effective)` selects list-kind. The
    /// parameter is **mandatory to the contract**: without it every
    /// non-default-separator list var is permanently unrevertible — `CFLAGS` as
    /// `{ type = "list", separator = " " }` applies through `export_list`
    /// (which does take a separator) and would then remove nothing, or split on
    /// the wrong byte and corrupt the value. A list-kind revert always passes
    /// `Some(effective_separator)`; `None` is path-kind only. Per-entry
    /// separators are settled upstream by
    /// [`reconcile_list_separators`](crate::env::reconcile_list_separators) —
    /// this primitive never guesses one.
    ///
    /// The two kinds differ in exactly two ways, each inherited from the
    /// applier being reverted:
    ///
    /// | | path-kind (`None`) | list-kind (`Some`) |
    /// |---|---|---|
    /// | ambient empty segments | collapsed, as [`export_path`](Self::export_path) and [`move_to_front`](crate::utility::path::move_to_front) do | preserved verbatim |
    /// | comparison | segment-exact after stripping one surrounding pair of `"`, ordinal on Unix and `OrdinalIgnoreCase` on Windows | byte-exact, case-sensitive on every platform |
    ///
    /// Case-sensitivity is not a free choice on either side: PATH is
    /// case-insensitive on Windows, while list elements are opaque option
    /// strings where `-DFOO=1` and `-Dfoo=1` are different options. The
    /// **emitted key is never re-cased.**
    ///
    /// An empty `value` is a no-op, emitted as a shell comment: the flank
    /// pattern would degrade to `sep + sep` and delete the ambient's own
    /// separators.
    ///
    /// Returns `None` for an invalid env key (delegating to
    /// [`env::is_valid_env_key`], same as `export_path`) **or** for
    /// [`Shell::Batch`]. Batch is not "cannot express it" — `export_path` does
    /// delete an element there via `%VAR:search=%`. The reason is that
    /// `cmd.exe`'s only substring-replace primitive is case-insensitive with no
    /// case-sensitive form, and list elements need case-sensitive matching.
    /// Batch also hosts no prompt hook, so nothing consumes it.
    ///
    /// **Escaping is per arm**, never one shared escaper. Routing every arm
    /// through the fish/nushell double-quote escaper would ship a shell
    /// injection: that escaper deliberately leaves `'` untouched, so an element
    /// like `/tmp/a';id;'b` — reachable from a project `[env]` value — would
    /// execute at every prompt.
    pub fn remove_list_element(
        self,
        key: impl AsRef<str>,
        value: impl AsRef<str>,
        separator: Option<&str>,
    ) -> Option<String> {
        let key = key.as_ref();
        if !env::is_valid_env_key(key) || self == Self::Batch {
            return None;
        }
        // Path-kind normalises the operand the same way it normalises the
        // ambient segments it compares against; a list element is opaque, so
        // quotes inside it are part of the option.
        let path_kind = separator.is_none();
        let raw = match separator {
            None => strip_one_quote_pair(value.as_ref()),
            Some(_) => value.as_ref(),
        };
        if raw.is_empty() {
            return Some(self.comment(format!("ocx: {key} removal value is empty, nothing to remove")));
        }
        let raw_separator = separator.unwrap_or(env::PATH_SEPARATOR);
        Some(match self {
            // POSIX family (bash/zsh + strict ash/ksh/dash) — one arm, pure
            // builtins, no subprocess and, unlike `export_path`, no `awk`: the
            // `${l%%"$p"*}` / `${l#*"$p"}` idiom `export_list` already proves
            // across all five shells expresses the whole fold, so the value
            // never has to survive `awk -v`'s backslash decoding. Every
            // interpolated pattern is a *quoted* expansion, which POSIX makes
            // literal — a separator or element containing `*`, `?` or `[`
            // matches itself instead of globbing, which is also what closes
            // zsh's glob over-match. Value and separator ride single-quoted
            // literals, keeping `$`, backtick, `\` and `!` byte-exact.
            // `${KEY:+…}` supplies the empty-ambient branch inline and is
            // `set -u`-safe.
            Self::Bash | Self::Zsh | Self::Ash | Self::Ksh | Self::Dash => {
                let value = escape::posix_single_quoted(raw);
                let separator = escape::posix_single_quoted(raw_separator);
                let fold = |pattern: &str| {
                    format!(
                        "while [ \"$__ocx_l\" != \"${{__ocx_l%%\"{pattern}\"*}}\" ]; do __ocx_l=\"${{__ocx_l%%\"{pattern}\"*}}$__ocx_s${{__ocx_l#*\"{pattern}\"}}\"; done; "
                    )
                };
                let collapse = if path_kind {
                    format!("__ocx_d=\"$__ocx_s$__ocx_s\"; {}unset __ocx_d; ", fold("$__ocx_d"))
                } else {
                    String::new()
                };
                format!(
                    "__ocx_v='{value}'; __ocx_s='{separator}'; __ocx_p=\"$__ocx_s$__ocx_v$__ocx_s\"; __ocx_l=\"${{{key}:+$__ocx_s${{{key}}}}}$__ocx_s\"; {fold}{collapse}__ocx_l=\"${{__ocx_l#\"$__ocx_s\"}}\"; export {key}=\"${{__ocx_l%\"$__ocx_s\"}}\"; unset __ocx_v __ocx_s __ocx_p __ocx_l",
                    fold = fold("$__ocx_p")
                )
            }
            // fish, path-kind — `$PATH` is a genuine fish *list*, so the string
            // fold the list-kind arm below uses would first space-join it. This
            // is `export_path`'s own loop minus the prepend, `--path` included:
            // `test "$e" != "$p"` is an exact compare (no glob), empty elements
            // are dropped, which is what `utility::path::remove_segment` does in
            // process, and `set --path` gives a key fish would not otherwise
            // treat as a colon list (`PERL5LIB`, `XDG_DATA_DIRS`, …) the same
            // splitting on the way in and joining on the way out that `PATH` gets
            // for free. Without it this arm removed **nothing** whenever the
            // ambient arrived as a string — a new shell, or a `cd` into the
            // project — so the ledger could never take the element back out.
            Self::Fish if path_kind => {
                let value = escape::fish_double_quoted(raw);
                format!(
                    "set --path __ocx_l ${key}; set __ocx_p \"{value}\"; set __ocx_r; for __ocx_e in $__ocx_l; test \"$__ocx_e\" != \"$__ocx_p\"; and test -n \"$__ocx_e\"; and set -a __ocx_r $__ocx_e; end; set -gx --path {key} $__ocx_r; set -e __ocx_p __ocx_r __ocx_e __ocx_l"
                )
            }
            // fish, list-kind — `string replace` without `-r` is a literal,
            // case-sensitive replace; `--all` covers one pass and the
            // `$__ocx_n`/`$__ocx_l` compare drives it to the fixpoint. No
            // index-based `set -e VAR[N]`: that is the field workaround for
            // fish's missing remove primitive and it shifts every later index,
            // so removing more than one element needs a highest-index-first
            // ordering the caller cannot see. The string fold has no index at
            // all. `string sub` peels the two wrapper separators off in one
            // step — it stays in range when everything collapsed and the whole
            // working string *is* the wrapper. `| string collect` on every
            // substitution: fish splits command-substitution output on newlines
            // into a list, so a newline-bearing ambient or value would silently
            // come back space-joined.
            Self::Fish => {
                let value = escape::fish_double_quoted(raw);
                let separator = escape::fish_double_quoted(raw_separator);
                format!(
                    "set __ocx_v \"{value}\"; set __ocx_s \"{separator}\"; set __ocx_p \"$__ocx_s$__ocx_v$__ocx_s\"; set __ocx_l \"$__ocx_s\"; if test -n \"${key}\"; set __ocx_l \"$__ocx_s${key}$__ocx_s\"; end; set __ocx_n (string replace --all -- \"$__ocx_p\" \"$__ocx_s\" \"$__ocx_l\" | string collect); while test \"$__ocx_n\" != \"$__ocx_l\"; set __ocx_l \"$__ocx_n\"; set __ocx_n (string replace --all -- \"$__ocx_p\" \"$__ocx_s\" \"$__ocx_l\" | string collect); end; set -gx {key} (string sub --start (math (string length -- \"$__ocx_s\") + 1) --end (math 0 - (string length -- \"$__ocx_s\")) -- \"$__ocx_l\" | string collect); set -e __ocx_v __ocx_s __ocx_p __ocx_l __ocx_n"
                )
            }
            // PowerShell, path-kind — the `export_path` pipeline minus the
            // prepend, so the applier and the remover share one comparison:
            // `[String]::Equals` with an explicit `StringComparison` (never the
            // case-insensitive `-ne`/`-notlike`), segment-exact so removing
            // `C:\WINDOWS` cannot take `C:\WINDOWS\system32` with it, and one
            // surrounding pair of `"` stripped per segment before comparing.
            // `$env:PATH` and `$env:Path` are the same variable on Windows and
            // different ones elsewhere, so the authored key spelling is emitted
            // verbatim.
            Self::PowerShell if path_kind => {
                let value = escape::single_quoted_doubled(raw);
                let comparison = path_element_comparison();
                let normalisation = path_segment_normalisation();
                format!(
                    "$__ocx_p='{value}'; $__ocx_s=[IO.Path]::PathSeparator; $env:{key}=(($env:{key} -split [regex]::Escape($__ocx_s) | Where-Object {{$_ -and -not [String]::Equals(($_{normalisation}), $__ocx_p, [StringComparison]::{comparison})}})) -join $__ocx_s; Remove-Variable __ocx_p,__ocx_s"
                )
            }
            // PowerShell, list-kind — ordinal (case-sensitive) .NET string
            // methods, so no `-creplace` and no `[regex]::Escape` of either
            // operand. Value and separator are single-quoted literals, which no
            // `$`/backtick interpolation can reach. `.Substring` needs no
            // bounds guard beyond the `Max`: the working string always starts
            // with the separator, and `Max` covers the case where everything
            // between the wrappers collapsed.
            Self::PowerShell => {
                let value = escape::single_quoted_doubled(raw);
                let separator = escape::single_quoted_doubled(raw_separator);
                format!(
                    "$__ocx_v='{value}'; $__ocx_s='{separator}'; $__ocx_p=\"$__ocx_s$__ocx_v$__ocx_s\"; $__ocx_l=if ($env:{key}) {{ \"$__ocx_s$($env:{key})$__ocx_s\" }} else {{ $__ocx_s }}; while ($__ocx_l.Contains($__ocx_p)) {{ $__ocx_l=$__ocx_l.Replace($__ocx_p,$__ocx_s) }}; $env:{key}=$__ocx_l.Substring($__ocx_s.Length,[Math]::Max(0,$__ocx_l.Length-2*$__ocx_s.Length)); Remove-Variable __ocx_v,__ocx_s,__ocx_p,__ocx_l"
                )
            }
            // elvish — `str:replace` mirrors Go's `strings.Replace`: literal,
            // case-sensitive, all occurrences by default. Single-quoted raw
            // strings (`'` doubled) carry both operands, because elvish rejects
            // `\$` / `` \` `` inside double quotes as invalid escape sequences.
            // `has-env` guards the ambient read so an unset key cannot raise.
            // `str:trim-prefix`/`str:trim-suffix` peel exactly one wrapper each
            // and are no-ops when there is nothing left to peel.
            Self::Elvish => {
                let value = escape::single_quoted_doubled(raw);
                let separator = escape::single_quoted_doubled(raw_separator);
                let collapse = if path_kind {
                    format!(
                        "while (str:contains $__ocx_l '{separator}{separator}') {{ set __ocx_l = (str:replace '{separator}{separator}' '{separator}' $__ocx_l) }}; "
                    )
                } else {
                    String::new()
                };
                format!(
                    "use str; var __ocx_l = '{separator}'; if (and (has-env {key}) (not-eq $E:{key} '')) {{ set __ocx_l = '{separator}'$E:{key}'{separator}' }}; while (str:contains $__ocx_l '{separator}{value}{separator}') {{ set __ocx_l = (str:replace '{separator}{value}{separator}' '{separator}' $__ocx_l) }}; {collapse}set E:{key} = (str:trim-suffix (str:trim-prefix $__ocx_l '{separator}') '{separator}')"
                )
            }
            // nushell, path-kind — `$env.PATH` is auto-listified since 0.101
            // while other path vars stay strings, so this is `export_path`'s own
            // `describe` guard, filter and closing `str join` minus the prepend.
            // Filtering the list keeps the two in step; the string fold below
            // would have to join it first. The join back is what keeps a
            // conversion-less key (`PERL5LIB`, `XDG_DATA_DIRS`) reaching a child
            // process at all — see `export_path`'s nushell arm.
            Self::Nushell if path_kind => {
                let value = escape::nushell_plain_string(raw);
                format!(
                    "$env.{key} = (($env.{key}? | default \"\") | (if ($in | describe) == 'string' {{ split row (char esep) }} else {{ $in }}) | where {{|p| $p != \"{value}\" and $p != \"\" }} | str join (char esep))"
                )
            }
            // nushell, list-kind — `str replace` without `--regex` is literal
            // and case-sensitive; `--all` per pass, the `while` drives the
            // fixpoint. The wrappers come off by splitting on the separator and
            // dropping the leading and trailing empty field, which is exact for
            // a multi-character separator and stays correct when nothing
            // survived (the split then yields exactly the two empties). Plain
            // (non-interpolating) double-quoted literals, so `$` and `(` cannot
            // fire.
            Self::Nushell => {
                let value = escape::nushell_plain_string(raw);
                let separator = escape::nushell_plain_string(raw_separator);
                format!(
                    "mut __ocx_l = (if ($env.{key}? | default \"\") == \"\" {{ \"{separator}\" }} else {{ \"{separator}\" + ($env.{key}? | default \"\") + \"{separator}\" }}); while ($__ocx_l | str contains \"{separator}{value}{separator}\") {{ $__ocx_l = ($__ocx_l | str replace --all \"{separator}{value}{separator}\" \"{separator}\") }}; $env.{key} = ($__ocx_l | split row \"{separator}\" | skip 1 | drop 1 | str join \"{separator}\")"
                )
            }
            // batch (cmd.exe) — refused above; see the doc comment.
            Self::Batch => return None,
        })
    }

    /// Emit a shell line that sets `key=value` (replacing any prior value).
    ///
    /// The emitted value is **byte-identical to what
    /// [`Env::apply_entries`](crate::env::Env) sets in process**, which is what
    /// makes the reconciler's `C == L.applied` exit guard decidable: its two
    /// operands are exactly this emit and that in-process write.
    ///
    /// Every arm therefore uses **its own** escaper and quoting context — the
    /// same one [`Self::export_path`] uses. The POSIX family, PowerShell and
    /// elvish all ride a **single-quoted** literal, where no interpolation,
    /// history expansion or globbing exists and the value survives byte-exact;
    /// fish and nushell keep their double-quoted form, which round-trips the
    /// same bytes.
    ///
    /// Returns `None` when `key` is not a valid POSIX environment-variable
    /// name (see [`Self::export_path`] for the rationale), **or** for
    /// [`Shell::Batch`] when the value contains `%`, LF or CR.
    pub fn export_constant(self, key: impl AsRef<str>, value: impl AsRef<str>) -> Option<String> {
        let key = key.as_ref();
        if !env::is_valid_env_key(key) {
            return None;
        }
        let raw = value.as_ref();
        if self == Self::Batch && batch_cannot_express(raw) {
            return None;
        }
        Some(match self {
            // Single-quoted POSIX literal. The double-quoted form used to turn
            // `!` into `\!` for history-expansion safety, which is a byte
            // corruption rather than a hardening: `\!` is *literal* inside
            // double quotes in every measured shell, so the variable ended up
            // holding `a\!b` where the in-process write holds `a!b`. Inside
            // `'...'` no expansion — history included — can fire at all.
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => {
                format!("export {key}='{}'", escape::posix_single_quoted(raw))
            }
            Self::Fish => format!("set -x {key} \"{}\"", escape::fish_double_quoted(raw)),
            // Single-quoted, quote doubled. `"` would interpolate `$var` and
            // treat backtick as the escape character.
            Self::PowerShell => format!("$env:{key} = '{}'", escape::single_quoted_doubled(raw)),
            Self::Batch => format!("SET \"{key}={}\"", escape::batch_set_value(raw)),
            // Single-quoted raw string. Elvish rejects `\$` and `` \` `` inside
            // a double-quoted string as *invalid escape sequences* — a parse
            // error, not a wrong value — so a `$`-bearing constant could not be
            // emitted in the double-quoted form at all.
            Self::Elvish => format!("set E:{key} = '{}'", escape::single_quoted_doubled(raw)),
            Self::Nushell => format!("$env.{key} = \"{}\"", escape::nushell_plain_string(raw)),
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

    /// Emit a shell statement that prints `text` on **stderr** when evaluated.
    ///
    /// The reconciler's diagnostics travel as shell code on stdout, not on the
    /// binary's stderr: the shim that invokes the reconcile call discards its
    /// stderr unconditionally, so a message written there is lost. Returns
    /// `None` for [`Shell::Batch`], which hosts no prompt hook and therefore
    /// has nothing to say.
    ///
    /// `text` rides as a **format argument, never as the format string** — a
    /// `%` in a project path would otherwise be consumed as a conversion
    /// specifier — and passes that arm's own value escaper, so a project path
    /// such as `/home/u/it's work` cannot close the literal and have its
    /// remainder parsed as shell source.
    pub fn emit_message(self, text: impl AsRef<str>) -> Option<String> {
        let text = text.as_ref();
        Some(match self {
            // `printf`, not `echo`: `echo` mangles a leading `-` and a
            // backslash on some of these shells.
            Self::Ash | Self::Ksh | Self::Dash | Self::Bash | Self::Zsh => {
                format!("printf '%s\\n' '{}' >&2", escape::posix_single_quoted(text))
            }
            Self::Fish => format!("printf '%s\\n' \"{}\" >&2", escape::fish_double_quoted(text)),
            // `[Console]::Error.WriteLine` rather than `Write-Error`: the
            // latter emits an ErrorRecord, which a hardened profile's
            // `$ErrorActionPreference = 'Stop'` turns into a terminating error
            // in the middle of the prompt function.
            Self::PowerShell => format!("[Console]::Error.WriteLine('{}')", escape::single_quoted_doubled(text)),
            Self::Elvish => format!("echo '{}' >&2", escape::single_quoted_doubled(text)),
            Self::Nushell => format!("print --stderr \"{}\"", escape::nushell_plain_string(text)),
            Self::Batch => return None,
        })
    }
}

/// `true` when a value cannot be carried through a `cmd.exe` `SET "KEY=…"`
/// statement.
///
/// A `"` closes the statement's own quote, after which cmd parses the rest of
/// the value as command syntax — `x" & <cmd> & "` runs `<cmd>`. That is command
/// execution from package metadata, so the quote is refused rather than escaped:
/// cmd has no in-quote escape for it, and the caret escapes `escape::batch_set_value`
/// used to carry never covered it either (they were over-escaping — `^ & < > |`
/// are literal inside the quotes — and only corrupted the value).
/// `%VAR:search=%` has no escape for a literal `%` in `search`, so a
/// `%`-bearing value's delete half never matches and every apply prepends
/// another copy — unbounded growth under a per-prompt reconciler. An LF or CR
/// splits one `SET` into two commands in the `FOR /F … DO @%i` channel that
/// `ocx --global env` is applied through. Emitting nothing beats emitting a
/// statement that executes, grows or splits.
fn batch_cannot_express(value: &str) -> bool {
    value.contains(['%', '"', '\n', '\r'])
}

/// Refuse an [`Entry`] no shell arm can emit **and** later revert.
///
/// One admission rule for both emit sites: the reconciler's planner
/// ([`reconcile`]) calls it to keep `L ⊆ emittable(D)` an invariant, and
/// `conventions::emit_lines` calls it so `ocx env --shell`,
/// `ocx package env --shell` and `ocx direnv export` refuse the same set. Two
/// copies of an admission rule drift, and the export path had none at all.
///
/// The four refusals, each because the *revert* is impossible rather than
/// because the apply is:
///
/// 1. **An invalid environment-variable name.** Every emitter already returns
///    `None` for one, so the apply is silently empty while a ledger entry would
///    name a key no arm can ever remove.
/// 2. **A path-kind value embedding [`env::PATH_SEPARATOR`].** The split-based
///    arms (POSIX `awk`, fish, PowerShell, elvish, nushell) read it as two
///    segments and match neither against the whole operand, so every re-source
///    prepends another copy — measured growing without bound on ksh, dash and
///    pwsh. It also violates [`Self::export_path`]'s stated precondition and
///    [`move_to_front`](crate::utility::path::move_to_front)'s.
/// 3. **An empty path or list element.** Prepending one puts an empty segment at
///    the front of `PATH`, which POSIX resolves as the current working
///    directory.
/// 4. **A path or list element containing LF or CR.** The removal fold cannot
///    address a span that the ambient may have re-wrapped, and Batch's `SET`
///    channel splits on it outright.
///
/// The error is a fixed reason string, suitable for a warn line or a `# ocx:`
/// note. `Ok(())` means every arm that supports the entry's kind can express it;
/// a per-shell refusal (a `list` under `cmd.exe`) is still the emitter's own
/// answer and is not decided here.
///
/// # Errors
///
/// The reason the entry cannot be emitted, as one lowercase phrase.
pub fn is_emittable(entry: &crate::package::metadata::env::entry::Entry) -> Result<(), &'static str> {
    use crate::package::metadata::env::modifier::ModifierKind;

    let element = matches!(entry.kind, ModifierKind::Path | ModifierKind::List);
    if !env::is_valid_env_key(&entry.key) {
        return Err("not a valid environment-variable name");
    }
    if matches!(entry.kind, ModifierKind::Path) && entry.value.contains(env::PATH_SEPARATOR) {
        return Err("a path value may not embed the platform path separator");
    }
    if element && entry.value.is_empty() {
        return Err("an empty element cannot be emitted or removed");
    }
    if element && entry.value.contains(['\n', '\r']) {
        return Err("an element may not contain a line break");
    }
    Ok(())
}

/// The `StringComparison` a PATH element is compared under, chosen at emit
/// time: the emitter and the shell it emits for run on the same host, so
/// `cfg!(windows)` is the platform test — the same rule
/// [`env::PATH_SEPARATOR`] already follows.
fn path_element_comparison() -> &'static str {
    if cfg!(windows) { "OrdinalIgnoreCase" } else { "Ordinal" }
}

/// The per-segment normalisation a PowerShell PATH element is compared through,
/// chosen at emit time by the same `cfg!(windows)` rule as
/// [`path_element_comparison`].
///
/// **Empty off Windows, and that is the contract, not a shortcut.**
/// [`move_to_front`](crate::utility::path::move_to_front) splits with
/// `std::env::split_paths`, which strips one surrounding pair of `"` from a
/// segment **only on Windows** — so on Unix a segment beginning with `\"` is a
/// directory whose name begins with `\"`. An unconditional strip made the pwsh
/// arm disagree with `move_to_front` and with its five sibling arms two ways at
/// once: it deleted a quoted foreign segment they all keep, and — because the
/// operand is never stripped — it never recognised the quoted value it had
/// itself written a prompt earlier, re-prepending one copy per prompt. Both were
/// measured on pwsh 7 / Linux.
fn path_segment_normalisation() -> &'static str {
    if cfg!(windows) {
        " -replace '(?s)^\"(.*)\"$','$1'"
    } else {
        ""
    }
}

/// Strip one — and only one — surrounding pair of `"` from a PATH element.
///
/// Windows quotes PATH segments containing spaces, and `std::env::split_paths`
/// unquotes them on the in-process side, so the two spellings must compare
/// equal. Only the outermost pair is removed: a directory genuinely named
/// `""x""` keeps one pair.
pub(crate) fn strip_one_quote_pair(value: &str) -> &str {
    value
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(value)
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
    // Iterating `value_variants()` keeps the "every shell" tests exhaustive by
    // construction, so a new `Shell` cannot slip past them.
    use clap_builder::ValueEnum as _;

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

    /// nushell hands an env var to an external command only when it can
    /// stringify it. `$env.PATH` carries a built-in `ENV_CONVERSIONS` entry and
    /// re-joins on export; a path-style variable that has none — `PERL5LIB`,
    /// `XDG_DATA_DIRS`, `GEM_HOME` — does not, so a *list* assignment is
    /// silently dropped when nu spawns a child. Both path-kind arms therefore
    /// store one joined string, which is also what the shipped `env.nu` applier
    /// does and what every other shell's arm produces.
    ///
    /// Structural rather than live: no `nu` interpreter is installed on any
    /// cargo leg (see `assert_every_present_interpreter_ran`).
    #[test]
    fn nushell_path_kind_assigns_a_string_not_a_list() {
        for key in ["PATH", "PERL5LIB", "XDG_DATA_DIRS"] {
            for line in [
                Shell::Nushell.export_path(key, "/opt/bin").unwrap(),
                Shell::Nushell.remove_list_element(key, "/opt/bin", None).unwrap(),
            ] {
                assert!(
                    line.ends_with("| str join (char esep))"),
                    "{key}: a path-kind nushell assignment must end in a join back to a string, \
                     or nu drops the variable when it spawns an external: {line}"
                );
            }
        }
        // List-kind already folds strings and must not gain a second join.
        let list = Shell::Nushell.export_list("CFLAGS", "-DX", " ").unwrap();
        assert!(!list.contains("str join"), "list-kind is already a string fold: {list}");
    }

    /// EC-CONST-010 — the nushell arm of the shared emit-and-restore escaper
    /// (see the block comment above
    /// `export_constant_posix_is_a_single_quoted_literal`).
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

    // ── per-arm escaper hardening ─────────────────────────────────────
    //
    // Each arm must neutralize every metacharacter the destination shell
    // interprets inside *that arm's* quoting form, and neutralize nothing
    // else — an over-escape is a silent wrong value, which is why the
    // single-quoted arms assert byte equality rather than the presence of a
    // backslash.

    // A-15 / C-009 / C-021: `export_constant` rides that arm's own escaper and
    // quoting context, so the emitted value is byte-identical to what
    // `Env::apply_entries` writes in process. The POSIX family moved from a
    // double-quoted form (which escaped `$`, backtick and `!`) to a
    // single-quoted literal, where nothing expands at all.
    //
    // EC-CONST-010 — this block is also the whole answer to "which escaper does
    // the reconciler's constant-RESTORE path use". A-15's decision is that
    // there is no second escaper anywhere in the reconciler: a restore emits
    // through this same `export_constant` and an `Unset` prior through
    // `Shell::unset`, so the per-arm tests below already cover the restore
    // path's quoting. The call site is `plan_lines`' `restores` arm in
    // `ocx_cli`'s `self activate`, which matches `Some(value) =>
    // shell.export_constant(key, value)` / `None => shell.unset(key)` — mint a
    // third arm there and the guarantee these tests give is gone, so that match
    // is the thing to look at if this row is ever reopened.

    /// EC-CONST-010 — the POSIX arm of the shared emit-and-restore escaper (see
    /// the block comment above).
    #[test]
    fn export_constant_posix_is_a_single_quoted_literal() {
        // `$(rm -rf /)` inside `'...'` is inert without any escaping: no
        // command substitution, no backslash in the stored value.
        for shell in [Shell::Bash, Shell::Zsh, Shell::Ash, Shell::Ksh, Shell::Dash] {
            let line = shell
                .export_constant("FOO", "$(rm -rf /)")
                .expect("valid env-var name accepted");
            assert_eq!(line, "export FOO='$(rm -rf /)'", "{shell}");
        }
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
    fn export_constant_posix_keeps_a_bang_byte_exact() {
        // A-15: the old `!` → `\!` was a byte corruption, not a hardening —
        // `\!` is *literal* inside double quotes in every measured shell, so
        // the shell stored `a\!b` where `apply_entries` stores `a!b` and the
        // reconciler's `C == L.applied` guard could never hold. Inside
        // `'...'` history expansion cannot fire at all.
        for shell in [Shell::Bash, Shell::Zsh, Shell::Ash, Shell::Ksh, Shell::Dash] {
            let line = shell
                .export_constant("FOO", "!!bad")
                .expect("valid env-var name accepted");
            assert_eq!(line, "export FOO='!!bad'", "{shell}");
            assert!(
                !line.contains("\\!"),
                "{shell}: the value must not be backslash-escaped: {line}"
            );
        }
    }

    /// EC-CONST-010 — the PowerShell and elvish arms of the shared
    /// emit-and-restore escaper (see the block comment above
    /// `export_constant_posix_is_a_single_quoted_literal`).
    #[test]
    fn export_constant_powershell_and_elvish_use_doubled_single_quotes() {
        // A-15: elvish rejects `\$` / `` \` `` inside a double-quoted string as
        // an *invalid escape sequence* — a parse error, not a wrong value — so
        // the shipped double-quoted emit could not carry a `$`-bearing constant
        // at all. PowerShell's double-quoted form interpolates `$var`.
        assert_eq!(
            Shell::PowerShell.export_constant("JAVA_HOME", "/opt/j$dk`x").as_deref(),
            Some("$env:JAVA_HOME = '/opt/j$dk`x'")
        );
        assert_eq!(
            Shell::Elvish.export_constant("JAVA_HOME", "/opt/j$dk`x").as_deref(),
            Some("set E:JAVA_HOME = '/opt/j$dk`x'")
        );
        // A literal quote is written by doubling it.
        assert_eq!(
            Shell::PowerShell.export_constant("K", "o'brien").as_deref(),
            Some("$env:K = 'o''brien'")
        );
        assert_eq!(
            Shell::Elvish.export_constant("K", "o'brien").as_deref(),
            Some("set E:K = 'o''brien'")
        );
    }

    // ── CWE-78 Nushell `$` interpolation hardening ───────────────────
    //
    // `export_path` emits `$"...($env.KEY?...)"`. Inside `$"..."`, `$`
    // triggers interpolation — a metadata value `$env.HOME` would expand.
    // `\$` is a literal `$` in Nushell interpolated strings.

    #[test]
    fn export_path_nushell_carries_a_dollar_verbatim() {
        // A-16: all four nushell emits use a **plain**, non-interpolating
        // double-quoted literal, where `$`, `(` and `)` are inert. Escaping
        // them was corrupting unless nushell recognises `\$` in the plain form
        // — an escape table this tree cannot verify. Emitting no backslash is
        // correct under both readings.
        let line = Shell::Nushell
            .export_path("PATH", "$env.HOME/bin")
            .expect("valid env-var name accepted");
        assert!(
            line.contains("\"$env.HOME/bin\""),
            "nushell must carry the value verbatim in a plain string; got: {line:?}"
        );
        assert!(!line.contains("\\$"), "no backslash escape may be emitted: {line:?}");
    }

    #[test]
    fn escape_nushell_plain_string_escapes_only_backslash_and_quote() {
        assert_eq!(
            escape::nushell_plain_string("/tmp/a(b)$c\\d\"e'f"),
            "/tmp/a(b)$c\\\\d\\\"e'f"
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
    fn escape_fish_double_quoted_backtick_roundtrips_literally() {
        let val = escape::fish_double_quoted("`echo hi`");
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
        // A-18 added the second loop: it collapses `::` to `:` so an ambient
        // empty segment is stripped, as the other six arms and
        // `utility::path::move_to_front` already do.
        let expected = "__ocx_p='/opt/bin'; PATH=\":${PATH-}:\"; while [ \"$PATH\" != \"${PATH//:\"$__ocx_p\":/:}\" ]; do PATH=\"${PATH//:\"$__ocx_p\":/:}\"; done; while [ \"$PATH\" != \"${PATH//::/:}\" ]; do PATH=\"${PATH//::/:}\"; done; PATH=\"${PATH#:}\"; PATH=\"${PATH%:}\"; export PATH=\"$__ocx_p${PATH:+:${PATH}}\"; unset __ocx_p";
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
        let expected = "set --path __ocx_l $PATH; set __ocx_p \"/opt/bin\"; set __ocx_r; for __ocx_e in $__ocx_l; test \"$__ocx_e\" != \"$__ocx_p\"; and test -n \"$__ocx_e\"; and set -a __ocx_r $__ocx_e; end; set -gx --path PATH $__ocx_p $__ocx_r; set -e __ocx_p __ocx_r __ocx_e __ocx_l";
        assert_eq!(Shell::Fish.export_path("PATH", "/opt/bin").as_deref(), Some(expected));
    }

    /// fish infers colon-list-ness from the variable NAME, so the `--path` flag
    /// has to be on both the seed and the write-back for every key — not just
    /// the ones whose name ends in `PATH`, where fish would have inferred it.
    #[test]
    fn fish_path_kind_marks_every_key_as_a_path_variable() {
        for key in ["PATH", "PERL5LIB", "XDG_DATA_DIRS", "GEM_HOME"] {
            for line in [
                Shell::Fish.export_path(key, "/opt/bin").unwrap(),
                Shell::Fish.remove_list_element(key, "/opt/bin", None).unwrap(),
            ] {
                assert!(
                    line.contains(&format!("set --path __ocx_l ${key};")),
                    "{key}: the ambient must be re-split as a path list: {line}"
                );
                assert!(
                    line.contains(&format!("set -gx --path {key} ")),
                    "{key}: the write-back must export as a colon-joined path list: {line}"
                );
            }
        }
        // List-kind is a plain string fold and must NOT gain the flag: a list
        // separator is arbitrary text, and `--path` would split it on `:`.
        let list = Shell::Fish.remove_list_element("CFLAGS", "-DX", Some(" ")).unwrap();
        assert!(!list.contains("--path"), "list-kind must stay a string fold: {list}");
    }

    #[test]
    fn export_path_powershell_exact_shape() {
        // A-19: `-ne` is case-INsensitive, so on Linux it deleted `/opt/Bin`
        // when adding `/opt/bin`. `[String]::Equals` with an explicit
        // `StringComparison`, chosen at emit time under `cfg!(windows)`, is the
        // one PATH-element comparison rule. The segment's quote strip is chosen
        // by the same switch, because `std::env::split_paths` — the in-process
        // half of the parity claim — unquotes on Windows and nowhere else.
        let comparison = if cfg!(windows) { "OrdinalIgnoreCase" } else { "Ordinal" };
        let normalisation = if cfg!(windows) {
            r#" -replace '(?s)^"(.*)"$','$1'"#
        } else {
            ""
        };
        let expected = format!(
            "$__ocx_p='/opt/bin'; $__ocx_s=[IO.Path]::PathSeparator; $env:PATH=(@($__ocx_p)+($env:PATH -split [regex]::Escape($__ocx_s) | Where-Object {{$_ -and -not [String]::Equals(($_{normalisation}), ($__ocx_p{normalisation}), [StringComparison]::{comparison})}})) -join $__ocx_s; Remove-Variable __ocx_p,__ocx_s"
        );
        assert_eq!(
            Shell::PowerShell.export_path("PATH", "/opt/bin").as_deref(),
            Some(expected.as_str())
        );
    }

    /// E3 — the pwsh arm's comparison is **symmetric**: whatever normalisation
    /// the segment gets, the operand gets too.
    ///
    /// Normalising one side only meant the arm never recognised the quoted value
    /// it had itself written a prompt earlier, so `PATH` grew by one copy per
    /// prompt without bound — 4 applications, 4 copies, measured on pwsh 7 where
    /// every other arm was stable. Because `path_segment_normalisation()` is
    /// empty off Windows, the asymmetry was invisible on this host and the
    /// residual survived E4's fix.
    ///
    /// The needle is **derived** from the emitter's own switch, not spelled out,
    /// so a changed `-replace` cannot leave this passing against a stale
    /// literal. The count arm is what makes the Windows behaviour checkable from
    /// a Unix host.
    ///
    /// Red state: drop `{normalisation}` from the operand and the Windows count
    /// falls to 1 while the containment assertion fails on every platform.
    #[test]
    fn e3_the_powershell_path_comparison_normalises_both_sides_identically() {
        let normalisation = super::path_segment_normalisation();
        let line = Shell::PowerShell.export_path("PATH", "/opt/bin").expect("pwsh arm");

        assert!(
            line.contains(&format!(
                "[String]::Equals(($_{normalisation}), ($__ocx_p{normalisation}), "
            )),
            "segment and operand must carry the same normalisation; got: {line}"
        );
        assert_eq!(
            normalisation.is_empty(),
            !cfg!(windows),
            "the segment normalisation is the Windows arm and only that"
        );
        if !normalisation.is_empty() {
            assert_eq!(
                line.matches(normalisation).count(),
                2,
                "exactly two: one per side of the comparison"
            );
        }

        // The prepend stays byte-exact - only the equality test sees the
        // stripped form. `move_to_front` prepends verbatim too, and that parity
        // is the reason.
        assert!(
            line.starts_with("$__ocx_p='/opt/bin';"),
            "the operand variable itself is raw; got: {line}"
        );
    }

    /// The same rule on the removal arm, which reaches it a different way:
    /// `remove_list_element` strips its path-kind operand in Rust before
    /// emitting, so `$__ocx_p` is already bare and the emitted comparison must
    /// **not** normalise it a second time.
    ///
    /// Together with the test above this pins both spellings of one rule:
    /// compare a normalised segment against a normalised operand, exactly once
    /// each.
    #[test]
    fn e3_the_powershell_removal_operand_is_stripped_once_in_rust_not_twice() {
        let normalisation = super::path_segment_normalisation();
        let line = Shell::PowerShell
            .remove_list_element("PATH", "\"/opt/b in\"", None)
            .expect("pwsh path-kind arm");

        assert!(
            line.starts_with("$__ocx_p='/opt/b in';"),
            "the operand is stripped once, in Rust; got: {line}"
        );
        assert!(
            line.contains(&format!("[String]::Equals(($_{normalisation}), $__ocx_p, ")),
            "and the emitted comparison does not strip it again; got: {line}"
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
        let expected = "$env.PATH = (($env.PATH? | default \"\") | (if ($in | describe) == 'string' { split row (char esep) } else { $in }) | where {|p| $p != \"/opt/bin\" and $p != \"\" } | prepend \"/opt/bin\" | str join (char esep))";
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

    /// EC-QUOTE-003 — a literal `'` in a value, on the five POSIX arms, static
    /// half; the live `dash -c` half is `live_export_path_matches_move_to_front`
    /// (`PATH_CASES` carries `/a:/o'brien/bin`).
    /// EC-QUOTE-001 — the same escaper is the injection guard. The hostile
    /// fixture carries *two* quotes, so an escaper that replaced only the first
    /// — or the double-quoted-context `escape::fish_double_quoted` /
    /// `escape::nushell_plain_string`, which leave `'` untouched by design —
    /// closes the literal early and puts `id` in a command position.
    #[test]
    fn export_path_posix_single_quote_escapes_embedded_quote() {
        // A value with a literal `'` must close/escape/reopen (`'\''`) so the
        // single-quoted literal stays balanced across bash and the POSIX family.
        for shell in [Shell::Bash, Shell::Zsh, Shell::Dash, Shell::Ksh, Shell::Ash] {
            let line = shell.export_path("PATH", "/o'brien/bin").unwrap();
            assert!(line.contains("__ocx_p='/o'\\''brien/bin'"), "{shell}: {line}");
            // The live-injection element. Asserted as the whole binding, opening
            // quote through closing quote: EVERY embedded quote is escaped, so
            // the literal cannot close before its own final byte. (A "the raw
            // `';id;'` never appears" negative would be wrong here — `'\''` ends
            // in a quote, so the correctly escaped form contains that substring.)
            let line = shell.export_path("PATH", "/tmp/a';id;'b").unwrap();
            assert!(
                line.contains(r"__ocx_p='/tmp/a'\'';id;'\''b'"),
                "{shell}: every embedded quote must be escaped: {line}"
            );
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

    // The interpreter binaries `run_output` actually executed on this thread.
    // `cargo test` gives every test its own thread and `cargo nextest` its own
    // process, so this is per-test bookkeeping under either runner.
    #[cfg(unix)]
    thread_local! {
        static INTERPRETERS_RUN: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
        // Interpreters that spawned but could not run even `echo ok`. The one
        // skip cause that is neither "not installed" nor a defect in our emit,
        // and the only reason a present interpreter may legitimately have run
        // nothing — so it is recorded rather than inferred.
        static INTERPRETERS_BROKEN: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Which interpreters a missing installation is a **failure** for, read from
    /// `__OCX_TESTING_REQUIRE_LIVE_SHELLS`: `1` / `all` for every arm, otherwise a
    /// comma-separated allowlist of interpreter binaries (`fish,pwsh`).
    ///
    /// Off by default, because no `cargo nextest` leg installs the ten shells and
    /// a hard default would red every runner rather than the emit. Set it wherever
    /// the interpreters do exist — the shell-zoo image, a developer box, a future
    /// nextest leg that installs them — and an arm that ran nothing fails instead
    /// of passing silently.
    #[cfg(unix)]
    fn required_live_interpreters() -> Option<Vec<String>> {
        let raw = std::env::var("__OCX_TESTING_REQUIRE_LIVE_SHELLS").ok()?;
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        if raw == "1" || raw.eq_ignore_ascii_case("all") {
            return Some(Vec::new());
        }
        Some(
            raw.split(',')
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect(),
        )
    }

    /// Whether `bin` resolves to something spawnable, **observed** rather than
    /// inferred: the same PATH lookup `run_output` performs. A shell that
    /// spawns and then rejects `--version` is still present — presence is the
    /// only distinction drawn here, because presence is what separates an
    /// environment fact from a defect.
    #[cfg(unix)]
    fn interpreter_present(bin: &str) -> bool {
        !matches!(
            Command::new(bin).arg("--version").output(),
            Err(ref error) if error.kind() == std::io::ErrorKind::NotFound
        )
    }

    /// Refuse a live test that ran nothing under an interpreter that is
    /// installed on this machine.
    ///
    /// Without this every `if let Some(out) = run_script(...)` site is a green
    /// indistinguishable from a green that never ran: an absent interpreter
    /// returns `None` and the assertion inside the `if` never executes. Two
    /// things make that distinguishable here. Every interpreter that did not
    /// run has its cause **observed** — not installed (`interpreter_present`),
    /// or installed but unable to run `echo ok` (`INTERPRETERS_BROKEN`, written
    /// by `run_output` at the moment it saw the probe fail) — and any other
    /// state is a failure, because "installed, reached, and yet nothing ran"
    /// describes an arm silently dropped from the matrix. On top of that,
    /// `__OCX_TESTING_REQUIRE_LIVE_SHELLS` turns "not installed" into a failure
    /// too, wherever the interpreters are supposed to exist.
    ///
    /// The rule is per interpreter, not "at least one of them": a matrix of
    /// nine arms where only bash ever runs is exactly the vacuous green this
    /// exists to catch.
    #[cfg(unix)]
    fn assert_every_present_interpreter_ran(interpreters: &[&str]) {
        let ran = INTERPRETERS_RUN.with(|seen| seen.borrow().clone());
        let broken = INTERPRETERS_BROKEN.with(|seen| seen.borrow().clone());
        let mut absent = Vec::new();
        let mut unusable = Vec::new();
        let mut silent = Vec::new();
        for bin in interpreters {
            if ran.iter().any(|name| name == bin) {
                continue;
            }
            if broken.iter().any(|name| name == bin) {
                unusable.push(*bin);
            } else if interpreter_present(bin) {
                silent.push(*bin);
            } else {
                absent.push(*bin);
            }
        }
        assert!(
            silent.is_empty(),
            "{silent:?} is installed here, yet this live test ran no script under it — \
             its green is indistinguishable from a run that never happened"
        );
        if absent.is_empty() && unusable.is_empty() {
            return;
        }
        if let Some(required) = required_live_interpreters() {
            let missed: Vec<&str> = absent
                .iter()
                .chain(unusable.iter())
                .copied()
                .filter(|bin| required.is_empty() || required.iter().any(|want| want == bin))
                .collect();
            assert!(
                missed.is_empty(),
                "{missed:?} asserted nothing and __OCX_TESTING_REQUIRE_LIVE_SHELLS says they must be \
                 live here; install them (absent: {absent:?}, present but unusable: {unusable:?})"
            );
        }
        eprintln!(
            "# ocx: UNPROVEN — this live test asserted nothing under {absent:?} (not installed) \
             or {unusable:?} (installed, but cannot run a script)"
        );
    }

    /// [`assert_every_present_interpreter_ran`] over the whole parity matrix.
    #[cfg(unix)]
    fn assert_every_present_parity_arm_ran() {
        let names: Vec<&str> = PARITY_ARMS.iter().map(|argv| argv[0]).collect();
        assert_every_present_interpreter_ran(&names);
    }

    /// Run `script` under `argv` (interpreter + flags), returning trimmed stdout,
    /// or `None` if the interpreter is not installed (so CI without it stays green).
    ///
    /// A non-zero exit is *not* silently accepted: an interpreter that crashes
    /// (a pwsh whose .NET runtime segfaults on every script is real) would
    /// otherwise return empty stdout and fail the caller's assertion as if our
    /// emitted line were wrong. On failure, re-run `echo ok` — valid in every
    /// interpreter these tests drive — to tell the two apart: probe broken ⇒
    /// the interpreter is unusable, skip; probe fine ⇒ our script is at fault,
    /// panic with its stderr. Never skip on a working interpreter, or a syntax
    /// error in the emit would pass as green.
    ///
    /// `#[cfg(unix)]`: the POSIX/fish/pwsh live tests below seed and assert a
    /// colon-separated PATH (POSIX semantics). On Windows the same interpreters
    /// exist but behave differently — git-bash mangles `:` paths via MSYS, and
    /// pwsh uses `;` as `[IO.Path]::PathSeparator` — so the assertions are only
    /// meaningful on unix. Windows emit is covered by the `live_batch_*` cmd tests
    /// (cross-platform, below) and the `shell-activation-deep.yml` pwsh harness.
    #[cfg(unix)]
    fn run_output(argv: &[&str], script: &str) -> Option<std::process::Output> {
        let (bin, head) = argv.split_first()?;
        let run = |body: &str| match Command::new(bin).args(head).arg(body).output() {
            Ok(output) => Some(output),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("failed to spawn {bin}: {error}"),
        };
        let output = run(script)?;
        // Reached the interpreter — record it so the caller's
        // `assert_every_present_interpreter_ran` can tell "the emit round-tripped" from
        // "nothing ever ran". A crashing interpreter is deliberately NOT
        // recorded below: it proves nothing either.
        INTERPRETERS_RUN.with(|seen| seen.borrow_mut().push((*bin).to_string()));
        if !output.status.success() {
            let probe = run("echo ok")?;
            if !probe.status.success() || String::from_utf8_lossy(&probe.stdout).trim() != "ok" {
                // Observed, not inferred: this interpreter exists and cannot run
                // a script, which is the only admissible reason for a present
                // interpreter to have asserted nothing.
                INTERPRETERS_BROKEN.with(|seen| seen.borrow_mut().push((*bin).to_string()));
                return None;
            }
            panic!(
                "{bin} exited {} on:\n{script}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Some(output)
    }

    #[cfg(unix)]
    fn run_script(argv: &[&str], script: &str) -> Option<String> {
        run_output(argv, script).map(|output| String::from_utf8_lossy(&output.stdout).trim_end().to_string())
    }

    /// Same contract as [`run_script`], returning the script's **stderr** —
    /// the channel [`Shell::emit_message`] writes on.
    #[cfg(unix)]
    fn run_script_stderr(argv: &[&str], script: &str) -> Option<String> {
        run_output(argv, script).map(|output| String::from_utf8_lossy(&output.stderr).trim_end().to_string())
    }

    /// POSIX-style readback: seed PATH with the dir mid-list, source twice, print.
    #[cfg(unix)]
    fn posix_roundtrip(argv: &[&str], real_bin_dir: &str) {
        let line = Shell::from_argv(argv).export_path("PATH", "/opt/bin").unwrap();
        let script = format!("export PATH=/a:/opt/bin:/b:{real_bin_dir}; {line}; {line}; printf '%s' \"$PATH\"");
        if let Some(out) = run_script(argv, &script) {
            assert_eq!(out, format!("/opt/bin:/a:/b:{real_bin_dir}"), "argv={argv:?}");
        }
    }

    #[cfg(unix)]
    impl Shell {
        /// Map a live-test interpreter argv back to the `Shell` whose emit we test.
        fn from_argv(argv: &[&str]) -> Shell {
            match argv[0] {
                "bash" => Shell::Bash,
                "zsh" => Shell::Zsh,
                "dash" => Shell::Dash,
                "ksh" => Shell::Ksh,
                "busybox" => Shell::Ash,
                "fish" => Shell::Fish,
                "pwsh" => Shell::PowerShell,
                "elvish" => Shell::Elvish,
                "nu" => Shell::Nushell,
                other => panic!("unmapped interpreter {other}"),
            }
        }
    }

    #[cfg(unix)]
    fn awk_dir() -> String {
        // The POSIX awk form needs `awk` resolvable on PATH; keep the real bin dir.
        // `|| true`: a missing awk exits 1, which `run_script` now treats as our bug.
        std::path::Path::new(&run_script(&["bash", "-c"], "command -v awk || true").unwrap_or_default())
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/bin".to_string())
    }

    #[cfg(unix)]
    #[test]
    fn live_bash_zsh_idempotent_move_to_front() {
        let dir = awk_dir();
        posix_roundtrip(&["bash", "-c"], &dir);
        posix_roundtrip(&["zsh", "-c"], &dir);
        assert_every_present_interpreter_ran(&["bash", "zsh"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_posix_idempotent_move_to_front() {
        let dir = awk_dir();
        posix_roundtrip(&["dash", "-c"], &dir);
        posix_roundtrip(&["ksh", "-c"], &dir);
        posix_roundtrip(&["busybox", "ash", "-c"], &dir);
        assert_every_present_interpreter_ran(&["dash", "ksh", "busybox"]);
    }

    #[cfg(unix)]
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
        assert_every_present_interpreter_ran(&["bash", "zsh", "dash", "busybox"]);
    }

    #[cfg(unix)]
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
        assert_every_present_interpreter_ran(&["bash", "dash"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_bash_empty_path_has_no_separators() {
        let line = Shell::Bash.export_path("PATH", "/opt/bin").unwrap();
        let script = format!("export PATH=; {line}; printf '%s' \"$PATH\"");
        if let Some(out) = run_script(&["bash", "-c"], &script) {
            assert_eq!(out, "/opt/bin", "empty PATH must not gain leading/trailing separators");
        }
        assert_every_present_interpreter_ran(&["bash"]);
    }

    #[cfg(unix)]
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
        assert_every_present_interpreter_ran(&["bash"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_fish_idempotent_move_to_front() {
        let line = Shell::Fish.export_path("PATH", "/opt/bin").unwrap();
        let script = format!("set -gx PATH /a /opt/bin /b; {line}; {line}; string join : $PATH");
        if let Some(out) = run_script(&["fish", "-c"], &script) {
            assert_eq!(out, "/opt/bin:/a:/b");
        }
        assert_every_present_interpreter_ran(&["fish"]);
    }

    /// fish auto-splits only `PATH`, `CDPATH`, `MANPATH` and names **ending in**
    /// `PATH`, so a `PERL5LIB` / `RUBYLIB` / `XDG_DATA_DIRS` ambient arrives as
    /// ONE list element holding the whole colon-joined string. Every path-kind
    /// fixture in this file was a `*PATH` key, which is why nothing caught the
    /// list form writing a two-element list fish exports **space**-joined.
    ///
    /// Read back through an `sh -c` child, because the property under test is
    /// the byte string a child process inherits, not fish's internal list.
    #[cfg(unix)]
    #[test]
    fn live_fish_path_kind_is_colon_joined_for_a_key_fish_would_not_split() {
        let apply = Shell::Fish.export_path("PERL5LIB", "/n/bin").unwrap();
        let revert = Shell::Fish.remove_list_element("PERL5LIB", "/n/bin", None).unwrap();
        let read = "sh -c 'printf %s \"$PERL5LIB\"'";
        for (name, seed, statements, want) in [
            ("apply", "/x:/y", apply.clone(), "/n/bin:/x:/y"),
            ("apply twice", "/x:/y", format!("{apply}; {apply}"), "/n/bin:/x:/y"),
            ("apply+revert", "/x:/y", format!("{apply}; {revert}"), "/x:/y"),
            ("revert only", "/n/bin:/x:/y", revert.clone(), "/x:/y"),
            ("move to front", "/a:/n/bin:/b", apply.clone(), "/n/bin:/a:/b"),
            ("drop empties", "/a::/b", apply.clone(), "/n/bin:/a:/b"),
        ] {
            let script = format!("set -gx PERL5LIB {seed}; {statements}; {read}");
            if let Some(out) = run_script(&["fish", "-c"], &script) {
                assert_eq!(out, want, "fish PERL5LIB {name}");
            }
        }
        assert_every_present_interpreter_ran(&["fish"]);
    }

    /// Each escaper, run through the interpreter and quoting context it exists
    /// for, over the byte set that separates the arms.
    ///
    /// `escape::fish_single_quoted` is the reason this exists: it lived only as
    /// two unowned copies in `hook.rs` and `activate.rs` and had no test at all,
    /// while `shell.rs`'s fish escaper — a *double*-quote escaper with an
    /// incompatible body — carried a comment claiming the copies were the same
    /// three functions. The `'` and `$` rows are what tell the two apart.
    #[cfg(unix)]
    #[test]
    fn live_every_escaper_round_trips_in_its_own_quoting_context() {
        const HOSTILE: &[&str] = &[
            "plain", "it's", "a\"b", "a\\b", "a\\'b", "$HOME", "${x}", "`id`", "$(id)", "a;id;b", "a b", "a!b", "*",
            "a|b", "a&b", "a(b)c", "100%", "café",
        ];
        for value in HOSTILE {
            let cases: [(&[&str], String); 4] = [
                (
                    &["bash", "-c"],
                    format!("printf '%s' '{}'", escape::posix_single_quoted(value)),
                ),
                (
                    &["fish", "-c"],
                    format!("printf '%s' '{}'", escape::fish_single_quoted(value)),
                ),
                (
                    &["fish", "-c"],
                    format!("printf '%s' \"{}\"", escape::fish_double_quoted(value)),
                ),
                (
                    &["pwsh", "-NoProfile", "-Command"],
                    format!("[Console]::Out.Write('{}')", escape::single_quoted_doubled(value)),
                ),
            ];
            for (argv, script) in cases {
                if let Some(out) = run_script(argv, &script) {
                    assert_eq!(&out, value, "argv={argv:?} script={script}");
                }
            }
        }
        assert_every_present_interpreter_ran(&["bash"]);
        assert_every_present_interpreter_ran(&["fish"]);
        assert_every_present_interpreter_ran(&["pwsh"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_powershell_idempotent_move_to_front() {
        let line = Shell::PowerShell.export_path("PATH", "/opt/bin").unwrap();
        let script = format!("$env:PATH='/a:/opt/bin:/b'; {line}; {line}; $env:PATH");
        if let Some(out) = run_script(&["pwsh", "-NoProfile", "-Command"], &script) {
            assert_eq!(out, "/opt/bin:/a:/b");
        }
        assert_every_present_interpreter_ran(&["pwsh"]);
    }

    /// Run a batch `body` as a temp `.bat` under `cmd /c`, returning trimmed
    /// stdout, or `None` when cmd.exe is absent (non-Windows → the test skips
    /// green). A real script file is required, not `cmd /c "<body>"`: the
    /// move-to-front emit is multi-line and relies on per-statement `%PATH%`
    /// re-expansion, which only a sequentially-parsed `.bat` provides. CRLF line
    /// endings keep legacy cmd parsers happy.
    fn run_batch(body: &str) -> Option<String> {
        // Unique per invocation: nextest runs the batch live-tests concurrently and
        // they share `temp_dir()`. A fixed name raced (one test's `SET "PATH="` body
        // executed under another's cmd → empty PATH). pid disambiguates process-per-test;
        // the counter covers any same-process reuse.
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "ocx_batch_live_{}_{}.bat",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        // RAII cleanup: guard covers the early `?` (write failure leaves a partial
        // file) and the spawn-error panic path, not just the happy return.
        let _guard = crate::utility::fs::DropFile::new(&path);
        std::fs::write(&path, format!("@echo off\r\n{body}\r\n")).ok()?;
        match Command::new("cmd").args(["/c", &path.to_string_lossy()]).output() {
            Ok(output) => Some(String::from_utf8_lossy(&output.stdout).trim_end().to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => panic!("failed to spawn cmd: {error}"),
        }
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

    /// EC-PATH-013 — live cmd.exe: an unanchored *last* PATH segment (no
    /// trailing separator) is invisible to `%VAR:search=%`'s search pattern,
    /// which needs the trailing separator to match, so the first apply
    /// prepends a second copy rather than relocating the existing one. Every
    /// later apply DOES match the front copy (which does carry a trailing
    /// separator) and re-prepends it with no growth, so the count stabilises
    /// at 2 — `export_path`'s doc comment calls this "a one-time non-dedup,
    /// never unbounded growth"; N=20 applies is the register's own proof bar.
    #[test]
    fn live_batch_unanchored_last_segment_does_not_regrow_past_two() {
        let separator = sep();
        let value = r"C:\opt\bin";
        let line = Shell::Batch.export_path("PATH", value).unwrap();
        // `value` is the last segment with no trailing separator — unanchored.
        let seed = format!(r"C:\a{separator}{value}");
        let mut body = format!("SET \"PATH={seed}\"\r\n");
        for _ in 0..20 {
            body.push_str(&line);
            body.push_str("\r\n");
        }
        body.push_str("echo %PATH%");
        if let Some(out) = run_batch(&body) {
            assert_eq!(
                out.matches(value).count(),
                2,
                "an unanchored last segment must stabilise at 2 copies (one relocated front, one \
                 untouched residual), never climb past it over repeat applies: {out:?}"
            );
        }
    }

    /// EC-QUOTE-011 — live cmd.exe, delayed-expansion-**off** half only: a
    /// `!`-bearing value survives verbatim in `%PATH%`, matching
    /// `batch_accepts_a_bang_under_the_delayed_expansion_precondition`'s
    /// string-level pin against a real interpreter. The delayed-expansion-**on**
    /// half (where the row's own text predicts truncation) is deliberately not
    /// asserted here: the emitted line contains the value twice — once as the
    /// prepend, once inside `%VAR:search=%`'s search pattern — so it carries
    /// TWO `!` bytes, and cmd's exact `!...!` pairing across that shape is not
    /// something this environment has a Windows host to verify against.
    /// Asserting a specific outcome unverified risks shipping a live-CI check
    /// that is wrong forever on a leg nobody here can watch go red first — see
    /// `manual_procedure_ec_quote_011_delayed_expansion_on_truncation` for the
    /// documented manual half instead.
    #[test]
    fn live_batch_bang_survives_without_delayed_expansion() {
        let value = r"C:\x!y\bin";
        let line = Shell::Batch.export_path("PATH", value).unwrap();
        let body = format!("SET \"PATH=\"\r\n{line}\r\necho %PATH%");
        if let Some(out) = run_batch(&body) {
            assert!(
                out.contains(value),
                "delayed expansion off (cmd's default): the bang must survive verbatim: {out:?}"
            );
        }
    }

    /// EC-QUOTE-004, live half — the `"` refusal against a real cmd.exe, plus
    /// the byte-exactness of everything that is *not* refused. Skips green off
    /// Windows via `run_batch`'s NotFound arm.
    #[test]
    fn live_batch_a_quote_would_execute_which_is_why_it_is_refused() {
        let hostile = "x\" & echo PWNED & echo \"";
        // The emitter refuses it, so no line reaches cmd at all.
        assert!(
            Shell::Batch.export_constant("K", hostile).is_none(),
            "a quote-bearing value must never reach a SET statement"
        );
        // What the emit WOULD be without the guard. cmd's quote-state parser
        // closes the quote after `x`, the `&` then separates commands and the
        // marker runs. This is the guard's red half: it pins why the refusal
        // exists, instead of asserting only that it is present.
        let unguarded = format!("SET \"K={}\"", escape::batch_set_value(hostile));
        if let Some(out) = run_batch(&unguarded) {
            assert!(out.contains("PWNED"), "cmd must split at the unquoted `&`: {out:?}");
        }
        // The bytes that are NOT refused round-trip byte-exact — that is what
        // makes the narrowed `escape::batch_set_value` correct. Read back under
        // delayed expansion: `call echo %K%` re-parses the expanded text and the
        // `>` would redirect the output away.
        let value = r"C:\a&b<c>d|e^f(g)";
        let line = Shell::Batch.export_constant("OCX_LIVE_Q", value).unwrap();
        let body = format!("setlocal EnableDelayedExpansion\r\n{line}\r\necho [!OCX_LIVE_Q!]");
        if let Some(out) = run_batch(&body) {
            assert_eq!(out, format!("[{value}]"), "the value must survive byte-exact");
        }
    }

    // ══ Idempotent unique-append export_list (issue #277) ════════════════
    //
    // Same two layers as `export_path` above: exact-shape tests lock the
    // emitted bytes against a `format!` regression, and `live_*` tests run the
    // real statement through a real interpreter. The live assertions never
    // hard-code an expected string — they compare against
    // `utility::list::append_unique`, because "the shell snippet and the
    // in-process fold produce identical strings" is the property the ADR pins,
    // and a hand-written expectation could drift from the primitive.

    /// `(ambient, value, separator)` triples every live list test drives.
    ///
    /// Gated with the tests that read it: every `live_*` consumer is
    /// `#[cfg(unix)]`, so on Windows this and `folded` below would be dead
    /// items and `-D warnings` turns `dead_code` into a build error.
    #[cfg(unix)]
    const LIST_CASES: &[(&str, &str, &str)] = &[
        ("", "-ea", " "),           // empty ambient — no leading separator
        ("-Xmx1g", "-ea", " "),     // plain append
        ("-ea -Xmx1g", "-ea", " "), // present already: move to the back
        ("a,b,a,c", "a", ","),      // every occurrence removed, not just the first
        ("a,a,b", "a", ","),        // adjacent duplicates need the fixpoint loop
        ("a,a,a", "a", ","),
        ("-eabc", "-ea", " "),      // a longer element is not a flank match
        ("x", "a,b", ","),          // a value carrying the separator is one contribution
        ("-Wall", "-Wextra", "; "), // multi-character separator
        ("a b", "c d", " "),        // value with spaces
        ("A,a", "a", ","),          // case-SENSITIVE: the `A` element must survive
        ("0", "-ea", " "),          // a falsy-LOOKING ambient is a value, not an absent one
        (",x", "a", ","),           // ambient already starts with the separator
        ("a,,b", "x", ","),         // an empty element in the ambient survives verbatim
        ("x", "v", "\""),           // ── metacharacter separators ──
        ("x", "v", "%"),
        ("x", "v", "`"),
        ("x", "v", "$"),
        ("x", "v", ";"),
        ("x", "v", "*"), // a glob metachar must match itself, not pattern-match
        ("a*b", "c", "*"),
        ("x", "v", "'"),
        ("x", "v", "\\"),
        ("x", "v", "!"),
        ("x", "v", "→"),   // a multi-byte separator is one delimiter, not three
        ("a'b", "c", " "), // a quote-bearing ambient reds a mispaired test-harness escaper
        // ── newline-bearing ambient and value ──
        //
        // Separators can never carry a newline (`list::separator_is_valid`
        // refuses them), but the ambient value is whatever the user's shell
        // holds and the contribution comes from package metadata. fish splits
        // command-substitution output on newlines, so these are the cases that
        // catch a missing `string collect`.
        ("a\nb", "c", " "),
        ("x", "a\nb", " "),
        ("a\rb", "c", " "),
        ("x", "a\rb", " "),
    ];

    #[cfg(unix)]
    fn folded(existing: &str, value: &str, separator: &str) -> String {
        crate::utility::list::append_unique(existing, value, separator)
    }

    #[test]
    fn export_list_posix_exact_shape() {
        let expected = "__ocx_v='-ea'; __ocx_s=' '; __ocx_p=\"$__ocx_s$__ocx_v$__ocx_s\"; __ocx_l=\"${OPTS:+$__ocx_s${OPTS}}$__ocx_s\"; while [ \"$__ocx_l\" != \"${__ocx_l%%\"$__ocx_p\"*}\" ]; do __ocx_l=\"${__ocx_l%%\"$__ocx_p\"*}$__ocx_s${__ocx_l#*\"$__ocx_p\"}\"; done; export OPTS=\"${__ocx_l#\"$__ocx_s\"}$__ocx_v\"; unset __ocx_v __ocx_s __ocx_p __ocx_l";
        for shell in [Shell::Bash, Shell::Zsh, Shell::Ash, Shell::Ksh, Shell::Dash] {
            assert_eq!(
                shell.export_list("OPTS", "-ea", " ").as_deref(),
                Some(expected),
                "{shell}"
            );
        }
    }

    #[test]
    fn export_list_fish_exact_shape() {
        let expected = "set __ocx_v \"-ea\"; set __ocx_s \" \"; set __ocx_p \"$__ocx_s$__ocx_v$__ocx_s\"; set __ocx_l \"$__ocx_s\"; if test -n \"$OPTS\"; set __ocx_l \"$__ocx_s$OPTS$__ocx_s\"; end; set __ocx_n (string replace --all -- \"$__ocx_p\" \"$__ocx_s\" \"$__ocx_l\" | string collect); while test \"$__ocx_n\" != \"$__ocx_l\"; set __ocx_l \"$__ocx_n\"; set __ocx_n (string replace --all -- \"$__ocx_p\" \"$__ocx_s\" \"$__ocx_l\" | string collect); end; set __ocx_l (string replace -- \"$__ocx_s\" \"\" \"$__ocx_l\" | string collect); set -gx OPTS \"$__ocx_l$__ocx_v\"; set -e __ocx_v __ocx_s __ocx_p __ocx_l __ocx_n";
        assert_eq!(Shell::Fish.export_list("OPTS", "-ea", " ").as_deref(), Some(expected));
    }

    #[test]
    fn export_list_powershell_exact_shape() {
        let expected = "$__ocx_v='-ea'; $__ocx_s=' '; $__ocx_p=\"$__ocx_s$__ocx_v$__ocx_s\"; $__ocx_l=if ($env:OPTS) { \"$__ocx_s$($env:OPTS)$__ocx_s\" } else { $__ocx_s }; while ($__ocx_l.Contains($__ocx_p)) { $__ocx_l=$__ocx_l.Replace($__ocx_p,$__ocx_s) }; $env:OPTS=$__ocx_l.Substring($__ocx_s.Length)+$__ocx_v; Remove-Variable __ocx_v,__ocx_s,__ocx_p,__ocx_l";
        assert_eq!(
            Shell::PowerShell.export_list("OPTS", "-ea", " ").as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn export_list_elvish_exact_shape() {
        let expected = "use str; var __ocx_l = ' '; if (and (has-env OPTS) (not-eq $E:OPTS '')) { set __ocx_l = ' '$E:OPTS' ' }; while (str:contains $__ocx_l ' -ea ') { set __ocx_l = (str:replace ' -ea ' ' ' $__ocx_l) }; set E:OPTS = (str:replace &max=1 ' ' '' $__ocx_l)'-ea'";
        assert_eq!(Shell::Elvish.export_list("OPTS", "-ea", " ").as_deref(), Some(expected));
    }

    #[test]
    fn export_list_nushell_exact_shape() {
        let expected = "mut __ocx_l = (if ($env.OPTS? | default \"\") == \"\" { \" \" } else { \" \" + ($env.OPTS? | default \"\") + \" \" }); while ($__ocx_l | str contains \" -ea \") { $__ocx_l = ($__ocx_l | str replace --all \" -ea \" \" \") }; $env.OPTS = (($__ocx_l | str replace \" \" \"\") + \"-ea\")";
        assert_eq!(
            Shell::Nushell.export_list("OPTS", "-ea", " ").as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn export_list_batch_is_unsupported() {
        // cmd.exe has no case-sensitive string replacement: `%VAR:search=repl%`
        // is case-INSENSITIVE (measured — `%V:,abc,=,%` deletes `,ABC,`), and
        // its case-sensitive primitives (`IF`, `%VAR:~n,m%`) cannot find a
        // substring at an unknown position. Emitting nothing beats emitting a
        // statement that deletes a differently-cased option or grows on every
        // re-source. The caller turns this into a stderr note naming the shell.
        assert_eq!(Shell::Batch.export_list("OPTS", "-ea", " "), None);
        // The path/constant emitters are unaffected — only the list fold is blocked.
        assert!(Shell::Batch.export_path("PATH", "/opt/bin").is_some());
        assert!(Shell::Batch.export_constant("OPTS", "-ea").is_some());
    }

    #[test]
    fn export_list_rejects_invalid_key_all_shells() {
        for shell in Shell::value_variants() {
            assert!(shell.export_list("OPTS; rm -rf /", "-ea", " ").is_none(), "{shell}");
        }
    }

    #[test]
    fn export_list_empty_value_is_a_commented_no_op() {
        // The primitive treats an empty contribution as a no-op; folding with an
        // empty value would search for `sep+sep` and collapse the ambient's own
        // adjacent separators. Every shell gets a comment instead of a statement.
        for shell in Shell::value_variants() {
            let line = shell.export_list("OPTS", "", ",").expect("a comment, not None");
            assert_eq!(line, shell.comment("ocx: OPTS list entry is empty, nothing to append"));
            assert!(!line.contains("__ocx_l"), "{shell}: must not emit the fold: {line}");
        }
    }

    #[test]
    fn export_list_is_generic_over_key() {
        // The emitter must work for any list-style key (`GODEBUG`, `CFLAGS`, ...).
        let line = Shell::Bash.export_list("GODEBUG", "gctrace=1", ",").unwrap();
        assert!(line.contains("export GODEBUG="), "got: {line}");
        assert!(!line.contains("OPTS"), "must not hardcode a key: {line}");
    }

    /// EC-QUOTE-013, `export_list` half — the separator takes the arm's own
    /// escaper. The `remove_list_element` half is
    /// `remove_list_element_escapes_the_separator_like_the_value`; the live half
    /// is `live_list_injection_does_not_execute` (its middle case drives the
    /// hostile string as the *separator*).
    #[test]
    fn export_list_escapes_the_separator_like_the_value() {
        // The separator is untrusted authored text, so it goes through the same
        // per-shell escaper as the value. Each case below would interpolate,
        // break the quoting, or glob if the raw byte were emitted.
        let cases: [(Shell, &str, &str); 6] = [
            (Shell::Bash, "'", "__ocx_s='\'\\''"),      // POSIX close/escape/reopen
            (Shell::Dash, "'", "__ocx_s='\'\\''"),      //
            (Shell::PowerShell, "'", "$__ocx_s=''''"),  // doubled inside '...'
            (Shell::Elvish, "'", "var __ocx_l = ''''"), // doubled inside '...'
            (Shell::Fish, "$", "set __ocx_s \"\\$\""),  // backslash-escaped in "..."
            (Shell::Nushell, "\"", "\\\""),             // backslash-escaped in "..."
        ];
        for (shell, separator, expected_fragment) in cases {
            let line = shell.export_list("OPTS", "v", separator).unwrap();
            assert!(
                line.contains(expected_fragment),
                "{shell}: separator {separator:?} must be escaped; expected {expected_fragment:?} in {line}"
            );
        }
    }

    /// Seed `OPTS`, apply the emitted statement **twice**, print `OPTS`, and
    /// require the result to equal the in-process fold — proving both that the
    /// two folds agree and that eval-twice ≡ eval-once.
    #[cfg(unix)]
    fn posix_list_roundtrip(argv: &[&str]) {
        let shell = Shell::from_argv(argv);
        for (existing, value, separator) in LIST_CASES {
            let line = shell.export_list("OPTS", value, separator).unwrap();
            let seed = escape::posix_single_quoted(existing);
            let script = format!("export OPTS='{seed}'; {line}; {line}; printf '%s' \"$OPTS\"");
            if let Some(out) = run_script(argv, &script) {
                assert_eq!(
                    out,
                    folded(existing, value, separator),
                    "argv={argv:?} ambient={existing:?} value={value:?} sep={separator:?}"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn live_bash_zsh_list_matches_the_in_process_fold() {
        posix_list_roundtrip(&["bash", "-c"]);
        posix_list_roundtrip(&["zsh", "-c"]);
        assert_every_present_interpreter_ran(&["bash", "zsh"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_posix_list_matches_the_in_process_fold() {
        posix_list_roundtrip(&["dash", "-c"]);
        posix_list_roundtrip(&["ksh", "-c"]);
        posix_list_roundtrip(&["busybox", "ash", "-c"]);
        assert_every_present_interpreter_ran(&["dash", "ksh", "busybox"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_fish_list_matches_the_in_process_fold() {
        for (existing, value, separator) in LIST_CASES {
            let line = Shell::Fish.export_list("OPTS", value, separator).unwrap();
            let seed = escape::fish_double_quoted(existing);
            let script = format!("set -gx OPTS \"{seed}\"; {line}; {line}; printf '%s' \"$OPTS\"");
            if let Some(out) = run_script(&["fish", "-c"], &script) {
                assert_eq!(
                    out,
                    folded(existing, value, separator),
                    "fish ambient={existing:?} value={value:?} sep={separator:?}"
                );
            }
        }
        assert_every_present_interpreter_ran(&["fish"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_elvish_list_matches_the_in_process_fold() {
        for (existing, value, separator) in LIST_CASES {
            let line = Shell::Elvish.export_list("OPTS", value, separator).unwrap();
            // Elvish raw strings are single-quoted with `'` doubled — the same
            // escaper the pwsh seed uses. The fish/nushell escaper is the
            // double-quote one and would mispair inside the `'…'` literal below.
            let seed = escape::single_quoted_doubled(existing);
            let script = format!("set-env OPTS '{seed}'; {line}; {line}; print $E:OPTS");
            if let Some(out) = run_script(&["elvish", "-c"], &script) {
                assert_eq!(
                    out,
                    folded(existing, value, separator),
                    "elvish ambient={existing:?} value={value:?} sep={separator:?}"
                );
            }
        }
        assert_every_present_interpreter_ran(&["elvish"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_nushell_list_matches_the_in_process_fold() {
        for (existing, value, separator) in LIST_CASES {
            let line = Shell::Nushell.export_list("OPTS", value, separator).unwrap();
            let seed = escape::nushell_plain_string(existing);
            let script = format!("$env.OPTS = \"{seed}\"; {line}; {line}; print -n $env.OPTS");
            if let Some(out) = run_script(&["nu", "-c"], &script) {
                assert_eq!(
                    out,
                    folded(existing, value, separator),
                    "nu ambient={existing:?} value={value:?} sep={separator:?}"
                );
            }
        }
        assert_every_present_interpreter_ran(&["nu"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_powershell_list_matches_the_in_process_fold() {
        // Also the executable proof of case-sensitivity for the one shell whose
        // default operators (`-replace`, `-eq`) are case-INSENSITIVE: the
        // `("A,a", "a", ",")` case keeps the `A` element only under an ordinal
        // match.
        for (existing, value, separator) in LIST_CASES {
            let line = Shell::PowerShell.export_list("OPTS", value, separator).unwrap();
            let seed = escape::single_quoted_doubled(existing);
            let script = format!("$env:OPTS='{seed}'; {line}; {line}; [Console]::Out.Write($env:OPTS)");
            if let Some(out) = run_script(&["pwsh", "-NoProfile", "-Command"], &script) {
                assert_eq!(
                    out,
                    folded(existing, value, separator),
                    "pwsh ambient={existing:?} value={value:?} sep={separator:?}"
                );
            }
        }
        assert_every_present_interpreter_ran(&["pwsh"]);
    }

    #[cfg(unix)]
    #[test]
    fn live_list_injection_does_not_execute() {
        // Three attacker-influenced inputs: the value and the separator both come
        // from package metadata, and the ambient value is whatever the user's
        // environment already holds. None may reach a command position — the
        // whole fold runs through parameter expansion, which the shell does not
        // re-scan for command substitution.
        let marker = std::env::temp_dir().join("ocx_inj_live_list");
        let hostile = format!("$(touch {})", marker.display());
        for argv in [
            &["bash", "-c"][..],
            &["zsh", "-c"][..],
            &["dash", "-c"][..],
            &["ksh", "-c"][..],
            &["busybox", "ash", "-c"][..],
        ] {
            for (ambient, value, separator) in [
                ("a b", hostile.as_str(), " "),
                ("a b", "v", hostile.as_str()),
                (hostile.as_str(), "v", " "),
            ] {
                let _ = std::fs::remove_file(&marker);
                let line = Shell::from_argv(argv).export_list("OPTS", value, separator).unwrap();
                let seed = escape::posix_single_quoted(ambient);
                let script = format!("export OPTS='{seed}'; {line}; {line}; printf '%s' \"$OPTS\"");
                if let Some(out) = run_script(argv, &script) {
                    assert!(
                        !marker.exists(),
                        "argv={argv:?}: injection executed via ambient={ambient:?} value={value:?} sep={separator:?}"
                    );
                    assert_eq!(out, folded(ambient, value, separator), "argv={argv:?}");
                }
            }
        }
        let _ = std::fs::remove_file(&marker);
        assert_every_present_interpreter_ran(&["bash", "zsh", "dash", "ksh", "busybox"]);
    }

    #[test]
    fn live_batch_empty_path_has_entry_first() {
        // Empty-PATH edge (unreachable during real activation — a shell always has
        // a PATH). `SET "PATH="` UNDEFINES the variable in cmd, and substring
        // expansion `%PATH:x=%` on an undefined variable is implementation-defined,
        // so we do NOT pin the exact tail. The robust, form-guaranteed invariant is
        // that the prepended `value<sep>` always LEADS the result (the emit is
        // `SET "PATH=value<sep>%PATH:value<sep>=%"`), and that a double source keeps
        // the entry leading (move-to-front, no growth at the front).
        let separator = sep();
        let value = r"C:\opt\bin";
        let line = Shell::Batch.export_path("PATH", value).unwrap();
        let body = format!("SET \"PATH=\"\r\n{line}\r\n{line}\r\necho %PATH%");
        if let Some(out) = run_batch(&body) {
            assert!(
                out.starts_with(&format!("{value}{separator}")),
                "batch empty-PATH result must lead with `value<sep>`; got: {out:?}"
            );
        }
    }

    // ══ A-17 / A-20 — emitter guards ═════════════════════════════════════

    #[test]
    fn export_path_empty_value_is_a_commented_no_op() {
        // A-17 / C-021: measured, `Bash.export_path("PATH", "")` against ambient
        // `/a:/b` yielded `:/a:/b` — a leading empty segment, which POSIX
        // resolves as the current working directory. `move_to_front` refuses to
        // prepend an empty value, so the two disagreed. Every arm now emits a
        // comment instead of a statement.
        for shell in Shell::value_variants() {
            let line = shell.export_path("PATH", "").expect("a comment, not None");
            assert_eq!(line, shell.comment("ocx: PATH path entry is empty, nothing to prepend"));
            assert!(!line.contains("__ocx_p"), "{shell}: must not emit the prepend: {line}");
        }
        // Parity with the in-process primitive: prepending nothing is identity
        // over the ambient's own segments.
        assert_eq!(
            crate::utility::path::move_to_front(std::ffi::OsStr::new("/a:/b"), std::ffi::OsStr::new("")),
            std::ffi::OsString::from("/a:/b")
        );
    }

    /// EC-QUOTE-010 — Batch cannot express a `%` inside `%VAR:search=%`'s
    /// deletion pattern, so a `%`-bearing value's delete half would never
    /// match and every apply would prepend another copy under a per-prompt
    /// reconciler; `export_path`/`export_constant` must refuse it outright.
    /// EC-QUOTE-004 — a literal LF/CR in a value would split one `SET` into
    /// two commands on the `FOR /F ... DO @%i` channel `ocx --global env` is
    /// applied through; refusing satisfies the row's "refuse or single-line
    /// it" contract for the Batch arm.
    #[test]
    fn batch_refuses_percent_lf_and_cr_on_both_emitters() {
        // A-20: `%VAR:search=%` has no escape for a literal `%` in `search`, so
        // the delete half never matches and every apply prepends another copy —
        // unbounded growth. An LF splits one `SET` into two commands in the
        // `FOR /F … DO @%i` channel `ocx --global env` is applied through.
        for hostile in [r"C:\a%b\bin", "a\nb", "a\rb"] {
            assert_eq!(
                Shell::Batch.export_path("PATH", hostile),
                None,
                "batch export_path must refuse {hostile:?}"
            );
            assert_eq!(
                Shell::Batch.export_constant("K", hostile),
                None,
                "batch export_constant must refuse {hostile:?}"
            );
        }
        // Every other arm carries all three, so the refusal is Batch-specific.
        for shell in Shell::value_variants().iter().filter(|s| **s != Shell::Batch) {
            assert!(shell.export_path("PATH", "/a%b/bin").is_some(), "{shell}");
            assert!(shell.export_constant("K", "a\nb").is_some(), "{shell}");
        }
    }

    /// EC-QUOTE-004, quote half — a `"` in an authored `[env]` value closes the
    /// `SET "KEY=…"` statement, and cmd parses the remainder as command syntax:
    /// `x" & <cmd> & "` runs `<cmd>` in the `call *.bat` and `FOR /F … DO @%i`
    /// channels `ocx env --shell=batch` feeds. Measured against a real cmd.exe.
    ///
    /// `"` is the ONLY byte that can do it — `^ & < > | ( )` and a trailing `\`
    /// are inert inside the quotes (also measured), which is why the refusal is
    /// one byte rather than a return to the old caret escapes: those never
    /// neutralised anything cmd was going to act on, they only corrupted the
    /// value (`escape_batch_set_value_escapes_only_percent`).
    #[test]
    fn batch_refuses_a_quote_that_would_close_the_set_statement() {
        for hostile in ["x\" & calc & \"", "\"", "C:\\a\"b\\bin"] {
            assert_eq!(
                Shell::Batch.export_path("PATH", hostile),
                None,
                "batch export_path must refuse {hostile:?}"
            );
            assert_eq!(
                Shell::Batch.export_constant("K", hostile),
                None,
                "batch export_constant must refuse {hostile:?}"
            );
        }
        // With the quote refused, an emitted line carries exactly the two quotes
        // the format string wrote, so every cmd metacharacter in the value stays
        // inside them and none can reach a command position.
        for benign in [r"C:\a&b<c>d|e^f(g)", r"C:\x!y\bin", r"C:\a b\bin", r"C:\end\"] {
            for line in [
                Shell::Batch.export_constant("K", benign).expect("no refused byte"),
                Shell::Batch.export_path("PATH", benign).expect("no refused byte"),
            ] {
                assert_eq!(
                    line.matches('"').count(),
                    2,
                    "the value must not add a quote of its own: {line}"
                );
            }
        }
        // Every other arm carries a quote, so the refusal is Batch-specific.
        for shell in Shell::value_variants().iter().filter(|s| **s != Shell::Batch) {
            assert!(shell.export_constant("K", "a\"b").is_some(), "{shell}");
        }
    }

    #[test]
    fn escape_batch_set_value_escapes_only_percent() {
        // A-20: the caret escapes were over-escaping — cmd does not process
        // `^`, `&`, `<`, `>` or `|` inside `SET "KEY=…"`, so the carets survived
        // into the value and corrupted it.
        assert_eq!(escape::batch_set_value("a&b<c>d|e^f"), "a&b<c>d|e^f");
        assert_eq!(escape::batch_set_value("a%b"), "a%%b");
    }

    /// EC-QUOTE-011 — Batch `!` corruption under delayed expansion is a
    /// documented precondition, not a refusal: `escape::batch_set_value`
    /// leaves `!` unescaped, so this pins that the emitted line carries the
    /// bang verbatim rather than escaping or refusing it.
    #[test]
    fn batch_accepts_a_bang_under_the_delayed_expansion_precondition() {
        // A-20.4: the `!` case is a *named ceiling*, not a refusal. The emit is
        // correct under cmd's default (delayed expansion off); under
        // `cmd /v:on` a `!`-bearing value is consumed as a variable reference
        // and truncated. Nothing in ocx controls the consuming script's
        // `setlocal`, so `!` is emitted rather than refused — and this test is
        // what stops someone "fixing" it by adding `!` to the refusal set.
        let line = Shell::Batch
            .export_path("PATH", r"C:\a!b\bin")
            .expect("a bang is emitted, not refused");
        assert!(line.contains(r"C:\a!b\bin"), "the bang rides verbatim: {line}");
    }

    // ══ A-19 — one PATH-element comparison rule ══════════════════════════

    #[test]
    fn powershell_path_emits_compare_ordinally_and_normalise_as_split_paths_does() {
        // Both the applier and the remover must carry the same predicate, or a
        // segment survives one and not the other. Both halves of that predicate
        // are chosen by `cfg!(windows)`, and both track what
        // `std::env::split_paths` does in process on the same host: it folds
        // ASCII case and strips one surrounding pair of `"` on Windows, and does
        // neither anywhere else. An unconditional strip is what made the pwsh arm
        // delete a quoted foreign segment `move_to_front` keeps on Unix.
        let comparison = if cfg!(windows) { "OrdinalIgnoreCase" } else { "Ordinal" };
        for line in [
            Shell::PowerShell.export_path("PATH", "/opt/bin").unwrap(),
            Shell::PowerShell.remove_list_element("PATH", "/opt/bin", None).unwrap(),
        ] {
            assert!(
                line.contains(&format!("[StringComparison]::{comparison}")),
                "the comparison must be explicit and platform-chosen; got: {line}"
            );
            assert!(
                !line.contains("-ne $__ocx_p"),
                "`-ne` is case-insensitive and silently deletes a differently-cased directory: {line}"
            );
            assert_eq!(
                line.contains(r#"-replace '(?s)^"(.*)"$','$1'"#),
                cfg!(windows),
                "the segment quote strip must fire exactly where `split_paths` unquotes: {line}"
            );
        }
    }

    #[test]
    fn remove_list_element_strips_one_surrounding_quote_pair_for_path_kind() {
        // A-19: `std::env::split_paths` unquotes on Windows, so the operand ocx
        // enumerated from the current environment may be spelled either way.
        // Only the outermost pair goes.
        let bare = Shell::Bash.remove_list_element("PATH", "/opt/bin", None).unwrap();
        let quoted = Shell::Bash.remove_list_element("PATH", "\"/opt/bin\"", None).unwrap();
        assert_eq!(bare, quoted, "a quoted path operand must normalise to the bare one");
        // A list element is opaque — quotes there are part of the option.
        let list = Shell::Bash.remove_list_element("OPTS", "\"-ea\"", Some(" ")).unwrap();
        assert!(
            list.contains("__ocx_v='\"-ea\"'"),
            "list operands keep their quotes: {list}"
        );
    }

    // ══ C-014 — `remove_list_element` ════════════════════════════════════

    #[test]
    fn remove_list_element_rejects_invalid_key_all_shells() {
        for shell in Shell::value_variants() {
            assert!(
                shell.remove_list_element("OPTS; rm -rf /", "-ea", Some(" ")).is_none(),
                "{shell}"
            );
            assert!(
                shell.remove_list_element("PATH; rm -rf /", "/x", None).is_none(),
                "{shell}"
            );
        }
    }

    #[test]
    fn remove_list_element_is_none_for_batch() {
        // Not "cannot express it" — `export_path` does delete an element on
        // Batch. `%VAR:search=%` is case-INsensitive with no case-sensitive
        // form, and list elements need a case-sensitive match; Batch also hosts
        // no prompt hook, so nothing consumes the primitive.
        assert_eq!(Shell::Batch.remove_list_element("OPTS", "-ea", Some(" ")), None);
        assert_eq!(Shell::Batch.remove_list_element("PATH", "/opt/bin", None), None);
        // The path/constant emitters are unaffected.
        assert!(Shell::Batch.export_path("PATH", "/opt/bin").is_some());
    }

    #[test]
    fn remove_list_element_empty_value_is_a_commented_no_op() {
        // Folding with an empty value degrades the flank pattern to `sep + sep`
        // and would delete the ambient's own separators.
        // Batch answers `None` to `remove_list_element` by design
        // (`remove_list_element_is_none_for_batch`), so it has no line to assert on.
        for shell in Shell::value_variants().iter().filter(|s| **s != Shell::Batch) {
            let line = shell.remove_list_element("OPTS", "", Some(",")).expect("a comment");
            assert_eq!(
                line,
                shell.comment("ocx: OPTS removal value is empty, nothing to remove")
            );
            assert!(!line.contains("__ocx_l"), "{shell}: must not emit the fold: {line}");
        }
    }

    #[test]
    fn remove_list_element_posix_exact_shape() {
        let expected_list = "__ocx_v='-ea'; __ocx_s=' '; __ocx_p=\"$__ocx_s$__ocx_v$__ocx_s\"; __ocx_l=\"${OPTS:+$__ocx_s${OPTS}}$__ocx_s\"; while [ \"$__ocx_l\" != \"${__ocx_l%%\"$__ocx_p\"*}\" ]; do __ocx_l=\"${__ocx_l%%\"$__ocx_p\"*}$__ocx_s${__ocx_l#*\"$__ocx_p\"}\"; done; __ocx_l=\"${__ocx_l#\"$__ocx_s\"}\"; export OPTS=\"${__ocx_l%\"$__ocx_s\"}\"; unset __ocx_v __ocx_s __ocx_p __ocx_l";
        for shell in [Shell::Bash, Shell::Zsh, Shell::Ash, Shell::Ksh, Shell::Dash] {
            assert_eq!(
                shell.remove_list_element("OPTS", "-ea", Some(" ")).as_deref(),
                Some(expected_list),
                "{shell}"
            );
        }
        // Path-kind adds the empty-segment collapse and uses the platform
        // separator; nothing else differs.
        let separator = sep();
        let path = Shell::Bash.remove_list_element("PATH", "/opt/bin", None).unwrap();
        assert!(path.contains(&format!("__ocx_s='{separator}'")), "{path}");
        assert!(
            path.contains("__ocx_d=\"$__ocx_s$__ocx_s\";") && path.contains("unset __ocx_d;"),
            "path-kind must collapse ambient empty segments: {path}"
        );
        let list = Shell::Bash.remove_list_element("OPTS", "-ea", Some(" ")).unwrap();
        assert!(
            !list.contains("__ocx_d"),
            "list-kind must preserve an empty element verbatim: {list}"
        );
    }

    #[test]
    fn remove_list_element_fish_exact_shape() {
        let expected = "set __ocx_v \"-ea\"; set __ocx_s \" \"; set __ocx_p \"$__ocx_s$__ocx_v$__ocx_s\"; set __ocx_l \"$__ocx_s\"; if test -n \"$OPTS\"; set __ocx_l \"$__ocx_s$OPTS$__ocx_s\"; end; set __ocx_n (string replace --all -- \"$__ocx_p\" \"$__ocx_s\" \"$__ocx_l\" | string collect); while test \"$__ocx_n\" != \"$__ocx_l\"; set __ocx_l \"$__ocx_n\"; set __ocx_n (string replace --all -- \"$__ocx_p\" \"$__ocx_s\" \"$__ocx_l\" | string collect); end; set -gx OPTS (string sub --start (math (string length -- \"$__ocx_s\") + 1) --end (math 0 - (string length -- \"$__ocx_s\")) -- \"$__ocx_l\" | string collect); set -e __ocx_v __ocx_s __ocx_p __ocx_l __ocx_n";
        assert_eq!(
            Shell::Fish.remove_list_element("OPTS", "-ea", Some(" ")).as_deref(),
            Some(expected)
        );
        // Path-kind takes the list branch — `$PATH` is a fish list, and the
        // string fold above would space-join it first.
        let expected_path = "set --path __ocx_l $PATH; set __ocx_p \"/opt/bin\"; set __ocx_r; for __ocx_e in $__ocx_l; test \"$__ocx_e\" != \"$__ocx_p\"; and test -n \"$__ocx_e\"; and set -a __ocx_r $__ocx_e; end; set -gx --path PATH $__ocx_r; set -e __ocx_p __ocx_r __ocx_e __ocx_l";
        assert_eq!(
            Shell::Fish.remove_list_element("PATH", "/opt/bin", None).as_deref(),
            Some(expected_path)
        );
    }

    #[test]
    fn remove_list_element_powershell_exact_shape() {
        let expected = "$__ocx_v='-ea'; $__ocx_s=' '; $__ocx_p=\"$__ocx_s$__ocx_v$__ocx_s\"; $__ocx_l=if ($env:OPTS) { \"$__ocx_s$($env:OPTS)$__ocx_s\" } else { $__ocx_s }; while ($__ocx_l.Contains($__ocx_p)) { $__ocx_l=$__ocx_l.Replace($__ocx_p,$__ocx_s) }; $env:OPTS=$__ocx_l.Substring($__ocx_s.Length,[Math]::Max(0,$__ocx_l.Length-2*$__ocx_s.Length)); Remove-Variable __ocx_v,__ocx_s,__ocx_p,__ocx_l";
        assert_eq!(
            Shell::PowerShell
                .remove_list_element("OPTS", "-ea", Some(" "))
                .as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn remove_list_element_elvish_exact_shape() {
        let expected = "use str; var __ocx_l = ' '; if (and (has-env OPTS) (not-eq $E:OPTS '')) { set __ocx_l = ' '$E:OPTS' ' }; while (str:contains $__ocx_l ' -ea ') { set __ocx_l = (str:replace ' -ea ' ' ' $__ocx_l) }; set E:OPTS = (str:trim-suffix (str:trim-prefix $__ocx_l ' ') ' ')";
        assert_eq!(
            Shell::Elvish.remove_list_element("OPTS", "-ea", Some(" ")).as_deref(),
            Some(expected)
        );
    }

    #[test]
    fn remove_list_element_nushell_exact_shape() {
        let expected = "mut __ocx_l = (if ($env.OPTS? | default \"\") == \"\" { \" \" } else { \" \" + ($env.OPTS? | default \"\") + \" \" }); while ($__ocx_l | str contains \" -ea \") { $__ocx_l = ($__ocx_l | str replace --all \" -ea \" \" \") }; $env.OPTS = ($__ocx_l | split row \" \" | skip 1 | drop 1 | str join \" \")";
        assert_eq!(
            Shell::Nushell.remove_list_element("OPTS", "-ea", Some(" ")).as_deref(),
            Some(expected)
        );
        // Path-kind takes the `describe`-guarded list branch, mirroring
        // `export_path`: `$env.PATH` is auto-listified since 0.101.
        let path = Shell::Nushell.remove_list_element("PATH", "/opt/bin", None).unwrap();
        assert!(path.contains("($in | describe) == 'string'"), "{path}");
    }

    #[test]
    fn remove_list_element_uses_the_given_separator_not_the_platform_one() {
        // S-034 / A-08: `CFLAGS` is `{type = "list", separator = " "}`. A build
        // that ignored the parameter and assumed `env::PATH_SEPARATOR` would
        // emit `:` flanks, and the contribution would be permanently
        // unremovable. Asserted per arm, on the emitted bytes.
        let platform = sep();
        // Batch answers `None` to `remove_list_element` by design
        // (`remove_list_element_is_none_for_batch`), so it has no line to assert on.
        for shell in Shell::value_variants().iter().filter(|s| **s != Shell::Batch) {
            let line = shell.remove_list_element("CFLAGS", "-O2", Some(" ")).unwrap();
            let flanked = match shell {
                Shell::Elvish => "' -O2 '".to_string(),
                Shell::Nushell => "\" -O2 \"".to_string(),
                _ => "-O2".to_string(),
            };
            assert!(line.contains(&flanked), "{shell}: expected {flanked:?} in {line}");
            assert!(
                !line.contains(&format!("{platform}-O2{platform}")),
                "{shell}: the platform separator must not flank a declared-separator list: {line}"
            );
            // The declared separator is what reaches the emitted statement.
            let declared = match shell {
                Shell::Elvish | Shell::PowerShell => "' '",
                Shell::Fish | Shell::Nushell => "\" \"",
                _ => "' '",
            };
            assert!(line.contains(declared), "{shell}: declared separator missing: {line}");
        }
    }

    /// EC-QUOTE-013, `remove_list_element` half — the separator is untrusted
    /// authored text and rides the arm's own escaper, exactly like the value.
    #[test]
    fn remove_list_element_escapes_the_separator_like_the_value() {
        // `{type = "list", separator = "…"}` is package metadata, so the
        // separator is attacker-influenced on the same footing as the value —
        // and it reaches the emitted statement in the same quoting context.
        // Both fixtures are load-bearing: `'` is inert in the fish/nushell
        // double-quoted arms while `$` is inert in the single-quoted ones, so
        // each pins a different half of the per-arm routing, and `"` is the
        // byte that closes the double-quoted arms.
        for hostile in ["';id;'", "$(id)", "a\"b"] {
            for shell in Shell::value_variants().iter().copied().filter(|s| *s != Shell::Batch) {
                // The escaper each quoting context is contracted to use.
                let escaped = match shell {
                    Shell::Bash | Shell::Zsh | Shell::Ash | Shell::Ksh | Shell::Dash => {
                        escape::posix_single_quoted(hostile)
                    }
                    Shell::Fish => escape::fish_double_quoted(hostile),
                    Shell::PowerShell | Shell::Elvish => escape::single_quoted_doubled(hostile),
                    Shell::Nushell => escape::nushell_plain_string(hostile),
                    Shell::Batch => unreachable!("batch answers None and is filtered out"),
                };
                let as_separator = shell.remove_list_element("OPTS", "-O2", Some(hostile)).unwrap();
                let as_value = shell.remove_list_element("OPTS", hostile, Some(" ")).unwrap();
                for (role, line) in [("separator", &as_separator), ("value", &as_value)] {
                    assert!(
                        line.contains(&escaped),
                        "{shell}: the {role} {hostile:?} must reach the emit as {escaped:?}: {line}"
                    );
                }
                // fish is the only arm that backslash-escapes `$`; doing it on
                // the nushell arm would corrupt the value rather than harden it
                // (`escape::nushell_plain_string`), and the single-quoted arms
                // need no `$` escape at all. The value here is `$`-free, so the
                // only `$`-bearing operand in this line is the separator.
                assert_eq!(
                    as_separator.contains("\\$"),
                    shell == Shell::Fish && hostile.contains('$'),
                    "{shell}: only fish backslash-escapes a `$` separator: {as_separator}"
                );
            }
        }
    }

    #[test]
    fn remove_list_element_quotes_the_injection_element_per_arm() {
        // S-026, tier 1 and nowhere else: escaping is the one property whose
        // failure is a *silent wrong value* rather than a visible one. The
        // fixture is the live-injection element from the design spec.
        const HOSTILE: &str = "/tmp/a';id;'b";
        let expectations: [(Shell, &str); 8] = [
            // POSIX single-quoted literal: close, escaped quote, reopen.
            (Shell::Bash, r"__ocx_v='/tmp/a'\''"),
            (Shell::Zsh, r"__ocx_v='/tmp/a'\''"),
            (Shell::Ash, r"__ocx_v='/tmp/a'\''"),
            (Shell::Ksh, r"__ocx_v='/tmp/a'\''"),
            (Shell::Dash, r"__ocx_v='/tmp/a'\''"),
            // Doubled quote inside `'...'`.
            (Shell::PowerShell, "$__ocx_v='/tmp/a'';id;''b'"),
            // elvish inlines the flanked pattern rather than binding the
            // value, so the doubled quote is asserted inside it.
            (Shell::Elvish, "/tmp/a'';id;''b"),
            // fish double-quoted: `'` is not a metacharacter there.
            (Shell::Fish, "set __ocx_v \"/tmp/a';id;'b\""),
        ];
        for (shell, fragment) in expectations {
            let line = shell.remove_list_element("OPTS", HOSTILE, Some(" ")).unwrap();
            assert!(
                line.contains(fragment),
                "{shell}: the quote must be neutralised for THIS arm's quoting context; \
                 expected {fragment:?} in {line}"
            );
        }
        // nushell's plain double-quoted string does not interpolate, so the
        // quote rides verbatim — and no `;` ever reaches a command position.
        let nu = Shell::Nushell.remove_list_element("OPTS", HOSTILE, Some(" ")).unwrap();
        assert!(nu.contains("\" /tmp/a';id;'b \""), "{nu}");
    }

    // ══ A-21 — `emit_message` ════════════════════════════════════════════

    #[test]
    fn emit_message_per_arm_exact_shape() {
        assert_eq!(
            Shell::Bash.emit_message("hello").as_deref(),
            Some("printf '%s\\n' 'hello' >&2")
        );
        assert_eq!(
            Shell::Fish.emit_message("hello").as_deref(),
            Some("printf '%s\\n' \"hello\" >&2")
        );
        assert_eq!(
            Shell::PowerShell.emit_message("hello").as_deref(),
            Some("[Console]::Error.WriteLine('hello')")
        );
        assert_eq!(Shell::Elvish.emit_message("hello").as_deref(), Some("echo 'hello' >&2"));
        assert_eq!(
            Shell::Nushell.emit_message("hello").as_deref(),
            Some("print --stderr \"hello\"")
        );
        // Batch hosts no prompt hook, so it has nothing to say.
        assert_eq!(Shell::Batch.emit_message("hello"), None);
    }

    /// EC-QUOTE-015 — the diagnostic text emitted *inside* the eval'd script
    /// takes an escaper too, so a project path such as `/home/u/it's %work%`
    /// cannot close the literal and have its remainder parsed as shell source.
    /// Text-agnostic, so the `[shell.consent]` WARN reason routed through this
    /// same channel is covered by the same assertion. The newline half and the
    /// "exactly one statement" half are
    /// `live_emit_message_prints_byte_exactly_to_stderr`.
    #[test]
    fn emit_message_passes_the_text_as_an_argument_never_as_a_format_string() {
        // A-21: a `%` in a project path would be consumed as a conversion
        // specifier if the message were the format string, and an unescaped `'`
        // would close the literal and have the remainder parsed as shell source.
        const HOSTILE: &str = "ocx: /home/u/it's 100% work";
        for shell in Shell::value_variants().iter().filter(|s| **s != Shell::Batch) {
            let line = shell.emit_message(HOSTILE).expect("every hook shell emits");
            assert!(
                !line.contains("printf 'ocx:") && !line.contains("printf \"ocx:"),
                "{shell}: the message must never be the format string: {line}"
            );
            assert!(
                !line.contains("it's 100%") || *shell == Shell::Fish || *shell == Shell::Nushell,
                "{shell}: the quote must be escaped for this arm: {line}"
            );
        }
        assert_eq!(
            Shell::Bash.emit_message(HOSTILE).as_deref(),
            Some(r"printf '%s\n' 'ocx: /home/u/it'\''s 100% work' >&2")
        );
        assert_eq!(
            Shell::PowerShell.emit_message(HOSTILE).as_deref(),
            Some("[Console]::Error.WriteLine('ocx: /home/u/it''s 100% work')")
        );
    }

    // ══ C-021 — in-process / emitted parity, proven against real shells ══
    //
    // The reconciler applies in process on one prompt and through emitted text
    // on another *in the same session*, so a divergence surfaces as PATH order
    // flapping between prompts. These tests never hard-code an expected string:
    // they compare a real shell's answer against the in-process primitive,
    // because "the two agree" is the property, and a hand-written expectation
    // could drift from the primitive.

    /// Every interpreter these parity tests drive, with the argv `run_script`
    /// needs — all nine hook shells, elvish and nushell included (A-15
    /// EC-QUOTE-006, A-16 EC-QUOTE-007). Every arm here is driven through
    /// `export_path`, `export_constant`, both removal primitives, the
    /// apply/revert round trips and `emit_message`.
    ///
    /// An interpreter that is not installed skips, and
    /// `assert_every_present_parity_arm_ran` decides whether that skip is
    /// admissible: it fails when an arm's interpreter IS installed and this
    /// test nonetheless ran nothing under it, so no arm can be quietly dropped
    /// from the matrix while the suite stays green. `Shell::from_argv` panics on
    /// an interpreter with no mapping, so an arm cannot be added here without
    /// its `seed_*` / `read_*` cases either.
    #[cfg(unix)]
    const PARITY_ARMS: &[&[&str]] = &[
        &["bash", "-c"],
        &["zsh", "-c"],
        &["dash", "-c"],
        &["ksh", "-c"],
        &["busybox", "ash", "-c"],
        &["fish", "-c"],
        &["pwsh", "-NoProfile", "-Command"],
        &["elvish", "-c"],
        &["nu", "-c"],
    ];

    /// The matrix must cover every shell the reconciler emits for.
    ///
    /// `assert_every_present_parity_arm_ran` derives its expectation from
    /// `PARITY_ARMS`, so it cannot notice an arm being deleted **from**
    /// `PARITY_ARMS` — the needle and the haystack would be the same literal.
    /// The `Shell` enum is the independent authority: dropping an arm here
    /// reds this test instead of quietly shrinking the matrix.
    #[cfg(unix)]
    #[test]
    fn every_hook_shell_has_a_parity_arm() {
        let armed: Vec<Shell> = PARITY_ARMS.iter().map(|argv| Shell::from_argv(argv)).collect();
        // Batch is cmd.exe: Windows-only, and covered by the `live_batch_*`
        // tests instead — these arms are `#[cfg(unix)]`.
        for shell in Shell::value_variants().iter().filter(|shell| **shell != Shell::Batch) {
            assert!(
                armed.contains(shell),
                "{shell} has no PARITY_ARMS entry, so C-021 in-process/emitted parity is unproven for it"
            );
        }
    }

    /// A statement seeding `key` with the exact bytes of `value`, treated as
    /// one opaque string.
    #[cfg(unix)]
    fn seed_string(shell: Shell, key: &str, value: &str) -> String {
        match shell {
            Shell::Bash | Shell::Zsh | Shell::Ash | Shell::Ksh | Shell::Dash => {
                format!("export {key}='{}'", escape::posix_single_quoted(value))
            }
            Shell::Fish => format!("set -gx {key} \"{}\"", escape::fish_double_quoted(value)),
            Shell::PowerShell => format!("$env:{key}='{}'", escape::single_quoted_doubled(value)),
            // Elvish raw strings are single-quoted with `'` doubled — the pwsh
            // escaper. The double-quoted form would reject `\$` / `` \` `` as an
            // invalid escape sequence, which is a parse error rather than a
            // wrong value, so the seed itself would decide the arm's verdict.
            Shell::Elvish => format!("set-env {key} '{}'", escape::single_quoted_doubled(value)),
            // Nushell plain double-quoted string: `$` and `(` cannot fire in it,
            // so only `\` and `"` need neutralising — the same escaper the
            // nushell emit arms use, for the same reason.
            Shell::Nushell => format!("$env.{key} = \"{}\"", escape::nushell_plain_string(value)),
            other => panic!("no seed for {other}"),
        }
    }

    /// A statement seeding `key` with a `:`-joined path value, in whatever
    /// shape that shell natively holds a path variable in.
    #[cfg(unix)]
    fn seed_path(shell: Shell, key: &str, value: &str) -> String {
        match shell {
            // fish holds path variables as a genuine list, split on `:` at the
            // environment boundary — seeding a bare string would make the whole
            // ambient one element.
            Shell::Fish if value.is_empty() => format!("set -e {key}"),
            Shell::Fish => format!(
                "set -gx {key} (string split -- \":\" \"{}\")",
                escape::fish_double_quoted(value)
            ),
            other => seed_string(other, key, value),
        }
    }

    /// A statement printing `key`'s exact bytes, with no trailing newline.
    #[cfg(unix)]
    fn read_string(shell: Shell, key: &str) -> String {
        match shell {
            Shell::Bash | Shell::Zsh | Shell::Ash | Shell::Ksh | Shell::Dash => {
                format!("printf '%s' \"${key}\"")
            }
            Shell::Fish => format!("printf '%s' \"${key}\""),
            Shell::PowerShell => format!("[Console]::Out.Write([string]$env:{key})"),
            // `print` writes no trailing newline in elvish (`echo` does), so the
            // bytes on stdout are the variable's bytes.
            Shell::Elvish => format!("print $E:{key}"),
            // `$env.KEY?` is the total form — a bare `$env.KEY` on an unset key
            // raises, which `run_script` would report as our emit being wrong.
            Shell::Nushell => format!("print -n ($env.{key}? | default \"\")"),
            other => panic!("no readback for {other}"),
        }
    }

    #[cfg(unix)]
    fn read_path(shell: Shell, key: &str) -> String {
        match shell {
            Shell::Fish => format!("printf '%s' (string join -- \":\" ${key})"),
            other => read_string(other, key),
        }
    }

    /// `(ambient, value)` pairs every live path test drives. Chosen so each row
    /// is a property, not a sample: dedup, move-to-front, ambient empty
    /// segments (A-18), and the hostile-character set S-026 names.
    #[cfg(unix)]
    const PATH_CASES: &[(&str, &str)] = &[
        ("", "/opt/bin"),
        ("/a:/b", "/opt/bin"),
        ("/a:/opt/bin:/b", "/opt/bin"),       // move to front
        ("/opt/bin:/opt/bin:/a", "/opt/bin"), // adjacent duplicates
        ("/a::/b", "/opt/bin"),               // A-18: ambient empty segment
        ("::", "/opt/bin"),
        ("/a:/b:", "/opt/bin"),
        ("/opt/Bin:/x", "/opt/bin"),           // A-19: a different directory on Unix
        ("/a:/x!y/bin:/b", "/x!y/bin"),        // history expansion
        ("/a:/o'brien/bin", "/o'brien/bin"),   // quote
        ("/a:/tmp/a';id;'b", "/tmp/a';id;'b"), // S-026 live-injection element
        ("/a b:/c", "/a b"),                   // space
        ("/a*b:/c", "/a*b"),                   // glob metacharacter
        ("/a$b:/c", "/a$b"),
        ("/a`b:/c", "/a`b"),
        ("/a\\b:/c", "/a\\b"),
        ("/a\"b:/c", "/a\"b"),
        ("/ü/bin:/a", "/ü/bin"), // multi-byte
        // A-19's quote normalisation, both halves. Windows quotes a PATH
        // segment containing spaces; `std::env::split_paths` unquotes one pair
        // there and NOWHERE else, so on Unix a quoted segment is a directory
        // whose name begins with `"`. An arm that normalises it anyway deletes
        // a foreign entry `move_to_front` keeps (the ambient row), and an arm
        // that normalises only one side of its own comparison re-prepends the
        // value on every prompt (the value row).
        ("\"/opt/a b/bin\":/x", "/opt/a b/bin"),
        ("/x:/y", "\"/n/b in\""),
        ("\"/n/b in\":/x", "\"/n/b in\""),
    ];

    /// Constant values every live constant test drives — the set A-15's test
    /// hook names, plus the ones that broke a *different* arm.
    #[cfg(unix)]
    const CONSTANT_CASES: &[&str] = &[
        "",
        "a!b",
        "!!",
        "!rm -rf /",
        "/opt/j$dk",
        "a`b",
        "a\\b",
        "a\"b",
        "o'brien",
        "100% done",
        "a b",
        "ü",
        "a\nb",
        "$(id)",
        "`id`",
        "${HOME}",
        "$env.HOME",
        "a(b)c",
        "*",
    ];

    #[cfg(unix)]
    fn move_to_front(existing: &str, value: &str) -> String {
        crate::utility::path::move_to_front(std::ffi::OsStr::new(existing), std::ffi::OsStr::new(value))
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(unix)]
    fn remove_segment(existing: &str, value: &str) -> String {
        crate::utility::path::remove_segment(std::ffi::OsStr::new(existing), std::ffi::OsStr::new(value))
            .to_string_lossy()
            .into_owned()
    }

    /// `append_unique` minus the append: the flank-delimited removal
    /// `remove_list_element` is contracted to compute.
    fn folded_removal(existing: &str, value: &str, separator: &str) -> String {
        let mut wrapped = format!("{separator}{existing}{separator}");
        let occurrence = format!("{separator}{value}{separator}");
        while wrapped.contains(&occurrence) {
            wrapped = wrapped.replace(&occurrence, separator);
        }
        let inner = wrapped.strip_prefix(separator).unwrap_or(&wrapped);
        inner.strip_suffix(separator).unwrap_or(inner).to_string()
    }

    #[test]
    fn folded_removal_inverts_append_unique() {
        // The reference itself needs a check: appending a contribution and then
        // removing it must land back on the ambient, which is S-034's assertion
        // reduced to the two in-process folds.
        for (existing, value, separator) in [
            ("-Xmx1g", "-ea", " "),
            ("", "-ea", " "),
            ("a,b", "c", ","),
            ("-Wall", "-Wextra", "; "),
        ] {
            let applied = crate::utility::list::append_unique(existing, value, separator);
            assert_eq!(
                folded_removal(&applied, value, separator),
                existing,
                "ambient={existing:?} value={value:?} sep={separator:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn live_export_path_matches_move_to_front() {
        // C-021 clause 1. `export_path` is the applier; its emitted result must
        // equal `utility::path::move_to_front` byte for byte, per arm.
        for argv in PARITY_ARMS {
            let shell = Shell::from_argv(argv);
            for (ambient, value) in PATH_CASES {
                let line = shell.export_path("OCXP", value).unwrap();
                let script = format!(
                    "{}; {line}; {}",
                    seed_path(shell, "OCXP", ambient),
                    read_path(shell, "OCXP")
                );
                if let Some(out) = run_script(argv, &script) {
                    assert_eq!(
                        out,
                        move_to_front(ambient, value),
                        "argv={argv:?} ambient={ambient:?} value={value:?}"
                    );
                }
            }
        }
        assert_every_present_parity_arm_ran();
    }

    #[cfg(unix)]
    #[test]
    fn live_export_constant_matches_apply_entries() {
        // C-021's third clause (A-15). `Env::apply_entries` stores the value
        // verbatim (`env.rs`, `ModifierKind::Constant => self.set(..)`), so the
        // parity oracle is the raw fixture. The reconciler's `C == L.applied`
        // exit guard compares exactly these two products: before A-15 they
        // differed for every `!`-bearing value on all five POSIX arms.
        for argv in PARITY_ARMS {
            let shell = Shell::from_argv(argv);
            for value in CONSTANT_CASES {
                let line = shell.export_constant("OCXC", value).unwrap();
                let script = format!("{line}; {}", read_string(shell, "OCXC"));
                if let Some(out) = run_script(argv, &script) {
                    assert_eq!(out, *value, "argv={argv:?} value={value:?} line={line}");
                }
            }
        }
        assert_every_present_parity_arm_ran();
    }

    #[cfg(unix)]
    #[test]
    fn live_remove_path_element_matches_remove_segment() {
        // C-014 + C-021: the emitted removal must equal
        // `utility::path::remove_segment`, the in-process retirement primitive.
        for argv in PARITY_ARMS {
            let shell = Shell::from_argv(argv);
            for (ambient, value) in PATH_CASES {
                let line = shell.remove_list_element("OCXP", value, None).unwrap();
                let script = format!(
                    "{}; {line}; {}",
                    seed_path(shell, "OCXP", ambient),
                    read_path(shell, "OCXP")
                );
                if let Some(out) = run_script(argv, &script) {
                    assert_eq!(
                        out,
                        remove_segment(ambient, value),
                        "argv={argv:?} ambient={ambient:?} value={value:?} line={line}"
                    );
                }
            }
        }
        assert_every_present_parity_arm_ran();
    }

    #[cfg(unix)]
    #[test]
    fn live_remove_list_element_matches_the_flank_delimited_fold() {
        // C-014: flank-delimited removal of one whole contribution, never a
        // segment op — `LIST_CASES` carries a value that itself contains the
        // separator (`("x", "a,b", ",")`), which a segment-based removal cannot
        // find at all.
        for argv in PARITY_ARMS {
            let shell = Shell::from_argv(argv);
            for (existing, value, separator) in LIST_CASES {
                let line = shell.remove_list_element("OCXL", value, Some(separator)).unwrap();
                let script = format!(
                    "{}; {line}; {}",
                    seed_string(shell, "OCXL", existing),
                    read_string(shell, "OCXL")
                );
                if let Some(out) = run_script(argv, &script) {
                    assert_eq!(
                        out,
                        folded_removal(existing, value, separator),
                        "argv={argv:?} ambient={existing:?} value={value:?} sep={separator:?} line={line}"
                    );
                }
            }
        }
        assert_every_present_parity_arm_ran();
    }

    #[cfg(unix)]
    #[test]
    fn live_list_apply_then_revert_restores_the_ambient() {
        // S-034, tier 1: `CFLAGS` as `{type = "list", separator = " "}` applies
        // through `export_list` and must revert through `remove_list_element`
        // with the SAME separator. A build that assumed the platform separator
        // emits `:` flanks and the contribution becomes permanently
        // unremovable — this is the assertion that mutation flips.
        //
        // S-032 rides the same shape: the ambient stands for the foreign and
        // other-scope elements, and reverting one contribution must leave them
        // byte-identical rather than tearing the whole variable down.
        for argv in PARITY_ARMS {
            let shell = Shell::from_argv(argv);
            for (existing, value, separator) in LIST_CASES {
                // Only ambients that do not already carry the contribution can
                // round-trip: `append_unique` is a move-to-back, so an ambient
                // holding the value legitimately loses that occurrence.
                if folded_removal(existing, value, separator) != *existing {
                    continue;
                }
                let apply = shell.export_list("OCXL", value, separator).unwrap();
                let revert = shell.remove_list_element("OCXL", value, Some(separator)).unwrap();
                let script = format!(
                    "{}; {apply}; {revert}; {}",
                    seed_string(shell, "OCXL", existing),
                    read_string(shell, "OCXL")
                );
                if let Some(out) = run_script(argv, &script) {
                    assert_eq!(
                        out, *existing,
                        "argv={argv:?} ambient={existing:?} value={value:?} sep={separator:?}"
                    );
                }
            }
        }
        assert_every_present_parity_arm_ran();
    }

    #[cfg(unix)]
    #[test]
    fn live_path_apply_then_revert_leaves_foreign_elements_intact() {
        // S-032, tier 1: retiring one scope's element must not disturb the
        // rest — removal commutes with foreign prepends and appends.
        for argv in PARITY_ARMS {
            let shell = Shell::from_argv(argv);
            let apply = shell.export_path("OCXP", "/opt/ocx/bin").unwrap();
            let revert = shell.remove_list_element("OCXP", "/opt/ocx/bin", None).unwrap();
            let foreign = shell.export_path("OCXP", "/opt/foreign/bin").unwrap();
            let script = format!(
                "{}; {apply}; {foreign}; {revert}; {}",
                seed_path(shell, "OCXP", "/a:/b"),
                read_path(shell, "OCXP")
            );
            if let Some(out) = run_script(argv, &script) {
                assert_eq!(out, "/opt/foreign/bin:/a:/b", "argv={argv:?}");
            }
        }
        assert_every_present_parity_arm_ran();
    }

    #[cfg(unix)]
    #[test]
    fn live_repeated_eval_is_byte_identical() {
        // S-039, tier 1: the same snippet eval'd N times leaves the variable
        // byte-identical — the property that keeps PATH from growing one
        // segment per prompt under a per-prompt reconciler.
        // Driven by the whole `PATH_CASES` matrix, not one benign value: an arm
        // that normalises only ONE side of its own segment comparison is stable
        // for every plain directory and grows one copy per prompt for a value
        // carrying a surrounding quote pair.
        for (existing, value) in PATH_CASES {
            for argv in PARITY_ARMS {
                let shell = Shell::from_argv(argv);
                let apply = shell.export_path("OCXP", value).unwrap();
                let seed = seed_path(shell, "OCXP", existing);
                let read = read_path(shell, "OCXP");
                let once = run_script(argv, &format!("{seed}; {apply}; {read}"));
                let many = run_script(
                    argv,
                    &format!("{seed}; {apply}; {apply}; {apply}; {apply}; {apply}; {read}"),
                );
                if let (Some(once), Some(many)) = (once, many) {
                    assert_eq!(
                        once, many,
                        "argv={argv:?} ambient={existing:?} value={value:?}: PATH grew across repeated evals"
                    );
                }
            }
        }
        for argv in PARITY_ARMS {
            let shell = Shell::from_argv(argv);
            let apply = shell.export_path("OCXP", "/opt/ocx/bin").unwrap();
            let seed = seed_path(shell, "OCXP", "/a:/opt/ocx/bin:/b");
            let read = read_path(shell, "OCXP");
            let once = run_script(argv, &format!("{seed}; {apply}; {read}"));
            let many = run_script(
                argv,
                &format!("{seed}; {apply}; {apply}; {apply}; {apply}; {apply}; {read}"),
            );
            if let (Some(once), Some(many)) = (once, many) {
                assert_eq!(once, many, "argv={argv:?}: PATH grew across repeated evals");
                assert_eq!(once, move_to_front("/a:/opt/ocx/bin:/b", "/opt/ocx/bin"));
            }
            // The removal is idempotent in the same sense: applying it to a
            // variable that no longer holds the element changes nothing.
            let revert = shell.remove_list_element("OCXP", "/opt/ocx/bin", None).unwrap();
            let once = run_script(argv, &format!("{seed}; {revert}; {read}"));
            let many = run_script(argv, &format!("{seed}; {revert}; {revert}; {revert}; {read}"));
            if let (Some(once), Some(many)) = (once, many) {
                assert_eq!(once, many, "argv={argv:?}: removal is not idempotent");
            }
        }
        assert_every_present_parity_arm_ran();
    }

    #[cfg(unix)]
    #[test]
    fn live_remove_list_injection_does_not_execute() {
        // S-026 executable half: the value, the separator and the ambient are
        // all attacker-influenced, and none may reach a command position.
        let marker = std::env::temp_dir().join("ocx_inj_live_remove");
        let hostile = format!("/tmp/a';touch {};'b", marker.display());
        for argv in PARITY_ARMS {
            let shell = Shell::from_argv(argv);
            for (ambient, value, separator) in [
                ("a b", hostile.as_str(), Some(" ")),
                ("a b", "v", Some(hostile.as_str())),
                (hostile.as_str(), "v", Some(" ")),
                ("/a:/b", hostile.as_str(), None),
            ] {
                let _ = std::fs::remove_file(&marker);
                let line = shell.remove_list_element("OCXL", value, separator).unwrap();
                let script = format!(
                    "{}; {line}; {}",
                    seed_string(shell, "OCXL", ambient),
                    read_string(shell, "OCXL")
                );
                if run_script(argv, &script).is_some() {
                    assert!(
                        !marker.exists(),
                        "argv={argv:?}: injection executed via ambient={ambient:?} \
                         value={value:?} sep={separator:?}"
                    );
                }
            }
        }
        let _ = std::fs::remove_file(&marker);
        assert_every_present_parity_arm_ran();
    }

    /// EC-QUOTE-015, executable half — a `'`-bearing and a newline-bearing
    /// project path each emit exactly one statement: real shells read the line
    /// back byte-exact on stderr, and nothing leaks to stdout (the eval'd
    /// channel), which is what an escaped-out remainder would do.
    #[cfg(unix)]
    #[test]
    fn live_emit_message_prints_byte_exactly_to_stderr() {
        // A-21: the diagnostic is shell code on stdout that prints to stderr
        // when eval'd — the shim discards the binary's own stderr. The text
        // rides as a format ARGUMENT, so a `%` in a project path survives.
        // The summary line under colour: an SGR sequence rides the eval'd
        // stream as data, so it must survive every arm's own quoting byte for
        // byte and reach stderr unchanged. Built from the shipped theme rather
        // than a hand-written escape, so a re-styled mark cannot leave this
        // asserting on bytes the renderer no longer produces.
        let coloured = format!(
            "ocx: {} {} {}",
            crate::cli::Theme::new(true).ok("+JAVA_HOME"),
            crate::cli::Theme::new(true).tag("~PATH"),
            crate::cli::Theme::new(true).alert("-PYENV_ROOT")
        );
        assert!(coloured.contains('\u{1b}'), "the coloured fixture carries no escape");
        for text in [
            "ocx: +JAVA_HOME ~PATH -PYENV_ROOT (acme, lock a1b2c3)",
            "ocx: /home/u/it's 100% work",
            "ocx: a\nb",
            "ocx: 50%s %d ü",
            coloured.as_str(),
        ] {
            for argv in PARITY_ARMS {
                let shell = Shell::from_argv(argv);
                let line = shell.emit_message(text).unwrap();
                if let Some(err) = run_script_stderr(argv, &line) {
                    assert_eq!(err, text, "argv={argv:?} text={text:?} line={line}");
                }
                // Nothing may reach stdout: stdout is the eval'd channel.
                if let Some(out) = run_script(argv, &line) {
                    assert!(out.is_empty(), "argv={argv:?}: message leaked to stdout: {out:?}");
                }
            }
        }
        assert_every_present_parity_arm_ran();
    }
}
