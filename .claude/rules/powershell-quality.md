---
paths:
  - "**/*.ps1"
  - "**/*.psm1"
  - "**/*.psd1"
summary: The PowerShell quality index, holding the gate, the target by script kind, the hoisted MUST rules, the tell cells, and where the depth lives
keywords: powershell,pwsh,windows-powershell,ps1,psm1,psd1,psscriptanalyzer,pester,installer,irm-iex,encoding,native-commands
license: Apache-2.0
repository: https://github.com/ocx-sh/grimoire-lore
---

# PowerShell Quality

Traps, not tutorials. Every line names a mistake generated PowerShell makes by default.
The language, its cmdlets and the .NET types are already in the model, and a repository's layout is discoverable by reading it, so neither is in this file.

Contents: [The Gate](#the-gate) · [Rules This File Owns](#rules-this-file-owns) · [Target by Kind](#target-by-kind) · [Non-Negotiables](#non-negotiables) ·
[Tell Cells](#tell-cells) · [Waiver Tokens](#waiver-tokens) · [Where the Depth Is](#where-the-depth-is) · [Severity](#severity) · [Siblings](#siblings) · [Dates](#dates)

**Before editing a PowerShell file, decide its kind and its target edition from the table in Target by Kind (PS-CORE-02).** The same text runs on Windows PowerShell 5.1
and on PowerShell 7, and the two disagree on syntax, encoding and error handling. "5.1-bound" below means a file or code path that `powershell.exe` can start on stock Windows.
This rule installs with `paths: ["**/*.ps1", "**/*.psm1", "**/*.psd1"]` and loads on every edit of those files and on nothing else. A workflow `run:` body, a Taskfile item
or a `.yml` file loads `shell-quality` instead, which carries the run-step copies SH-CI-11 and SH-CI-12. It ships beside `shell-quality` in the `shell-essentials` bundle.

## The Gate

Run it after every change to a file this rule covers, from the repository root, and stop at the first non-zero exit. Copy the scripts the depth files name from this rule's
`powershell-quality/checks/` directory to `scripts/` once (pinned decision, adopter may override the directory). Run step 1 once per kind directory with that kind's settings
file (PS-GATE-03 holds the four), step 2 on a Windows PowerShell 5.1 host over the 5.1-bound paths, and step 3 where a `*.Tests.ps1` exists. A script is judged by its exit status.
A cell in a depth file is judged by its output: empty stdout and empty stderr is the pass, and a `PREFLIGHT:` line means stop and fix the host (SH-GATE-25, SH-GATE-26).

```sh
for t in git pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
pwsh -NoProfile -File scripts/Invoke-PSGate.ps1 -Path . -Settings PSScriptAnalyzerSettings.script.psd1 -RequiredVersion 1.25.0    # 1 PS-GATE-01. Rename path and settings per kind. Exit 0 clean, 1 findings, 2 misconfigured or no result on this host
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/gates-parse-gate.ps1 src tools    # 2 PS-GATE-04. Rename the paths to the 5.1-bound directories. Exit 0 clean, 1 a syntax error, 2 cannot judge
pwsh -NoProfile -File scripts/Invoke-PesterGate.ps1 -Path tests -RequiredVersion 6.2.0 -MaxSkipped 0    # 3 SH-TEST-19. Exit 0 green, non-zero red
```

## Rules This File Owns

The PS-CORE family is defined here and nowhere else. Every other family is defined in a depth file and cited here by ID, and the three rows below restate none of them.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-CORE-01 | Gate every change to a PowerShell file through the block above: the PSScriptAnalyzer wrapper (PS-GATE-01 to PS-GATE-03, PS-GATE-11), the 5.1 parse on a 5.1 host for every 5.1-bound file (PS-GATE-04, which also gives the Linux-only fallback) and, where `*.Tests.ps1` exist, the Pester census gate (SH-TEST-19). Judge each step by its exit status. Never gate on a raw `Invoke-ScriptAnalyzer -EnableExit`, and never accept a pwsh 7 parse of a 5.1-bound file. | Raw PSScriptAnalyzer 1.25.0 fails open five ways (256 findings exit 0, a `Severity` list without `ParseError`, a missing settings path, a mistyped rule name, an empty directory), a pwsh 7 parse accepts every 7-only construct, and `Run.Exit` defaults to `False` so a failing Pester suite exits 0. Each leaves a green job over broken code. | The block above, each step judged by exit status, and the gates.md cells 01-a, 02-a, 02-b, 03-a, 03-b, 04-a, 05-a, 11-a and 19-a for a file that bypasses it. Each script is watched red and green in its owner row. Watched red (measured 2026-10-11): step 1 exits 1 on a planted `Invoke-Expression` and step 2 exits 1 on a planted ternary on Windows PowerShell 5.1.26100.9444 but 0 on pwsh 7.6.6, step 3 exits non-zero on two failing tests, and every step exits 0 on the twin on both engines. | MUST |
| PS-CORE-02 | Write each file to the target its kind pins in the table below, and declare it with the `#Requires` line of PS-COMPAT-01. The kind follows the delivery route and never the file name: a script that `irm \| iex` or `powershell -File` runs on stock Windows is an installer even under `tools/`. | An agent trained on 7-era text writes 7-only syntax into a file stock Windows runs on 5.1. One such line makes the whole file unrunnable there, while the author's pwsh 7 run stays green. | Cells `C1a` and `C1b` of compat.md for the `#Requires` line, the 5.1 parse of PS-CORE-01 for installer and module files. Choosing the kind is a reading step. Pinned decision (adopter may override). | MUST |
| PS-CORE-03 | Run the tell cells `T1` to `T10` below over authored PowerShell, and fix every hit or end its line with `# tell-ok: REASON`. They find and never decide: the MUST for each tell lives in its owner rule, and a hit in a 7.6-only script is fine where the owner scopes the rule to 5.1-bound code. | These are the failures an agent emits unprompted that pass silently on the author's pwsh 7: a native that fails under `Stop`, 7-only syntax, a writer with the wrong bytes, a bare `curl`, `iwr` without `-UseBasicParsing`, `exit` in an `iex` script. | The cells below, PREFLIGHT first. Output is a lead, empty is the pass, and `T4` and `T8` list files, so a waiver cannot end their line. Watched red (measured 2026-10-11): 24 lines on a plant holding every tell (`T10` included) and nothing on its twin, identical on GNU grep 3.12 under bash, dash and `zsh -f` and on busybox 1.37 with git. A bare or empty `# tell-ok:` is still printed. | SHOULD |

## Target by Kind

Pinned decision (adopter may override): the target follows the kind. PS-COMPAT-01 owns the `#Requires` line. A `shell: powershell` workflow step is 5.1 and a `shell: pwsh` step is 7.x.

| Kind | Target and `#Requires` | Source bytes | Settings file (PS-GATE-03) |
|---|---|---|---|
| installer: bootstrap, anything `irm \| iex` or `powershell -File` runs on stock Windows | Windows PowerShell 5.1, `#Requires -Version 5.1`, plus a version test in code because `iex` ignores it | ASCII, and no BOM on a file `iex` fetches or that starts with `#!` (PS-ENC-01, PS-ENC-02) | installer |
| script: repo, dev and CI scripts | PowerShell 7.6, `#Requires -Version 7.6`, never `7.0`, `7.4` or `7.5` | UTF-8 without BOM (PS-ENC-06) | script |
| module | Both editions: manifest `PowerShellVersion = '5.1'` and `CompatiblePSEditions = @('Desktop','Core')`, imported on a real 5.1 and on 7.6 (PS-COMPAT-08) | 5.1-bound: a BOM or `[char]0xNNNN` for non-ASCII (PS-ENC-01) | module |
| test: `*.Tests.ps1` | Pester 6.2.0 on pwsh 7.6, the script under test on its production host (SH-TEST-18, SH-TEST-22) | as script | test |

## Non-Negotiables

Every line below blocks a merge within the scope it names. IDs resolve through [Where the Depth Is](#where-the-depth-is), where each rule carries its rationale and verification.

| # | Rule | ID |
|---|---|---|
| 1 | Gate through `checks/Invoke-PSGate.ps1`, never a raw `Invoke-ScriptAnalyzer -EnableExit`. Parse every 5.1-bound file on Windows PowerShell 5.1 in a required CI job, because a pwsh 7 parse is a false green. | PS-GATE-01, PS-GATE-04 |
| 2 | The CI Pester step is `checks/Invoke-PesterGate.ps1`, red on zero tests, skip-only runs and over-budget skips, never a bare `Invoke-Pester`. | SH-TEST-19 |
| 3 | Keep 7-only syntax out of 5.1-bound files and prove it with the 5.1 parser (`compat-ast.ps1` under `powershell.exe`), never with pwsh 7 or PSScriptAnalyzer alone. | PS-COMPAT-02 |
| 4 | Detect the platform in 5.1-bound code with `[Environment]::OSVersion.Platform`, never `$IsWindows` alone or an unguarded `$IsX` variable under StrictMode. | PS-COMPAT-04 |
| 5 | Pass `-UseBasicParsing` to every `Invoke-WebRequest` and `iwr` in 5.1-bound code. | PS-COMPAT-06 |
| 6 | Check the exit status of every native command whose failure matters, because `$ErrorActionPreference = 'Stop'` does not stop on a native exit code on any engine. A `pwsh` or `powershell` run step with two or more natives checks each one, because the last native wins. | PS-ERR-01, PS-ERR-03 |
| 7 | Never rely on `$PSNativeCommandUseErrorActionPreference` in code that can run on 5.1, where it is a silent no-op. | PS-ERR-02 |
| 8 | Write `curl.exe` and `tar.exe`, never bare `curl` or `wget`, in any script a 5.1 host can run. | PS-NATIVE-01 |
| 9 | Build native argv as an array and splat it, never one string, `Invoke-Expression` or `Start-Process -ArgumentList` with a string. | PS-NATIVE-02 |
| 10 | No BOM on a `.ps1` that `irm \| iex` fetches or that starts with `#!`, and never add one to silence `PSUseBOMForUnicodeEncodedFile`. | PS-ENC-02 |
| 11 | In 5.1-bound code write files only through `UTF8Encoding($false)` .NET writers or `-Encoding ascii`, never `>`, `>>` or `-Encoding utf8`, with one exception: the runner files `GITHUB_ENV`, `GITHUB_PATH`, `GITHUB_OUTPUT` and `GITHUB_STATE` take `AppendAllText` with `UTF8Encoding($false)` or `Out-File -Encoding utf8 -Append`, never `>>` or `Add-Content`. | PS-ENC-03, PS-ENC-10 |
| 12 | In an `irm \| iex` script no function calls `exit` and nothing uses `break` as an exit. Functions throw or return, and one guarded top-level exit sets `LASTEXITCODE` and returns when `iex` delivered it. | PS-SEC-09 |
| 13 | An installer verifies the download (SHA-256, or a `Valid` Authenticode signature with an anchored signer pin) before every path that reaches an extract or a run. A blank or absent digest aborts. | PS-SEC-01 |
| 14 | In 5.1-bound code touch `ServicePointManager.SecurityProtocol` only on the Desktop edition, assign exactly `3072` (plus `12288` behind a `GetNames` test), never name `Tls13`, `Tls11`, `Tls` or `Ssl3` and never `-bor` onto the current value. | PS-SEC-06 |

## Tell Cells

Each cell prints sites, and the owner rule in its comment carries the MUST. They are the cheap layer for a host with no 5.1 interpreter, and a hit inside a string or here-string is a lead to read.
`Write-Host` for data and an empty `catch` are PSScriptAnalyzer's (PS-ERR-07, PS-ERR-08), so the gate of PS-CORE-01 already runs them. `T10` runs `P1` and `P2` of SH-TEST-18 and SH-TEST-20.

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
ps1s() { git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1'; }
# T1 7-only syntax: && and || chains, ??. Owner PS-COMPAT-02
ps1s | xargs -0 -r grep -H -n -E -e '^[^#]* && ' -e '^[^#]* [|][|] ' -e '[?][?]' -- | grep -v -e 'tell-ok: *[^ ]'
# T2 $IsWindows with no edition, version, Get-Variable or variable: existence test on the line. Owner PS-COMPAT-04
ps1s | xargs -0 -r grep -H -n -E -e '[$]IsWindows([^A-Za-z0-9_]|$)' -- | grep -v -i -e 'PSVersionTable' -e 'PSEdition' -e 'Get-Variable' -e 'variable:' -e 'tell-ok: *[^ ]'
# T3 curl or wget with an option, used as a command. Owner PS-NATIVE-01
ps1s | xargs -0 -r grep -H -n -E -e '^[[:space:]]*([$][A-Za-z0-9_]+[[:space:]]*=[[:space:]]*)?[(]?(curl|wget)[[:space:]]+-' -- | grep -v -e 'tell-ok: *[^ ]'
# T4 Invoke-WebRequest or iwr in a file that never names UseBasicParsing. Owner PS-COMPAT-06
ps1s | xargs -0 -r grep -H -l -i -E -e 'Invoke-WebRequest' -e '(^|[^[:alnum:]_-])iwr([^[:alnum:]_-]|$)' -- | tr '\n' '\0' | xargs -0 -r grep -H -L -i -e 'UseBasicParsing' --
# T5 exit in a file documented as piped to iex. Owner PS-SEC-09
ps1s | xargs -0 -r grep -l -E -e '[|][[:space:]]*(iex|Invoke-Expression)([[:space:]]|$)' -- | tr '\n' '\0' | xargs -0 -r grep -H -n -w -e 'exit' -- | grep -v -e 'tell-ok: *[^ ]'
# T6 a text writer with no -Encoding on its line. Owner PS-ENC-03
ps1s | xargs -0 -r grep -H -n -F -e 'Out-File' -e 'Set-Content' -e 'Add-Content' -e 'Tee-Object' -e 'Export-Csv' -- | grep -v -e '-Encoding' -e 'tell-ok: *[^ ]'
# T7 a locale or 7-only encoding name. Owners PS-ENC-03 and PS-ENC-07
ps1s | xargs -0 -r grep -H -n -i -E -e '-Encoding[[:space:]:]+.?(utf8NoBOM|utf8BOM|ansi|default|oem)([^[:alnum:]_]|$)' -- | grep -v -e 'tell-ok: *[^ ]'
# T8 a native called at line start in a file that never checks LASTEXITCODE (add the repository's tool names as -e). Owner PS-ERR-01
ps1s | xargs -0 -r grep -H -L -e 'LASTEXITCODE' -e 'Invoke-Native' -- | tr '\n' '\0' | xargs -0 -r grep -H -l -e '^[[:space:]]*&[[:space:]]' -e '^[[:space:]]*tar[[:space:]]' -e '^[[:space:]]*git[[:space:]]' --
# T9 Set-ExecutionPolicy beyond -Scope Process. Owner PS-GATE-07
ps1s | xargs -0 -r grep -H -n -E -e '^[^#]*Set-ExecutionPolicy' -- | grep -v -E -e '-Scope[ :]+.?Process' -e 'tell-ok: *[^ ]'
# T10 the Pester pin and the removed-in-6 forms, P1 and P2, from the repository root
sh scripts/pester-lint.sh
```

## Waiver Tokens

Two tokens exist. A waiver ends the line it excuses and carries a reason. A bare token or an empty reason is still printed by the filter `grep -v -e 'TOKEN: *[^ ]'` and counts as a defect.

| Token | Owner | Waives |
|---|---|---|
| `tell-ok: REASON` | PS-CORE-03 | a hit of a tell cell above, and a hit of any listing cell in the depth files that a reviewer accepts after reading it |
| `vd-ok: REASON` | SH-GATE-21 to SH-GATE-29 | a catalog line that must show a banned form, so a self-check cell skips it |

## Where the Depth Is

Read the file for the work you are about to do, not for the topic it is filed under. One level deep: these files do not point at each other, so a task that spans two families names both.
A script a depth file names as `checks/NAME` lives in this rule's `powershell-quality/checks/` directory.

| Doing… | Read |
|---|---|
| Writing PowerShell that must run on both Windows PowerShell 5.1 and PowerShell 7.x: syntax, cmdlets and parameters by edition, platform detection, `#Requires`, `-UseBasicParsing`, importing a module on both editions | [powershell-quality/compat.md](powershell-quality/compat.md) |
| A native command fails and the script keeps going: `$ErrorActionPreference`, a `try/catch` that does not catch, `2>&1` under `Stop`, a CI step that goes green on a failed build, an entry point's exit status, stray output on stdout, a StrictMode error in `finally` | [powershell-quality/errors.md](powershell-quality/errors.md) |
| Calling a native executable: argument arrays and splatting, `curl.exe` against the 5.1 alias, `Start-Process -ArgumentList`, empty strings and embedded quotes | [powershell-quality/native.md](powershell-quality/native.md) |
| Writing or reading a file, adding non-ASCII text, a BOM, an em dash or a curly quote to a script, appending to `GITHUB_ENV` or `GITHUB_PATH`, or decoding a native command's output | [powershell-quality/encoding.md](powershell-quality/encoding.md) |
| Setting up PSScriptAnalyzer, the 5.1 parse gate, the gate wrapper or the command resolver, calling `Set-ExecutionPolicy`, exporting from a manifest, or writing Pester tests (`*.Tests.ps1`, SH-TEST-18 to 22) | [powershell-quality/gates.md](powershell-quality/gates.md) |
| Writing or reviewing an `install.ps1` or anything run through `irm \| iex`: hashes and signatures, TLS, `exit` against `throw`, `Invoke-Expression`, a URL override | [powershell-quality/security.md](powershell-quality/security.md), plus [compat.md](powershell-quality/compat.md) for the 5.1 floor and [encoding.md](powershell-quality/encoding.md) for the BOM |
| Editing a `.psd1`: a module manifest (`PowerShellVersion`, `CompatiblePSEditions`, `*ToExport`) or a PSScriptAnalyzer settings file (`Severity`, `ExcludeRules`) | [powershell-quality/compat.md](powershell-quality/compat.md) and [powershell-quality/gates.md](powershell-quality/gates.md) |
| Writing a script that a workflow step runs, or the PowerShell body of a `run:` step: several natives in one step, a custom `shell:`, appending to the runner files | [powershell-quality/errors.md](powershell-quality/errors.md) and [powershell-quality/encoding.md](powershell-quality/encoding.md), plus `shell-quality` (sibling rule: SH-CI, loaded on the workflow file) |
| Writing the `.sh` half of a script pair, a workflow, a Taskfile or a GitLab job | `shell-quality` (sibling rule: SH-DIAL, SH-ERR, SH-CI and the rest) |
| Emitting PowerShell from Rust, Go or Python | `shell-quality` (sibling rule: SH-EMIT, in its emit depth), and the `shell-integration` skill |
| Writing or auditing an `install.ps1` end to end, or diagnosing a PowerShell 5.1 parse error or an exit under `irm \| iex` from its symptom | the `installer-script` and `shell-diagnose` skills |
| Writing a usage header or a comment block in a script | `code-docs` (sibling rule) |

## Severity

MUST = Block: fix before it lands. SHOULD = Warn: fix, or state why not in the commit body. CONSIDER = Suggest: never blocks, never re-raised after a decline.
Every MUST row in a depth file names the kind or edition it binds, so the rows that block a single edit stay few.

A row with `provisional: true` on its Scope line has a watched check and waits for a confirmation round on fresh trees. The pinned decisions encode an agreed choice rather than a derivable fact,
and each is a default an adopter may override once, in the repository's shared config and never per file: the target by kind (PS-CORE-02), the 7.6 floor, new on-disk files ASCII-only (PS-ENC-01),
PSScriptAnalyzer 1.25.0 with four per-kind settings files, Pester 6.2.0 (5.9.1 for a repository that stays on Pester 5), and `scripts/` as the home of the copied checks.
Overriding one is a recorded decision, and re-arguing one in a pull request is not a review comment.

## Siblings

- **`shell-quality`**: `.sh`, `.bash`, workflow, Taskfile and GitLab CI files, with its own gate script. It owns SH-GATE (the cell dialect every command here is written in), SH-CI (SH-CI-11 and SH-CI-12
  are the self-contained copies of the PowerShell run-step rules, because a workflow edit loads that rule and not this one) and SH-EMIT (SH-EMIT-08 and SH-EMIT-40 keep emitted PowerShell ASCII).
- **`code-docs`** and the `code-docs-cleanup` skill: comments and header blocks in every language. It already loads on `*.ps1`, and PS-SEC-08 reads an `irm | iex` script's usage header.
- **`installer-script`, `shell-integration`, `shell-diagnose` and `shell-port`**: procedures that cite these IDs by number and never restate them.
  The `shell-essentials` bundle ships all of them with this rule, `shell-quality`, `code-docs` and `code-docs-cleanup`.

## Dates

Every version claim in this rule set was measured 2026-10-11 on Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 and 7.6.6, PSScriptAnalyzer 1.25.0 and Pester 5.9.1 and 6.2.0.
Dated end point: pwsh 7.4 and 7.5 end support 2026-11-10, so no `#Requires` floor names them. macOS and BSD rows are unverified: read only.
