# Harnesses

Every script in this directory takes the interpreter as an argument, so the same file runs on the
oldest and the newest version of a shell. Each one ships with a compliant twin and at least one
plant, and each was run red on the plant and green on the twin. A harness you point at your own
emitter is only trusted after it has gone red on its own plant in your environment.

Contents: [Layout](#layout) · [Round trip](#round-trip) · [rc editor](#rc-editor) ·
[Hooks](#hooks) · [PowerShell](#powershell) · [Completions](#completions) · [Batch](#batch) ·
[Interpreter spelling](#interpreter-spelling) · [Dates and gaps](#dates-and-gaps)

## Layout

| Path | Proves | Rules |
|---|---|---|
| `corpus.txt` | The 36 hostile values, one `name<TAB>hex` line each | SH-EMIT-04 |
| `roundtrip.py` | Reference quoters and your own emitter, byte for byte, per interpreter | SH-EMIT-01..07, 41 |
| `rc/` | An rc-block editor applied twice, on LF, CRLF, symlink and mode | SH-EMIT-18, 19, 21 |
| `hooks/hook-harness.py` | Prompt and directory hooks beside a user hook | SH-EMIT-24..29, 42 |
| `pwsh/` | PowerShell prompt, directory, profile and empty-value probes | SH-EMIT-05, 29, 42, 51 |
| `completions/` | Completion registration read back from the live engine | SH-EMIT-33..41 |
| `batch/` | cmd.exe `SET "K=v"` on a real Windows host | SH-EMIT-48..50 |

The exit status is the verdict everywhere: 0 compliant, 1 a finding, 64 a bad invocation. A run that
produced no marker or no output is reported INVALID or "no output" and exits 1, never a pass.

## Round trip

```sh
python3 -I roundtrip.py --list
python3 -I roundtrip.py QUOTER -- INTERP [ARGS...]
python3 -I roundtrip.py --family sh --shim "your-emitter --quote" -- dash
```

`--shim CMD` runs your emitter. It is called as `CMD NAME HEX` once per case and prints one
statement that sets `NAME`. It exits 65 with empty stdout to refuse a value, which is the right
answer for the `nul` case and the wrong one for every other case. `--family` is `sh`, `fish`, `nu`,
`elvish` or `pwsh`, `--env` reads the value as an environment variable, `--corpus FILE` swaps in
another `name<TAB>hex` file. The interpreter runs in an empty directory, and a file named `pwned`
appearing there fails the run even when every byte matched. A `yash` run needs `LC_ALL=C.UTF-8`.

Measured 2026-10-11 on the newest builds (Linux x86_64). Plants are the red rows.

| Quoter | Interpreter | Result |
|---|---|---|
| `sh` | dash, busybox ash 1.37, bash 3.2 `--posix`, bash 5.3 | 35 of 35, `nul` refused |
| `sh_dollarq` | dash | 0 of 35 (the text is literal), green on bash |
| `fish` | fish 3.6.0, 4.9.3 | 35 of 35 |
| `fish_posix` | fish 3.6.0, 4.9.3 | 32 of 35 (`backslash trailing_bs dbl_bs`) |
| `nu_raw`, `nu_dq`, `nu_json` | nu 0.105.1, 0.116.0 | 35 of 35 |
| `nu_posix`, `elvish_posix` | nu, elvish 0.21.0 | 29 of 35 |
| `elvish`, `elvish_json` | elvish 0.21.0 | 35 of 35 |
| `pwsh_sq`, `pwsh_dq`, `pwsh_b64` | pwsh 7.6.2, 7.4.7, Windows PowerShell 5.1 | 35 of 35 |
| `pwsh_apos` | pwsh 7.6.2, 7.6.6, 7.4.7, Windows PowerShell 5.1 | 28 of 35, the curly-quote cases fail on the hex compare (no canary: the injection payloads of this corpus only print text) |
| `pwsh_dq_naive` | pwsh 7.6.2, Windows PowerShell 5.1 | 30 of 35 |
| `pwsh_json` | pwsh 7.6.2, 7.4.7 | 34 of 35 (`datelike`) |
| `pwsh_json` | Windows PowerShell 5.1 | 35 of 35 (5.1 keeps the date-like string as text) |

`completions/help-payloads.txt` is the same format for hostile help text. Run it with `--corpus`
against the quoter your completion generator uses. The plant `sh_dq` (double quotes with no
escaping of `$` and the backtick) fails 4 of 14 and trips the canary.

Windows PowerShell 5.1 is driven from a Windows Python (5.1.26100.9444, 2026-10-11). Not run here: macOS shells.

## rc editor

```sh
sh rc/check-rc-edit.sh IMPL
RC_INPROC=1 dash rc/check-rc-edit.sh IMPL
```

`IMPL` is a POSIX sh script called as `IMPL apply|remove RCFILE BLOCKFILE`. The scenarios are a file
with and without a final newline, an update, a removal, an LF file re-saved as CRLF between two
applies, a file born CRLF, the user's own bytes, a symlinked rc and the mode of a 0644 file.
`rc/rc-edit.sh` is the reference editor. `plant-append.sh` has no marker, `plant-rename.sh` writes a
temp file and renames it, `plant-awk.sh` filters with awk and is blind to CR.

Watched red and green (measured 2026-10-11) on dash 0.5.12, busybox ash 1.37 (alpine 3.22) and bash 3.2
`--posix`: the reference editor passes, `plant-append` fails the born-CRLF scenarios,
`plant-rename` fails the symlink and mode scenarios, `plant-awk` fails the LF-then-CRLF and
born-CRLF scenarios. Git Bash 5.2.15 (msys, 2026-10-11): the reference editor passes every scenario
and the three plants go red on theirs, except the symlink scenario, which prints `INVALID` because
`ln -s` makes no symlink without the native-symlink privilege (it exits 1 and judges nothing).

## Hooks

```sh
python3 -I hooks/hook-harness.py SHELL IMPL [--event prompt|dir] [-v] -- INTERP...
```

`SHELL` is `bash`, `zsh`, `fish`, `nu` or `elvish`. `IMPL` is the file your emitter produced. The
hook must print `FIRED:ours` (nu prints `FIRED:ours_pp` and `FIRED:ours_pwd`, the directory event
prints `FIRED:ours_dir`). Every scenario runs once with `IMPL` and once with an empty file, and a
scenario fails when a user hook fires less often than in the baseline, when `ours` fires other than
once per event, on a syntax error, or on a lost `$?` in bash. Presets: bash has five (no hook, a
string, a string with a trailing semicolon, a two-element array, the file sourced twice), zsh four, fish two,
nu four, elvish two.

Interpreter spelling: `bash --norc --noediting -i`, `zsh -f -i`, `fish -i --no-config`, `nu`,
`elvish`. For an old version use `docker run --rm -i -v DIR:DIR IMAGE bash --norc --noediting -i`
with the fixtures directory mounted at the same path and no `:ro`. fish, nu and elvish are driven
through a pty, so add `-t` to docker.

Measured 2026-10-11 (scenarios failing of total, newest local builds unless noted):

| Shell | Twin | Plants |
|---|---|---|
| bash 3.2 and 5.3 (docker) | 0 of 5 | clobber 4, naive string 3, array append (`+=(...)`) 1 on 5.3 and 4 on 3.2 and 5.0, array-index-1 1 on 3.2 and 5.0 |
| zsh 5.9 | 0 of 4 | `precmd()` 2, `precmd_functions` assign 1 |
| fish 4.9.3 | 0 of 2 | `fish_prompt` redefinition 2 |
| nu 0.116.0 | 0 of 4 | list literal 3, unmarked append 1 |
| elvish 0.21.0 | 0 of 2 | clobber 2, append 1 |
| zsh 5.9 `--event dir` | 0 of 4 | `chpwd()` 2, array 1, `cd` wrapper 4 |
| fish 4.9.3 `--event dir` | 0 of 3 | `cd` wrapper 3 |
| elvish 0.21.0 `--event dir` | 0 of 3 | clobber 2, append 1 |

Gaps. There is no ble.sh matrix and no bash directory-event fixture in this kit (no harness exists
to re-watch), so SH-EMIT-24 and SH-EMIT-43 keep their ble.sh and `PROMPT_COMMAND` compare clauses as
"translated 2026-10-11, re-watch pending". The nu `env_change.PWD` fixture is exercised inside the prompt run only.

## PowerShell

All scripts are single files that exit 1 on a finding. Run each on Windows PowerShell 5.1, 7.4 and
7.6 (`powershell -NoProfile -ExecutionPolicy Bypass -File` for 5.1, `pwsh -NoProfile -File` for 7).

| Script | Arguments | Red looks like |
|---|---|---|
| `pwsh/hook-case.ps1` | `-Impl twin\|plant-clobber\|plant-wrap-unguarded\|plant-wrap-leaky`, or `-ImplFile PATH`, `-Twice`, `-Undo` | user prompt ran 0x, prompt returned 2 objects, hook ran 41x |
| `pwsh/dir-case.ps1` | `-Impl twin\|plant-assign\|plant-append\|twin-prompt`, `-Twice` | user handler ran 0x, `op_Addition` error |
| `pwsh/dir-guard.ps1` | `[-Guard]` | the hook body ran for provider `Environment` |
| `pwsh/profile-edit.ps1` | `-Method add-content\|append-all-text\|out-file\|twin` | load error on a profile shape |
| `pwsh/env-empty.ps1` | none | `Test-Path env:XE` is False after assigning an empty string |

`-ImplFile` dot-sources your emitted hook once (twice with `-Twice`). Your file must add one to
`$global:__xFired` each time the hook runs and must write nothing to the output stream.
`dir-case.ps1 -Impl twin` needs `LocationChangedAction` and so exits 1 on 5.1, where `twin-prompt` is
the form to test. `profile-edit.ps1` builds four profile shapes (ASCII LF, ASCII CRLF, UTF-8 BOM,
UTF-16LE) in a temp directory, appends twice by the chosen method, dot-sources the result and
reports a load error, a wrong character or a changed prefix.

Watched red and green (measured 2026-10-11, PowerShell 7.6.2 locally, 7.4.7 in docker, Windows PowerShell
5.1.26100.9444 on the Windows host):

| Script | Green | Red |
|---|---|---|
| `hook-case` | `twin` once, twice and `-Undo` on 7.6.2, 7.4.7 and 5.1 | `plant-clobber`, `plant-wrap-leaky` and `plant-wrap-unguarded -Twice` on all three |
| `dir-case` | `twin` and `-Twice` on 7.6.2 and 7.4.7, `twin-prompt` on 7.6.2, 7.4.7 and 5.1 | `plant-assign` and `plant-append` on 7.6.2 and 7.4.7 |
| `dir-guard` | `-Guard` on 7.6.2, 7.4.7 and 5.1 | no guard on all three |
| `profile-edit` | `twin` and `add-content` 4 of 4 shapes on 7.6.2 and 5.1 | `out-file` and `append-all-text` fail `utf16le-crlf` on 7.6.2, `out-file` fails 3 of 4 on 5.1 |
| `env-empty` | exit 0 on 7.6.2 (the variable is kept) | exit 1 on 7.4.7 and 5.1 |

Not run: pwsh on macOS. The 7.4.7 rows and the 7.6.6 repeat were run 2026-10-11.

## Completions

Every script reads the registration back from the live engine in a throwaway home. `samples/`
holds a twin and a plant for a command named `demo` with the subcommands `sub` and `help`. Point the
script at your generated file in its place. The pty scripts use `ptydrive.py SCRIPT -- CMD...`, which
sends each line of SCRIPT when the output goes quiet and prints the ANSI-stripped session.

| Shell | Command | Green | Red |
|---|---|---|---|
| bash | `bash readback-bash.sh FILE CMD` | `samples/demo.bash` on bash 3.2, 5.0, 5.3 | `samples/demo-trunc.bash` on the same |
| bash lazy | `bash lazy-dir.sh RELPATH FILE CMD -- bash` | `.local/share/bash-completion/completions/demo`, `.bash_completion` | `.bash_completion.d/demo`, `.local/share/bash-completions/demo` |
| zsh | `zsh readback-zsh.zsh MODE FILE CMD` | `autoload`, `source-after`, `guard-only`, `guard-then-compinit`, `compdef-then-compinit` | `fpath-late`, `nofpath`, `source-before` |
| fish | `fish readback-fish.fish RELPATH FILE CMD WANT [stub]` | `.config/fish/completions/demo.fish` with `stub`, `.local/share/fish/vendor_completions.d/demo.fish` with `stub` | `.config/fish/completion/demo.fish`, any location without `stub` |
| nu extern | `bash readback-nu.sh FILE extern CMD -- nu` | `samples/demo.nu` | `samples/completer-plant.nu` (no row) |
| nu completer | `bash readback-nu.sh FILE completer CMD -- nu` | `samples/completer-twin.nu` | `samples/completer-plant.nu` (the user's answer is gone) |
| elvish | `bash readback-elvish.sh MODE FILE CMD -- elvish` | `rc-eval`, `rc-eval-try` | `lib-only`, `noop` |
| PowerShell | `readback-pwsh.ps1 -Mode bare\|both\|exe-only` | `bare`, `both`: six spellings answer | `exe-only`: four of six spellings serve file names |
| PowerShell fallback | `native-fallback.ps1 -Mode twin\|plant` | `twin` | `plant` on 7.6 (the user's fallback is displaced), the call throws on 5.1 |
| ASCII | `sh ascii-check.sh FILE...` | an ASCII file | a curly quote or a BOM |

For `lazy-dir.sh` the interpreter after `--` is a bash that has bash-completion installed. In docker
use `docker run --rm -it -v "$PWD:$PWD" -e HOME -e TERM IMAGE bash` with the current directory
mounted at the same path. The fish no-stub rows are red on 3.6.0 and, in `complete -C` form, on
4.9.3 as well, so the stub is part of the registration check there. `ascii-check.sh` runs
`grep` in the C locale, so it behaves the same on GNU grep, busybox and bash 3.2 `--posix`.

Watched red and green (measured 2026-10-11): bash on 3.2, 5.0 and 5.3, lazy-dir on a local bash 5.3 with
bash-completion 2.x, zsh on 5.9 for all eight modes and on 5.0.8 and 5.8 for `autoload`,
`fpath-late` and `source-before`, fish on 3.6.0 and 4.9.3, nu on 0.116.0, elvish on 0.21.0, PowerShell
on 7.6.2 and Windows PowerShell 5.1.26100.9444 (`bare` and `both` six of six, `exe-only` two of six),
`ascii-check.sh` on dash, busybox ash 1.37 and bash 3.2 `--posix`.

Also watched 2026-10-11, plant red and twin green: lazy-dir in docker under bash-completion 2.11
(Ubuntu 24.04 and Debian 12) and 2.16 (Debian 13), zsh 5.3.1, fish 4.0.2 and 4.7.0, nu 0.105.1 for
the readback, and generated completion scripts from clap (bash 3.2 and 5.3, zsh 5.0.8 and 5.9, fish,
elvish and nu twins green). Not run: the pty fish variant. Translated 2026-10-11, re-watch pending.

## Batch

```sh
python3 -I batch/batch-set.py twin --stage /mnt/c/Users/NAME/batch-run
python3 -I batch/batch-set.py plant --stage /mnt/c/Users/NAME/batch-run
python3 -I batch/batch-set.py --shim "your-emitter --batch" --stage /mnt/c/Users/NAME/batch-run
```

It needs a real Windows host. From WSL it starts `/mnt/c/Windows/System32/cmd.exe` with a `C:`
working directory, because a Windows process cannot use a `\\wsl$` directory. Pass `--cmd` for another
cmd.exe location. `twin` and `plant` are two reference predicates for which values a `SET` line may
carry. Every admitted value is emitted raw and read back in four channels, delayed expansion off
and on, code page 437. A run is OK, CORRUPT, NOVAR or PWNED, and a NOVAR launch failure is retried
at most 3 times. `batch/twin.log` and `batch/plant.log` are the recorded runs.

Measured 2026-10-11 on Windows 11 build 26100: `twin` admitted 25 of 43 values and 200 runs, 0
failing, exit 0. `plant` (refuses `% " LF CR` only) admitted 37 of 43 values and 296 runs, 90
failing, exit 1. A `--shim` reference emitter that mirrors the twin predicate also scored 25 of 43
and 0 failing.

## Interpreter spelling

| Need | Spelling |
|---|---|
| sh floor | `dash`, `busybox ash` (alpine 3.22 `sh`), `bash --posix` on bash 3.2 (docker `bash:3.2`) |
| Old and new bash | docker `bash:3.2`, `bash:5.0`, `bash:5.3` |
| Old and new zsh | docker `zshusers/zsh:5.0.8`, `zshusers/zsh:5.8` and a local 5.9 |
| Old and new fish | docker image with fish 3.6.0, a local 4.9.3 |
| Old and new nu | nu 0.105.1 and 0.116.0 binaries |

Run a python harness against a docker interpreter by mounting the directory that holds its temp
files at the same path (`TMPDIR=DIR docker run --rm -i -v DIR:DIR ...`).

## Dates and gaps

Everything here was measured on 2026-10-11. macOS and BSD are `unverified: read only`. Not shipped
and stated as gaps: a ble.sh matrix, a bash directory-hook fixture, Git Bash for the rc editor,
PowerShell 5.1 for the quoters, and a PowerShell completer under an installed MSI or Store pwsh.
