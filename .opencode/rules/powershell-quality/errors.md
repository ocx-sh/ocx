---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.powershell-quality-depth-on-demand"
title: PowerShell Error Model
summary: The PS-ERR family. Native exit codes under ErrorActionPreference, the native-error preference, CI run steps, entry-point status, stderr redirection, try/catch and terminating errors, output streams and finally under StrictMode
---

# PowerShell Error Model

Rows bind to Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 and 7.6.6 on Linux and Windows, and PSScriptAnalyzer 1.25.0
(measured 2026-10-11). pwsh 7.4 reaches end of support on 2026-11-10 and is a transitional column until then. The git-fed
cells run on GNU grep and busybox 1.37 with git, and macOS or BSD is `unverified: read only`.

Owns what a script does when a command fails: the exit status of natives, what `Stop` does and does not stop, the 5.1
stderr hazard, CI `run:` steps that hold several natives, the status an entry point returns, `try/catch/finally`, and
which stream carries data. Script kinds are INST (installer and bootstrap), script, module, test and CI step.
Not owned here: how a native is invoked (`curl.exe`, argument quoting) is `PS-NATIVE`, targets and `#Requires` are
`PS-COMPAT`, encoding is `PS-ENC`, the analyzer wrapper and exclusions are `PS-GATE`, and `exit` under `irm | iex` plus
download verification are `PS-SEC`. Workflow step shells are `SH-CI`, and errexit for sh and bash is `SH-ERR`.
`PS-ERR-05` is retired and never reused (`exit` under `iex` is `PS-SEC-09`).

Contents: [Group 1: A Grep Decides](#group-1-a-grep-decides) · [Group 2: An Analyzer or a Script Decides](#group-2-an-analyzer-or-a-script-decides) ·
[Group 3: A Behavioural Run Decides](#group-3-a-behavioural-run-decides) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Group 1: A Grep Decides

Every cell prints sites and empty output is the pass. E1 to E4 are listings: a hit is a site to read, and the pass is every
hit fixed or justified. Judge `grep -L` by the files listed, never by exit status.

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
psfiles() { git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' ':(exclude,glob)**/vendor/**'; }
# E1 files that call a native at line start and never check one (add the repo's tool names as -e)
psfiles | xargs -0 -r grep -L -e 'LASTEXITCODE' -e 'Invoke-Native' -- | tr '\n' '\0' | xargs -0 -r grep -l -e '^[[:space:]]*&[[:space:]]' -e '^[[:space:]]*tar[[:space:]]' -e '^[[:space:]]*git[[:space:]]' --
# E2 files that name the native-error preference without a 7.6 floor
psfiles | xargs -0 -r grep -l -F -e 'PSNativeCommandUseErrorActionPreference' -- | tr '\n' '\0' | xargs -0 -r grep -L -e '^#[Rr]equires -Version 7\.[6-9]' --
# E3 stderr redirects in files that set Stop and never set Continue
psfiles | xargs -0 -r grep -l -e 'ErrorActionPreference[[:space:]]*=[[:space:]]*.Stop' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'ErrorActionPreference[[:space:]]*=[[:space:]]*.Continue' -- | tr '\n' '\0' | xargs -0 -r grep -n -H -F -e '2>&1' -e '2>$null' -e '*>&1' -e '*>$null' --
# E4 statements that start with a bare .Add( call
psfiles | xargs -0 -r grep -n -H -E -e '^[[:space:]]*[$][A-Za-z_][A-Za-z0-9_.]*[.]Add[(]' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-ERR-01 | Check the exit status of every native command whose failure matters. Call it through `Invoke-Native` (below) or test `$LASTEXITCODE` on the next line. `$ErrorActionPreference = 'Stop'` is not an exit-code check. Never read `$LASTEXITCODE` after a cmdlet or inside a `catch`. Scope: all PowerShell, and the idiom must run on 5.1. | `Stop` does not stop on a native exit code on 5.1, 7.4.7 and 7.6.6, Linux and Windows: the script prints `reached` and exits 0. A failing cmdlet leaves the previous native's code, after a 5.1 `RemoteException` the code is -1, and `$null -ne 0` makes the helper fail closed for a native that never started. | Cell E1 (a file that checks any one native passes) and the PS-ERR-01 probe in Group 3. PSScriptAnalyzer has no rule for it. Watched red (measured 2026-10-11): E1 plant 2 files, twin 0, probe RED on 5.1, 7.4.7 and 7.6.6, and the helper twin exits 1 on all three. | MUST |
| PS-ERR-02 | Never rely on `$PSNativeCommandUseErrorActionPreference = $true` in a script that can run on 5.1 (installers, `shell: powershell` steps, `#Requires -Version 5.1`). Scope: 5.1-capable code, and a `#Requires -Version 7.6` file may use it. | On 5.1 the assignment creates an ordinary variable and the script prints `reached` with exit 0. On 7.4+ it also makes `if (native)` and `native \| Out-Null` throw under `Stop`. | Cell E2 (read each hit) and the PS-ERR-02 probe. Watched red (measured 2026-10-11): E2 plant 1 file, twin 0, probe RED on 5.1 only. | MUST |
| PS-ERR-06 | In code that can run on 5.1, never redirect a native's stderr inside PowerShell (`2>&1`, `2>$null`, `*>&1`, `*>$null`) while `$ErrorActionPreference` is `Stop`. Wrap the call in a scoped `Continue` with try/finally and check `$LASTEXITCODE`, or redirect in the child shell (`cmd /c "x 2>&1"`). Scope: 5.1-capable code. | 5.1 throws `RemoteException` on the first stderr byte (git and tar progress) and loses the exit code, 7.2+ does not. | Cell E3 (a file that sets `Continue` anywhere is skipped, so read the scope of it). Watched red (measured 2026-10-11): plant 2 lines, twin 0, and the plant redirect exits 1 on 5.1 and 0 on 7.4.7 and 7.6.6. | MUST |

The helper reads `$args` on purpose. A `param()` block with a remaining-arguments parameter binds a native's `-Path` or `-A`
to a parameter or its abbreviation (measured on 7.4.7 and 7.6.6), and `$args` never does.

```powershell
function Invoke-Native {
    $global:LASTEXITCODE = $null
    $exe, $rest = $args
    & $exe @rest
    if ($LASTEXITCODE -ne 0) { throw "$exe exited with code $LASTEXITCODE" }
}
Invoke-Native tar -xf a.tar   # right. Wrong: a bare "tar -xf a.tar" with more work after it under Stop
```

## Group 2: An Analyzer or a Script Decides

Run PSScriptAnalyzer 1.25.0 from the repository root. Pass is no output. The two scripts live under `checks/`, which the
index tells adopters to copy to `scripts/`, and SCRIPT in the last block names the script file under review.

```powershell
Invoke-ScriptAnalyzer -Path . -Recurse -IncludeRule PSAvoidUsingEmptyCatchBlock, PSAvoidUsingWriteHost
```

```sh
for t in python3 git; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done
python3 -I scripts/errors-run-lint.py
```

```sh
pwsh -NoProfile -File scripts/finally-masks.ps1 -Path "$SCRIPT"
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-ERR-03 | In a `shell: pwsh` or `shell: powershell` `run:` step (and any composite-action step) with two or more native commands, check each native (PS-ERR-01), or on pwsh 7.x set `$PSNativeCommandUseErrorActionPreference = $true` first and scope it to `$false` inside `& { }` for tools whose non-zero code is information (robocopy). A custom `shell: pwsh -File {0}` drops the runner's prelude and epilogue, so that script sets `Stop` and exits itself. Scope: CI steps, the preference alternative is pwsh 7.6 only (also SH-CI-12). | The runner prepends `$ErrorActionPreference = 'stop'` and appends `if (Test-Path variable:\LASTEXITCODE) { exit $LASTEXITCODE }`, so the last native wins: build fails, test passes, and the step exits 0 on every engine. | `checks/errors-run-lint.py` (needs PyYAML in the interpreter's own site, honours `defaults.run.shell` and the Windows default shell, one line per unguarded step). Exit 0 clean, 1 finding, 69 PyYAML or git missing, 78 not a git work tree. Watched red (measured 2026-10-11): plant 2 steps (an explicit pwsh step and a step inheriting a workflow-level `shell: pwsh` on ubuntu), twin 0 (a bash default on a Windows runner and a here-string). | MUST |
| PS-ERR-07 | `try/catch` sees only terminating errors. Put `-ErrorAction Stop` on each cmdlet inside a `try` or set `Stop` at the top, never write an empty `catch {}`, and report with `[Console]::Error.WriteLine(...)` then `exit`, not `Write-Error` then `exit`. Scope: all PowerShell. | `Get-Item /nope` under `Continue` inside a `try` prints an error and the `catch` never runs. `Write-Error` under `Stop` terminates first, so the exit code is always 1 and the message a stack trace. | PSScriptAnalyzer `PSAvoidUsingEmptyCatchBlock` (default Warning). The `-ErrorAction Stop` half is a reading step. Watched red (measured 2026-10-11): plant 1 line, twin 0. | SHOULD |
| PS-ERR-08 | A function returns data only through `Write-Output` or implicit output. Diagnostics go to stderr with `[Console]::Error.WriteLine`. Discard every value-returning method (`ArrayList.Add`, `HashSet.Add`, an un-nulled `New-Item`) with `[void]` or `$null =`. `Write-Host` is allowed for GHA workflow commands (`::error::` must reach stdout), in `Show-` display functions and in installers whose stdout carries no data (stated in the file header). Scope: script, module and test kinds. The installer kind keeps the `PSAvoidUsingWriteHost` exclusion (also PS-GATE-03). | `Write-Host`, `Write-Warning`, `Write-Verbose -Verbose` and `Write-Information -InformationAction Continue` all land on stdout of a `-File` process, only `Write-Error` and `[Console]::Error` reach stderr, and a method's return value leaks into the function's output. | PSScriptAnalyzer `PSAvoidUsingWriteHost` (default Warning, exempts `Show-` verbs) and cell E4, a reading heuristic: `ArrayList` and `HashSet` return a value, `List[T]` and `Hashtable` do not. Watched red (measured 2026-10-11): PSSA plant 1 line, E4 plant 1 line, twin 0 on both. | MUST |
| PS-ERR-09 | Initialise every variable a `finally` block reads before the `try`, and guard its use. Scope: any script under `Set-StrictMode -Version Latest`, 5.1 and 7.x. | A `finally` that reads a variable assigned only inside the `try` throws under StrictMode, and that error replaces the real failure message. A `finally` also runs after `exit`. | `checks/finally-masks.ps1` (RED and exit 1 when the output holds the undefined-variable error). It runs the script, so use a throwaway container or HOME. Watched red (measured 2026-10-11): plant RED and twin clean on 5.1.26100.9444 and 7.6.6. Confirmed 2026-10-11 on 4 fresh scripts of fleschutz/PowerShell that carry a `finally` (7.4.7, 7.6.6 and 5.1): GREEN, as the scripts initialise what the block reads. | SHOULD |

## Group 3: A Behavioural Run Decides

Run the probe on each engine the code targets: `pwsh` for 7.x and `powershell.exe` for 5.1. Pass is no output. Each RED line
names the row whose hazard the engine shows, and the compliant twin of each is the `Invoke-Native` form above, which stops
with exit 1 on all three engines.

```powershell
function Invoke-Probe([string]$Code) {
    $f = Join-Path ([IO.Path]::GetTempPath()) 'probe.ps1'
    [IO.File]::WriteAllText($f, $Code)
    $o = & (Get-Process -Id $PID).Path -NoProfile -NonInteractive -File $f 2>&1 | Out-String
    [pscustomobject]@{ Out = $o; Exit = $LASTEXITCODE }
}
$nat = if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) { 'cmd /c exit 3' } else { 'sh -c "exit 3"' }
$r = Invoke-Probe "`$ErrorActionPreference = 'Stop'`n$nat`n'reached'"
if ($r.Out -match 'reached') { 'RED PS-ERR-01: Stop did not stop on a native exit code' }
$r = Invoke-Probe "`$ErrorActionPreference = 'Stop'`n`$PSNativeCommandUseErrorActionPreference = `$true`n$nat`n'reached'"
if ($r.Out -match 'reached') { 'RED PS-ERR-02: the preference is a no-op on this engine' }
$r = Invoke-Probe "'body'`n$nat"
if ($r.Exit -eq 0) { 'RED PS-ERR-04: -File exits 0 after a failed last native' }
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-ERR-04 | End an entry-point script run by `-File` on an explicit status: a top-level `try` around `Main` whose `catch` writes the error with `[Console]::Error.WriteLine` and calls `exit 1`, and `exit $LASTEXITCODE` where the last native's code is the result. A caller that needs the exact code uses `-File`, not `-Command` or the built-in GHA shell. Scope: entry points. | `-File` returns 0 when the script merely ends after a failed native, and `-Command` and the GHA wrapper collapse every non-zero code to 1 (`exit 7` becomes 1). | The PS-ERR-04 probe line, plus a review step: the last statement of the entry point is not a bare native call. Watched red (measured 2026-10-11): probe RED on 5.1, 7.4.7 and 7.6.6. | SHOULD |

## What Agents Get Wrong Here

| Rank | Mistake | Rule |
|---|---|---|
| 1 | Sets `$ErrorActionPreference = 'Stop'` and trusts it to stop on a failed `git`, `tar` or `cargo`. | PS-ERR-01 |
| 2 | Writes a multi-command `run:` step and expects the first failure to fail the step: the last native wins. | PS-ERR-03 |
| 3 | Sets `$PSNativeCommandUseErrorActionPreference` in an installer or a 5.1-capable script, where it is silently a no-op. | PS-ERR-02 |
| 4 | Writes `2>&1` on a native under `Stop` in code that runs on 5.1, which throws on the first stderr byte. | PS-ERR-06 |
| 5 | Prints diagnostics with `Write-Host` or leaves `$list.Add(x)` un-voided, so stdout carries noise and extra data. | PS-ERR-08 |
| 6 | Wraps a cmdlet in `try/catch` without `-ErrorAction Stop`, or writes an empty `catch {}`. | PS-ERR-07 |
| 7 | Lets the entry point end on a native call and expects `-File` to return its code. | PS-ERR-04 |
| 8 | Reads in `finally` a variable assigned late in `try`, so StrictMode masks the real failure. | PS-ERR-09 |
