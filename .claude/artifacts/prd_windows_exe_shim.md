# PRD: Windows native `.exe` launcher shim

**Status:** Draft | **Date:** 2026-05-18 | **Resolves:** #66
**Related:** adr_windows_exe_shim.md, system_design_windows_exe_shim.md,
adr_windows_cmd_argv_injection.md (parent), adr_package_entry_points.md

## Problem

OCX's Windows `.cmd` launchers inherit `cmd.exe` limitations: residual `%*`
argument-injection (BatBadBut class, unclosable in pure batch), `PATHEXT`
must list `.CMD` (friction: auto-inject + warnings across 5 commands),
`argv[0]` is `cmd.exe` (breaks self-introspecting tools), ~10× spawn cost,
shell-dependent Ctrl+C.

## Goals

- Native `.exe` launcher resolvable from any Windows shell with default PATHEXT.
- Eliminate the `cmd.exe` re-parse surface (no shell layer in the call path).
- Faithful `argv[0]`, correct exit-code + Ctrl+C propagation.
- Additive rollout — zero break, no installed-launcher regeneration.
- Path to a valid Authenticode signature without blocking the security fix.

## Non-Goals

- Migrating already-installed `.cmd` launchers.
- Replacing the Unix `.sh` launcher.
- aarch64-windows shim in v1 (follow-on).
- Removing PATHEXT warn/inject machinery in v1 (downgrade deferred until
  `.exe` is proven primary).

## Users

Primary: CI/automation (GitHub Actions, Bazel, devcontainers) passing
machine-generated argument strings — precisely the BatBadBut threat surface.

## Requirements

| # | Requirement | Acceptance |
|---|---|---|
| R1 | `<name>.exe` + `<name>.shim` written on Windows install | files exist after `install --select` |
| R2 | Shim reproduces current wire ABI `ocx launcher exec "<pkg_root>" -- "<stem>" <argv>`, honors `OCX_BINARY_PIN` | argv passthrough test; argv[0]=target not cmd.exe |
| R3 | Resolves from cmd/pwsh/bash with default PATHEXT | resolution test on windows-latest |
| R4 | Exit code propagated; Ctrl+C forwarded; spaces/unicode/`&` in args safe | acceptance matrix |
| R5 | `.cmd` retained; `.exe` wins PATHEXT order | both present, `.exe` invoked |
| R6 | Embedded bytes copied verbatim (signature-preserving model) | SHA guard test; (Phase 2) `signtool verify /pa` |
| R7 | Non-Windows builds unaffected (`SHIM_BYTES=&[]`, emission skipped) | Linux/macOS `task verify` green |

## Success Metrics

BatBadBut `%*` vector closed for `.exe`-resolved launchers; no PATHEXT
configuration needed for default Windows; CI shim build+embed reproducible.

## Risks

Blob/source drift (SHA guard); GHA nested job objects (best-effort kill);
Windows acceptance harness — `registry:2` Docker fixture won't start on
`windows-latest` (registry-independent fixture required); signing ops
onboarding (deferred to Phase 2, not blocking).
