---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.powershell-quality-depth-on-demand"
title: PowerShell Native Commands
summary: The PS-NATIVE family. Which native a bare name reaches on 5.1, how argv is built and splatted, and what each engine does to empty strings, embedded quotes and trailing backslashes
---

# PowerShell Native Commands

Rows bind to Windows PowerShell 5.1.26100.9444, pwsh 7.4.7 (end of support 2026-11-10) and pwsh 7.6.6, with PSScriptAnalyzer 1.25.0 where a row names it (measured 2026-10-11). macOS and BSD behaviour is `unverified: read only`.

Owns the names a script uses for native tools, the shape of the argument list it hands them and the engine differences in how that list reaches the child. Not owned here: checking a native's exit status, `$PSNativeCommandUseErrorActionPreference` and stderr under `Stop` are `PS-ERR`. Engine targets and 5.1 syntax are `PS-COMPAT`. The encoding of a native's output is `PS-ENC`. Untrusted input, secrets in argv and installer front doors are `PS-SEC`. The Pester and PSSA gates are `PS-GATE`.

Contents: [Group 1: A Grep or PSSA Decides](#group-1-a-grep-or-pssa-decides) · [Group 2: A Probe and Reading Decide](#group-2-a-probe-and-reading-decide) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Group 1: A Grep or PSSA Decides

Cells `N01` and `N01w` list sites to read (every hit fixed or waived). `N02` and `N02i` pass on empty output. All four run from the repository root.

```sh
for t in git xargs grep pwsh; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
# N01: a statement that starts with curl or wget and an option
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e '^[[:space:]]*([$][A-Za-z0-9_]+[[:space:]]*=[[:space:]]*)?[(]?(curl|wget)[[:space:]]+-' --
# N01w: the same in workflow run blocks, keep hits in a step whose shell is powershell
git ls-files -z --cached --others --exclude-standard -- '*.yml' '*.yaml' | xargs -0 -r grep -H -n -E -e '^[[:space:]]*(run:[[:space:]]*|-[[:space:]]+)?(curl|wget)[[:space:]]+-' --
# N02: Start-Process with one string argument that contains a space
git ls-files -z --cached --others --exclude-standard -- '*.ps1' '*.psm1' | xargs -0 -r grep -H -n -E -e '^[^#<]*Start-Process[^#]*-ArgumentList[ ]+("[^",]*[ ][^",]*"|'\''[^'\'',]*[ ][^'\'',]*'\'')[ ]*(-[A-Za-z]+|$|[|])' --
# N02i: Invoke-Expression, found by the analyzer (parse diagnostics such as TypeNotFound come out of -IncludeRule too, so the rule name is filtered)
pwsh -NoProfile -NonInteractive -Command 'if (-not (Get-Module -ListAvailable PSScriptAnalyzer)) { "PREFLIGHT: PSScriptAnalyzer missing" } else { Invoke-ScriptAnalyzer -Path . -Recurse -IncludeRule PSAvoidUsingInvokeExpression | Where-Object { $_.RuleName -eq "PSAvoidUsingInvokeExpression" } | ForEach-Object { "{0}:{1}: {2}" -f $_.ScriptPath, $_.Line, $_.RuleName } }'
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-NATIVE-01 | Write `curl.exe` and `tar.exe` (and `sort.exe` or `diff.exe` when the Unix tool is meant) in `.ps1`, `.psm1` and `powershell` run blocks that can run on 5.1. Never a bare `curl` or `wget`. | On 5.1 they are aliases of `Invoke-WebRequest`, so `curl -fsSL` dies with "parameter name 'fsSL'". pwsh 7.4.7 and 7.6.6 on Windows resolve `curl.exe`, so the bug is 5.1's and PSSA run from a Linux pwsh cannot see it. Scope: 5.1-capable code. A file that runs only on 7.x is out of scope (23 of 23 held-out hits were one 7.x test file). The first form of the grep matched a package name and comments, 25 of 48 false. | `N01`, `N01w`. Watched red on a plant, silent on the `curl.exe` twin, GNU grep and busybox 1.37 (measured 2026-10-11). | MUST |
| PS-NATIVE-02 | Build native argv as an array and splat it: `$argv = @('xf', $archive, '-C', $dest)` then `& tar.exe @argv`. Never pass one string, never `Invoke-Expression "tool $args"`, never `Start-Process -ArgumentList "string"`. Use `Start-Process` only for `-Wait`, elevation or redirect-to-file, with each element pre-quoted. | One string is one argument on every mode. `Start-Process -ArgumentList @('has space')` splits on spaces on all three engines and 5.1 rejects an empty element. `Invoke-Expression` re-parses the text, so an argument that holds `;` or `$(` runs. Scope: all PowerShell. | `N02` (the first form was 13 of 18 false) and `N02i`. An array `-ArgumentList` is not grep-visible: read every remaining `Start-Process` site. Watched red, array twin silent, PSSA red on `Invoke-Expression` (measured 2026-10-11). | MUST |

```powershell
# wrong: one string, then a re-parse
Start-Process tar -ArgumentList "xf $archive -C $dest" -Wait
Invoke-Expression "tar xf $archive"
# right
$argv = @('xf', $archive, '-C', $dest)
& tar.exe @argv
```

## Group 2: A Probe and Reading Decide

Copy `checks/native-args.ps1` to `scripts/`. It prints what a child process receives for five hazards and always exits 0, so the evidence is the rows. Run it under each engine the script targets and read the `argc` and bracketed values. On pwsh 7.4.7 and 7.6.6 (`Standard`) every row is intact. On 5.1 `empty-middle` is `argc=2`, `dquote-and-space` splits into `[say hi] [there]` and `trailing-backslash` is `[C:\dir with space"]`. On 7.x with `Legacy` set, the empty and quote rows match 5.1.

```sh
command -v pwsh >/dev/null 2>&1 || echo "PREFLIGHT: pwsh missing"
pwsh -NoProfile -NonInteractive -File scripts/native-args.ps1
# Windows only: powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/native-args.ps1
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-NATIVE-03 | In 5.1-bound code do not pass an empty string, an embedded `"` or a trailing `\` before a closing quote to a native. Prefer an environment variable, stdin or a file. Never pass untrusted input to a `.bat` or `.cmd`. If a quote cannot be avoided, escape it only when `$PSNativeCommandArgumentPassing` is absent or `Legacy`. | 5.1 and 7.x `Legacy` drop the empty string, strip the quotes and corrupt `C:\dir with space\`. 7.3 and later `Standard` and `Windows` pass them intact, so an unconditional `\"` escape is wrong on 7. `.cmd`, `.bat`, `cmd.exe` and `find.exe` keep `Legacy` parsing even in `Windows` mode. Scope: 5.1-bound code and anything that sets `Legacy`. | The probe on 5.1 (red rows above) and on 7.4.7 or 7.6.6 (intact). Call sites are a reading step: any `''` or interpolated user text in a native call. Watched on three engines (measured 2026-10-11). | SHOULD |

## What Agents Get Wrong Here

Ranked by how often a coding agent emits it unprompted times how long it stays green on the author's pwsh 7.

1. **`curl -fsSL` or `wget` in a script that a 5.1 host runs.** It works on 7 and dies on 5.1 with a parameter error. `PS-NATIVE-01`, cell `N01`.
2. **Building the command as a string.** `Invoke-Expression "tool $args"` and `Start-Process -ArgumentList "a b"` inject or collapse. `PS-NATIVE-02`, cells `N02`, `N02i`.
3. **Fixing a 5.1 quoting bug with `\"` everywhere.** It double-escapes on 7.3 and later. `PS-NATIVE-03`, the probe.
4. **An empty string or a path ending in `\` handed to a native.** 5.1 drops the first and eats the closing quote on the second. `PS-NATIVE-03`, a reading step.
5. **Expecting `@('has space')` to survive `Start-Process`.** It splits on every engine. `PS-NATIVE-02`, a reading step.
