# Windows hosts, CI engines and installers

You are here because a failure involves `irm | iex`, Git Bash on Windows, a Taskfile, a container job or a downloader on a minimal image. Each section gives the captured symptom, the mechanism and the reproduction. The rule that owns the fix is named, and the procedure for the fix is in the rule. Captured 2026-10-11.

Contents: [exit under irm and iex](#exit-under-irm-and-iex) · [cmd /c in Git Bash](#cmd-c-in-git-bash) · [Taskfile panic](#taskfile-panic) · [Container job step](#container-job-step) · [BusyBox wget](#busybox-wget)

## exit under irm and iex

A script delivered by `irm URL | iex` runs in the caller's session, so `exit` ends the caller's host. Run as a child that plays the interactive caller, `Invoke-Expression 'Write-Output "installer: failing"; exit 3'` prints `installer: failing`, and the line after it (`caller: still alive`) never prints, on Windows PowerShell 5.1, 7.4.7 and 7.6.6. The guarded twin returns and the caller prints `caller: still alive (LASTEXITCODE=3)`. The 5.1 child also emits a `#< CLIXML` progress preamble, which is noise.

The detection line that older guidance gave, `$IsIex = ($null -eq $MyInvocation.MyCommand.Path)`, throws `The property 'Path' cannot be found on this object` under `Set-StrictMode -Version Latest` when delivered by `iex`, on 5.1, 7.4.7 and 7.6.6, and then `$IsIex` is unset. A test on `$PSCommandPath` being empty does not throw. The rule for the whole shape is PS-SEC-09. Run this listing to find the `exit` sites, then read each one for iex delivery:

```sh
for t in git xargs grep; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e '^[[:space:]]*exit([[:space:]]|$)' --
```

Every hit is a site to read. The pass is every hit guarded or the script not delivered by `iex`.

## cmd /c in Git Bash

Git Bash 5.2.15 on the Windows host. The `/c` is read as a path and converted, so `cmd` starts interactive and waits on stdin.

| Command | Result |
|---|---|
| `cmd /c echo hi` | prints the `Microsoft Windows [Version ...]` banner and no `hi`, exit 124 after 5 s |
| `cmd //c echo hi` | `hi` |
| `MSYS_NO_PATHCONV=1 cmd /c echo hi` | `hi` |
| `MSYS_NO_PATHCONV=1 cmd //c echo hi` | hangs, because with conversion off the doubled slash is not collapsed |
| `MSYS2_ARG_CONV_EXCL='*' cmd /c echo hi` and `MSYS2_ARG_CONV_EXCL='/c' cmd /c echo hi` | `hi` |
| `cmd /c echo hi </dev/null` | the banner and no `hi`, exit 0, a silent pass |

A timeout around the call only turns the hang into a failure. The fix is one of the two working spellings (SH-CI-14, owned by SH-USER-15 for the facts). The Actions runner's Git Bash is 5.3.15 and was not measured (unverified there). Find the call sites:

```sh
for t in git xargs grep shfmt tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
files() { git ls-files -z --cached --others --exclude-standard | xargs -0 -r sh -c 'for f; do if [ -f "$f" ]; then printf "%s\0" "$f"; fi; done' _ | xargs -0 -r shfmt -f -- | tr '\n' '\0'; }
files | xargs -0 -r grep -H -n -E -e 'cmd /[cCkK] ' -- | grep -v -e 'MSYS_NO_PATHCONV=1'
```

Output is the violation. A hit whose previous line already sets the variable is read, not fixed.

## Taskfile panic

go-task embeds the mvdan/sh interpreter. An unquoted hyphenated literal key read from an associative array panics it:

```yaml
version: '3'
tasks:
  panic:
    cmds:
      - 'declare -A m; m[x86_64-linux]=v; echo "ok=${m[x86_64-linux]}"'
```

`task panic` prints `panic: interface conversion: syntax.ArithmExpr is *syntax.BinaryArithm, not *syntax.Word` with a `mvdan.cc/sh/v3/expand.(*Config).varInd` frame. It reproduced on task 3.45.4 (sh v3.12.0), 3.51.1, 3.53.1 and 3.54.0 (sh v3.14.1). Quoting the key (`m["x86_64-linux"]`), a variable key, a compound assignment with a quoted key and a `case` statement all print `ok=v`. No upstream fix existed as of 3.54.0, so rerun the plant on each task bump. The rule is SH-CI-18.

## Container job step

A job with `container:` and a step with no `shell:` key runs the body as `sh -e {0}`, and the image decides what `sh` is. Emulate the step against the image:

```sh
docker run --rm -v "$PWD":/w:ro -w /w debian:12 sh -e ./step.sh; echo "rc=$?"
```

With a body of `if [[ -n x ]]; then echo bash-test-ok; fi`, `false | cat` and `echo REACHED`:

| Image | Output | Step status |
|---|---|---|
| debian:12 (dash 0.5.12-2) | `[[: not found`, then `REACHED` | 0 |
| ubuntu:24.04 (dash 0.5.12-6ubuntu5) | `[[: not found`, then `REACHED` | 0 |
| alpine:3.22 (busybox ash 1.37) | `bash-test-ok`, then `REACHED` | 0 |

The `[[` sits in an `if` condition, so the failure is a false branch and the step stays green. Adding `shell: bash` needs bash in the image, so check `command -v bash` there first (SH-CI-04). A real-runner confirmation is a measurement the emulation cannot supply.

## BusyBox wget

BusyBox v1.37.0 wget (alpine:3.22) rejects the GNU flags an installer passes without a probe:

```sh
docker run --rm alpine:3.22 wget --secure-protocol=TLSv1_2 --https-only -O /dev/null https://example.invalid/
```

It prints `wget: unrecognized option: secure-protocol=TLSv1_2` and the BusyBox usage text, exit 1, and an installer built on it exits with its own code (3 in the captured case). The twin without the two flags fails later at DNS (`bad address`), which proves the flags were the cause. Probe the flags first (SH-USER-10 for installers and bootstrap scripts, SH-INST-22).
