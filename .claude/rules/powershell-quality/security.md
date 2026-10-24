---
# Read on demand through the index. This glob matches no file, so a client never auto-loads the file.
paths:
  - "**/.powershell-quality-depth-on-demand"
title: PowerShell Installer Security
summary: The PS-SEC family. Digest and signature checks before extraction, where the digest comes from, zip containment, TLS protocol selection, how an irm-to-iex script is documented and survives, Invoke-Expression, execution-policy advice and https-only URL overrides
---

# PowerShell Installer Security

Rows bind to Windows PowerShell 5.1.26100.9444 (.NET Framework 4.8.1), pwsh 7.4.7 and 7.6.6 and PSScriptAnalyzer 1.25.0, all measured 2026-10-11. pwsh 7.4 ends support 2026-11-10. The git-fed cells run on GNU grep and busybox 1.37 with git, and macOS or BSD is `unverified: read only`.

Owns what an `install.ps1` or any script delivered by `irm | iex` must do to be safe to run: verify a download before it is extracted or run, the source of the expected digest, honest verification claims, zip-entry containment, `ServicePointManager.SecurityProtocol`, the one-liner's spelling, the iex-safe script shape (one guarded exit, no `Mandatory`, no `$PSScriptRoot`, no session leaks), `Invoke-Expression`, execution-policy advice and https-only overrides. Script kinds: INST is an installer or bootstrap, a repo script is not bound. Not owned here: encoding, BOM and the 5.1 runner-file writer (`PS-ENC`), `$ErrorActionPreference` and native exit codes (`PS-ERR`, `PS-NATIVE`), `#Requires`, `-UseBasicParsing` and the 5.1 syntax floor (`PS-COMPAT`), the analyzer wrapper and the ban on persistent policy changes (`PS-GATE`), `install.sh` (`SH-INST`) and CI steps (`SH-CI`). `PS-SEC-12` and `PS-SEC-13` are retired and not reused, and `PS-COMPAT-09` and `PS-ERR-05` are retired into `PS-SEC-06` and `PS-SEC-09`.

Contents: [Group 1: A Script Run Decides](#group-1-a-script-run-decides) · [Group 2: A Listing Decides](#group-2-a-listing-decides) · [Group 3: A Reading Step Decides](#group-3-a-reading-step-decides) · [What Agents Get Wrong Here](#what-agents-get-wrong-here)

## Group 1: A Script Run Decides

Checks `A1`, `B1`, `B2` and `B3` (copy them from `checks/` to `scripts/`). Pass is exit 0, a finding is exit 1. Run them under `powershell.exe` for a 5.1 installer, and again under pwsh 7.6 when the script also targets it.
`A1` is `ps-sec-ast.ps1`. It parses the file with the host's own parser and prints `UNVERIFIED-PATH`, `NO-LITERALPATH`, `EXIT-IN-FUNCTION`, `BREAK-OUTSIDE-LOOP`, `EXIT-TOPLEVEL-UNGUARDED` and `INVOKE-EXPRESSION-TOKEN`, each with its rule ID. `UNVERIFIED-PATH` needs a verifying statement before the extract or run call on that call's own path, so a hash in one branch does not excuse the other, and `-Verifier REGEX` names the project's own helper. A `Start-Process` of a local file is a known false positive, clear it by reading. Exit 78 no git work tree, 79 no file.
`B1`, `ps-sec-iex-survival.ps1`, delivers the script as `irm | iex` does in a child host and requires that the host survives and that a forced failure still reaches the step status. `-EnvVar` drives the script down its failure path. `B2`, `ps-sec-iex-leak.ps1`, reports what iex left in the caller's session, and reports RED (exit 1) for a script whose top-level `exit` ends the host, since the caller's session ends the same way. `B3`, `ps-sec-signer.ps1`, extracts the installer's signature function without running the script and feeds it mocked `Get-AuthenticodeSignature` results. `B1` to `B3` execute code, run them in a throwaway container with no credentials.

```sh
for t in git powershell; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree"
# A1: findings by AST (add -Verifier REGEX for the project's own verifying helper)
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/ps-sec-ast.ps1
# B1: SUBJECT is the iex-delivered script, run on both host models
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/ps-sec-iex-survival.ps1 -Path "$SUBJECT" -HostModel repl
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/ps-sec-iex-survival.ps1 -Path "$SUBJECT" -HostModel ci
# B2: the caller's session after iex
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/ps-sec-iex-leak.ps1 -Path "$SUBJECT"
# B3: the signature function (default name Test-Signature), PIN is the pinned subject component such as O=Vendor Inc.
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/ps-sec-signer.ps1 -Path "$SUBJECT" -Signer "$PIN"
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-SEC-01 | In every `install.ps1` that extracts or runs a downloaded archive or binary, verify it before every path that reaches `Expand-Archive`, `ExtractToDirectory`, `ExtractToFile` or `& $payload`. Compare its SHA-256 (`Get-FileHash -LiteralPath $f -Algorithm SHA256`) with an expected digest, or require a verified Authenticode signature, accepted only with `Status -eq 'Valid'` and an anchored full-string signer pin such as `(^\|, )O=Vendor Inc\.(,\|$)`. A mismatch, an unreadable file, an absent or blank expected digest or an invalid signature aborts (`throw`, exit 4 at top level, also `SH-INST-09`). Scope: INST, 5.1 and 7.x. One hash call somewhere in the file is not the check, a second branch can extract unverified. | cargo-dist's installer extracts straight after the download, a fail-open `Verify-Checksum` warns and returns on a blank digest, and an unanchored signer regex accepts a look-alike publisher, so each installs a swapped payload. | `A1` (`UNVERIFIED-PATH`), `G1`, `G2`, `B3` for a signature, the tamper run `R2` | MUST |
| PS-SEC-05 | Pass `-LiteralPath` to `Get-FileHash` and `Expand-Archive`, and to `Test-Path` and `Remove-Item` on computed paths. Never `-Path` or a positional path. Scope: 5.1 and 7.x. | `[` and `]` are wildcards: `Get-FileHash -Path 'ocx[1].zip'` returns `$null` with an empty error list on all three engines. SHOULD because the fail-closed verifier of `PS-SEC-01` turns that into an abort. | `A1` (`NO-LITERALPATH`, positional forms too), `G5` | SHOULD |
| PS-SEC-09 | In an iex-delivered script no function calls `exit` and no code uses `break` or `continue` outside a loop as an exit. Functions `throw` or `return` a status, one guarded exit sits at script top level (below). Detect iex with `[string]::IsNullOrEmpty($PSCommandPath)`. Scope: `install.ps1` and any script a user pastes at a prompt. `& file.ps1` is safe with `exit`, it runs in a child scope. Fleet sites: www-setup `src/install.ps1:182` (the `Err` helper calls `exit`) and `:734`. | `exit` inside an `irm \| iex` script kills the interactive host on 5.1, 7.4.7 and 7.6.6. The scoop `break` idiom survives a prompt and swallows the failure in a CI step, the single-exit shape passes both models and `-File`. | `A1` (`EXIT-IN-FUNCTION`, `BREAK-OUTSIDE-LOOP`, `EXIT-TOPLEVEL-UNGUARDED`), `B1` on `repl` and `ci` | MUST |
| PS-SEC-10 | Keep `Invoke-Expression` and `iex` out of a delivered `.ps1` unless a front door has hash-verified the second stage in memory and carries a justified `SuppressMessageAttribute('PSAvoidUsingInvokeExpression', ...)`, and run that analyzer rule in the gate (also `PS-GATE-01`). `& ([scriptblock]::Create($text))` is not a fix by itself. Scope: PSScriptAnalyzer 1.25.0. | The rule matches a token (Warning, always enabled), and the scriptblock form has the same power and the same host kill. | `A1` (`INVOKE-EXPRESSION-TOKEN`, comments and help blocks are not code), else `G10` | SHOULD |

```powershell
function Err($m) { [Console]::Error.WriteLine($m); exit 3 }          # wrong: kills the host that pasted irm | iex
$IsIex = [string]::IsNullOrEmpty($PSCommandPath)                      # right: functions throw, one exit at the top
$rc = 0
try { Main } catch { [Console]::Error.WriteLine("installer: error: $($_.Exception.Message)"); $rc = 3 }
if ($rc -ne 0) { if ($IsIex) { $Global:LASTEXITCODE = $rc; return }; exit $rc }
```

Watched: `A1` red and green on Windows PowerShell 5.1.26100.9444 and pwsh 7.6.6 (watched red, measured 2026-10-11). Eight plants exit 1, each with its one token (an unverified branch, no hash, a bare `ExtractToDirectory`, positional paths, `exit` in a function, the `break` idiom, a top-level `exit`, a real `iex`) and seven twins exit 0. A project helper passes only with `-Verifier`. `B1` red and green on both engines: `exit` in a function is red on `repl` and green on `ci`, the `break` form is green on `repl` and red on `ci`, the single-exit form is green on both, and a script that floods stderr does not hang the check. `B2` red on a script that sets `Stop`, StrictMode, a function and a variable, green on the `& { }` twin. `B3` is green on an anchored pin and red on an unanchored regex, a missing `Status` test and a status-only check, on both engines.

## Group 2: A Listing Decides

Checks `G1` to `G12`. A hit is a file or line to read and the pass is every hit fixed or waived. `grep -L` cells list files, so judge them by the files listed, never by exit status. Narrow the pathspec to the installer directory when repo scripts live beside it. `G10` lists the hits `A1` skips (a `<# #>` help block holding `irm \| iex`).

```sh
for t in git xargs grep tr; do command -v "$t" >/dev/null 2>&1 || echo "PREFLIGHT: $t missing"; done; git rev-parse --is-inside-work-tree >/dev/null 2>&1 || echo "PREFLIGHT: no usable git work tree" # vd-ok: xargs here is a tool name, not an invocation
psls() { git ls-files -z --cached --others --exclude-standard -- "$@"; }
# G1: a file that extracts and names no hash or signature call (presence only, order is A1)
psls '*.ps1' | xargs -0 -r grep -l -e Expand-Archive -e ExtractToDirectory -e ExtractToFile -- | tr '\n' '\0' | xargs -0 -r grep -L -e Get-FileHash -e ComputeHash -e Get-AuthenticodeSignature --
# G2: a file that reads an Authenticode signature and never compares Status with Valid
psls '*.ps1' | xargs -0 -r grep -l -e Get-AuthenticodeSignature -- | tr '\n' '\0' | xargs -0 -r grep -L -F -e "-eq 'Valid'" -e "-ne 'Valid'" -e '-eq "Valid"' -e '-ne "Valid"' --
# G3: a file or doc that claims a SHA-256 or checksum in a file with no hash call
psls '*.ps1' '*.md' | xargs -0 -r grep -l -i -E -e 'sha-?256' -e checksum -- | tr '\n' '\0' | xargs -0 -r grep -L -e Get-FileHash -e ComputeHash --
# G4: ExtractToFile in a file with no GetFullPath( containment check
psls '*.ps1' | xargs -0 -r grep -l -e ExtractToFile -- | tr '\n' '\0' | xargs -0 -r grep -L -F -e 'GetFullPath(' --
# G5: Get-FileHash or Expand-Archive with -Path (positional forms are A1)
psls '*.ps1' | xargs -0 -r grep -n -H -E -e 'Get-FileHash( +-Algorithm +[A-Za-z0-9]+)? +-Path' -e 'Expand-Archive +-Path' --
# G6a: a named legacy protocol, or -bor onto the current value
psls '*.ps1' '*.psm1' | xargs -0 -r grep -n -H -E -e 'SecurityProtocolType\]::Tls13' -e 'SecurityProtocolType\]::Tls11' -e 'SecurityProtocolType\]::Ssl3' -e 'SecurityProtocolType\]::Tls[^0-9]' -e 'SecurityProtocolType\]::Tls$' -e "SecurityProtocol *= *'(Ssl3|Tls|Tls11)'" -e 'SecurityProtocol *= *"(Ssl3|Tls|Tls11)"' -e 'SecurityProtocol *-bor' -e 'SecurityProtocol *= *[^=]*SecurityProtocol *-bor' --
# G6b: an assignment in a file that never tests PSEdition
psls '*.ps1' '*.psm1' | xargs -0 -r grep -l -E -e 'SecurityProtocol *=' -- | tr '\n' '\0' | xargs -0 -r grep -L -e PSEdition --
# G7: the .Content pipe form of the one-liner, in scripts and docs
psls '*.ps1' '*.md' | xargs -0 -r grep -n -H -E -e '[.]Content *[|] *(iex|Invoke-Expression)' -e '(iex|Invoke-Expression) *[(].*[)][.]Content' --
# G8: in a file documented as piped to iex, a Mandatory parameter or $PSScriptRoot
psls '*.ps1' | xargs -0 -r grep -l -E -e '[|][[:space:]]*(iex|Invoke-Expression)([[:space:]]|$)' -- | tr '\n' '\0' | xargs -0 -r grep -n -H -E -e 'Mandatory' -e '[$]PSScriptRoot' --
# G8a: a file documented as piped to iex with no in-code version test
psls '*.ps1' | xargs -0 -r grep -l -E -e '[|][[:space:]]*(iex|Invoke-Expression)([[:space:]]|$)' -- | tr '\n' '\0' | xargs -0 -r grep -L -F -e PSVersion --
# G9: the Path formula in a file that sets StrictMode
psls '*.ps1' | xargs -0 -r grep -l -F -e 'Set-StrictMode -Version' -- | tr '\n' '\0' | xargs -0 -r grep -n -H -F -e 'MyInvocation.MyCommand.Path' --
# G10: an Invoke-Expression or iex token outside a comment (a hit in a help block is a false positive, A1 skips it)
psls '*.ps1' | xargs -0 -r grep -L -e 'SuppressMessage.*PSAvoidUsingInvokeExpression' -- | tr '\n' '\0' | xargs -0 -r grep -n -H -E -e '^[^#]*(Invoke-Expression|[|;(] *iex( |$))' --
# G11: any Get-ExecutionPolicy
psls '*.ps1' | xargs -0 -r grep -n -H -F -e Get-ExecutionPolicy --
# G12: a URL read from the environment in a file with no https test
psls '*.ps1' '*.psm1' | xargs -0 -r grep -l -i -E -e 'env:[A-Za-z_]*URL' -- | tr '\n' '\0' | xargs -0 -r grep -L -F -e "-notmatch '^https://" -e 'StartsWith("https://"' --
```

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-SEC-03 | Make no verification claim the delivered `.ps1` does not perform. A header, README or docs sentence containing `SHA-256`, `sha256` or `checksum` requires a `Get-FileHash` or `ComputeHash` call in the same file, or names the verified delegate and states what stays unverified. Scope: INST and the docs that describe it. | A front door that says the installer "verifies the downloaded archive's SHA-256" while the generated installer has no hash call gives a false assurance. | `G3` (prose in docs is a reading step) | MUST |
| PS-SEC-04 | Never extract a zip with `Join-Path $dest $entry.FullName` plus `ExtractToFile` lacking a containment check. Use `[System.IO.Compression.ZipFile]::ExtractToDirectory` (after `Add-Type -AssemblyName System.IO.Compression.FileSystem` on 5.1), `Expand-Archive -LiteralPath` (name the file `.zip`, 5.1 rejects other extensions), or a per-entry loop that rejects rooted, drive-letter and `..` entries and requires `[IO.Path]::GetFullPath([IO.Path]::Combine($root, $name)).StartsWith($root + $sep, OrdinalIgnoreCase)`. Scope: 5.1 and 7.x. Older .NET Framework or Archive 1.0.1.0 builds are `unverified: read only`. | The manual loop wrote `evil-dotdot.txt` and `evil-backslash.txt` outside the destination on all three engines. It is also what an agent writes when told to avoid `Expand-Archive`. | `G4` (`GetFullPath(` needs the open paren, a bare name matched a comment) | MUST |
| PS-SEC-06 | Touch `ServicePointManager.SecurityProtocol` only in a 5.1-bound script, only when `$PSVersionTable.PSEdition -eq 'Desktop'` and the value is not `SystemDefault`. Then assign exactly `3072` (Tls12), `-bor 12288` (Tls13) only if `[enum]::GetNames([Net.SecurityProtocolType]) -contains 'Tls13'`. Never name `::Tls13`, `::Tls11`, `::Tls` or `::Ssl3` and never `-bor` onto the current value. Scope: every script, module or installer bound to 5.1 that touches the property. pwsh 7 ignores it. A real .NET below 4.7 host (Server 2012 R2) is `unverified: read only`. | Microsoft says not to set it on .NET 4.7+. A missing enum member throws `PropertyNotFoundException` under StrictMode Latest, an unconditional assignment persists in the caller's session under iex, and `-bor Tls12` keeps the legacy protocols on. | `G6a`, `G6b`, the 5.1 print `R3` | MUST |
| PS-SEC-07 | Document the iex one-liner as `irm URL \| iex` or `iwr -UseBasicParsing URL \| iex`, never `(iwr ...).Content \| iex` and never `iex (iwr ...).Content`. Document flags through environment variables and `& ([scriptblock]::Create((irm URL))) -Flag`. Scope: scripts and docs, 5.1 and 7.x. | `.Content` is `byte[]` for `application/octet-stream` (release assets), so iex evaluates numbers and the installer silently does not run. `param()` cannot be bound by iex. | `G7` | MUST |
| PS-SEC-08 | A script documented as `\| iex` (a) tests the PowerShell version in code, not by `#Requires` alone. (b) Declares params with defaults only, never `Mandatory`. (c) Never uses `$PSScriptRoot`, `$PSCommandPath` or `$MyInvocation.MyCommand.Path` for resources. (d) Detects iex with `[string]::IsNullOrEmpty($PSCommandPath)`, never `$null -eq $MyInvocation.MyCommand.Path` once `Set-StrictMode -Version` is in effect. (e) Runs the body in `& { ... }` or restores `$ErrorActionPreference`, StrictMode and `SecurityProtocol` in `finally`, and defines no global function named like a common command (`Say`, `Err`, `Warn`, `Main`). Scope: iex-delivered scripts, "documented as" is decided by the header usage line. | `#Requires -Version 99.0` ran under iex on all three engines. A `Mandatory` param is silently empty, `$PSScriptRoot` is the empty string so `Join-Path` throws, the Path formula throws under StrictMode Latest, and iex leaves `Stop`, StrictMode and every function in the caller's session. | (a) `G8a`, (b) and (c) `G8`, (d) `G9`, (e) `B2` | MUST |
| PS-SEC-11 | Do not gate an installer on `Get-ExecutionPolicy` and print no `Set-ExecutionPolicy` advice beyond `-Scope Process`. Document `powershell -ExecutionPolicy Bypass -c "irm URL \| iex"` for hosts that block scripts. Scope: INST, the stock Windows client default is Restricted. The ban on persistent policy changes is `PS-GATE-07`. | iex text is not subject to the policy: under `Restricted` a harmless iex ran, the gate threw, and `-File` was refused by the policy itself. Microsoft calls it defense in depth, not a security boundary. | `G11` | MUST |
| PS-SEC-14 | A `.ps1` or `.psm1` that takes a download URL from an environment variable or a parameter refuses a non-https URL with `throw` before any request: `if ($u -notmatch '^https://') { throw "refusing non-https download URL: $u" }`. Scope: INST and modules, `install.sh` is `SH-INST-06`. | PowerShell has no `--proto`, so an environment override reaches the request unchecked and a plain http request left a real installer on 5.1. | `G12` for the environment half, the parameter half and the order are `R2` | MUST |

Watched: `G1` to `G12` red on a planted file and silent on its twin, on GNU grep and busybox 1.37 with git (watched red, measured 2026-10-11). `G6a` fires on each of eight legacy spellings and stays silent on `Tls12` and the guarded block, `G12` covers a `.psm1`, and `G8a` lists a `$text \| iex` pipe as well as a usage line. Confirmed 2026-10-11 on 12 fresh trees: `G12` lists 3 sites (mesa, Yayaml, PSResourceGet), each a line to read. `A1` and `B1` to `B3` were run on a fresh script with a top-level `exit` (fleschutz `list-aliases.ps1`, read-only) and a twin that returns instead, with `env -i` and a throwaway HOME on pwsh 7.6.6 and an in-process environment scrub on 5.1: `A1` red (2 `EXIT-TOPLEVEL-UNGUARDED`) and green, `B1` red on `repl` and `ci` and green on the twin's `repl`, `B2` exits 2 on the exiting script and green on the twin. `B3` found no fresh subject (no file pins a signer in a function that takes a path).

## Group 3: A Reading Step Decides

- `R1` digest provenance trace (`PS-SEC-02`): for each `Get-FileHash` comparison find where the expected value is fetched and compare that host with the archive URL host. No lexical pattern separates a manifest on another host from a sidecar on the same host, so it cannot be a command.
- `R2` tamper and refusal runs (`PS-SEC-01`, `PS-SEC-14`): serve a valid zip with other bytes through the installer's URL override and require an abort before anything is extracted, and point the URL variable at a loopback `http` listener and require that it sees no request.
- `R3` on a 5.1 host, run the TLS block and print `[Net.ServicePointManager]::SecurityProtocol`, it reads `Tls12` or `Tls12, Tls13`.

| ID | Rule | Rationale | Verification | Severity |
|---|---|---|---|---|
| PS-SEC-02 | Take the expected digest from a source other than the archive's own download response: embedded in the script, inline in a manifest served from a different origin than the archive, or covered by a signature. A same-origin sidecar is allowed only when the header and docs call it corruption detection, not a trust root. Scope: INST. | A compromised release replaces the archive and its sidecar together. | `R1` (reading only, no command can decide it) | MUST |

## What Agents Get Wrong Here

| Rank | Mistake | Rule | Check |
|---|---|---|---|
| 1 | Write an `Err` or `Exit-Install` helper that calls `exit`, or leave with `break`, in an `irm \| iex` installer | PS-SEC-09 | `A1`, `B1` |
| 2 | Extract right after the download, or verify in only one branch, or warn and continue on a blank digest | PS-SEC-01 | `A1`, `G1` |
| 3 | Set `SecurityProtocol` unconditionally, `-bor Tls12`, or name `Tls13` literally | PS-SEC-06 | `G6a`, `G6b` |
| 4 | Document `(iwr URL).Content \| iex` | PS-SEC-07 | `G7` |
| 5 | Give an iex script a `Mandatory` param, `$PSScriptRoot` or a `#Requires` floor and leave `Stop` and StrictMode set | PS-SEC-08 | `G8`, `G8a`, `B2` |
| 6 | Fetch the digest from the same release as the archive and call it verification | PS-SEC-02 | `R1` |
| 7 | Accept any Authenticode `Valid` signature, or match the signer with an unanchored regex | PS-SEC-01 | `B3` |
| 8 | Extract a zip by hand with `Join-Path $dest $entry.FullName` | PS-SEC-04 | `G4` |
| 9 | Say the installer verifies a SHA-256 with no hash call | PS-SEC-03 | `G3` |
| 10 | Let an environment variable choose the download URL without an https test | PS-SEC-14 | `G12` |
| 11 | Check `Get-ExecutionPolicy` before installing | PS-SEC-11 | `G11` |
| 12 | Reach for `Invoke-Expression` or `-Path` on a computed file name | PS-SEC-10, PS-SEC-05 | `A1`, `G5` |
