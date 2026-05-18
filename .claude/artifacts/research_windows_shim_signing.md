# Research: Authenticode Signing + Win32 Console (issue #66)

**Date:** 2026-05-18 | **Axis:** Domain | **Expires:** 2027-05-18

## Direct Answer

Sign the shim `.exe` via **SignPath Foundation** (free OSS tier, no geo
restriction, Linux-CI compatible, GitHub-OIDC provenance). Ship Phase-1
unsigned (BatBadBut fix should not block on signing ops); add SignPath in
Phase 2 after a code-signing-policy page exists on ocx.sh. Verbatim copy
preserves the signature. Win32 console shim: console subsystem (default),
`std::process::Command` stdio inherit, no-op `SetConsoleCtrlHandler`, job
object kill-on-close, `dunce::canonicalize()` for pkg_root.

## Signing options

| Option | Cost | Linux CI | Geo | Verdict |
|---|---|---|---|---|
| **SignPath Foundation** | $0 | yes (server-side) | none | **Chosen** — OSS profile matches; needs policy page + MFA + GH-hosted runners |
| Azure Artifact Signing (GA Jan 2026) | $10/mo | no (Windows runner) | individuals USA/CA only — EU blocked | Fallback if org entity in US/CA/EU/UK |
| EV/OV cert + cloud HSM | $400–700/yr | yes (osslsigncode+PKCS#11) | none | Not worth it; no SmartScreen edge post-2024 |
| dlemstra/code-sign-action | — | — | — | Archived Oct 2025 — do NOT use |
| cargo-dist built-in | — | — | — | SSL.com only; SignPath = separate workflow step (#1693) |

Notes: EV no longer instant-SmartScreen since Mar 2024. RFC3161 timestamp
mandatory (else sig dies with short-lived cert). cargo-dist 0.31 native signing
= SSL.com eSigner only.

## Authenticode mechanics

Hash excludes (1) CheckSum, (2) Cert-Table Data-Directory entry, (3) Cert Table.
Byte-for-byte `fs::write` of embedded bytes preserves signature →
`signtool verify /pa` / `osslsigncode verify` pass. "Bytes never modified
per-install" model is correct (sidecar carries per-install state). Smart App
Control (Win11) requires real signature (ad-hoc/self-signed insufficient in
enterprise).

## Win32 console

- `#![windows_subsystem="console"]` (default); inherits parent console Win8+.
- `std::process::Command` + `Stdio::inherit()` — sets `bInheritHandles=TRUE`.
- Exit: `child.wait()` then `exit(status.code())`; never exit before wait.
- Ctrl+C: no-op `SetConsoleCtrlHandler(handler→TRUE)`; child handles its own.
- Job object `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` for orphan cleanup.
- `std::fs::canonicalize` yields `\\?\` verbatim paths `CreateProcessW` rejects
  → use `dunce::canonicalize()`.
- `windows-sys` for size; `dunce` crate for path normalization.

## Recommendation

Phase 1: ship unsigned `.exe` shim (definitive `%*`/BatBadBut fix); release note
+ user-guide note; `.cmd` stays as fallback. Phase 2 (next cycle): SignPath
Foundation — add ocx.sh `/code-signing-policy` page, enforce maintainer MFA,
confirm GH-hosted runners + PE product/version metadata, then ~15-line
`SignPath/github-action-submit-signing-request` step (no Windows runner). Phase 3
fallback: Azure Artifact Signing if OCX is a US/CA/EU/UK legal entity.

## Product positioning

SignPath signature = supply-chain trust signal for OCX's primary audience
(CI/Bazel/devcontainer). "Audited OSS on verified GitHub infra" vs
"unsigned curl-pipe tool". Flag to product-context.md per workflow-swarm.md.

## Sources

Azure Artifact Signing FAQ + action repo + geo Q&A; signpath.org/terms +
docs.signpath.io/github + submit-signing-request action; Hanselman Azure
Trusted Signing; textslashplain Authenticode 2025; Trail of Bits verify-without-
Windows; DigiCert osslsigncode+PKCS#11; mtrojnar/osslsigncode; cargo-dist signing
docs + #1693; CA/B CSC-31; rprichard/win32-console-docs; MS SetConsoleCtrlHandler
/AttachConsole; rust-lang/rust#42869 #101645; rustup#1568; hermetic-launcher;
dunce crate.
