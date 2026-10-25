---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.powershell-quality-depth-on-demand"
title: PowerShell 5.1 and 7 Compatibility
summary: The PS-COMPAT family. The target by script kind, 7-only syntax, unbraced null-conditional, platform detection, commands, parameters and members 5.1 lacks, -UseBasicParsing, module imports on both editions and the OS text trap
---

# PowerShell 5.1 and 7 Compatibility

Rows bind to Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 and 7.6.6, PSScriptAnalyzer 1.25.0, all measured 2026-10-11 on a de-DE Windows host and on Linux, with GNU grep and busybox 1.37 (with git) for the cells. pwsh 7.4 and 7.5 end support 2026-11-10 (until then 7.4 stays a parse-matrix column only). macOS and BSD behaviour is `unverified: read only`.

Owns which edition a file targets and what that edition cannot run: the `#Requires` floor, syntax, variables, commands, parameters, .NET members, web-call switches and module import. Not owned here: encoding and BOM rules (`PS-ENC`), native exit codes and `$ErrorActionPreference` (`PS-ERR`, `PS-NATIVE`), the gate wrappers that call these checks, the repo-wide 5.1 parse with its `.psd1` handling and the command resolver (`PS-GATE`), and `SecurityProtocol` and download integrity (`PS-SEC`). The script kinds and the target-by-kind table are in the index (`PS-CORE`).

Contents: [Group 1: A 5.1 Host Run Decides](#group-1-a-51-host-run-decides) · [Group 2: A Listing Decides](#group-2-a-listing-decides) · [Group 3: A Module Import Decides](#group-3-a-module-import-decides) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Group 1: A 5.1 Host Run Decides

Check `H1`: `compat-ast.ps1` (copy it from `checks/` to `scripts/`) under `powershell.exe`. It parses with the 5.1 parser, then reports `PS-COMPAT-03` to `-06` findings by AST, with static members resolved against the host's .NET Framework. Exit 0 clean, 1 a MUST finding, 69 not 5.1 (`-AnyHost` runs the host-independent rules elsewhere), 78 no git work tree, 79 no file. `-Notes` adds the SHOULD lines. A pwsh 7 run parses every plant, so the wrong host is a silent green, which is why 69 exists. Cell `C5s` runs a script's dry or help path on 5.1 under `Stop` (`-File` alone exits 0 after a missing cmdlet), `SUBJECT` is the script and its arguments as PowerShell source, for example `./tools/install.ps1 -WhatIf` or `./tools/install.ps1 -?`. Cells `C2` to `C6` are the no-5.1-host fallback: a hit inside a string or here-string is a lead to read. PSSA `PSUseCompatibleSyntax` with `TargetVersions = @('5.1')` sees `&&`, `||`, `??`, `??=`, ternary and `${x}?.m`, and is blind to a leading `|`, a trailing `&`, `clean {}` and `${a}?[0]`.

```sh
for t in git powershell; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
# H1: findings on the target host
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/compat-ast.ps1
# C5s: the script's own dry or help path on 5.1, SUBJECT is PowerShell source: the script (quote a path with a space) and its dry-run arguments. Stop makes the first error exit 1, because -File exits 0 after a missing cmdlet
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "\$ErrorActionPreference = 'Stop'; & $SUBJECT; exit \$LASTEXITCODE"
```

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
# C2: 7-only syntax (&& ||, end-of-line &&, clean block, trailing &, leading pipe, generic static call)
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e '^[^#]* && ' -e '^[^#]* [|][|] ' -e '(&&|[|][|])[[:space:]]*$' -e '^[[:space:]]*clean[[:space:]]*[{]' -e '^[^#"]*[^&"]&[[:space:]]*$' -e '^[[:space:]]*[|]($|[^|])' -e '\]::[[:alnum:]_]+\[[^]$]+\]\(' --
# C3: unbraced variable before ?. or ?[
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e '^[^#]*[$][[:alnum:]_]+[?][.[]' --
# C4: IsWindows with no edition, version, Get-Variable or variable: existence test on the line. C4b lists Platform reads (a hit under StrictMode is the finding)
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e '[$]IsWindows([^A-Za-z0-9_]|$)' -- | grep -v -i -e 'PSVersionTable' -e 'PSEdition' -e 'Get-Variable' -e 'variable:'
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -F -e 'PSVersionTable.Platform' --
# (GNU grep) C4 as one lookahead: grep -H -n -P -i -e '^(?!.*(PSVersionTable|PSEdition|Get-Variable|variable:)).*[$]IsWindows(?![A-Za-z0-9_])' over the same list
# C5: an illustrative list of 7-only names, never the whole set
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e '-AsHashtable' -e '-AsByteStream' -e 'ForEach-Object[^|]*-Parallel' -e 'Join-String([[:space:]]|$)' -e 'Test-Json' -e 'Get-Error([[:space:]]|$)' -e '-SkipHttpErrorCheck' -e '-AsArray' --
# C6: Invoke-WebRequest or iwr in a file that never names UseBasicParsing
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -l -i -E -e 'Invoke-WebRequest' -e '(^|[^[:alnum:]_-])iwr([^[:alnum:]_-]|$)' -- | tr '\n' '\0' | xargs -0 -r grep -L -i -e 'UseBasicParsing' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-COMPAT-02 | Keep 7-only syntax out of 5.1-bound files: `&&`, `\|\|`, `?:`, `??`, `??=`, `${x}?.m`, `${x}?[i]`, a pipeline continued by a leading `\|`, a trailing `&`, a `clean {}` block and a generic static call `[T]::M[X]()`. Scope: installer and module kinds. A file with `#Requires -Version 7.x` is out of the 5.1 parse (`PS-GATE-04`). | One such line makes the whole file unrunnable on 5.1 and nothing before it runs. The 5.1 parser is the check of record, PSSA misses four of these. | `H1`, else `C2` | MUST |
| PS-COMPAT-03 | Never write `$o?.Name` or `$a?[0]` with an unbraced variable. Write `${o}?.Name` in 7.6-only code and `if ($null -ne $o) { $o.Name }` in 5.1-bound code. Scope: all PowerShell. | `?` is a legal variable-name character, so `$o?.Name` reads the variable `$o?` and silently yields nothing on 5.1, 7.4 and 7.6, with no parse error and no PSSA finding. | `H1`, else `C3` | MUST |
| PS-COMPAT-04 | Detect the platform in 5.1-bound code with `[Environment]::OSVersion.Platform -eq 'Win32NT'`. Never use `$IsWindows` alone, positive or negated. Under `Set-StrictMode` also never read `$IsLinux`, `$IsMacOS`, `$IsCoreCLR` or `$PSVersionTable.Platform` unguarded. The accepted guards test the edition or version first, as in `$PSVersionTable.PSEdition -eq 'Desktop' -or $IsWindows`, or test that the variable exists first, as in `-not (Test-Path variable:IsWindows) -or $IsWindows` and `(Get-Variable IsLinux -ErrorAction Ignore) -and $IsLinux`. Outside StrictMode a bare `$IsLinux` or `$IsMacOS` is a SHOULD. Scope: 5.1-bound, 7.6-only code may use the variables freely. The `.OS` text is `PS-COMPAT-11`. | The variables do not exist on 5.1, so an undefined `$IsWindows` is false on Windows, and under StrictMode a read is an error whose operand order decides whether it is ever evaluated. An existence test on the left of `-or` or `-and` is never evaluated past on 5.1 (watched under StrictMode Latest), and both are common in published modules. | `H1`, else `C4` and `C4b` | MUST, SHOULD for `$IsLinux` and `$IsMacOS` outside StrictMode |
| PS-COMPAT-05 | Use no command, parameter or .NET member that 5.1 lacks in 5.1-bound code, for example `Join-Path` with more than two parts or `-AdditionalChildPath`, `ConvertFrom-Json -AsHashtable`, `Test-Json`, `ForEach-Object -Parallel`, `Get-Content -AsByteStream`, `Join-String`, `Get-Error`, `Invoke-WebRequest -SkipHttpErrorCheck`, `[Convert]::ToHexString`, `[IO.Path]::GetRelativePath`, `[Math]::Clamp`, and `"1111p2222q3333".Split('pq')`, which is 3 parts on 5.1 and 1 on 7. The list is illustrative, the resolver's unknown-parameter mode is the check. Scope: 5.1-bound. `-Encoding utf8NoBOM` is `PS-ENC-03`. | These fail only at run time, on code paths CI may never run on 5.1. `[Math]::Clamp` is "does not contain a method named" on .NET Framework 4.8. | `H1` (names, `Join-Path`, static members, `.Split`), `C5`, `C5s`, the unknown-parameter mode of `PS-GATE-06` (also `PS-GATE-06`) | MUST |
| PS-COMPAT-06 | Pass `-UseBasicParsing` to every `Invoke-WebRequest` and `iwr` in 5.1-bound code, or set `$PSDefaultParameterValues['Invoke-WebRequest:UseBasicParsing'] = $true` before the first call. `Invoke-RestMethod` and `-OutFile`-only downloads need nothing. Scope: 5.1-bound, 7.x ignores the switch. | Since the 2025-12-09 update (CVE-2025-54100) a 5.1 call that parses the response prompts, then exits 1 under `-NonInteractive` and throws `NullReferenceException` with closed stdin. `-OutFile -PassThru` is affected. | `H1`, else `C6` (a comment naming the switch evades it) | MUST |

```powershell
if ($IsWindows) { $sep = '\' }                                   # wrong: false on 5.1, even on Windows
if ([Environment]::OSVersion.Platform -eq 'Win32NT') { $sep = '\' }   # right on 5.1, 7.4 and 7.6
```

Watched red (measured 2026-10-11): `H1` on Windows PowerShell 5.1.26100.9444 exits 1 on 11 planted files with 15 findings and 0 on 11 twins, and `-Notes` adds the one SHOULD line on a twin without changing the status. The same plants on pwsh 7.4.7 and 7.6.6 with `-AnyHost` give 12 findings, because the 5.1-only parse and static-member findings vanish, and a 7.x run without the flag exits 69. In a work tree `H1` exits 1 on the plant, 0 on the twin, 79 with no file and 78 outside git. Every name in its 7-only list was confirmed absent on 5.1 with `Get-Command`. `C2` to `C6` print the plant lines and nothing on the twin, identical on GNU grep, ubuntu 24.04 and busybox 1.37 with git (a hit in a string or here-string stays a lead). `C5s` is watched 2026-10-11: on 5.1 a script that calls a missing cmdlet in its dry path exits 1 under the `Stop` preamble (the same script under plain `-File` exits 0), its twin exits 0, and a path with a space runs when quoted in `SUBJECT`. Confirmed 2026-10-11 on 12 fresh PowerShell trees (9 repositories and the PowerShell bodies of 3 GitLab trees): `H1` gives 160 findings on pwsh 7.6.6 and 246 on 5.1.26100.9444 (the 7.4.7 counts match 7.6.6). The round found `Test-Path variable:IsWindows -or $IsWindows` and `(Get-Variable IsLinux -ErrorAction Ignore) -and $IsLinux` flagged as unguarded, fixed in `H1`, `C4` and `T2` (14 findings removed, red and green on a plant and a twin), and a DSC `ResourceNotDefined` error counted as a syntax error, fixed.

## Group 2: A Listing Decides

Checks `C1a`, `C1b`, `C7`, `C10` and `C11`. A hit is a file or site to read, narrow the pathspec to the kind's directory when installers and repo scripts live apart. Pass is every hit fixed or waived.

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
# C1a: no #Requires -Version line at all. A leading BOM is allowed and CRLF is safe
git ls-files -z --cached --others --exclude-standard -- '*.ps1' | xargs -0 -r grep -L -E -e '^[^[:alnum:]]{0,3}#[Rr]equires -Version' --
# C1b: any floor other than 5.1 or 7.6
git ls-files -z --cached --others --exclude-standard -- '*.ps1' | xargs -0 -r grep -H -n -E -e '^[^[:alnum:]]{0,3}#[Rr]equires.* -Version' -- | grep -v -E -e '-Version (5[.]1|7[.]6)([^0-9.]|$)'
# C7: the zip type in a file that never loads its assembly
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -l -F -e 'IO.Compression.ZipFile' -- | tr '\n' '\0' | xargs -0 -r grep -L -F -e 'Add-Type -AssemblyName System.IO.Compression.FileSystem' --
# C10: a script without StrictMode
git ls-files -z --cached --others --exclude-standard -- '*.ps1' | xargs -0 -r grep -L -F -e 'Set-StrictMode -Version Latest' --
# C11: a match on the OS text
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -n -H -F -e 'PSVersionTable.OS' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-COMPAT-01 | Declare the target with an exact `#Requires` line: `#Requires -Version 5.1` in installers, bootstrap and anything `irm \| iex` or `powershell -File` runs on stock Windows, `#Requires -Version 7.6` in repo, dev, CI and Pester scripts, never `7.0`, `7.4` or `7.5` as a floor. A script that `iex` delivers also tests `$PSVersionTable.PSVersion` in code. Scope: all PowerShell, pinned decision (adopter may override), modules `PS-COMPAT-08`. | On 5.1 `#Requires` wins over parse errors, so a mis-launched 7-only script says "requires PowerShell 7.6" instead of "Unexpected token '&&'". `iex` ignores it, and 7.4 and 7.5 end support 2026-11-10. | `C1a`, `C1b`, the in-code version test is a reading step | MUST |
| PS-COMPAT-07 | Load the zip assembly before use: `Add-Type -AssemblyName System.IO.Compression.FileSystem` before `[System.IO.Compression.ZipFile]` in 5.1-bound code. Scope: 5.1-bound. | 5.1 stops with "Unable to find type", loud, so SHOULD. | `C7` | SHOULD |
| PS-COMPAT-10 | Put `Set-StrictMode -Version Latest` at the top of each script. Scope: all PowerShell, identical on 5.1, 7.4.7 and 7.6.6 (eight probes, Latest equals 3.0). | Without it a misspelled variable is a silent `$null`. Evidence is a parity run only, so CONSIDER. | `C10` | CONSIDER |
| PS-COMPAT-11 | Read the OS from `$IsWindows`, `$IsMacOS` and `$IsLinux`, with `$PSVersionTable.PSVersion.Major -lt 6` meaning Windows. Never match the text of `$PSVersionTable.OS`, in any mode. Scope: all PowerShell. | `.OS` is free text, pwsh 7.6.6 on Fedora reports `Fedora Linux 43 (WSL)` and no `Darwin*`, `Linux*` or `Ubuntu*` pattern matches, so a real installer refused to run. | `C11` | MUST |

```powershell
if ($PSVersionTable.OS -like 'Linux*') { $p = 'linux' }                 # wrong: throws on 5.1 under StrictMode, misses Fedora
$p = if ($PSVersionTable.PSVersion.Major -lt 6 -or $IsWindows) { 'windows' } elseif ($IsMacOS) { 'macos' } else { 'linux' }   # right
```

Watched red (measured 2026-10-11): `C1a`, `C1b`, `C7`, `C10` and `C11` print the plant and nothing on the twin on the same three hosts, `C1a` stays silent on a BOM-prefixed CRLF twin and `C1b` on a twin that adds `-Modules`. `PS-COMPAT-11` was also held red on the public pnpm installer (`install.ps1`), where the real script prints `Platform could not be determined!` on Fedora. Confirmed 2026-10-11 on the 12 fresh trees: `C11` prints only `.OS` reads that are displayed or logged, never one matched against `Darwin*`, `Linux*` or `Windows*` text, so a hit is a line to read.

## Group 3: A Module Import Decides

Check `M1`: `compat-modules.ps1` once under `powershell.exe` and once under `pwsh` 7.6. It runs `Import-Module -Force` on every manifest and every loose `.psm1` and prints `path: import-FAIL message`. Exit 0 all imported, 1 a failure, 78 no git work tree, 79 no module. Importing runs the module's top-level code, so run it in CI.

```sh
for t in git powershell pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
# M1: both editions
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/compat-modules.ps1
pwsh -NoProfile -NonInteractive -File scripts/compat-modules.ps1
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-COMPAT-08 | Modules set `PowerShellVersion = '5.1'` and `CompatiblePSEditions = @('Desktop','Core')`, and CI imports every module on a real 5.1 and on 7.6. `Test-ModuleManifest` is not a gate and `CompatiblePSEditions` is documentation. Scope: shared or published modules. | `Test-ModuleManifest` passed a `.psm1` containing `??` that 5.1 rejects, and a Core-only manifest imported on 5.1. | `M1` | SHOULD |

Watched red (measured 2026-10-11): `M1` on 5.1.26100.9444 exits 1 on a manifest whose `.psm1` holds `??` and 0 on pwsh 7.4.7 and 7.6.6 for the same plant, a good module and a loose `.psm1` exit 0 on both editions, and an empty tree exits 79.

## What Agents Get Wrong Here

| Rank | Mistake | Rule | Check |
|---|---|---|---|
| 1 | Prove a 5.1 target on pwsh 7 or with PSSA alone, so `&&`, `??` and a leading `\|` pass | PS-COMPAT-02 | `H1` |
| 2 | Write `iwr` without `-UseBasicParsing`, or `curl -fsSL`, in a 5.1 installer | PS-COMPAT-06 | `H1`, `C6` |
| 3 | Test `$IsWindows` for the platform, or put it before the edition test | PS-COMPAT-04 | `H1`, `C4` |
| 4 | Believe `$obj?.Prop` is null-conditional on an unbraced variable | PS-COMPAT-03 | `H1`, `C3` |
| 5 | Use `-AsHashtable`, `Join-Path a b c`, `Test-Json` or `[Math]::Clamp` from 7-era habit | PS-COMPAT-05 | `H1`, `C5s` |
| 6 | Match `$PSVersionTable.OS` text to find the platform | PS-COMPAT-11 | `C11` |
| 7 | Floor a script at `7.0`, `7.4` or `7.5`, or declare none | PS-COMPAT-01 | `C1a`, `C1b` |
| 8 | Trust `Test-ModuleManifest` and never import on 5.1 | PS-COMPAT-08 | `M1` |
| 9 | Use `[ZipFile]` without `Add-Type` | PS-COMPAT-07 | `C7` |
