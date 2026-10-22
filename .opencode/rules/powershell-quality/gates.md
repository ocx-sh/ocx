---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.powershell-quality-depth-on-demand"
title: PowerShell Gates
summary: The PS-GATE family with SH-TEST-18 to 22. The PSScriptAnalyzer wrapper and its settings by kind, the Windows PowerShell 5.1 parse gate, the advisory command resolver, installer and manifest hygiene, and the Pester pin, census gate, removed constructs, discovery code and test host
---

# PowerShell Gates

Rows bind to PSScriptAnalyzer 1.25.0, Pester 6.2.0 (5.9.1 for a repo that stays on Pester 5), pwsh 7.4.7 (end of support 2026-11-10), pwsh 7.6.6 and Windows PowerShell 5.1.26100.9444 (measured 2026-10-11). macOS behaviour is `unverified: read only`.

Owns the gate that turns PowerShell findings into an exit status: the PSScriptAnalyzer wrapper and its settings by script kind, the 5.1 parse job, the command resolver, `Set-ExecutionPolicy` in installers, wildcard manifest exports, and Pester tests (pin, census gate, constructs removed in 6.0.0, discovery code, the host under test). Not owned here: the 5.1 constructs the parse finds are `PS-COMPAT`, encoding and BOMs are `PS-ENC`, try/catch and exit statuses inside scripts are `PS-ERR`, native argv is `PS-NATIVE`, the installer security rows are `PS-SEC`, the ShellCheck, shfmt and bats gates and the dialect of every cell are `SH-GATE` and `SH-TEST`, and workflow run-step semantics are `SH-CI`. Copy the `checks/` assets this file names to `scripts/` (pinned decision, adopter may override the directory).

Contents: [A. The PSScriptAnalyzer Gate](#a-the-psscriptanalyzer-gate) · [B. The 5.1 Parse Gate](#b-the-51-parse-gate) · [C. Command Resolution](#c-command-resolution) · [D. Hygiene Greps](#d-hygiene-greps) · [E. Pester](#e-pester) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

Output is the violation. A cell passes only with empty stdout and empty stderr. A cell marked listing prints sites to read, and its pass is every hit fixed or reasoned. Every grep cell was watched red on a plant and silent on a compliant twin on GNU grep and BusyBox 1.37 with git added (measured 2026-10-11). Every `checks/` script exits 0, 1 or 2 (PowerShell) or the `SH-GATE-31` map (shell), and states the engines it ran on in its header.

## A. The PSScriptAnalyzer Gate

The check is `checks/Invoke-PSGate.ps1` run sequentially, plus the cells below.

```sh
pwsh -NoProfile -File scripts/Invoke-PSGate.ps1 -Path . -Settings PSScriptAnalyzerSettings.script.psd1 -RequiredVersion 1.25.0
```

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# 01-a (listing: each hit is a raw -EnableExit site to read, pass is every hit routed through the wrapper or justified)
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -F -e 'EnableExit' --
# 02-a
git ls-files -z --cached --others --exclude-standard -- '*.psd1' | xargs -0 -r grep -H -n -E -e '^[[:space:]]*Severity[[:space:]]*=[[:space:]]*@[(]' -- | grep -v -e 'ParseError'
# 02-b
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e 'Invoke-ScriptAnalyzer.*-Severity' -- | grep -v -e 'ParseError'
# 03-a
git ls-files -z --cached --others --exclude-standard -- '*.psd1' | xargs -0 -r grep -H -n -F -e 'UseCompatibleCommands' -e 'UseCompatibleTypes' --
# 03-b (a call that splats its parameters is a false hit to read)
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -F -e 'Invoke-ScriptAnalyzer' -- | grep -v -e '-Settings'
# 05-a
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e 'Install-Module[[:space:]]+(-Name[[:space:]]+)?PSScriptAnalyzer' -- | grep -v -e '-RequiredVersion'
# 11-a (listing: a file that runs the analyzer and never says exit, throw or EnableExit)
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' | xargs -0 -r grep -l -e 'Invoke-ScriptAnalyzer' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'exit' -e 'throw' -e 'EnableExit' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-GATE-01 | Gate PowerShell through `checks/Invoke-PSGate.ps1`, which exits 0 clean, 1 findings, 2 misconfigured or no result. Never gate on a raw `Invoke-ScriptAnalyzer -EnableExit`. Run the analyzer sequentially. | PSScriptAnalyzer 1.25.0 raw fails open five ways: the exit status is the finding count modulo 256 (256 findings exit 0), `Severity = Error,Warning` drops ParseError (an unparseable file exits 0), a missing settings path exits 0, a typo in `Rules` or `IncludeRules` silently disables the rule, and an empty directory exits 0. A manifest naming a missing file also comes back as an ordinary finding. The wrapper exits 2 on each, yet an absent `Severity` stays valid because the analyzer then reports ParseError. A record that is a limit of the host (a missing type, module or DSC resource, the DSC `Configuration` keyword on Linux and every ParseError after it in that file) is not a finding: the wrapper prints it as `environment:` and exits 2 when nothing else is wrong, so judge that file on Windows; a real finding still exits 1. The analyzer is not thread-safe under `ForEach-Object -Parallel`. Scope: all PowerShell gates, pwsh 7.4.7, 7.6.6 and Windows PowerShell 5.1. | `checks/Invoke-PSGate.ps1`, cell 01-a. Watched: exit 1 on a plant and 0 on its twin, exit 2 on a bad path, a missing settings file, a Severity without ParseError, a mistyped rule, an empty directory and a broken manifest, on all three engines (measured 2026-10-11). Environment records watched on pwsh 7.6.6 on Linux (measured 2026-10-11): a DSC `Configuration` file (3 ParseError records) and a file naming a missing module and base type exit 2 with `environment:` lines, where the old wrapper exited 1; a file with a missing brace still exits 1. Cell 01-a translated 2026-10-11, watched red. | MUST |
| PS-GATE-02 | Every `Severity` list that is set, in a settings file or a CLI `-Severity` argument, includes `ParseError`. A settings file with no `Severity` is valid. | A list that omits it opts out of the parse check, and the default-looking `Error,Warning` is that opt-out: on 5.1, 7.4.7 and 7.6.6 a broken file gave 2 ParseError findings with `Severity` absent and 0 with `Error,Warning`. Scope: all PowerShell gates. The cells match one `Severity = @(` line and one `Invoke-ScriptAnalyzer ... -Severity` line, so a list split over lines is a reading step. A commented example does not match. | cells 02-a, 02-b. Watched red (measured 2026-10-11), the commented example silent. | MUST |
| PS-GATE-03 | Pass a per-kind settings file with `-Settings` and never rely on implicit `PSScriptAnalyzerSettings.psd1` discovery. Use the four kinds of the table below, each with `Severity = @('Error','Warning','ParseError')`. Keep `PSUseCompatibleCommands` and `PSUseCompatibleTypes` out of blocking settings and pass `TargetVersions` explicitly. | Implicit discovery applies one file to the whole recursion, and a noisy rule teaches the agent to disable the gate: `PSUseDeclaredVarsMoreThanAssignments` was 22 of 30 false positives (13 of 15 in tests) and `PSAvoidUsingWriteHost` 586 of 1,077 hits in a rule survey. An empty `TargetVersions` means 5, 6 and 7, and `'7.4'` and `'7.6'` behave as `7.0`, so no compat rule expresses a 7.6 target. No surveyed repo passed the unamended table (580 findings as script, 1,098 as test, 2,273 as module): adopt by baseline or ratchet, then block. Scope: gate settings, PSScriptAnalyzer 1.25.0. | cells 03-a, 03-b, and the wrapper, which exits 2 on an unknown rule name. Watched: the four settings files exit 0 on a compliant file, and a Write-Host plant exits 1 under module, script and test and 0 under installer, on 7.6.6 and 5.1 (measured 2026-10-11). | MUST |
| PS-GATE-05 | Pin PSScriptAnalyzer: `Install-Module PSScriptAnalyzer -RequiredVersion 1.25.0`, or use the preinstalled runner copy and assert `(Get-Module PSScriptAnalyzer -ListAvailable).Version`. The wrapper takes `-RequiredVersion`. | 1.24.0 changed `-EnableExit` semantics and raised the host minimum to 5.1, and an unpinned install floats across that. 1.25.0 (2026-03-20) is on the windows-2025 and ubuntu-24.04 images beside pwsh 7.6.6. Scope: all PowerShell gates (pinned decision, adopter may override the version). | cell 05-a. Watched red (measured 2026-10-11). | SHOULD |
| PS-GATE-11 | A PSScriptAnalyzer step turns findings into a non-zero status through the PS-GATE-01 wrapper, a `throw`, or `-EnableExit` on a pinned version. Printing the table is not a gate. | A step that prints findings and sets no status is a sixth fail-open mode: a green job over red findings, found in a public CI workflow. The cell is loose on purpose, any `exit`, `throw` or `EnableExit` in the file clears it, so a hit is a file to read. Scope: every file that calls `Invoke-ScriptAnalyzer`. | cell 11-a. Watched: plant lists the report-only workflow, twin silent (measured 2026-10-11). Confirmed 2026-10-11 on 12 fresh trees: 1 hit (PSAppDeployToolkit), a file to read. | MUST |

| Kind | Target | `ExcludeRules` | Enabled |
|---|---|---|---|
| script | 7.6 | `PSUseShouldProcessForStateChangingFunctions`, `PSUseApprovedVerbs`, `PSUseSingularNouns`, `PSUseBOMForUnicodeEncodedFile` (7.6-only scripts are BOM-less, also PS-ENC-06) | no compat rule |
| installer | 5.1 | the three style rules and `PSAvoidUsingWriteHost` (the exclusion does not mask `PSAvoidUsingInvokeExpression`) | `PSUseCompatibleSyntax`, `Enable = $true`, `TargetVersions = @('5.1')` |
| module | 5.1 and 7.6 | none, because verb and noun rules bind exported functions (mark private helpers with `Export-ModuleMember` or `SuppressMessage`) | `PSUseCompatibleSyntax` at 5.1 |
| test (Pester) | per target | `PSUseDeclaredVarsMoreThanAssignments`, `PSAvoidUsingConvertToSecureStringWithPlainText` (68 Error findings, all fixtures) and the script exclusions | none |

```powershell
# wrong: drops ParseError, so an unparseable file exits 0
Severity = @('Error', 'Warning')
# right: the installer kind, passed with -Settings
@{
    Severity     = @('Error', 'Warning', 'ParseError')
    ExcludeRules = @('PSAvoidUsingWriteHost', 'PSUseShouldProcessForStateChangingFunctions', 'PSUseApprovedVerbs', 'PSUseSingularNouns')
    Rules        = @{ PSUseCompatibleSyntax = @{ Enable = $true; TargetVersions = @('5.1') } }
}
```

## B. The 5.1 Parse Gate

The check is `checks/gates-parse-gate.ps1` under `powershell.exe`, plus the two listing cells.

```sh
for t in git xargs grep powershell.exe; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/gates-parse-gate.ps1 src tools
# 04-a (listing: files the parse skips by declaration, pass is every hit really 7-only)
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -i -E -e '^#requires[[:space:]]+-version[[:space:]]+[6-9]' --
# 08-a (listing: the built-in 5.1 shell wrapper)
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' | xargs -0 -r grep -H -n -E -e 'shell:[[:space:]]*powershell[[:space:]]*$' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-GATE-04 | Parse every 5.1-bound file (installer, module) on Windows PowerShell 5.1 in a required CI job that runs `checks/gates-parse-gate.ps1` under `powershell.exe`. On a Linux-only runner run `PSUseCompatibleSyntax` at 5.1 and the static greps of PS-COMPAT-02 instead, and label the job `5.1 syntax: static fallback`. | PSScriptAnalyzer misses four constructs that pwsh 7 parses, so a pwsh-only parse job is a false green: on the held-out trees `Parser::ParseFile` reported 76 errors in 19 files on 5.1 against 5 in 5 on pwsh 7.6.6, and 15 files failed only on 5.1. The script skips a file declaring `#Requires -Version 6` to 9 and any file under a `templates` or `*.template` directory (un-rendered placeholders), decodes a `.psd1` in a culture-named directory as UTF-8 as `Import-LocalizedData` does (43 false errors on 5 locale files otherwise), and treats `TypeNotFound`, `ModuleNotFoundDuringParse` and `ResourceNotDefined` (a DSC resource) as environment errors (exit 2: install the dependency, they are not syntax defects; confirmed 2026-10-11 on psake, Sampler and PSResourceGet, and the same ids are filtered in `compat-ast.ps1`, `ps-enc-writers.ps1`, `ps-sec-ast.ps1`, `ps-sec-signer.ps1` and the resolver). A run that parsed no file exits 2. Scope: installer and module kinds. | `checks/gates-parse-gate.ps1`, cell 04-a. Watched: a 7-only ternary exits 1 on 5.1 and 0 on 7.4.7 and 7.6.6, a compliant tree 0, a culture-directory file with a BOM-less UTF-8 string 0 and the same file elsewhere 1 on 5.1, a template-only tree, a missing type and a missing path exit 2 (measured 2026-10-11). | MUST |
| PS-GATE-08 | Run a 5.1 test job as the custom shell `powershell -NoProfile -NonInteractive -File {0}`, not the built-in `shell: powershell`, and let the script own Stop and its exit status (also PS-ERR-03, PS-ERR-04). | The built-in `-Command` wrapper closes stdin, which turns the `Invoke-WebRequest` prompt into a `NullReferenceException`, while `-NonInteractive -File` gives the deterministic message and exit 1. Scope: 5.1 installer test jobs. | cell 08-a (reading heuristic: the two invocation shapes were watched, the wiring was not). | SHOULD |

## C. Command Resolution

The check is `checks/gates-command-resolves.ps1`, run on the kind's target host (a `windows-latest` job running `powershell` for installers). Pass exact names with `-Allow`.

```sh
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/gates-command-resolves.ps1 -Path src -CheckParameters -Allow Invoke-ChocoThing
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-GATE-06 | Run `checks/gates-command-resolves.ps1` as an advisory check for installer and module kinds, in two modes: unresolved commands (the default) and unknown parameters (`-ParametersOnly`). Keep a repo-owned, reviewed `-Allow` list of exact command names, never a wildcard. Report in the job summary and never make either mode a required status check. | The default rule set gives 0 findings on a hallucinated cmdlet, and the resolver closes that gap (13 of 13 planted names and parameters caught, 0 findings on 25 twins) but untuned fails correct code: 3,986 of 4,062 unresolved-command findings (98.1%) over 9 held-out repos were false, mostly optional-module commands, and only 2 of 44 false names recur across repos, so each repo reviews its own list. A wildcard hides: `-Allow '*-Az*'` hides `Get-AzVMFooBarBaz`, so the script refuses one with exit 2. Unknown-parameter mode gave 58 findings and 0 false after the dynamic-parameter fix, on the same trees: it stays SHOULD after the fresh round (2026-10-11, 12 trees): on 5.1 it gave 18 findings, of which 5 were typos or wrong parameters (`Get-Command -Command-Type` twice, `Get-Command -Source` three times) and 13 were 7-only parameters in 7-bound files (`-Parallel`, `-AdditionalChildPath`, `-Prerelease`), and `Set-Item -Options` was a false positive until the `Function:` and `Alias:` provider parameters were added to the probe; on Linux pwsh 7.6.6 71 of 89 findings were `New-Object -ComObject`. A 5.1 run on a file declaring `#Requires -Version 6` or later now counts it as `requires7` and skips it. Version-gated commands are suppressed by design, which also hides a hallucination inside a gate, so the smoke run of PS-COMPAT-05 stays. Blind spots: .NET members, parameter values, splatted parameters, dynamic command names. Scope: the kind's target host (5.1 for installers). Host-bound: `Get-CimInstance` and `Get-NetAdapter` are unresolved on Linux pwsh 7.6.6 and resolve on 5.1, so a Linux run is a different, wrong check. | `checks/gates-command-resolves.ps1`. Watched: a hallucinated cmdlet exits 1, a bad parameter exits 1 under `-CheckParameters` and 0 without, a good file 0, an exact allow 0, a wildcard allow exits 2, a hallucination inside a version gate exits 0, on 5.1 and 7.6.6 (measured 2026-10-11). | SHOULD |

## D. Hygiene Greps

```sh
for t in git xargs grep; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# 07-a
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e '^[^#]*Set-ExecutionPolicy' -- | grep -v -E -e '-Scope[ :]+.?Process'
# 09-a
git ls-files -z --cached --others --exclude-standard -- '*.psd1' | xargs -0 -r grep -H -n -E -e "(Functions|Cmdlets|Aliases|Variables)ToExport[[:space:]]*=[[:space:]]*(@[(][[:space:]]*)?'[*]'" --
# 10-a (listing: a formatter step in a gate)
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -F -e 'Invoke-Formatter' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-GATE-07 | Installers never call `Set-ExecutionPolicy` beyond `-Scope Process`. Use `-ExecutionPolicy Bypass` on the command line, tell users `Unblock-File` for Mark-of-the-Web downloads, and never call it on non-Windows. | The policy is not a security boundary, writing CurrentUser or LocalMachine from an installer mutates the user's machine, and the call throws on non-Windows. Scope: installer kind (also PS-SEC). | cell 07-a. Watched red on a plant, silent on a twin with both parameter orders and a comment (measured 2026-10-11). | MUST |
| PS-GATE-09 | Module manifests carry no wildcard `*ToExport`. | A wildcard exports every function including private helpers, `PSUseToExportFieldsInManifest` warns on it by default, and 21 manifests on the held-out trees carried one. Scope: modules. | cell 09-a. Watched red on `'*'` and `@('*')`, silent on a named list (measured 2026-10-11). | SHOULD |
| PS-GATE-10 | Keep `Invoke-Formatter` out of the gate. | The formatter is idempotent but only 12 of 55 corpus files were already formatted, one errored, one failed on mixed line endings and 5.1 and 7.6 output differs for 1 of 25: a measured decision, not a check. Scope: all gates. | cell 10-a (listing). Watched red on a plant (measured 2026-10-11). | CONSIDER |

## E. Pester

The checks are `checks/pester-lint.sh` (run from the repository root, no arguments), `checks/Invoke-PesterGate.ps1` and `checks/Test-DiscoveryCode.ps1`, plus the cells. `pester-lint.sh` reports `P1` for rule 18 and `P2` for rule 20. All three scripts run on Pester 5.9.1 and 6.2.0. Pester also runs `*.tests.ps1`, so every pathspec here is `':(icase)*.tests.ps1'`.

```sh
pwsh -NoProfile -File scripts/Invoke-PesterGate.ps1 -Path tests -RequiredVersion 6.2.0 -MaxSkipped 0
```

```sh
for t in git xargs grep tr pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# 19-a (listing: a Pester caller that never sets Run.Exit and is not the gate)
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' '*.ps1' '*.psm1' | xargs -0 -r grep -l -e 'Invoke-Pester' -- | tr '\n' '\0' | xargs -0 -r grep -L -e 'Run.Exit' -e 'Exit[[:space:]]*=[[:space:]]*[$]true' -e 'Invoke-PesterGate' --
# 20-a (listing: a gating run that passes the flag)
git ls-files -z --cached --others --exclude-standard -- ':(icase)*.tests.ps1' '*.yml' '*.yaml' '*.ps1' | xargs -0 -r grep -H -n -F -e 'AllowNullOrEmptyForEach' --
# 21-a
git ls-files -z --cached --others --exclude-standard -- ':(icase)*.tests.ps1' | xargs -0 -r pwsh -NoProfile -NonInteractive -File scripts/Test-DiscoveryCode.ps1
# 22-a (listing: the host named in a test or a workflow)
git ls-files -z --cached --others --exclude-standard -- ':(icase)*.tests.ps1' '*.yml' '*.yaml' | xargs -0 -r grep -H -n -F -e '& pwsh' -e 'powershell.exe' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| SH-TEST-18 | Pin Pester with `-RequiredVersion` in every workflow, Taskfile and script that installs or imports it, and keep a version pin somewhere in every repo that calls `Invoke-Pester`: `Install-Module Pester -RequiredVersion 6.2.0 -SkipPublisherCheck` and `Import-Module Pester -RequiredVersion 6.2.0`. A repo that stays on Pester 5 pins 5.9.1, never 5.7.1. The pin decides the assertion syntax per file: `Should -Be` is valid on 5.7.1 through 6.2.0, `Should-*` only on 6.0.0 and later, and `Should.DisableV5 = $true` only where no classic assertion is left. | `Install-Module Pester -MinimumVersion 5.0` resolves to 6.2.0 today (`-MaximumVersion 5.99.99` gives 5.9.1). Pester 5 has been maintenance-only since 2026-07-07 and Pester 6 supports exactly the targets of 5.1 and 7.4 or later. Windows PowerShell 5.1 ships Pester 3.4.0 inbox, so an unversioned import can load it and answer a removed command in Pester 3 terms. A mismatch fails loudly (`Should-Be` on 5.7.1 is CommandNotFound), but a step titled "Install Pester (v5)" logged Pester v6.2.0. `#Requires -Modules` with a `ModuleVersion` is a floor that floats, not a pin. Scope: all Pester users, default 6.2.0 (pinned decision, adopter may override). | `checks/pester-lint.sh`, `P1`. Pin evidence anywhere in the repo clears the call-site half, a build file or a requirements manifest counts. Watched red on an unpinned install line and on a bare caller in a repo without a pin, silent on twins with a workflow pin, a manifest pin and a commented call, identical output under dash 0.5.12, busybox ash 1.37 and bash 3.2 `--posix` (measured 2026-10-11). Confirmed 2026-10-11 on 9 fresh repositories: `P1` matches case-insensitively (`Install-module Pester` in Crescendo was missed), counts `PesterVersion = 'x.y.z'` as a pin, and flags `Import-Module -Name pester -MaximumVersion 4.99` because a cap is not a pin; it found the unpinned `Invoke-Pester` calls in PSWriteHTML, Sampler and Invoke-Build. | MUST |
| SH-TEST-19 | Make the CI Pester step a census gate, `checks/Invoke-PesterGate.ps1`, with `Run.PassThru`, `Run.Exit = $true` and a post-run check that is red on failed tests, containers or blocks, zero tests discovered, zero passed, skipped above the leg's budget (`-MaxSkipped`, default 0), not-run, inconclusive, and for a generated suite a total different from `-ExpectedTotal`. A bare `Invoke-Pester` elsewhere sets `Run.Exit = $true`. | `Run.Exit` defaults to `False`, so a failing suite exits 0 on pwsh 7.4, 7.6 and Windows PowerShell 5.1. Even with it `$true`, a file with no `Describe`, a file whose every `It` is `-Skip`, and on Pester 5 an `-ForEach @()` all exit 0, and the gate forms of three public repos were green on zero tests and on skip-only runs. A generated suite whose data source vanishes shrinks its plan and its run together, and only `-ExpectedTotal` catches that. A Windows leg that legitimately skips 15 tests passes `-MaxSkipped 15`. Scope: every CI Pester run on pwsh 7.6 and Windows PowerShell 5.1 (the script is 5.1-safe), Pester 5.9.1 and 6.2.0. | `checks/Invoke-PesterGate.ps1` (exit 1 is red, exit 2 is an unimportable pin), cell 19-a. Watched on 5.9.1 and 6.2.0 under 7.4.7 and 5.1: a pass exits 0, a failing file, an empty file, a skip-only file, a skip over budget and a generated suite with no data and a pin of 3 exit 1, a skip within budget and a generated suite with data exit 0, and with no pin the empty generated suite is green on 5.9.1 and red on 6.2.0 (measured 2026-10-11). | MUST |
| SH-TEST-20 | Write no construct that Pester 6.0.0 removed: `Assert-MockCalled`, `Assert-VerifiableMock`, `Set-ItResult -Pending`, `It ... -Focus`. Use `Should -Invoke`, `-InvokeVerifiable`, `-Skipped` or `-Inconclusive`, tags and `Filter`. Never pass `-AllowNullOrEmptyForEach` in a gating run: pin the total with `-ExpectedTotal` (also SH-TEST-19). | These are the Pester 4 habits that training data is full of. On 6.2.0 they fail loudly, but on a 5.1 host without an explicit import the inbox Pester 3.4.0 answers `Assert-MockCalled` with an unrelated message. The flag, new in 6.0.0, turns the discovery error for an empty `-ForEach` into a silent empty `Describe`, and on 5.9.1 it is an unknown-parameter error. Scope: all `*.Tests.ps1` read case-insensitively, any pin of 6.0.0 or later. On a 5.x pin the forms still work, which is why SH-TEST-18 decides the pin first. A sparse checkout exits the scan with 74, so materialise the files first. | `checks/pester-lint.sh`, `P2`, cell 20-a. Watched red on `upper.Tests.ps1` and `lower.tests.ps1`, silent on a twin (measured 2026-10-11). The case-sensitive original listed only the first. | MUST |
| SH-TEST-21 | Put nothing at file level or directly inside `Describe` or `Context` except container commands (`Describe`, `Context`, `BeforeAll`, `AfterAll`, `BeforeDiscovery`, and inside bodies `It`, `BeforeEach`, `AfterEach`): setup goes in `BeforeAll`, generated cases in `BeforeDiscovery`. Each file is self-contained, with its own `Import-Module` and its own data. | Code outside `BeforeAll` runs at discovery, for filtered-out files and under `Run.SkipRun` too, and its variables and functions are invisible inside `It` (identical on Pester 5.7.1 and 6.2.0). Pester 6 does not promise cross-file discovery state. The first checker flagged 114 of 116 held-out hits wrongly (`InModuleScope` wrappers and `-ForEach` data), so this one treats `InModuleScope` as transparent and reports only names used inside run-phase blocks. Scope: all `*.Tests.ps1`. | cell 21-a (`checks/Test-DiscoveryCode.ps1`, output is the violation). Watched: a plant prints 4 findings and exits 1 and its twin exits 0, an `InModuleScope` plant 1 and its twin 0, a missing or unparseable file exits 1, on 7.4.7, 7.6.6 and 5.1 (measured 2026-10-11). | MUST |
| SH-TEST-22 | Run the script under test on the host it runs on in production. Run Pester itself on pwsh 7.6, and when the script under test is 5.1-bound pass the child host as a parameter (`powershell.exe` or `pwsh`) or keep a fixture-free 5.1 smoke. Import the pinned Pester by `-RequiredVersion` wherever Pester runs on 5.1. | Pester 6.2.0 behaved identically in 20 cases on pwsh 7.4.7, 7.6.6 and Windows PowerShell 5.1, so a Pester host matrix tests Pester, not the installer, and a suite that hard-codes `& pwsh -NoProfile -File` for a 5.1-bound installer tests the wrong host. Scope: PowerShell scripts under test whose production host differs from 7.6. | cell 22-a (reading heuristic: compare each hit with the host named by the script's `#Requires` line or its caller). Not watched red. | SHOULD |

## What Agents Get Wrong Here

Ranked by how often the failure appears. Each has a mechanical check.

| # | Failure | Check |
|---|---|---|
| 1 | Gates on `Invoke-ScriptAnalyzer -EnableExit` or a step that only prints the table, green on 256 findings or on an unparseable file | cells 01-a, 11-a, `checks/Invoke-PSGate.ps1` |
| 2 | Writes `Severity = @('Error','Warning')` "to cut noise", which drops ParseError | cells 02-a, 02-b |
| 3 | Runs the parse job on pwsh 7 and calls 5.1 code verified | `checks/gates-parse-gate.ps1` under `powershell.exe` |
| 4 | Runs `Invoke-Pester` with the default `Run.Exit` or counts only failures, green on zero tests | cell 19-a, `checks/Invoke-PesterGate.ps1` |
| 5 | Installs Pester with `-MinimumVersion 5.0` or imports it bare | `checks/pester-lint.sh` (`P1`) |
| 6 | Writes Pester 4 forms such as `Assert-MockCalled` | `checks/pester-lint.sh` (`P2`) |
| 7 | Puts setup in a `Describe` body or at file level, then reads it inside `It` | cell 21-a |
| 8 | Silences a gate with `-AllowNullOrEmptyForEach` or a wildcard `-Allow` | cell 20-a, the resolver's exit 2 |
| 9 | Has an installer call `Set-ExecutionPolicy RemoteSigned` | cell 07-a |
| 10 | Relies on implicit settings discovery or puts the compat rules in blocking settings | cells 03-a, 03-b |
