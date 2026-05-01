# Research: BatBadBut / CVE-2024-24576 — Windows `.cmd` Argument Injection Mitigations

**Date**: 2026-04-27
**Scope**: Mitigation options for `%*` re-parse vulnerability in OCX-generated Windows `.cmd` launchers (round-2 review finding Codex B1)
**Code**: `crates/ocx_lib/src/package_manager/entrypoints.rs:179-188`

## Direct Answer

Current OCX `.cmd` template:

```cmd
@ECHO off
SETLOCAL
ocx exec "file://{pkg_root}" -- "%~n0" %*
```

`%*` forwards caller argv via `cmd.exe` re-parse, BatBadBut class (CVE-2024-24576). Caller `cmake.cmd "& whoami"` → `& whoami` parsed as second command. Publisher-controlled surface (`pkg_root`, entrypoint name) already mitigated by `LauncherSafeString`. Caller-controlled `%*` not mitigated.

**Recommended path**: One-line interim hardening (`SETLOCAL DisableDelayedExpansion`) + ADR documenting residual `%*` risk + threat-model scoping + compiled `.exe` shim as deferred definitive fix.

## Industry Context

- **Trending**: Compiled binary shims (hermetic-launcher Rust 22KB, scoop-better-shimexe C 30KB) bypass cmd.exe entirely.
- **Established**: `.cmd` wrappers with unescaped `%*` — npm cmd-shim v8.0.0 (Oct 2025) still does this.
- **Emerging**: Runtime-level blocking (Node.js CVE-2024-27980 fix: disallow `.cmd` spawn without `shell:true`).
- **Declining**: Complete in-template `%*` escape loops in pure `.cmd` — Rust std team concluded intractable.

## CVE Timeline

| Date | Event |
|---|---|
| 2024-04-09 | RyotaK / Flatt Security disclosure. CVE-2024-24576 (Rust, CVSS 10.0), CVE-2024-27980 (Node.js), CVE-2024-1874 (PHP), CVE-2024-3566 (multi) |
| 2024-04-09 | Rust 1.77.2: improved escaping + `InvalidInput` on unsafe args. Authors: Chris Denton + Simon Sawicki (Grub4K) |
| 2024-08 | CVE-2024-43402 (GHSA-2xg3-7mm6-98jj): 1.77.2 fix bypassed via trailing whitespace/period. Fixed in Rust 1.81.0 |
| 2024-04-10 | Node.js 18.20.2 / 20.12.2 / 21.7.3: disallow `.bat`/`.cmd` direct spawn |

## Technical: cmd.exe Metacharacters

Authoritative source: Microsoft 2011 "Everyone quotes command line arguments the wrong way" (still canonical).

cmd.exe metachars when unescaped outside `"`-quoted region:

```
( ) % ! ^ " < > & |
```

**Two parsing layers, mismatch = vulnerability**:

1. **CommandLineToArgvW** (what `ocx exec` Rust binary sees): `\"` is literal quote; backslash-doubling rule applies.
2. **cmd.exe** (what `%*` expansion is parsed by): `"` is toggle (not `\"`-aware); `\` not escape.

When runtime correctly escapes for layer 1 but not layer 2, args via `%*` are re-parsed with layer 2's rules.

**`%` signs**: `%VAR%` expanded inside batch even within `"..."`. Caller `%PATH%` arg expanded by cmd.exe before reaching `ocx exec`. Already partly mitigated — `LauncherSafeString` blocks `%` in publisher fields, not caller argv.

**Delayed expansion `!`**: only active with `EnableDelayedExpansion`. Current OCX template uses plain `SETLOCAL` — `!` not metachar. **But** registry key can enable globally. Explicit `DisableDelayedExpansion` guards.

## Mitigation Options Analysis

### Option 1 — Escape `%*` in pure `.cmd` template

Pure-batch escape loops over `%*` are intractable for general case (Rust std team's conclusion). Each arg needs:
1. `%` → `%%` (prevent variable expansion)
2. CommandLineToArgvW quoting (backslash + quote rules)
3. `^`-prefix cmd.exe metachars

Pure `.cmd` cannot express this reliably. **Reject.**

### Option 2 — Rust-side static escape

Static parts (`pkg_root`, `name`) already escaped via `LauncherSafeString`. Caller-supplied `%*` cannot be pre-escaped at generation time. **Doesn't address the actual vulnerability.**

### Option 3 — Compiled `.exe` shim (Definitive, Deferred)

Tiny `.exe` reads own filename + sibling `.shim` for `pkg_root`, calls `CreateProcessW` with proper argv assembly, bypasses cmd.exe entirely.

| Shim | Lang | Size | Cross-compile from Linux | License | Status |
|---|---|---|---|---|---|
| 71/scoop-better-shimexe | C | ~30KB | Yes (MinGW) | MIT/Unlicense | Unmaintained (2019) |
| ScoopInstaller/Shim | C# | ~1MB+.NET | No | MIT | Active (v1.1.0 2024-04) |
| **hermetic-launcher** | Rust | ~22KB | Yes (`x86_64-pc-windows-gnu`) | MIT | Active, Bazel-oriented |
| aloneguid/win-shim | C++20 | ? | No (VS2022) | ? | Active |

**hermetic-launcher most relevant Rust prior art.**

OCX compiled shim sketch:
- Minimal Rust binary, reads own filename via `GetModuleFileNameW`
- `pkg_root` from sibling `.shim` text file (Scoop pattern; simpler than binary patching)
- `CreateProcessW` with `ocx exec "file://<pkg_root>" -- "<name>" <argv>` — Rust std `Command` correct for non-batch target
- Forward Ctrl+C/Break via `SetConsoleCtrlHandler`
- Size target <50KB (`opt-level=z`, `lto=true`, `panic=abort`, `strip=symbols`)
- Cross-compile from Linux CI via `x86_64-pc-windows-gnu`

### Option 4 — PowerShell `.ps1` shim (Partial)

PowerShell own arg parser (not cmd.exe), `%*`-style injection N/A. But:
- PS has own injection vectors
- Cmd.exe callers still hit `.cmd` launcher
- `.ps1` blocked by default execution policy on many Windows
- Already deferred in `adr_package_entry_points.md`

**Not a substitute for `.cmd` fix.**

### Option 5 — Documentation only (Inadequate)

Sole reliance on caller-shell-quoting unacceptable for CVE-class issue.

### **Option 6 (Recommended) — Interim hardening + ADR + deferred compiled shim**

1. **Template change** (one line): `SETLOCAL` → `SETLOCAL DisableDelayedExpansion`. Closes `!`-expansion vector against registry-level opt-in.
2. **ADR**: document residual `%*` risk + threat-model scoping (OCX = backend automation, not interactive end-user shell).
3. **Track compiled `.exe` shim** as definitive deferred fix. Reference hermetic-launcher.

## Existing Rust Crates

**No suitable crate** for cmd.exe escape:
- `winarg` (Chris Denton, CVE fix author): Windows arg *parsing*, not escape. Read-side only.
- `windows-args`: same — parsing only.
- `cmd_lib`: shell command building, no Windows batch escape.

**Recommendation**: Vendor inline if escape needed. Function <30 LoC, well-specified from Microsoft ArgvQuote reference. External dep adds maintenance risk for tiny benefit.

## npm cmd-shim Post-CVE Behavior

cmd-shim v8.0.0 (Oct 2025) STILL uses `%*` unescaped:

```cmd
"%_prog%" ${args} ${target} %*
```

Fix went into Node.js runtime, not template. cmd-shim relies on runtime gating. No regressions reported. **Industry norm = don't fix in template.**

## Threat Model

| Surface | Exploitable? | Mitigated? |
|---|---|---|
| `pkg_root` baked in template | No — `LauncherSafeString` blocks `%`, `"`, `\n`, `\r`, `\0`, `'` | Yes |
| Entrypoint `name` | No — `EntrypointName` slug blocks `/`, `\`, `..` | Yes |
| Caller argv via `%*` | **Yes** — cmd.exe metachar re-parse | **No** |
| `ocx exec` internal | No — Rust binary, clean argv via CommandLineToArgvW | N/A |
| `target` field | No — not baked into launcher; resolved by `ocx exec` from metadata | N/A |

**Trusted-args carve-out**: OCX = backend tool for CI/automation. Realistic attacker = CI passing user-supplied strings to wrapper. Scoping reduces but doesn't eliminate risk. Document explicitly in ADR; don't use as sole mitigation.

## Recommendation

**Immediate** (round-2 plan):
1. Template change: `SETLOCAL` → `SETLOCAL DisableDelayedExpansion` in `windows_launcher_body` (entrypoints.rs:185).
2. ADR `adr_windows_cmd_argv_injection.md`: threat model, options analysis, decision (interim hardening + deferred compiled shim).
3. Update `windows_launcher_body_byte_exact_match_adr_form` golden test.
4. CHANGELOG security note (interim mitigation, deferred definitive fix).

**Deferred ADR §"Future work"**: track `ocx-shim.exe` compiled shim. Reference hermetic-launcher. Cross-compile from Linux CI; ship as second Windows release artifact; sibling `.shim` file for pkg_root config; eliminate `.cmd` for Windows or keep as fallback.

**Do not**:
- Attempt complete in-template `%*` escape (Rust std team's intractable conclusion)
- Add `EnableDelayedExpansion`
- Depend on third-party crate
- Use `ShellExecuteW` in any compiled shim (recreates cmd.exe problem)

## Sources

- [Flatt Security BatBadBut disclosure](https://flatt.tech/research/posts/batbadbut-you-cant-securely-execute-commands-on-windows/) — RyotaK 2024-04
- [Rust Blog CVE-2024-24576](https://blog.rust-lang.org/2024/04/09/cve-2024-24576/)
- [GHSA-q455-m56c-85mh](https://github.com/rust-lang/rust/security/advisories/GHSA-q455-m56c-85mh) — Rust primary advisory
- [GHSA-2xg3-7mm6-98jj](https://github.com/rust-lang/rust/security/advisories/GHSA-2xg3-7mm6-98jj) — CVE-2024-43402 follow-up
- [Microsoft "Everyone quotes command line arguments the wrong way"](https://learn.microsoft.com/en-us/archive/blogs/twistylittlepassagesallalike/everyone-quotes-command-line-arguments-the-wrong-way) — authoritative ArgvQuote
- [CERT VU#123335](https://www.kb.cert.org/vuls/id/123335)
- [Node.js CVE-2024-27980 advisory](https://nodejs.org/en/blog/vulnerability/april-2024-security-releases-2)
- [npm/cmd-shim](https://github.com/npm/cmd-shim) — v8.0.0 unescaped `%*`
- [71/scoop-better-shimexe](https://github.com/71/scoop-better-shimexe)
- [ScoopInstaller/Shim](https://github.com/ScoopInstaller/Shim)
- [hermeticbuild/hermetic-launcher](https://github.com/hermeticbuild/hermetic-launcher)
- [aloneguid/win-shim](https://github.com/aloneguid/win-shim)
- [ss64.com cmd.exe escape reference](https://ss64.com/nt/syntax-esc.html)
- [winarg crate](https://github.com/ChrisDenton/winarg)
- [oss-security CVE-2024-24576 thread](https://www.openwall.com/lists/oss-security/2024/04/09/16)
