---
name: shell-integration
description: 'Procedure for building or auditing code that writes shell code for several shells or edits their startup files, such as a tool init command, a PATH or prompt hook, an rc or profile block, a generated completion script or a cmd.exe Batch line. Covers one quoter per target, refusing values a target cannot carry, the 36-case hostile round trip, rc file choice and byte-safe edits, append-safe hooks, completion registration proven by read-back, undo, and a harness kit run on the oldest and newest interpreter. Use when someone asks to "generate shell code from Rust, Go or Python", "write tool init for bash zsh fish nu elvish", "add a PROMPT_COMMAND or precmd hook", "edit .bashrc or a PowerShell profile", "install shell completions", "quote a value for fish, nu, elvish or pwsh", "audit an env emitter", "completions installed but nothing happens". Not for scripts that quote for themselves (shell-port), failing scripts (shell-diagnose) or curl-to-sh installers (installer-script).'
license: Apache-2.0
metadata:
  summary: Builds and audits multi-shell emitters, rc editors, hooks and completion registration, then proves them with a round-trip, hook, rc, read-back and Batch harness kit, measured 2026-10-11
  keywords: shell,emit,quoting,rc,profile,hooks,completions,bash,zsh,fish,nushell,elvish,powershell,cmd,batch,prompt,init
---

# shell-integration

An emitter is code that writes text another shell will run, and an rc editor is code that
rewrites a file a user owns. Both fail where nobody tests: on the fifth quote character, on a
CRLF profile, on a user's existing hook, on the second run. This skill carries the procedure
only. The standards live in the `shell-quality` rule (`emit.md`, `fish.md`, `nushell.md`,
`elvish.md`, `zsh.md`) and the `powershell-quality` rule (`encoding.md`), and this skill cites
them by ID. It ships the harness kit that goes red on each defect, because linters read none of
these strings.

Every result here was measured on **2026-10-11** on bash 3.2 to 5.3, zsh 5.0.8 to 5.9, dash, busybox
ash 1.37, fish 3.6.0 to 4.9.3, nushell 0.105.1 to 0.116.0, elvish 0.21.0, PowerShell 7.4.7 to 7.6.2
and Windows PowerShell 5.1.26100.9444. macOS and BSD are `unverified: read only`. A command marked
"translated 2026-10-11, re-watch pending" was not run on that version.

Contents: [Scope](#scope) · [Consent](#consent-and-a-throwaway-home) ·
[Step 0: inventory](#step-0-inventory-the-targets) · [Step 1: quoters](#step-1-one-quoter-per-target-and-context) ·
[Step 2: refuse](#step-2-refuse-what-a-target-cannot-carry) · [Step 3: round trip](#step-3-the-round-trip) ·
[Step 4: rc files](#step-4-choose-and-edit-the-rc-file) · [Step 5: hooks](#step-5-append-safe-hooks) ·
[Step 6: completions](#step-6-completions-proven-by-read-back) · [Step 7: undo](#step-7-undo-opt-out-and-version-guards) ·
[Batch](#the-cmdexe-batch-arm) · [The receipt](#the-receipt) ·
[MUST rows](#must-rows-this-procedure-surfaces) · [SHOULD rows](#should-rows-this-procedure-routes) ·
[What agents get wrong](#what-agents-get-wrong)

| Read | When |
|---|---|
| [references/harnesses.md](references/harnesses.md) | Before running anything: usage, expected red and green, gaps |
| [references/roundtrip.py](references/roundtrip.py) | Step 3: reference quoters, or your emitter through `--shim`, over `corpus.txt` |
| [references/rc/check-rc-edit.sh](references/rc/check-rc-edit.sh) | Step 4: apply an rc editor twice on LF, CRLF, symlink and mode |
| [references/hooks/hook-harness.py](references/hooks/hook-harness.py) | Step 5: bash, zsh, fish, nu, elvish hooks beside a user hook |
| [references/pwsh/hook-case.ps1](references/pwsh/hook-case.ps1) | Steps 2, 4, 5: the PowerShell probes in `pwsh/` |
| [references/completions/readback-bash.sh](references/completions/readback-bash.sh) | Step 6: one read-back script per shell, `samples/`, `help-payloads.txt` |
| [references/batch/batch-set.py](references/batch/batch-set.py) | Batch: the cmd.exe matrix, with `plant.log` and `twin.log` |

## Scope

Steps 0 to 3 and 7 bind a command that prints shell text (SH-EMIT-01 to 15). Step 4 binds an
installer or setup command that edits rc files (SH-EMIT-16 to 23, 51). Step 5 binds prompt,
directory and pre-exec hooks (SH-EMIT-24 to 29, 42 to 47), Step 6 completion generators and
registration (SH-EMIT-33 to 41), and the Batch step a cmd.exe emitter (SH-EMIT-48 to 50). Out of
scope: quoting inside a script for itself (`SH-EXP`), installer flow (`installer-script`), the sh
floor (`SH-DIAL`) and PowerShell source policy (`PS-ENC`, `PS-COMPAT`).

## Consent and a throwaway HOME

Every harness runs in a temp directory it creates and removes, with `HOME` and the XDG variables
pointed at it. Run the emitter under test the same way and never aim an rc editor at the real
`HOME`. Changing a user's real startup file needs the user's consent: print the block, the target
file and the removal command first, write only on a yes, and offer `--dry-run` and an opt-out
(SH-EMIT-21). Hook and completion text is silent, because it runs in every session (SH-EMIT-22).

## Step 0: inventory the targets

Order of work: this step, quoters and refusals (1 to 3), rc files (4), hooks (5), completions (6),
then undo (7). Run each harness on the oldest and the newest interpreter you can install, run each
plant once in your own environment to prove the harness goes red there, and treat a missing
interpreter as a gap, never a pass. A docker image gives an old version, with the fixtures directory
mounted at the same path, no `:ro`, and `-t` for fish, nu and elvish (`references/harnesses.md`).
Green lint is necessary and never sufficient, because ShellCheck 0.11.0 reads no Rust, Go or Python
string literal. Run `shellcheck -s sh` over the emitted text and PSScriptAnalyzer 1.25.0 with
`PSUseCompatibleSyntax` for 5.1 over emitted PowerShell (SH-EMIT-15).

List every file that writes shell text or startup files, then fill one row per target shell and
quoting context. A missing row is a missing quoter.

```sh
for t in git xargs grep; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
git ls-files -z --cached --others --exclude-standard -- '*.rs' '*.go' '*.py' '*.sh' '*.bash' '*.zsh' '*.fish' '*.ps1' '*.nu' '*.elv' '*.tmpl' '*.j2' | xargs -0 -r grep -H -n -e 'export PATH' -e 'set -gx' -e 'Register-ArgumentCompleter' -e 'PROMPT_COMMAND' -e 'add-zsh-hook' -e 'CurrentUserAllHosts' -e 'fish_add_path' -e 'precmd' -e 'bashrc' -e 'zshrc' -e 'config\.fish' -e 'rc\.elv' --
```

Watched red (measured 2026-10-11): it prints the two lines of a planted emitter and nothing
on a repository with none, and it hits in 10 of 12 fresh trees (the two silent ones write no shell text).
The shell extensions and the rc file names are in the list because a first version missed three
emitters (mcfly, broot, thefuck). The sites to read are the cells `Q1` to `Q6`, `S1` to `S4`, `E1` to
`E9`, `H1` to `H8` and `C1` to `C3` of the emit rule, one per step below.

## Step 1: one quoter per target and context

Write one function per (target, context) and name it for both, for example
`posix_single_quoted`, `fish_single_quoted`, `nu_raw_string`, `elvish_single_quoted`,
`pwsh_single_quoted`. A shared quoter is a finding even when it works today (SH-EMIT-01).

| Target | Form | Prefer instead | Rule |
|---|---|---|---|
| sh family (sh, bash, zsh, ksh, dash, ash, yash, posh) | `'..'` with `'` written `'\''` | the quoter is the channel | SH-EMIT-02 |
| fish | `'..'` escaping `\` first, then `'`, or in `".."` escape `\`, `$`, `"` | none | SH-EMIT-02 |
| nushell | raw string `r#'..'#` with n hashes, n = 1 + the longest run of `#` after any `'` | `from json` over an ASCII payload | SH-EMIT-02, SH-EMIT-06 |
| elvish | `'..'` with `'` doubled, backslash ordinary | `from-json` | SH-EMIT-02, SH-EMIT-06 |
| PowerShell | `'..'` doubling each of U+0027, U+2018, U+2019, U+201A, U+201B, or in `".."` backtick-escape the backtick, `$`, U+0022, U+201C, U+201D, U+201E | base64 | SH-EMIT-03, SH-EMIT-06 |
| cmd.exe Batch | no quoter, a refusal predicate | none | SH-EMIT-48 |

```powershell
$env:K = 'a’+(Set-Content pwned.txt PWNED)+’b'     # wrong: only U+0027 doubled, U+2019 ends the literal and the code runs
$env:K = 'a’’+(Set-Content pwned.txt PWNED)+’’b'   # right: the five-character class doubled, one literal
```

Rules that bind every quoter:

1. Never emit `$'..'` or `source <(cmd)` for an sh target. dash, yash and posh print the first
   literally, and bash 3.2 silently ignores the second. Capture the output, check the status, then
   `eval` it (SH-EMIT-12, SH-EMIT-13, SH-EMIT-14).
2. Never borrow a shell's own quoting (`printf %q`, `${x@Q}`, zsh `${(q)x}`) for another shell, and
   never parse fish `string escape` output (SH-EMIT-09). Quote once per parser layer (`ssh`,
   `sudo sh -c`, `docker exec`), innermost first, or pass a positional parameter (SH-EMIT-10).
3. Do not use `ConvertFrom-Json` as the PowerShell channel, because on 7.x it turns an ISO-date
   string into a `[DateTime]`. Use base64 (SH-EMIT-07).
4. Emitted PowerShell for 5.1 is ASCII with no BOM and no `?:`, `&&`, `||` or `??`, even in a dead
   branch (SH-EMIT-08).
5. Gate newer syntax by a numeric field or a feature probe, never by parsed version text, and put it
   in a separate file or an `eval` string (SH-EMIT-30).

## Step 2: refuse what a target cannot carry

Refuse instead of emitting a lossy form. A refusal exits 65 with empty stdout and a message on stderr
(SH-EMIT-05). Refuse NUL for every target, because dash, busybox ash, mksh, posh and bash silently
truncate it. Refuse an empty value for a PowerShell range that includes 7.4.7 or 5.1 and for every
cmd.exe target. The rest of the Batch set is in the Batch step (SH-EMIT-48).

Probe the empty case on each PowerShell edition instead of trusting memory. Watched red (measured
2026-10-11): `env-empty.ps1` exits 1 on 7.4.7 and 5.1.26100.9444, where `Test-Path env:XE` is False
after the assignment, and exits 0 on 7.6.2.

```sh
for t in pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
pwsh -NoProfile -NonInteractive -File "$KIT/pwsh/env-empty.ps1"
```

## Step 3: the round trip

For each of the 36 corpus lines, quote the value for the target, run the text in the real
interpreter, print the hex of the variable and compare. Pass is 35 of 35 equal and `nul`
refused (SH-EMIT-04). Run it on the oldest and the newest interpreter of each family, and run
yash under `LC_ALL=C.UTF-8`.

```sh
for t in python3 dash; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
EMITTER="./tools/quote-for"   # your emitter, called as EMITTER NAME HEX
python3 -I "$KIT/roundtrip.py" --family sh --shim "$EMITTER sh" -- dash
python3 -I "$KIT/roundtrip.py" --family fish --shim "$EMITTER fish" -- fish
python3 -I "$KIT/roundtrip.py" --family nu --shim "$EMITTER nu" -- nu --no-config-file
python3 -I "$KIT/roundtrip.py" --family elvish --shim "$EMITTER elvish" -- elvish
python3 -I "$KIT/roundtrip.py" --family pwsh --shim "$EMITTER pwsh" -- pwsh -NoProfile -NonInteractive -File
```

The shim prints one statement that sets `NAME` and exits 0, or exits 65 with empty stdout. Add
`--env` for a target that reads an environment variable. `--list` names each reference quoter and
its expected result, so run one plant first to prove the harness is live. Measured 2026-10-11: `sh`
35 of 35 on dash, busybox ash and bash 3.2 `--posix`, the apostrophe-only PowerShell quoter 28 of
35 (the curly-quote cases fail on the hex compare, and the canary file `pwned` stays absent because
the injection payloads of this corpus only print text), a POSIX quoter reused for fish 32 of 35 and
for nu and elvish 29 of 35. The canary fires on the help-payload corpus.

Minimum matrix: the posix floor (dash, busybox ash, bash 3.2 `--posix`), bash 3.2 and 5.x, zsh 5.0.8
and 5.9, fish 3.6.0 and 4.x, nu 0.105.1 and the newest, elvish 0.21.0, pwsh 7.4 or 7.6 and Windows
PowerShell 5.1 when the emitter claims it (watched 2026-10-11 from a Windows Python: `pwsh_sq`,
`pwsh_dq`, `pwsh_b64` and `pwsh_json` 35 of 35, `pwsh_apos` 28, `pwsh_dq_naive` 30). Do not shrink the corpus: one without the curly-quote cases scored three
apostrophe-only quoters 20 of 20.

## Step 4: choose and edit the rc file

Write each shell's init to the file that shell reads for that session type. Create only the
detected login shell's primary file, edit other rc files only if they already exist, and probe
instead of hard-coding (SH-EMIT-16).

| Shell | File | Rule |
|---|---|---|
| bash login | the first existing of `~/.bash_profile`, `~/.bash_login`, `~/.profile`, else create `~/.profile` | SH-EMIT-16 |
| bash interactive | the end of `~/.bashrc`, with the ble.sh branch of Step 5 | SH-EMIT-16, SH-EMIT-24 |
| zsh | environment in `ZDOTDIR` or `HOME` `.zprofile`, interactive at the end of `.zshrc`, silent | SH-EMIT-16 |
| sh, dash, ksh login | `~/.profile` | SH-EMIT-16 |
| fish | an own file `conf.d/TOOL.fish`, rewritten whole, never `config.fish` | SH-EMIT-17, SH-FISH-13 |
| nushell | an own file in the autoload directory that nu itself reports (`$nu.vendor-autoload-dirs`), never `config.nu` | SH-EMIT-17, SH-NU-07 |
| elvish | a managed block in `rc.elv` under `XDG_CONFIG_HOME` else `~/.config/elvish` | SH-EMIT-16 |
| PowerShell | ask each installed edition for `$PROFILE.CurrentUserAllHosts`, never join `Documents` | SH-EMIT-16 |

An rc editor follows these rules, and `rc/rc-edit.sh` is one reference implementation:

1. Match the marker pair as whole lines, with or without a trailing CR. Strip existing blocks,
   write the new one, repair a missing final newline, and write nothing when the result equals
   the file (SH-EMIT-18).
2. Keep every other byte. Write the block and any repaired newline with the file's own line
   ending. Never filter the user's lines through awk or sed when the script can meet a CRLF
   file or run under Git Bash, whose msys tools read in text mode (SH-EMIT-18).
3. Resolve a symlinked rc to its target with a `readlink` loop and write there. Keep the mode and
   owner, never create a user rc with mode 0600, and never rename a temp file onto the rc path
   (SH-EMIT-19).
4. Store a hash of the body in the opener and refuse a dirty body with a distinct exit code unless
   `--force` (SH-EMIT-20).
5. Append to a PowerShell profile with `Add-Content -Path $profilePath -Value $block`, an ASCII
   payload and the path from the target edition. Never `Out-File -Append`, `>>` or
   `[IO.File]::AppendAllText` (SH-EMIT-51).

```sh
for t in dash sh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
EDITOR_UNDER_TEST=./tools/rc-edit.sh   # called as EDITOR_UNDER_TEST apply or remove RCFILE BLOCKFILE
sh "$KIT/rc/check-rc-edit.sh" "$EDITOR_UNDER_TEST"
RC_INPROC=1 dash "$KIT/rc/check-rc-edit.sh" "$EDITOR_UNDER_TEST"
sh "$KIT/rc/check-rc-edit.sh" "$KIT/rc/plant-awk.sh"
```

Watched red and green (measured 2026-10-11) on dash 0.5.12, busybox ash 1.37 and bash 3.2 `--posix`:
the reference editor passes, the awk plant leaves two blocks after an LF-to-CRLF re-save, the rename
plant strands the symlink and changes mode `-rw-r--r--` to `-rw-------`, and the blind append leaves
a marker after removal. Also apply the emitter to an empty throwaway `HOME` with `SHELL=/bin/bash`
and list the files created, where only bash targets may appear.

PowerShell profile appends have their own harness. Run it on 5.1, 7.4 and 7.6:

```sh
for t in pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
pwsh -NoProfile -NonInteractive -File "$KIT/pwsh/profile-edit.ps1" -Method add-content
pwsh -NoProfile -NonInteractive -File "$KIT/pwsh/profile-edit.ps1" -Method out-file
```

Watched red and green (measured 2026-10-11): `add-content` passes all four shapes on 7.6.2 and 5.1,
`out-file` fails the UTF-16LE shape on 7.6.2 and three of four on 5.1, `append-all-text` fails the
UTF-16LE shape on both.

## Step 5: append-safe hooks

Register next to the user's hook, never in place of it. A hook body is one cheap, silent
function call with a marker in the registration text so that sourcing twice installs once
(SH-EMIT-47). The forms:

| Shell | Prompt hook | Directory hook | Undo |
|---|---|---|---|
| bash | extend `PROMPT_COMMAND[0]` or the string as one string, strip a trailing semicolon, dedupe by word, keep `$?`, and under ble.sh `blehook PRECMD!=FN` behind `declare -F blehook` | a `PROMPT_COMMAND` compare of `$PWD`, or `blehook CHPWD!=FN` | remove the word, `unset -f` |
| zsh | `add-zsh-hook precmd FN`, never `precmd()` or the array | `add-zsh-hook chpwd FN` | `add-zsh-hook -d`, `unfunction` |
| fish | `function FN --on-event fish_prompt`, never redefine `fish_prompt` | `function FN --on-variable PWD` | `functions -e FN` |
| nushell | normalise `$env.config.hooks.HOOK` to a list, drop the closure carrying your marker, append | `env_change.PWD` the same way | filter by marker |
| elvish | a closure whose rest-argument is named `@__tool_marker`, with a `has-value` probe on `arg-names` | `$after-chdir` the same way | filter by `arg-names` |
| PowerShell | wrap `prompt` once behind a live-body probe, capture `$LASTEXITCODE` first, write nothing, return one object | 7.x `LocationChangedAction` through `[Delegate]::Remove` then `Combine` with a FileSystem provider test, 5.1 a prompt wrap comparing `$PWD` | `$function:prompt = $global:__xPrevPrompt` |

Rules: SH-EMIT-24 (bash), 25 (zsh), 26 (fish), 27 (nu), 28 (elvish, SHOULD), 29 (PowerShell prompt),
42 and 43 (directory), 44 (bash pre-exec, SHOULD), 45 (no per-line point on PowerShell). The ble.sh
probe is `[[ -n ${BLE_VERSION-} ]] && declare -F blehook >/dev/null`, never `BLE_VERSION` alone,
because v0.3.4 sets it and has no `blehook`. An appended block lands after a user's `ble-attach`,
where v0.4.0-devel3 discards a `PROMPT_COMMAND` write. The right bash form is in the SH-EMIT-24
rule text. The wrong one is `PROMPT_COMMAND=__x_hook`, which replaces the user's value.

Run the hook harness once per shell version. It registers a user hook, applies your file once and
twice, drives a scripted session and compares each user token with a baseline run:

```sh
for t in python3 bash; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
python3 -I "$KIT/hooks/hook-harness.py" bash ./out/hook.bash -- bash --norc --noediting -i
python3 -I "$KIT/hooks/hook-harness.py" zsh ./out/hook.zsh -- zsh -f -i
python3 -I "$KIT/hooks/hook-harness.py" fish ./out/hook.fish -- fish -i --no-config
python3 -I "$KIT/hooks/hook-harness.py" nu ./out/hook.nu -- nu
python3 -I "$KIT/hooks/hook-harness.py" elvish ./out/hook.elv -- elvish
python3 -I "$KIT/hooks/hook-harness.py" zsh ./out/dir.zsh --event dir -- zsh -f -i
python3 -I "$KIT/hooks/hook-harness.py" bash "$KIT/hooks/fixtures/bash-plant-clobber.sh" -- bash --norc --noediting -i
```

Pass is `ours` once per event and every user token unchanged. A session with no marker is INVALID
and exits 1. The harness drives a pty, because a pipe-fed `bash -i` read 40 of 40 scenarios failing
where a pty run read 0 of 25. Watched red and green (measured 2026-10-11): twins 0 failing on bash
3.2 and 5.3, zsh 5.9, fish 4.9.3, nu 0.116.0 and elvish 0.21.0, and each plant (clobber, naive
string, `+=(...)`, `precmd()`, `fish_prompt`, list literal, unmarked append, `cd` wrapper) fails 1 to
4 scenarios. Bash before 5.1 runs only element 0 of a `PROMPT_COMMAND` array. There is no ble.sh
matrix and no bash directory-hook fixture in this kit, so those clauses of SH-EMIT-24 and SH-EMIT-43
are translated 2026-10-11, re-watch pending.

PowerShell hooks run on each edition, and your hook file adds one to `$global:__xFired` and writes nothing to the output stream:

```sh
for t in pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
pwsh -NoProfile -NonInteractive -File "$KIT/pwsh/hook-case.ps1" -ImplFile ./out/hook.ps1 -Twice
pwsh -NoProfile -NonInteractive -File "$KIT/pwsh/hook-case.ps1" -Impl plant-clobber
pwsh -NoProfile -NonInteractive -File "$KIT/pwsh/dir-case.ps1" -Impl twin
pwsh -NoProfile -NonInteractive -File "$KIT/pwsh/dir-guard.ps1" -Guard
```

Watched red and
green (measured 2026-10-11): the twin passes once, twice and undo on 7.6.2 and 5.1, the clobber and
leaky wrap fail on both, the unguarded wrap fails when applied twice, and `dir-guard.ps1` fails
without the provider test on 7.4.7, 7.6.2 and 5.1 and passes with it.

## Step 6: completions proven by read-back

"Registered" is a read from the live engine in a throwaway home, never a file on disk, an exit
status or an install log (SH-EMIT-33). A stored completion that registers nothing is silent.

| Shell | Zero-rc location or the rc line | Never | Rule |
|---|---|---|---|
| bash | `${BASH_COMPLETION_USER_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion}/completions/CMD`, lazy on first TAB, else an eager `eval` of a captured string | `~/.bash_completion.d/CMD`, a plural `bash-completions/`, `source <(...)` | SH-EMIT-34 |
| zsh | `_CMD` on `fpath` before `compinit`, or the guard prefix ahead of the script, no zero-rc user location | `fpath` after `compinit`, `compdef` before it, a user directory on no `fpath` | SH-EMIT-35 |
| fish | `completions/CMD.fish` or `vendor_completions.d/CMD.fish` | `completion/` singular | SH-EMIT-36 |
| nushell | an `export extern` module in the autoload directory | assigning `external.completer` | SH-EMIT-37 |
| elvish | `eval (slurp < FILE)` in `rc.elv` | `lib/CMD.elv`, which nothing loads | SH-EMIT-38 |
| PowerShell | a profile line `Register-ArgumentCompleter -Native -CommandName CMD`, the bare name | `-NativeFallback` from a tool, `-CommandName CMD.exe` alone | SH-EMIT-33, SH-EMIT-39 |

The zsh guard prefix is the `compdef` probe of SH-EMIT-35. fish before 4.7.0 loads completions only
for a command that resolves on `PATH`. nu autoload is read by the REPL only. The fallback slot is
one per session and exists on 7.6 only.

Read each registration back, in the shell it targets:

```sh
for t in bash zsh fish python3; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
C="$KIT/completions"
GEN=./out/demo   # your generated completion file for the command demo
bash "$C/readback-bash.sh" "$GEN.bash" demo
bash "$C/lazy-dir.sh" .local/share/bash-completion/completions/demo "$GEN.bash" demo -- bash
zsh "$C/readback-zsh.zsh" autoload "$GEN.zsh" demo
zsh "$C/readback-zsh.zsh" guard-then-compinit "$GEN.zsh" demo
fish "$C/readback-fish.fish" .config/fish/completions/demo.fish "$GEN.fish" demo sub stub
bash "$C/readback-nu.sh" "$GEN.nu" extern demo -- nu
bash "$C/readback-elvish.sh" rc-eval "$GEN.elv" demo -- elvish
```

On Windows read back all six spellings a user types (`CMD `, `CMD.exe `, an upper-case `CMD `, `.\CMD `
and `& CMD `) through `TabExpansion2`, the live engine, because `Get-Command` proves nothing:

```sh
for t in pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
pwsh -NoProfile -NonInteractive -File "$KIT/completions/readback-pwsh.ps1" -Mode bare
pwsh -NoProfile -NonInteractive -File "$KIT/completions/readback-pwsh.ps1" -Mode exe-only
pwsh -NoProfile -NonInteractive -File "$KIT/completions/native-fallback.ps1" -Mode twin
```

Watched red and green (measured 2026-10-11), each plant red and each twin green: bash on 3.2, 5.0 and
5.3, the lazy directories (the `.bash_completion.d` and plural plants stay silent), zsh 5.9 in all
eight modes, fish 3.6.0 and 4.9.3, nu 0.116.0, elvish 0.21.0 and Windows PowerShell 5.1 (`bare` six
of six, `exe-only` two of six). A real-TAB twin under a pty races now and then, so retry a twin once
and never a plant. Also watched 2026-10-11 (plant red, twin green): the lazy directory under
bash-completion 2.11 (Ubuntu 24.04, Debian 12) and 2.16 (Debian 13), zsh 5.0.8, 5.3.1, 5.8 and 5.9
in all eight modes, fish 4.0.2 and 4.7.0, and nu 0.105.1 for the readback.

Generated completion scripts are untrusted-input sinks:

1. Keep them ASCII only, so that Windows PowerShell 5.1 reads a BOM-less file correctly. A BOM is not
   the fix (SH-EMIT-40, SH-EMIT-08).
2. Treat help text as untrusted. Run `help-payloads.txt` through the quoter your generator uses,
   and for PowerShell escape the five-character class (SH-EMIT-41).

```sh
for t in python3 pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
sh "$KIT/completions/ascii-check.sh" ./out/demo.ps1 ./out/demo.zsh
python3 -I "$KIT/roundtrip.py" --family pwsh --corpus "$KIT/completions/help-payloads.txt" --shim "$EMITTER pwsh" -- pwsh -NoProfile -NonInteractive -File
python3 -I "$KIT/roundtrip.py" sh_dq --corpus "$KIT/completions/help-payloads.txt" -- bash
```

The last line is the plant. It fails 4 of 14 and prints `CANARY: a file named pwned appeared`. Watched
red and green (measured 2026-10-11): the apostrophe-only PowerShell quoter fails the three curly
payloads and trips the canary, the five-character quoter and the fish, nu, elvish and sh quoters pass
14 of 14, and `ascii-check.sh` exits 1 on a curly quote or a BOM and 0 on ASCII, on dash, busybox ash
and bash 3.2 `--posix`.

## Step 7: undo, opt-out and version guards

Ship a tested removal and an opt-out with every rc edit. Removal deletes exactly the marker pair or
the own file and leaves every other byte identical. Honour `--no-modify-path` or `NO_MODIFY_PATH` and
`--dry-run`, and give every hook of Step 5 its undo line (SH-EMIT-21). The `remove` leg of
`check-rc-edit.sh` is the test. Undo of a `blehook` registration, a `LocationChangedAction` delegate
and a completion is untested here, so say so in the receipt.

Make PATH edits idempotent with each shell's list type and separator (SH-EMIT-23): a `case ":$PATH:"`
guard for sh, `fish_add_path --global --path` for fish, `path add` for nu, a `has-value` test on
`$paths` for elvish and `[IO.Path]::PathSeparator` for PowerShell. Generated launchers validate every
embedded string and end in `exec ... "$@"` (SH-EMIT-31). Gate by numeric field or probe, for example
`BASH_VERSINFO`, `is-at-least` in zsh, `(version).minor` in nu, a `Get-Command` parameter probe in
PowerShell and the ble.sh probe above (SH-EMIT-30).

## The cmd.exe Batch arm

cmd.exe has no Linux twin and refuses most of the corpus, so its gate is the SH-EMIT-48 predicate:
every value is refused or round-trips in four channels (a `.bat` file, console stdin, `FOR /F` over a
file, `FOR /F` at the console), delayed expansion off and on. Refuse `%`, `"`, LF, CR, `!`, NUL,
U+001A, an empty value, any non-ASCII byte and a statement over 8000 characters. Emit the rest raw,
because doubling `%` is right in a `.bat` and wrong on `FOR /F` input (SH-EMIT-49). Run it on a real
Windows host from a `C:` working directory and retry only a NOVAR launch failure, at most 3 times
(SH-EMIT-50).

```sh
for t in python3; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/shell-integration/references   # the references directory on your host
STAGE=/mnt/c/Users/NAME/batch-run   # a directory on the Windows drive
python3 -I "$KIT/batch/batch-set.py" twin --stage "$STAGE"
python3 -I "$KIT/batch/batch-set.py" plant --stage "$STAGE"
python3 -I "$KIT/batch/batch-set.py" --shim "$EMITTER batch" --stage "$STAGE"
```

Watched red and green (measured 2026-10-11, Windows 11 build 26100): `twin` admits 25 of 43 values
and 200 runs with 0 failing, `plant` (refuses `% " LF CR` only) admits 37 and fails 90 of 296 runs,
and a shim mirroring the twin scores 25 of 43 and 0 failing. Logs are `batch/twin.log` and
`batch/plant.log`. With no Windows host write `unverified: no Windows host` in the receipt.

## The receipt

End every run with one block per emitter. A line with no evidence is written `unverified` or `n/a`,
never omitted.

```text
emitter:        paths, target shells and contexts, the quoter name for each pair
round_trip:     per interpreter and version, N of 35, nul refused, canary clean, help payloads N of 14
rc:             editor, interpreters, scenarios red and green, files created in an empty HOME
hooks:          per shell and version, scenarios failing of total, once and twice, undo
completions:    per shell, location and read-back result, ASCII check, six PowerShell spellings
windows_gaps:   PowerShell editions run, Batch runs and failing, every macOS, ble.sh or Git Bash claim marked read only
```

## MUST rows this procedure surfaces

Restated as findings so a run without the rules loaded still reports them with the right ID. The
rule text and the full verification live in the rules.

| # | Finding | Rule |
|---|---|---|
| 1 | One quoter is reused for targets whose grammar differs, such as a POSIX quoter for fish, nu or elvish | SH-EMIT-01 |
| 2 | A value is quoted in a form other than the target's, such as fish `\` unescaped, a fixed nu hash count or `'..'` for arbitrary nu data | SH-EMIT-02 |
| 3 | A PowerShell quoter doubles only U+0027, or a double-quoted form leaves U+201C to U+201E alone | SH-EMIT-03 |
| 4 | The emitter's tests lack the 36-case corpus, or it is not run in the real interpreter on the oldest and newest of each family | SH-EMIT-04 |
| 5 | NUL passes through, or an empty value reaches a PowerShell 7.4 or 5.1 or a Batch target | SH-EMIT-05 |
| 6 | `ConvertFrom-Json` is the PowerShell data channel | SH-EMIT-07 |
| 7 | Emitted PowerShell for 5.1 holds a non-ASCII byte, a BOM or 7-only syntax | SH-EMIT-08 |
| 8 | `$'..'` is emitted for an sh target | SH-EMIT-12 |
| 9 | `source <(cmd)` or `. <(cmd)` is emitted | SH-EMIT-13 |
| 10 | An rc file is hard-coded, wrong for a login bash, created for a shell the user does not run, or a PowerShell profile path is joined by hand | SH-EMIT-16 |
| 11 | A managed block is not matched as whole lines, an awk or sed pass rewrites the user's lines, or a CRLF file changes | SH-EMIT-18 |
| 12 | A symlinked rc is replaced by a rename, or a user rc is created with mode 0600 | SH-EMIT-19 |
| 13 | A bash prompt hook clobbers, appends `+=(hook)` without a gate, joins a string badly or gates ble.sh on `BLE_VERSION` alone | SH-EMIT-24 |
| 14 | A zsh hook is registered by defining `precmd()` or by assigning `precmd_functions` | SH-EMIT-25 |
| 15 | A fish hook is registered by redefining `fish_prompt` | SH-EMIT-26 |
| 16 | A nu hook is a list literal or an unmarked append | SH-EMIT-27 |
| 17 | The PowerShell prompt is redefined, wrapped twice, or the wrap prints or loses `$LASTEXITCODE` | SH-EMIT-29 |
| 18 | A completion is called registered without a read-back from the live engine, or a PowerShell completer is registered under `CMD.exe` | SH-EMIT-33 |
| 19 | A bash completion sits where nothing reads it, or is loaded with `source <(...)` | SH-EMIT-34 |
| 20 | A zsh completion is on no `fpath`, `fpath` is extended after `compinit`, or `compdef` runs before it | SH-EMIT-35 |
| 21 | A fish completion is in `completion/` or not named for the command | SH-EMIT-36 |
| 22 | A tool registers `-NativeFallback`, or registers PowerShell completion under a non-bare name | SH-EMIT-39 |
| 23 | A generated PowerShell or zsh completion script holds a non-ASCII byte | SH-EMIT-40 |
| 24 | Help text reaches generated script text unescaped, or the hostile-help corpus is not run | SH-EMIT-41 |
| 25 | A directory hook wraps `cd`, assigns a hook list, appends unmarked, or lacks the provider test | SH-EMIT-42 |
| 26 | A bash directory hook is a `cd` wrapper or a blind `PROMPT_COMMAND` write under ble.sh | SH-EMIT-43 |
| 27 | A cmd.exe `SET "K=v"` carries a value the predicate must refuse, or the Batch corpus was not run | SH-EMIT-48 |
| 28 | A PowerShell profile is appended with `Out-File -Append`, `>>` or `AppendAllText` | SH-EMIT-51 |

## SHOULD rows this procedure routes

These never block. Report them with the severity shown, and never re-raise one after a decline.

| Route | Finding | Severity | Rule |
|---|---|---|---|
| Step 1 | No data-over-code rung where a target has a structured parser | SHOULD | SH-EMIT-06 |
| Step 1 | Another shell's quoting facility used, or one layer of quoting through `ssh` or `sudo sh -c` | SHOULD | SH-EMIT-09, SH-EMIT-10 |
| Step 1 | A posh membership test with `%` or `%%` | CONSIDER | SH-EMIT-11 |
| Step 1 | `eval "$(tool)"` assumed to carry failure, or emitted sh not parsed under the posix family | SHOULD | SH-EMIT-14, SH-EMIT-15 |
| Step 4 | fish or nu files not owned whole, or first run detected by `__fish_initialized` | SHOULD | SH-EMIT-17 |
| Step 4 | Edited blocks not hash-protected | CONSIDER | SH-EMIT-20 |
| Step 5 | Rc or hook text that prints, reads stdin, is heavy or has no marker | SHOULD | SH-EMIT-22, SH-EMIT-47 |
| Step 5 | An elvish hook without a marker rest-argument | SHOULD | SH-EMIT-28 |
| Step 5 | A bash pre-exec hook by plain `trap fn DEBUG`, or `PreCommandLookupAction` as a per-line point | SHOULD | SH-EMIT-44, SH-EMIT-45 |
| Step 5 | Reliance on nu `env_change.PWD` for later statements of one line | CONSIDER | SH-EMIT-46 |
| Step 6 | A nu completer assigned instead of chained, or an elvish completion outside `rc.elv` | SHOULD | SH-EMIT-37, SH-EMIT-38 |
| Step 7 | No tested removal, opt-out or `--dry-run`, or a non-idempotent PATH edit | SHOULD | SH-EMIT-21, SH-EMIT-23 |
| Step 7 | A version gate by parsed text, or a launcher with an unquoted `$*` | SHOULD | SH-EMIT-30, SH-EMIT-31 |
| Batch | `%` doubled outside a batch file, or the corpus run off the Windows host | SHOULD | SH-EMIT-49, SH-EMIT-50 |

## What agents get wrong

Ranked by how often the mistake appeared across surveyed emitters, then by damage, with the one
mechanical check that catches it.

| Rank | Mistake | Mechanical check |
|---|---|---|
| 1 | Doubles `'` for PowerShell and stops, so U+2018 to U+201B run as code | `roundtrip.py pwsh_apos` is the red, your shim must score 35 of 35 with no canary |
| 2 | One `shlex.quote` or `shell_words::quote` helper for every shell | `roundtrip.py fish_posix` scores 32 of 35 and `nu_posix` 29, run your shared helper the same way |
| 3 | Appends to `~/.bashrc` with `echo >>` and no marker, or rewrites through a temp file at mode 0600 | `check-rc-edit.sh` on `plant-append.sh` and `plant-rename.sh` |
| 4 | Registers a hook by clobber or naive append | `hook-harness.py` per shell, plants in `hooks/fixtures` |
| 5 | A login env block in `~/.bashrc`, or in `~/.profile` when `.bash_profile` exists | Apply to an empty home with `SHELL=/bin/bash` and list the files created |
| 6 | `source <(tool init bash)` in an `sh` or macOS-bound file, or `$'..'` for an sh target | `shellcheck -s sh -i SC3001,SC3003,SC3046` over the emitted text, `roundtrip.py sh_dollarq -- dash` scores 0 of 35 |
| 7 | `eval "$(tool)"` assumed to propagate failure | `dash -c 'eval "$(sh -c "exit 3")"; echo $?'` prints 0 |
| 8 | `ConvertFrom-Json` as the PowerShell channel, or `$env:X = ''` tested with `-eq ''` | `roundtrip.py pwsh_json` scores 34 of 35, `env-empty.ps1` exits 1 on 7.4 and 5.1 |
| 9 | UTF-8 literals or `?:` in PowerShell meant for 5.1, or non-ASCII in a generated script | `ascii-check.sh` and PSScriptAnalyzer `PSUseCompatibleSyntax` |
| 10 | Reports "completions installed" because the file exists, or puts them where nothing reads | The `readback-*` script for that shell, red on its plant |
| 11 | Assigns nu's `external.completer`, or registers `-NativeFallback` from a tool | `readback-nu.sh` on `completer-plant.nu`, `native-fallback.ps1 -Mode plant` |
| 12 | Trusts help text in a generated completion script | `roundtrip.py --corpus help-payloads.txt` with the canary |
| 13 | Wraps `cd`, defines `chpwd()`, or assigns a directory hook list | `hook-harness.py --event dir`, `dir-case.ps1`, `dir-guard.ps1` |
| 14 | Edits an rc with awk or sed, tests only LF files, or appends to a PowerShell profile with `>>` | `check-rc-edit.sh` on `plant-awk.sh`, `profile-edit.ps1 -Method out-file` |
| 15 | Hard-codes nu's autoload directory or elvish's `rc.elv` from the Unix layout | Ask nu for `$nu.vendor-autoload-dirs`, read elvish back under `XDG_CONFIG_HOME` |
| 16 | Writes cmd.exe `SET "K=v"` with `%` doubled and a four-character refuse list | `batch-set.py plant` fails 90 of 296 runs |
