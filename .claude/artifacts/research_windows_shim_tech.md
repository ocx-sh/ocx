# Research: Windows `.exe` Shim Technology (issue #66)

**Date:** 2026-05-18 | **Axis:** Technology | **Expires:** 2027-05-18

## Direct Answer

Build `crates/ocx_shim` as a minimal Rust binary using `std` + `windows-sys`.
Shim reads its own name via `GetModuleFileNameW`, reads `pkg_root` from a
sibling one-line `.shim` sidecar, spawns
`ocx launcher exec "<pkg_root>" -- "<self-stem>" <argv>` (current OCX wire ABI —
NOT Scoop-style direct target passthrough), forwards stdio/exit/Ctrl+C.

## Findings

1. **Scoop `.shim` model** ([ScoopInstaller/Shim] C#, [kiennq/scoop-better-shimexe]
   C++): fixed signed `.exe` + sibling plaintext sidecar (`path=`, `args=`).
   `CreateProcess` + `WaitForSingleObject` + `GetExitCodeProcess`. kiennq variant
   adds job-object kill-on-parent + Ctrl+C handled by child. OCX differs: sidecar
   carries **pkg_root** (the shim re-invokes `ocx launcher exec`), not target path.
2. **Spawn:** `std::process::Command` (inherit stdio) suffices for the common
   case. For race-free tree-kill use direct `CreateProcessW` + `CREATE_SUSPENDED`
   + `AssignProcessToJobObject` + `ResumeThread` (Cargo PR #2370 pattern). Do NOT
   use `CREATE_NEW_PROCESS_GROUP` (breaks child Ctrl+C). GitHub Actions sets
   `JOB_OBJECT_LIMIT_BREAKAWAY_OK` — verify nested-job behaviour on CI.
3. **Ctrl+C:** `SetConsoleCtrlHandler` returning TRUE (no-op) so shim survives
   until `wait()`; child in same console group handles signal itself. Job object
   `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` covers forced-kill orphan cleanup.
4. **Size:** stable Rust + `opt-level="z"`, `lto=true`, `codegen-units=1`,
   `panic="abort"`, `strip="symbols"`, `windows-sys` (no winapi/COM/WinRT) →
   50–80 KB. `no_std` reaches ~20–30 KB but needs custom allocator — NOT worth
   complexity. Prior art: hermetic-launcher (Rust, ~22 KB, CreateProcessW, MIT).
5. **PATHEXT order:** default `.COM;.EXE;.BAT;.CMD` → `.exe` shadows `.cmd`
   automatically when both present. Coexistence is additive, no removal needed.
6. **`windows-sys` over `winapi`** (`winapi` in maintenance mode). mise/proto/Volta
   all moved to native Rust shims — trend confirmed.

## Recommendation

`std` + `windows-sys`; one-line `.shim` sidecar = absolute pkg_root; direct
`CreateProcessW` + suspended-spawn + job object for tree-kill; no-op
`SetConsoleCtrlHandler`; `dunce::canonicalize()` for pkg_root paths. Ship
x86_64 first; aarch64 follow-on.

## Product positioning

Compiled shim removes the last BatBadBut/`%*` CVE-class vector — strengthens
OCX's "backend-automation-first, security-conscious" story vs Scoop/Choco which
still ship unescaped `%*`. (per workflow-swarm.md flag → product-context.md)

## Sources

ScoopInstaller/Shim; kiennq/scoop-better-shimexe; Scoop#4333;
hermeticbuild/hermetic-launcher; iki/mise-shim (archived); moonrepo proto v0.26;
vx-shim/shimexe crates; microsoft windows-rs book (windows vs windows-sys);
johnthagen/min-sized-rust; Cargo PR#2370; meziantou job objects;
rust-lang/rust#101645; di-mgt PATHEXT.
