---
name: installer-script
description: 'Procedure for writing, auditing or reworking a one-line installer, either an install.sh run as curl piped to sh or an install.ps1 run as irm piped to iex, including a generated cargo-dist installer. Covers the header and exit table, truncation-safe structure, downloader probes, integrity before extraction, platform detection, prompts, user-space install, PATH edits, and the behavioural kit that runs the installer against stubs on several interpreters and PowerShell engines. Use when someone asks to "write an install.sh", "write an install.ps1", "make a curl | sh installer", "irm | iex installer", "audit this installer", "does the installer verify the checksum", "cargo-dist installer", "installer fails on alpine or busybox wget", "installer killed my PowerShell window", "truncated download ran half the script", or "ship an installer script". Not for porting an ordinary script (shell-port), diagnosing a failing script (shell-diagnose), or shell-init and completion emitters (shell-integration).'
license: Apache-2.0
metadata:
  summary: Writes and audits curl-to-sh and irm-to-iex installers, then proves them with a behavioural kit run on dash, busybox ash, bash 3.2 and Windows PowerShell 5.1 and 7, measured 2026-10-11
  keywords: installer,install.sh,install.ps1,curl,irm,iex,cargo-dist,checksum,truncation,busybox,wget,powershell,dash,posix,path
---

# installer-script

An installer is code a stranger pipes into an interpreter with no review, on a
host you have not seen, over a connection that can be cut. This skill carries
the procedure only. The standards live in the `shell-quality` rule
(`installers.md`) and the `powershell-quality` rule (`security.md`), and this
skill cites them by ID. It ships a template for each kind and a behavioural kit
that runs the finished file, because linters do not see these defects.

Every result here was measured on **2026-10-11** with dash 0.5.12, busybox ash
1.35 and 1.37, mksh R59c, ksh93u+m, yash 2.52, posh 0.14.1, bash 3.2 and 5.2
`--posix`, zsh 5.9 `--emulate sh`, Windows PowerShell 5.1.26100.9444 and
PowerShell 7.4.7 and 7.6.6 (7.4 reaches end of support 2026-11-10). macOS, BSD
and a .NET Framework older than 4.8.1 are `unverified: read only`. The rows that
were provisional (SH-INST-22, SH-INST-23, PS-COMPAT-11, PS-SEC-14) were confirmed
on 2026-10-11 against fresh third-party installers (the chezmoi install.sh and
install.ps1, the golangci-lint install.sh, a cargo-dist 0.25.2 installer pair) and
BusyBox 1.37.0 wget, each cell red on its plant and green on its twin.

Contents: [Scope](#scope) · [Step 0: classify](#step-0-classify) ·
[Step 1: header first](#step-1-header-first) ·
[install.sh steps](#installsh-steps) · [install.ps1 steps](#installps1-steps) ·
[Generated installers](#generated-installers) · [Run the kit](#run-the-kit) ·
[The receipt](#the-receipt) · [MUST rows](#must-rows-this-procedure-surfaces) ·
[SHOULD rows](#should-rows-this-procedure-routes) ·
[What agents get wrong](#what-agents-get-wrong)

| Read | When |
|---|---|
| [references/install-template.sh](references/install-template.sh) | Writing an install.sh: copy it, rename `toolx` and `TOOLX_`, set the base URL |
| [references/install-template.ps1](references/install-template.ps1) | Writing an install.ps1 for `irm \| iex` |
| [references/installer-kit.py](references/installer-kit.py) | Proving an install.sh: nine checks against stub tools in a throwaway HOME |
| [references/installer-kit.ps1](references/installer-kit.ps1) | Proving an install.ps1: host survival under iex, tamper before extraction |
| [references/cargo-dist-audit.sh](references/cargo-dist-audit.sh) | Auditing a generated installer: the static checklist rows |
| [references/cargo-dist-profile.json](references/cargo-dist-profile.json) | Pointing the kit at a generated installer |

## Scope

| Deliverable | Delivered by | Floor | Steps |
|---|---|---|---|
| `install.sh`, hand-written | `curl ... \| sh -s --` or download then run | POSIX sh: dash 0.5.12, busybox ash 1.37, bash 3.2 `--posix` | 0, 1, install.sh steps, kit |
| `install.ps1`, hand-written | `irm URL \| iex`, `powershell -File` | Windows PowerShell 5.1 (pinned decision, adopter may override) | 0, 1, install.ps1 steps, kit |
| Either, generated (cargo-dist 0.33.0) | A release job renders it | Whatever the generator emits | Generated installers |

Out of scope: an installer that must be root by design (a package-manager
dispatcher). It is labelled in its header and checked for the narrowest
escalation under SH-INST-23. Shell-init and completion
emitters are `shell-integration`. The rc edit an installer performs follows
that skill's rules (SH-EMIT-16, 18, 19).

Run the kit only on a file you may execute. It never touches the network or the
real HOME, and it starts the installer under `env -i` with a throwaway HOME,
`TMPDIR` and PATH (SH-CORE-16).

## Step 0: classify

Decide hand-written or generated before anything else. A generated file is
audited and never edited, because the next render erases the edit.

```sh
INSTALLER=install.sh   # rename
grep -n -i -e 'cargo-dist' -e 'generated by' -- "$INSTALLER"
```

A hit means generated, so go to [Generated installers](#generated-installers).
No hit means hand-written, so continue.

## Step 1: header first

Write the header before any code. It is the contract the kit and a reader
check. Both templates show the shape.

| Header line | Carries | Rule |
|---|---|---|
| Usage one-liner | `curl --proto '=https' --tlsv1.2 -fsSL URL \| sh -s -- [options]`, or `irm URL \| iex` | SH-INST-01, PS-SEC-07 |
| Floor | Interpreters it was tested on, ASCII only | SH-DIAL-04, SH-GATE-16 |
| `# Exit:` table | 0 ok, 1 generic, 2 usage or missing tool, 3 network, 4 checksum, 5 extract, 6 setup, 7 platform, 129 130 143 signals | SH-INST-09 |
| Options and environment | Every flag, every `*_INSTALL_*` variable | SH-INST-19 |
| Integrity class | pinned, manifest from another origin, sidecar from the same origin, signature, none | SH-INST-04, PS-SEC-02 |

State the integrity class in the words the code earns. A same-origin sidecar
"detects corruption, not a hostile origin". Never write "verifies" for a check
the file does not perform (SH-INST-04, PS-SEC-03).

## install.sh steps

Start from [references/install-template.sh](references/install-template.sh). The
steps say what to decide, the template shows one answer.

1. **Structure.** Prologue `set -u` for the installer kind (SH-ERR-23). Every
   executable statement sits in a function. Only assignments stay at top level.
   The last line is `main ${1+"$@"}` (SH-INST-01, SH-ERR-12). No brace range,
   no `[[ ]]`, no `pipefail` (SH-DIAL-04). ASCII only (SH-GATE-16).
2. **Signals and temp state.** One status-preserving `EXIT` handler bound
   before `mktemp`, and `HUP`, `INT`, `TERM` ending the shell with 129, 130 and
   143 (SH-ERR-15, SH-ERR-16). Never exit 123 to 125, 128 or 200 to 255
   (SH-ERR-18).
3. **Tools.** Probe every tool that is absent somewhere before using it
   (SH-USER-06, SH-USER-07).
4. **Downloader.** Probe curl with `curl --help all` before adding
   `--tlsv1.2`, always pass `--proto '=https'`, make one `curl` call per flag
   set, and assert https on every URL an environment variable can override
   (SH-INST-05, SH-INST-06, SH-USER-10). When wget is the fallback, probe its
   flags, send the same options on and off a terminal from the BusyBox v1.37.0
   set (SH-INST-22), warn on BusyBox, and refuse a same-origin
   checksum after "certificate validation not implemented" unless the digest is
   pinned (SH-INST-07).
5. **Integrity.** Take the expected digest from outside the archive's own
   response, or label it corruption detection. Verify before extracting.
   Fail closed in three cases: no hash tool, no expected digest, mismatch.
   Compare hex strings in the shell, and probe `sha256sum`, then
   `shasum -a 256`, then `openssl dgst -sha256` (SH-INST-03).
6. **Platform.** A total `case` on `uname -s` and `uname -m`. Anything unknown
   exits 7. Offer a `*_INSTALL_TARGET` override. Detect libc with three
   outcomes, glibc evidence, musl evidence, or exit 7. Detect Rosetta by value,
   never `sysctl ... || true | grep` (SH-INST-08). Add the print hatch
   `*_INSTALL_SH_PRINT=target` (SH-INST-19).
7. **Prompts.** Ask only through `/dev/tty` after `(: </dev/tty) 2>/dev/null`.
   Every prompt has a non-interactive default and a flag. Never `read` from
   stdin, because stdin is the script (SH-INST-11).
8. **Privilege.** Install into a user-writable prefix with no `sudo`. When an
   explicit prefix is unwritable, escalate only the final copy, announce the
   exact command, offer `--no-sudo`, and never interpolate into `su -c '...'`
   (SH-INST-12, SH-INST-23).
9. **PATH.** Edit only with consent and an opt-out flag. Write one marker block
   into the one file the login shell reads, through symlinks, and make the second
   run a no-op (SH-INST-12, SH-EMIT-16, SH-EMIT-18, SH-EMIT-19).
10. **Retry and latest version.** `curl --retry 3 --retry-delay 2`. Resolve
    "latest" from the `releases/latest/download` redirect, never the REST API
    (SH-INST-16, SH-INST-17, both SHOULD).

## install.ps1 steps

Start from [references/install-template.ps1](references/install-template.ps1).
The floor is Windows PowerShell 5.1, so write what 5.1 parses. Pwsh 7 is a
second engine to test, not the target.

1. **Delivery and flags.** Document `irm URL | iex` or
   `iwr -UseBasicParsing URL | iex`, never `(iwr ...).Content | iex`, because
   `.Content` is `byte[]` for an octet-stream and iex evaluates numbers.
   Flags go through environment variables. No `Mandatory`, no `param` that
   needs binding (PS-SEC-07, PS-SEC-08).
2. **Target.** Declare `#Requires -Version 5.1` and also test
   `$PSVersionTable.PSVersion` in code, because iex ignores `#Requires`
   (PS-COMPAT-01, PS-SEC-08). Read the OS from `$IsWindows` with
   `PSVersion.Major -lt 6` meaning Windows (PS-COMPAT-11).
3. **iex-safe body.** Wrap the body in `& { ... }` so preference variables,
   StrictMode and helper functions do not leak. No `$PSScriptRoot`. Detect
   delivery with `[string]::IsNullOrEmpty($PSCommandPath)`. Functions `throw`
   or `return`. One guarded exit at the end: under iex set
   `$Global:LASTEXITCODE` and `return`, otherwise `exit` (PS-SEC-08, PS-SEC-09).
4. **Network.** `-UseBasicParsing` on every request (PS-COMPAT-06). Throw on a
   non-https URL before any request (PS-SEC-14). Touch
   `SecurityProtocol` only on Desktop and only when not `SystemDefault`, with
   numeric values and no `Tls13` or `Tls11` literal (PS-SEC-06). Write a bounded
   retry loop, because 5.1 has no `-MaximumRetryCount`.
5. **Integrity.** Compare `Get-FileHash -LiteralPath` with an expected digest
   before `Expand-Archive`, `ExtractToDirectory` or running the file. A
   mismatch, an unreadable file, an absent or blank digest throws and the
   top level exits 4 (PS-SEC-01). Name the digest origin (PS-SEC-02) and claim
   nothing the code does not do (PS-SEC-03).
6. **Extract.** `ExtractToDirectory`, `Expand-Archive -LiteralPath` on a
   `.zip` name, or a loop that checks containment. Never `Join-Path` plus
   `ExtractToFile` alone (PS-SEC-04).
7. **Setup.** User scope only. No `Get-ExecutionPolicy` gate and no
   `Set-ExecutionPolicy` advice beyond `-Scope Process` (PS-SEC-11, PS-GATE-07).
   Build native argv as an array and check `$LASTEXITCODE` (PS-NATIVE-02,
   PS-ERR-01). Keep the file ASCII with no BOM, because iex serves text
   (PS-ENC-01, PS-ENC-02).

Do not wrap an install.ps1 in a truncation guard. `irm` buffers the whole body
before iex runs, so a cut download is a parse error and runs nothing.

## Generated installers

Audit every generated installer, fix upstream or wrap it, never hand-edit the
output, pin the generator version, and run `sh -n` on dash and busybox ash over
the rendered file in the release job (SH-INST-20). The static rows run on the
rendered file.

```sh
INSTALLER=install.sh   # the rendered file
sh references/cargo-dist-audit.sh "$INSTALLER" installer.ps1 dist-manifest.json
```

Pass the PowerShell installer and the manifest when you have them. Output is
one `row N OK|FAIL|READ` line per row, and exit 1 means a FAIL row.

| Row | Check | cargo-dist 0.33.0 template, measured |
|---|---|---|
| 1 | Every curl call carries `--proto` (all hops) | FAIL on the archive hop |
| 2 | Digests are embedded (class pinned) | OK |
| 3 | A missing hash tool refuses | FAIL: prints "skipping" and installs, exit 0 |
| 4 | Rosetta test compares a value | FAIL: `sysctl ... \|\| true \| grep` |
| 5 | A signal leaves no temp directory | FAIL: no trap |
| 6 | Last statement is one guarded call | OK |
| 7 | `sh -n` parses on dash and busybox ash | OK, but not enforced upstream |
| 8 | The PowerShell installer hashes before extracting | FAIL: no hash call |
| 9 | `--proto` on every hop | Merged into row 1 |
| 10 | Generator version pinned in the manifest | OK |

Rows 3 and 5 need the kit. Copy
[references/cargo-dist-profile.json](references/cargo-dist-profile.json), set
`name` and `installed`, and run the `tamper` and `trap` checks with
`--profile`. With digests embedded the kit swaps the archive and removes every
hash tool, and a pass is a refusal with an integrity word in stderr (checksum,
hash, digest, verify). A refusal for another reason, such as a side file the stub
does not serve, is RED with that note, so serve the file through `extra`. It
cannot build a good payload, so the control line reads `n/a`. A FAIL row is reported to the generator, or the
release job wraps the rendered file. The PowerShell half (row 8) has an open
upstream fix, so name the gap in the receipt and do not claim verification.

## Run the kit

The kit starts the installer as a file and as a piped script, in a throwaway
HOME, with stub `curl`, `wget`, `uname`, `ldd`, `sysctl` and `sudo`. Run every
check under every interpreter you can install. The floor set is dash, busybox
ash and `bash --posix`. Add mksh, ksh, yash, posh and `zsh --emulate sh` when
present, because posh aborts an unguarded empty `"$@"` and yash rejects a
non-ASCII byte under `LC_ALL=C`.

```sh
for t in python3 shellcheck dash git; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
KIT=.claude/skills/installer-script/references/installer-kit.py   # the references directory on your host
INSTALLER=install.sh   # rename
shellcheck -s sh -- "$INSTALLER"
for sh in "dash" "busybox ash" "bash --posix"; do
    python3 -I "$KIT" all "$INSTALLER" --shell "$sh" || echo "RED under $sh"
done
```

Exit status is the verdict: 0 pass, 1 a finding, 69 a tool or shell is
missing, 74 I/O failed, 78 a bad invocation or profile. A missing interpreter
is never a pass. Every check prints `ok`, `RED`, `na` or `info` lines.

| Check | Proves | Red looks like | Rules |
|---|---|---|---|
| `parse` | The shell parses under `LC_ALL=C` | `does not parse under LC_ALL=C` | SH-GATE-16, SH-DIAL-04 |
| `trunc` | A cut `curl \| sh` leaves the old install or the new one | `PARTIAL at line-cut N` or `VACUOUS` | SH-INST-01 |
| `tamper` | Swapped archive, no hash tool and blank digest all refuse | `RED B no hash tool` with exit 0 | SH-INST-03, SH-INST-04 |
| `hash` | Each hash-tool branch alone (sha256sum, shasum, openssl, the others off PATH): swapped archive refuses, good payload installs | `RED only openssl: swapped archive: rc=0` | SH-INST-03 |
| `flags` | curl flags probed, BusyBox wget set, http URL refused | `option rejected` or `plain-http request` | SH-INST-05, 06, 07, 22 |
| `platform` | 14 uname, ldd and sysctl cases map or exit non-zero | `want a failure, got rc=0` | SH-INST-08 |
| `root` | No `sudo` call as an unprivileged user | `escalated` | SH-INST-12, SH-INST-23 |
| `rc` | Second run leaves rc files identical, opt-out honoured | `rc files differ after run 2` | SH-INST-12, SH-EMIT-16 |
| `tty` | Piped install under a controlling terminal and under none | `/dev/tty leaked` | SH-INST-11 |
| `trap` | INT, TERM and HUP leave a signal status and no temp dir | `leaked-tmp=[...]` | SH-ERR-15, SH-ERR-16 |

`tamper` removes every hash tool at once, so it never reaches the fallback
branches. `hash` runs the installer once per tool in the profile key `hash_branches`
(default sha256sum, shasum, openssl) with every other hash tool off PATH, and a
tool the host lacks reads `n/a`, which is not a pass for that branch: run it on a host
that has the tool. Measured 2026-10-11: the chezmoi install.sh openssl branch fails
this check by source (`cut -d ' ' -f a` is not a field list, so the digest it computes is
never right); that is real-tree evidence of the check, and it is not
fixed by us.

Three limits to read before trusting a green. The `trunc` sweep is vacuous on an
installer that needs the network unless the stubs serve it, which is why it
seeds an old install and fails on a baseline that changes nothing. The `trap`
check signals at the first download, so pass `--nth 2` for an installer that
resolves a version first and creates its temp directory before the second call.
The `platform` table assumes Rust-style target triples, so give a `platform`
list in the profile when the asset names differ.

A profile is JSON with every key optional. The defaults describe
[references/install-template.sh](references/install-template.sh). The keys are
`name`, `version`, `asset`, `prefix`, `digest` (`sidecar` or `embedded`),
`sidecar` (a template over `{name}`, `{version}`, `{asset}` and `{target}`),
`installed`, `args`, `env`, `http_env`, `no_modify_flag`, `targets`, `platform`
and `extra`. `extra` maps a file name to its body and serves it beside the
archive, for an installer that fetches a tag lookup or a second file before the
archive (the golangci-lint profile serves `v1.2.3` as `{"tag_name":"v1.2.3"}`).
For an installer that takes its own flags, wrap it in a two-line script that
calls it with them, and point the kit at the wrapper.

For an install.ps1, run both PowerShell checks on every engine you have:

```sh
for t in pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
KIT=.claude/skills/installer-script/references/installer-kit.ps1   # the references directory on your host
pwsh -NoProfile -File "$KIT" -Check tamper -Path install.ps1 -PinEnv TOOLX_INSTALL_SHA256
pwsh -NoProfile -File "$KIT" -Check iex -Path install.ps1
```

On a non-Windows host the template exits 7 before any download, so pass
`-Env 'TOOLX_INSTALL_TARGET=x86_64-pc-windows-msvc;TOOLX_NO_MODIFY_PATH=1'`
to reach the integrity path. The tamper check saves and restores the user PATH on Windows.
On Windows PowerShell 5.1 use
`powershell -NoProfile -ExecutionPolicy Bypass -File $KIT` with the same
arguments. `iex` delivers the file in a child host in two models, a prompt and a
CI step, and both need a failing run, so pass `-Env` with a bad base URL when the
default one would succeed. `tamper` replaces `Invoke-WebRequest` and
`Invoke-RestMethod` with stubs, so it cannot judge a script that downloads
through WebClient, and an installer that looks up the release with
`Invoke-RestMethod` before it downloads ends in a red control line (`error
retrieving GitHub release`) that names the stub and not a defect. Switch to `Invoke-WebRequest -UseBasicParsing` or read the
order by hand and say so in the receipt. Then run PSScriptAnalyzer and parse
each 5.1-bound file with `[Parser]::ParseFile` on a 5.1 host (PS-GATE-04). Green
lint is necessary and never sufficient: ShellCheck 0.11.0 reports the defect on
1 of 26 planted installer defects, and PSScriptAnalyzer 1.25.0 flags only the
`iex` token on 3 of 12.

## The receipt

End every run with one block per installer. A line with no evidence is written
`unverified` or `n/a`, never omitted.

```text
installer:       path, sha256, line count, non-ASCII byte count
class:           hand-written | generated (tool, version)
integrity_class: pinned | manifest | sidecar | signature | none, digest origin host against archive host
interpreters:    each interpreter with its version, red=K of N for parse, trunc, tamper, hash, flags, platform, root, rc, tty, trap
powershell:      engines (5.1 build, 7.4 and 7.6 builds), iex repl and ci exit codes, tamper control and refusals, PSSA version
cells:           the SH-INST and PS-SEC cells of the rules, empty output = pass
static:          shellcheck status, PSSA status, cargo-dist-audit rows
unverified:      every macOS or legacy-Windows claim, marked "read only"
```

## MUST rows this procedure surfaces

Restated as findings so a run without the rules loaded still reports them with
the right ID. The rule text and the full verification live in the rules.

| # | Finding | Rule |
|---|---|---|
| 1 | An installer runs a statement at top level that a cut download can execute alone | SH-INST-01 |
| 2 | The payload is extracted or run before its digest is checked, or a missing tool, blank digest or mismatch lets it through | SH-INST-03 |
| 3 | A header or doc claims a verification the code does not perform, or the integrity class is unnamed | SH-INST-04 |
| 4 | A curl flag is used without a probe, or `--proto` is missing | SH-INST-05 |
| 5 | A download URL, including an overridable one, is not asserted https | SH-INST-06 |
| 6 | BusyBox wget is used with no warning, or its unvalidated certificate is trusted for a same-origin digest | SH-INST-07 |
| 7 | An unknown platform maps to a binary, or libc or Rosetta is detected wrongly | SH-INST-08 |
| 8 | No `# Exit:` table, or a code outside it is used | SH-INST-09 |
| 9 | A prompt reads stdin, or opens `/dev/tty` unprobed, or has no default | SH-INST-11 |
| 10 | The install needs `sudo`, edits PATH without consent, or the second run changes an rc file | SH-INST-12 |
| 11 | A generated installer shipped without the checklist, or hand-edited | SH-INST-20 |
| 12 | The wget option set differs on and off a terminal | SH-INST-22 |
| 13 | Escalation is blanket, announced late or interpolated into `su -c` | SH-INST-23 |
| 14 | A wget or curl flag in an installer is unprobed | SH-USER-10 |
| 15 | A non-ASCII byte in a `#!/bin/sh` installer | SH-GATE-16 |
| 16 | No handler for HUP, INT or TERM, or a temp directory leaks | SH-ERR-16 |
| 17 | An empty `"$@"` is forwarded unguarded under `set -u` | SH-ERR-12 |
| 18 | An iex script kills the host, leaves a failing CI step green, or its extract runs before the hash | PS-SEC-09 |
| 19 | The archive is extracted or run with no hash on its path, or an empty digest is skipped | PS-SEC-01 |
| 20 | The digest comes from the archive's own response with no label | PS-SEC-02 |
| 21 | A docs or header sentence claims SHA-256 verification the PowerShell file lacks | PS-SEC-03 |
| 22 | A hand-rolled zip loop with no containment check | PS-SEC-04 |
| 23 | `SecurityProtocol` is set unconditionally or names `Tls13` or `Tls11` | PS-SEC-06 |
| 24 | The documented one-liner uses `.Content`, or params need binding under iex | PS-SEC-07 |
| 25 | `#Requires`, `Mandatory`, `$PSScriptRoot` or leaked globals in an iex script | PS-SEC-08 |
| 26 | The installer gates on execution policy or advises a persistent change | PS-SEC-11 |
| 27 | An environment or parameter URL reaches a request without an https check | PS-SEC-14 |
| 28 | `-UseBasicParsing` missing on a 5.1-bound request | PS-COMPAT-06 |
| 29 | A native command's exit status is ignored, or its argv is a built string | PS-ERR-01 |
| 30 | A BOM or non-ASCII byte in a script that `irm \| iex` fetches | PS-ENC-02 |

## SHOULD rows this procedure routes

These never block. Report them with the severity shown, and never re-raise one
after a decline.

| Route | Finding | Severity | Rule |
|---|---|---|---|
| Step 10 | `--retry-all-errors` without a probe, or no retry flags | SHOULD | SH-INST-16 |
| Step 10 | The REST API or `curl -s` without `-f` resolves "latest" | SHOULD | SH-INST-17 |
| Step 6 | No print hatch or https-only base URL override | SHOULD | SH-INST-19 |
| PS step 6 | `-Path` instead of `-LiteralPath` for hash and extract | SHOULD | PS-SEC-05 |
| PS step 3 | `Invoke-Expression` in a delivered file with no hashed second stage | SHOULD | PS-SEC-10 |
| PS step 3 | A variable a `finally` reads is not initialised before `try` | SHOULD | PS-ERR-09 |
| Step 9 | No tested removal or silent, non-interactive rc text | SHOULD | SH-EMIT-21, SH-EMIT-22 |

## What agents get wrong

Ranked by how often the mistake appeared in 18 audited installers, with the one
mechanical check that catches it. Linters catch almost none of these.

| Rank | Mistake | Mechanical check |
|---|---|---|
| 1 | No integrity check, or one that fails open on a missing tool or blank digest | Kit `tamper`, and `tamper` in `installer-kit.ps1` |
| 2 | A header claims a verification the code lacks | SH-INST-04 and PS-SEC-03 cells, then read the delegate |
| 3 | Unprobed `--https-only`, `--secure-protocol` or `--tlsv1.2` copied from an answer | Kit `flags` on the BusyBox option set |
| 4 | No `# Exit:` table | SH-INST-09 cell |
| 5 | `-Path` instead of `-LiteralPath` for hash and extract | PS-SEC-05 cell |
| 6 | Install steps at top level, no `main` | Kit `trunc`, never a single 60 percent cut |
| 7 | `trap cleanup EXIT INT TERM` that returns, or no HUP | Kit `trap` on every interpreter |
| 8 | `exit` or `break` in an iex helper, or `$null -eq $MyInvocation.MyCommand.Path` under StrictMode | `installer-kit.ps1 -Check iex` in both host models |
| 9 | An unconditional `SecurityProtocol` assignment naming `Tls13` | PS-SEC-06 cell |
| 10 | A silent platform fall-through or a wrong Rosetta precedence | Kit `platform` |
| 11 | iex leaks `$ErrorActionPreference`, StrictMode or helper functions, or uses `#Requires`, `Mandatory`, `$PSScriptRoot` | PS-SEC-08 cells |
| 12 | A bare `read` or an unguarded `</dev/tty` in a piped installer | Kit `tty` |
| 13 | An execution-policy gate or `Set-ExecutionPolicy` advice | PS-SEC-11 cell |
| 14 | `(iwr ...).Content \| iex` | PS-SEC-07 cell |
| 15 | A hand-rolled zip loop written to avoid `Expand-Archive` | PS-SEC-04 cell |
| 16 | `curl -s` or the REST API to find the latest version | SH-INST-17 cell |
| 17 | A truncation guard wrapped around an install.ps1 | None needed, read the file: `irm` buffers the body |
| 18 | Trusting a generated installer, or calling it tested after one run on the host shell | `cargo-dist-audit.sh`, then the receipt's interpreter line |
