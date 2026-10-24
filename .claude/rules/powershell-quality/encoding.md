---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.powershell-quality-depth-on-demand"
title: PowerShell Encoding
summary: The PS-ENC family. BOMs, non-ASCII and curly quotes in code, en and em dashes as parameter prefixes, file writers and locale-dependent encoding names, reading files, native pipes and the console, and the runner files GITHUB_ENV, GITHUB_PATH, GITHUB_OUTPUT and GITHUB_STATE
---

# PowerShell Encoding

Rows bind to Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 (end of support 2026-11-10) and 7.6.6, and PSScriptAnalyzer 1.25.0 (measured 2026-10-11). Windows 7.x rows come from a de-DE host (ANSI cp1252, OEM cp437), Linux 7.6.6 is UTF-8 throughout. Interactive consoles, a UTF-8 system locale (ANSI 65001), CJK code pages, macOS and BSD are `unverified: read only`.

Owns the bytes of PowerShell sources and of the files and pipes they write: BOMs, non-ASCII and quote-like characters in code, writers and encoding names, reads, native stdout and the console, and the runner env files. Not owned here: targets, `#Requires` and 5.1 syntax walls are `PS-COMPAT`. Exit codes, `$ErrorActionPreference` and `exit` under `iex` are `PS-ERR`. Native argument passing is `PS-NATIVE`. Delivery by `irm | iex`, TLS and signing are `PS-SEC`. The PSScriptAnalyzer profile per kind and the gate wrapper are `PS-GATE`. Kinds: **5.1-bound** (installers, bootstrap, anything `irm | iex` or `powershell -File` runs on stock Windows), **7.6-only** (repo, dev, CI and Pester scripts), **module** (dual-edition). A listing hit closes by a fix or by `# tell-ok: <reason>` on the same line, and a bare `# tell-ok:` is still a hit.

Contents: [A Byte Scan Decides](#a-byte-scan-decides) · [A Grep or the Writer Scan Decides](#a-grep-or-the-writer-scan-decides) · [A Reading Heuristic Decides](#a-reading-heuristic-decides) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

## A Byte Scan Decides

Copy `checks/ps-enc-bytes.sh` and `checks/ps-enc-dash.sh` to `scripts/` (or set `ENC_BYTES` and `ENC_DASH`). Both are POSIX awk behind `#!/bin/sh` with the exit map 0 clean, 1 finding, 64 usage, 69 tool missing, 74 the scan failed, 78 not a work tree, 79 no files. Run from the repository root. Any `PREFLIGHT:` line means the host cannot judge. A cell passes with empty stdout and empty stderr, and `e02-served-listing` prints sites to read.

```sh
for t in git xargs grep awk tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
ENC_BYTES=${ENC_BYTES:-scripts/ps-enc-bytes.sh}
ENC_DASH=${ENC_DASH:-scripts/ps-enc-dash.sh}
[ -f "$ENC_BYTES" ] || echo "PREFLIGHT: $ENC_BYTES missing"
[ -f "$ENC_DASH" ] || echo "PREFLIGHT: $ENC_DASH missing"
# e01-nonascii
sh "$ENC_BYTES" nonascii
# e02-shebang-bom
sh "$ENC_BYTES" bom
# e02-served-listing
git ls-files -z --cached --others --exclude-standard -- '*.md' '*.ps1' '*.sh' '*.yml' '*.yaml' '*.txt' | xargs -0 -r grep -H -n -i -E -e '(irm|iwr|invoke-restmethod|invoke-webrequest)[^|#]*[|][[:space:]]*(iex|invoke-expression)' -e '(iex|invoke-expression)[^#]*(irm|iwr|invoke-restmethod|invoke-webrequest|downloadstring)' --
# e02-served-bom (name the files the listing shows, here install.ps1)
sh "$ENC_BYTES" bom-served ':(glob)**/install.ps1'
# e08-curly
sh "$ENC_BYTES" quote
# e11-dash
sh "$ENC_DASH"
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-ENC-01 | Give non-ASCII content in a 5.1-bound `.ps1`, `.psm1` or `.psd1` a UTF-8 BOM, or spell the character `[char]0xNNNN`. Keep a new on-disk file ASCII-only (owner default). Locale data loaded by `Import-LocalizedData` may be BOM-less UTF-8. | 5.1 reads a BOM-less UTF-8 file as cp1252: `café` has length 5, and an em dash in a double-quoted string decodes to `â€”` whose last character U+201D closes the string, so the file does not parse. The 35 of 82 held-out non-ASCII files without a BOM were the defect, the 47 with one worked. 7.4.7 and 7.6.6 on a cp1252 host read the same BOM-less file as UTF-8 (length 4). | `e01-nonascii`: any hit is a violation, except a locale `.psd1` that `Import-LocalizedData` loads (read the loader). Watched red (measured 2026-10-11): 3 files, twin 0. Behaviour watched on 5.1, 7.4.7 and 7.6.6 Windows (`dq-nobom` length 6 against 4). | MUST |
| PS-ENC-02 | Put no BOM (EF BB BF) on a `.ps1` that `irm \| iex` fetches or that starts with `#!`, and never add one to silence `PSUseBOMForUnicodeEncodedFile`: remove the non-ASCII. ASCII-only stays MUST for these files and for emitted PowerShell (also SH-EMIT-08, SH-EMIT-40). | `iex` keeps U+FEFF and fails with `The term '?Write-Output' is not recognized` on 5.1, 7.4.7 and 7.6.6 Windows. A BOM before a shebang exits 127. `irm \| iex` decodes by the server's charset: a bare `text/plain` or `application/octet-stream` gives ISO-8859-1 (`café` length 5). | `e02-shebang-bom` for shebang files. `e02-served-listing` names the served files, then `e02-served-bom` on each: any hit is a violation. A grep over all `.ps1` cannot judge: 391 BOM-ed files in one corpus held no shebang and no served file. Watched red (measured 2026-10-11): 1 and 1, twin 0. | MUST |
| PS-ENC-08 | Keep U+2018, U+2019, U+201A, U+201B, U+201C, U+201D and U+201E out of code positions of every `.ps1` and `.psm1`, 7.6-only files included. Non-ASCII is allowed in comments and in strings holding none of the seven. | All seven are quote tokens on 5.1, 7.4.7 and 7.6.6. With a correct UTF-8 decode `Write-Output "He said “hi” ok"` yields two outputs and no parse error, and `'it’s'` fails to parse. Chat and word-processor output produces them (also SH-EMIT-03). | `e08-curly`: any hit is a violation, except a help block or here-string you read as text. Watched red (measured 2026-10-11): 1 file, twin 0 (the twin keeps them in a comment). Tokenizer probe watched on all three engines. | MUST |
| PS-ENC-11 | Never put U+2013 or U+2014 in a code position, for example `Get-ChildItem –Path .`. | In a BOM-less 5.1-bound file it is a parse error, in a BOM-ed file or on 7.6.6 it runs. Chat output turns `-Path` into `–Path`. The BOM half is PS-ENC-01. | `e11-dash`: any hit is a violation, files with a BOM are skipped. Watched red (measured 2026-10-11): 1 file, twin 0 and BOM twin 0. Plant parse failure on 5.1 watched. Confirmed 2026-10-11 on 12 fresh trees against a tokenizer oracle: no BOM-less file has U+2013 or U+2014 in a code position, `e11-dash` prints 0 on all, and the one tree with code-position dashes (fleschutz, 22 tokens in 6 files) has a BOM on every file, which the cell skips by design. | MUST |

- Scope PS-ENC-01: 5.1-bound files, any engine reads them. 7.6-only files may be BOM-less UTF-8 (PS-ENC-06). Pinned decision (adopter may override): a new file is ASCII-only. cp932, cp936 and cp949 hosts are `unverified: read only`.
- Scope PS-ENC-02: all PowerShell. Shebang and served files are found by role, never by a grep over all files.
- Scope PS-ENC-08: all PowerShell. Corpus: 4 of 1,327 files, all deliberate tests or help prose.
- Scope PS-ENC-11: BOM-less files on 5.1, where the dash is a parse error. 7.6.6 and BOM-ed 5.1 files run it, so the rule still bans it for the reader.

## A Grep or the Writer Scan Decides

Copy `checks/ps-enc-writers.ps1` (AST, written in 5.1 syntax, exit 0, 1 or 79) and `checks/ps-enc-step-shell.py` (PyYAML, run as `python3 -I`) to `scripts/`. Run `e03-ast` on the oldest engine the code must support. The `e10-*` cells apply to 5.1-capable code, and `e10-redirect-ps1` prints sites to read.

```sh
for t in git xargs grep tr pwsh python3; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
python3 -I -c 'import yaml' >/dev/null 2>&1 || echo "PREFLIGHT: python3 PyYAML missing"
ENC_WRITERS=${ENC_WRITERS:-scripts/ps-enc-writers.ps1}
STEP_SHELL=${STEP_SHELL:-scripts/ps-enc-step-shell.py}
[ -f "$ENC_WRITERS" ] || echo "PREFLIGHT: $ENC_WRITERS missing"
[ -f "$STEP_SHELL" ] || echo "PREFLIGHT: $STEP_SHELL missing"
ps1s() { git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1'; }
yamls() { git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml'; }
# e03-ast
ps1s | xargs -0 -r pwsh -NoProfile -NonInteractive -File "$ENC_WRITERS"
# e03-names
ps1s | xargs -0 -r grep -H -n -i -E -e '^[^#]*-Encoding[[:space:]:]+.?utf8NoBOM([^[:alnum:]_]|$)' -e '^[^#]*-Encoding[[:space:]:]+.?utf8BOM([^[:alnum:]_]|$)' -e '^[^#]*-Encoding[[:space:]:]+.?ansi([^[:alnum:]_]|$)' --
# e03-utf8-static
ps1s | xargs -0 -r grep -H -n -i -E -e '(WriteAllText|AppendAllText|WriteAllLines)[^;]*\[(System\.)?Text\.Encoding\]::UTF8([^[:alnum:]_]|$)' --
# e07-locale
ps1s | xargs -0 -r grep -H -n -i -E -e '^[^#]*-Encoding[[:space:]:]+.?default([^[:alnum:]_]|$)' -e '^[^#]*-Encoding[[:space:]:]+.?oem([^[:alnum:]_]|$)' -e '^[^#]*-Encoding[[:space:]:]+.?ansi([^[:alnum:]_]|$)' -e '^[^#]*\[(System\.)?Text\.Encoding\]::Default([^[:alnum:]_]|$)' -- | grep -v -e 'tell-ok: *[^ ]'
# e10-redirect
yamls | xargs -0 -r grep -H -n -E -e '>>[[:space:]]*"?[$]env:GITHUB_(ENV|PATH|OUTPUT|STATE)' -- | while IFS=: read -r f l _; do python3 -I "$STEP_SHELL" "$f" "$l" || echo "PS-ENC-10: step-shell failed on $f:$l"; done | grep -e 'shell=powershell' -e 'NOSTEP'
# e10-redirect-ps1
ps1s | xargs -0 -r grep -H -n -E -e '>>[[:space:]]*"?[$]env:GITHUB_(ENV|PATH|OUTPUT|STATE)' --
# e10-writers
{ ps1s; yamls; } | xargs -0 -r grep -H -n -E -e 'Add-Content.*GITHUB_[A-Z]+' -e 'Out-File.*GITHUB_[A-Z]+' -- | grep -v -E -e 'Out-File.*-Encoding[[:space:]]+.?utf8.?([[:space:]]|$)'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-ENC-03 | In 5.1-bound code write text files only with `[IO.File]::WriteAllText($p, $t, (New-Object System.Text.UTF8Encoding $false))` or `AppendAllText`, or `-Encoding ascii` for data that is ASCII by construction. Never `>`, `>>`, `Out-File`, `Tee-Object`, `Set-Content`, `Add-Content` or `Export-Csv` without `-Encoding ascii`. Never `-Encoding utf8` (one exception: PS-ENC-10), `utf8NoBOM`, `utf8BOM`, `ansi` or a code page number. Never pass `[Text.Encoding]::UTF8` to a writer. | 5.1 bytes depend on the cmdlet: `>`, `Out-File` and `Tee-Object` write UTF-16LE with BOM, `Set-Content` and `Add-Content` cp1252 (a CJK character becomes `?`), `-Encoding utf8` a BOM, `Export-Csv` ASCII. `Out-File -Append` onto a UTF-8 file appends UTF-16LE into it. The 5.1 ValidateSet rejects `utf8NoBOM`, `utf8BOM`, `ansi` and numbers. 7.4.7 and 7.6.6 write UTF-8 without BOM for all of those by default and for `utf8`, but `unicode`, `utf32` and `utf8BOM` keep a BOM, so never infer 7.x bytes from 7.x defaults. | `e03-ast` is the check of record. `e03-names` and `e03-utf8-static` are the fallback on a host without pwsh: they pass `-Encoding utf8` and do not see `>`. Any hit is a violation. Watched red (measured 2026-10-11): `e03-ast` 23 lines on 7.4.7 and 7.6.6 and 24 on 5.1.26100.9444 (the en-dash plant does not parse there), `e03-names` 2, `e03-utf8-static` 2, twin 0 on all three (the twin keeps `$null` redirects, the runner-file form and `[Text.Encoding]::UTF8.GetString`). Writer bytes watched on all three engines. Confirmed 2026-10-11 on 12 fresh trees on 5.1 and pwsh 7.6.6: `e03-ast` runs from the root with `.`, and `ps-enc-writers.ps1` no longer reports a missing-type or DSC parse error as `PARSE`. `e03-names` and `e07-locale` now skip a commented line (`^[^#]*`, so a `#` earlier on the line hides a hit, which is why `e03-ast` stays the check of record): a commented `-Encoding utf8NoBOM` in a tree was a false hit, red and green on a plant and a twin. | MUST |
| PS-ENC-07 | Never pass a locale-dependent encoding: `-Encoding Default`, `OEM`, `ANSI` or `[Text.Encoding]::Default`. Name `ascii`, `utf8` as PS-ENC-03 allows, or `New-Object System.Text.UTF8Encoding $false`. For a legacy consumer use `[Text.Encoding]::GetEncoding(437)` or `GetEncoding(1252)` with a comment naming the consumer. | The same source writes different bytes by engine and machine. `Default` is cp1252 on 5.1 and UTF-8 on 7.4.7 and 7.6.6, `ANSI` is rejected on 5.1 and is cp1252 on 7.4+, and `OEM` follows the system OEM code page (437 here, `chcp 850` did not change it) on every engine on Windows and is UTF-8 on Linux 7.6.6. | `e07-locale` and `e03-ast`: a hit whose same line says `# tell-ok: consumer NAME` is the exception. It misses `-Enc` abbreviations and a splatted `Encoding` key. Watched red (measured 2026-10-11): `e07-locale` 4 sites, twin 0 (a `# tell-ok:` line is silent in both cells). A commented-out `-Encoding Default` is silent after the 2026-10-11 round. | MUST |
| PS-ENC-10 | In code that can run under 5.1, write `GITHUB_ENV`, `GITHUB_PATH`, `GITHUB_OUTPUT` and `GITHUB_STATE` with exactly one of two writers: `` [IO.File]::AppendAllText($env:GITHUB_ENV, "NAME=value`n", (New-Object System.Text.UTF8Encoding $false)) `` (the default) or `"NAME=value" \| Out-File -FilePath $env:GITHUB_ENV -Encoding utf8 -Append`. Never `>>`, `Out-File` without `-Encoding utf8`, `-Encoding unicode` or `Add-Content`, and never mix a safe writer with a banned one on one file. On pwsh 7.x `>>` is also safe (also SH-CI-11). | The runner creates the files empty and reads them with BOM detection, and both safe writers decode alone and mixed, because `Out-File -Encoding utf8` writes its BOM only into an empty file. Of 21 writer sequences on 5.1, 7 corrupt the file, each with a banned writer. A lone `>>` decodes (UTF-16LE), and `Add-Content` breaks on the first non-ASCII value (`C:\Users\Jürgen\bin`). On 7.4.7 and 7.6.6 all 21 decode. | `e10-redirect` (a `>>` in a step whose shell is `powershell`, or no step found), `e10-writers`, and `e10-redirect-ps1` to read. Watched red (measured 2026-10-11): `e10-redirect` 1 line, `e10-writers` 3, `e10-redirect-ps1` 1, twin 0 (`shell: pwsh` and the two safe writers are silent). The runner readers are emulated from runner source, not observed on a hosted job. | MUST |

- Scope PS-ENC-03: 5.1-bound code. 7.6-only scripts may redirect, and runner files are PS-ENC-10.
- Scope PS-ENC-07: all PowerShell. Corpus: 2 hits, a checksum writer with `-Encoding oem` for ASCII hex (the named-consumer form).
- Scope PS-ENC-10: installers, `shell: powershell` steps and a `.ps1` started by `powershell.exe`. `shell: pwsh` steps are exempt.

```powershell
"NAME=$v" >> $env:GITHUB_ENV                 # wrong on 5.1: UTF-16LE
$t | Set-Content $p                          # wrong on 5.1: cp1252
```

```powershell
$u8 = New-Object System.Text.UTF8Encoding $false
[IO.File]::AppendAllText($env:GITHUB_ENV, "NAME=$v`n", $u8)
[IO.File]::WriteAllText($p, $t, $u8)
```

## A Reading Heuristic Decides

The cells list sites. A listing pass is every hit fixed, waived with a reason, or read and left.

```sh
for t in git xargs grep; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
ps1s() { git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1'; }
# e04-bare-read-listing
ps1s | xargs -0 -r grep -H -n -e 'Get-Content' -- | grep -v -e '-Encoding' -e 'tell-ok: *[^ ]'
# e05-console-listing
ps1s | xargs -0 -r grep -H -n -i -E -e 'Console\]::OutputEncoding' -e '[$]OutputEncoding' --
# e06-bom-rule-listing
git ls-files -z --cached --others --exclude-standard -- '*.psd1' | xargs -0 -r grep -H -n -F -e 'PSUseBOMForUnicodeEncodedFile' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-ENC-04 | Pass the encoding when reading a file this script did not write: `Get-Content -Encoding UTF8` or `[IO.File]::ReadAllText` for UTF-8, the legacy encoding named per PS-ENC-07 for legacy input. | Bare `Get-Content` decodes ANSI on 5.1 and UTF-8 on 7.x. A BOM-less UTF-8 `é` reads `00c3 00a9` on 5.1 and `00e9` on 7.x. A cp1252 file (what 5.1 `Set-Content` writes) reads `00e9` on 5.1 and U+FFFD on 7.x. | `e04-bare-read-listing` lists every bare read, ASCII-only inputs included (leads, not violations). Watched red (measured 2026-10-11): 1 site, twin 0. Behaviour watched on all three engines. | SHOULD |
| PS-ENC-05 | On Windows keep text that crosses a native pipe or the console ASCII. Where UTF-8 must pass, set `[Console]::OutputEncoding` to `New-Object System.Text.UTF8Encoding $false` around the call on every engine (on 5.1 also `$OutputEncoding`) and restore both in `finally`. Never decode a child's stdout as UTF-8 unless the child set the console encoding. | On 5.1 `$OutputEncoding` is `us-ascii` (`é` becomes `?`) and the console is IBM437. On 7.4.7 and 7.6.6 `$OutputEncoding` is UTF-8 but the console is still the OEM code page, so UTF-8 stdout decodes as mojibake (length 8 against 5), and a redirected `pwsh.exe` or `powershell.exe` stdout carries OEM bytes (`0x81` for `ü`). The setting outlives the process, so the next process on the console emits UTF-8 unless restored. Linux 7.6.6 is UTF-8. | `e05-console-listing`: every assignment to `[Console]::OutputEncoding` has a restore in `finally`, read each. Reading heuristic. Behaviour watched on all three engines. Since the 2026-10-11 round the cell matches `[Console]::OutputEncoding` and `$OutputEncoding` only, not a Pester or tool config key named `OutputEncoding` (a `CodeCoverage.OutputEncoding` setting was a false hit). | SHOULD |
| PS-ENC-06 | Let 7.6-only scripts be UTF-8 without BOM, with `PSUseBOMForUnicodeEncodedFile` excluded in their PSScriptAnalyzer settings under a reason comment. Keep it on in the installer and module settings. | The rule is always on and contradicts the advice to avoid BOMs on Unix-visible files, and the agent's usual fix breaks `#!` scripts (exit 127). 7.4.7 and 7.6.6 on a cp1252 host decode a BOM-less UTF-8 `.ps1`, `Get-Content -Raw` and `Import-Module` as UTF-8. | `e06-bom-rule-listing`: each hit sits in `ExcludeRules` of a 7.6-script or test settings file with a comment, and in no installer or module file. Placement is a reading heuristic. The warning on a BOM-less non-ASCII file watched red, green with the exclusion. | SHOULD |
| PS-ENC-09 | To save a native command's stdout to a file, use `child > file` only on 7.4+, or `cmd /c "child > file"` on any engine. Never `child \| Out-File` or `child \| Set-Content`. | `>` of a native command is byte-preserving on 7.4+ (UTF-8 bytes arrive as written) and UTF-16LE mojibake on 5.1. The pipeline forms decode the child's bytes with the console code page first (7.4.7 and 7.6.6 wrote `e2 94 9c ...`). | Reading heuristic for call sites, no grep. Behaviour watched on all three engines: `>`, `\| Out-File`, `\| Set-Content` and `cmd /c`. | SHOULD |

- Scope PS-ENC-04: any engine. The 5.1 and 7.x halves matter when a file crosses editions.
- Scope PS-ENC-05: Windows, any engine. 5.1-bound code keeps messages ASCII.
- Scope PS-ENC-06: script and test kinds only. Confirmed on a Windows host, not just Linux.
- Scope PS-ENC-09: Windows, any engine. `-Encoding` on the pipeline writers does not change the decode of the child's bytes.

## What Agents Get Wrong Here

| # | Mistake | Rule | Check |
|---|---|---|---|
| 1 | Writes a log, report or config with `>`, `Out-File`, `Set-Content` or `-Encoding utf8` and assumes UTF-8 | PS-ENC-03 | `e03-ast` |
| 2 | Appends to `$GITHUB_ENV` or `$GITHUB_PATH` with `>>` or `Add-Content` in a `shell: powershell` step | PS-ENC-10 | `e10-redirect`, `e10-writers` |
| 3 | Adds a BOM to silence `PSUseBOMForUnicodeEncodedFile`, then the `#!` script exits 127 or `iex` rejects the file | PS-ENC-02 | `e02-shebang-bom` |
| 4 | Pastes an em dash, en dash or curly quote from chat into code | PS-ENC-11, PS-ENC-08 | `e11-dash`, `e08-curly` |
| 5 | Writes `é` or `—` into a BOM-less 5.1-bound file | PS-ENC-01 | `e01-nonascii` |
| 6 | Uses `-Encoding utf8NoBOM`, `ansi`, `Default` or `OEM`, which 5.1 rejects or which differ by machine | PS-ENC-03, PS-ENC-07 | `e03-names`, `e07-locale` |
| 7 | Decodes a native child's stdout as UTF-8 on Windows without setting the console encoding | PS-ENC-05 | `e05-console-listing` |
