# ADR: Windows Native `.exe` Shim Replacing the `.cmd` Launcher

## Metadata

**Status:** Accepted (Axis C cutover, supersedes the initial C1 coexist decision)
**Date:** 2026-05-18
**Deciders:** Michael Herwig (issue #66)
**GitHub Issue:** [ocx-sh/ocx#66](https://github.com/ocx-sh/ocx/issues/66) — canonical feature issue. The parent ADR `adr_windows_cmd_argv_injection.md` (§Deferred step 5, §Future Work) cites tracking issue **#72**; #66 is the canonical feature issue this ADR is scoped to. The discrepancy is recorded here deliberately rather than silently resolved — `adr_windows_cmd_argv_injection.md` should be updated in the same change set that lands this work, or #72/#66 reconciled by the maintainer.
**Related ADRs:**
- [`adr_windows_cmd_argv_injection.md`](./adr_windows_cmd_argv_injection.md) — PARENT. §Future Work commits to this `.exe`+`.shim` design. **That ADR's §Future Work is STALE**: (a) the wire form it shows is `ocx exec "file://{pkg_root}"` — the current wire ABI is `ocx launcher exec "<pkg_root>" -- "<stem>" <argv>` (no `file://`, `launcher exec` subcommand pair, reproduced by the native shim via `CreateProcessW`); (b) it describes the `.exe` shim as *additive alongside a retained `.cmd` fallback* — this ADR's Axis C cutover (C2) **supersedes that**: the `.cmd` is removed entirely, so the residual `%*` orphan that parent ADR left open is fully closed, not merely shadowed.
- [`adr_package_entry_points.md`](./adr_package_entry_points.md) — parent launcher design; byte-exact golden tests at `body.rs::tests` are One-Way-Door canaries.
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path (`product-tech-strategy.md`) — Rust 2024, Tokio not involved (shim is sync `std`), no new language. New crate is Rust.
- [x] One deviation justified: prebuilt committed binary blob in-tree (uv/pixi model) rather than a `build.rs`-orchestrated cross-compile. Justified in §Decision Outcome and §Considered Options.
**Domain Tags:** security, package-manager, cli, devops
**Supersedes:** Wire-form description **and** the `.cmd`-retained-fallback rollout posture in §Future Work of `adr_windows_cmd_argv_injection.md` (Axis C cutover removes `.cmd` outright)
**Superseded By:** —

**Reversibility class:** One-Way Door — **High**. New crate, signed-binary embed, CI change, on-disk launcher contract (`.shim` sidecar format is a published-to-disk artifact contract). Reversibility analysis in §Reversibility.

---

## Context

OCX generates per-entrypoint launcher scripts at install time. On Windows the
launcher today is `<name>.cmd`. Its body (current code, `body.rs:81-110`):

```bat
@ECHO off
SETLOCAL DisableDelayedExpansion
IF DEFINED OCX_BINARY_PIN (
  "%OCX_BINARY_PIN%" launcher exec "<pkg_root>" -- "%~n0" %*
) ELSE (
  ocx launcher exec "<pkg_root>" -- "%~n0" %*
)
EXIT /B %ERRORLEVEL%
```

`%*` forwards caller argv through a second `cmd.exe` parse — the BatBadBut /
CVE-2024-24576 class. `adr_windows_cmd_argv_injection.md` applied the interim
`DisableDelayedExpansion` mitigation and explicitly deferred the definitive fix
(a compiled `.exe` shim that bypasses `cmd.exe`) to a follow-on. This ADR is
that follow-on.

**Established facts (from Discover + the three research artifacts — not
re-derived here):**

1. **Current wire ABI** (NOT the stale parent-ADR `file://` form): the launcher
   invokes `ocx launcher exec "<pkg_root>" -- "<name>" %*` on Windows
   (`'<pkg_root>'` single-quoted on Unix), honoring `OCX_BINARY_PIN` (fallback
   PATH `ocx`). The `.exe` shim MUST reproduce this exact invocation. It carries
   `pkg_root` in a sibling `.shim` sidecar and spawns `ocx launcher exec` — it
   does **not** exec the target binary directly (the Scoop model differs; OCX
   keeps the `launcher exec` resolution indirection).
2. **Seam:** `launcher::generate()` loop spawns one write task per entrypoint
   per platform. Windows now emits exactly two files: `<name>.exe` (verbatim
   `SHIM_BYTES`) + `<name>.shim` (one-line absolute `pkg_root`). **No
   `<name>.cmd` is emitted** (Axis C cutover — the `.cmd` write was removed;
   `.EXE` is unconditionally in the default Windows `PATHEXT` so `.cmd` had no
   functional dependency, and dropping it eliminates the residual `%*`
   orphan). `LauncherSafeString` pre-validation at the `generate()` entry
   boundary already covers the sidecar content.
3. **Embed:** committed prebuilt blobs (uv/pixi model) + arch-gated
   `include_bytes!` in a new `crates/ocx_lib/src/shim.rs`; **no `build.rs`**.
   Non-Windows: `SHIM_BYTES = &[]`, emission skipped via `cfg`.
4. **New crate `crates/ocx_shim`:** `std` + `windows-sys`;
   `GetModuleFileNameW` → stem; read `<stem>.shim`; job object
   (`KILL_ON_JOB_CLOSE`) created first; no-op `SetConsoleCtrlHandler`
   installed before the child runs; `CreateProcessW` via `STARTUPINFOEXW`
   with a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` (stdio-only inheritance) +
   `PROC_THREAD_ATTRIBUTE_JOB_LIST` (born-in-job, no `CREATE_SUSPENDED`);
   `dunce::canonicalize`; propagate child exit code. Profile `opt-level="z"`/`lto`/`panic="abort"`/`strip` → ~128–138 KiB
   (budget `SHIM_SIZE_BUDGET` = 256 KiB; "<80 KB" was an initial estimate).
   x86_64 + aarch64 v1.
5. **Signing:** SignPath Foundation (free OSS) chosen; **Phase 1 ships
   UNSIGNED** (the BatBadBut fix must not block on signing ops); Phase 2 adds
   SignPath. Authenticode survives verbatim copy (strip BEFORE sign).
6. **PATHEXT warn/inject machinery removed in-scope.** Under the `.exe`-only
   cutover the entire PATHEXT subsystem is dead (`.EXE` is unconditionally in
   the default Windows PATHEXT — nothing to inject, nothing to warn about).
   The launcher `pathext` module (`emplace_pathext`,
   `pathext_with_launcher`, `includes_launcher`, `LAUNCHER_EXT`),
   `conventions.rs::warn_if_pathext_missing_launcher` + its four call sites
   (`install`/`select`/`shell env`/`ci export`), the `emplace_pathext` calls
   in `exec`/`run`/`launcher exec`/`package_test`, and
   `env.rs::synthetic_pathext_entry` are all deleted in the same change set
   (no longer deferred — KISS/YAGNI).

---

## Decision Drivers

1. **Security baseline (primary).** Eliminate the residual `%*` caller-argv
   re-parse surface that the parent ADR left open. CVSS 10.0-class
   (CVE-2024-24576). OCX's primary audience is CI/Bazel/devcontainer automation
   that interpolates untrusted strings (branch names, issue titles) into
   launcher argv.
2. **Wire-ABI fidelity.** The shim must reproduce the *exact* current
   `launcher exec` invocation including `OCX_BINARY_PIN` honoring. Any drift
   forks behavior between `.cmd` and `.exe` paths.
3. **Additive rollout.** OCX's compat guarantee covers OCI
   manifests/metadata only, **not** on-disk launchers (confirmed in
   `subsystem-package-manager.md` and the patterns research). No installed
   launcher needs regeneration; `.exe` shadows `.cmd` via default `PATHEXT`.
4. **One-Way-Door cost.** The `.shim` sidecar is a written-to-disk format
   contract. Getting its byte/encoding/newline spec right the first time avoids
   a future migration of every installed package.
5. **CI / supply-chain cost.** A new binary artifact must be reproducibly
   built, embedded, and (Phase 2) signed without blocking the security fix on
   signing operations.
6. **Boring-tech budget.** `std` + `windows-sys` (no `no_std`, no custom
   allocator, no winapi/COM). Committed-blob embed (uv/pixi precedent) over
   bespoke `build.rs` cross-compile orchestration.

## Industry Context & Research

**Research artifacts:**
- [`research_windows_shim_tech.md`](./research_windows_shim_tech.md) — runtime tech (windows-sys, CreateProcessW + job object, Ctrl+C, size budget).
- [`research_windows_shim_patterns.md`](./research_windows_shim_patterns.md) — embed + rollout (committed blob, Authenticode survives copy, additive rollout).
- [`research_windows_shim_signing.md`](./research_windows_shim_signing.md) — SignPath Foundation, Authenticode mechanics, Win32 console.

**Trending approach:** mise / proto / Volta all moved from `.cmd`/`.bat` to
native Rust shims with `windows-sys`; uv / pixi commit prebuilt trampolines and
embed via `include_bytes!`. OCX differs from Scoop in one respect: the sidecar
carries `pkg_root` and the shim re-invokes `ocx launcher exec` rather than
exec-ing the target binary directly — preserving OCX's clean-env resolution.

**Key insight:** Authenticode hash excludes the CheckSum, the Cert-Table
Data-Directory entry, and the Cert Table itself, so a verbatim
`include_bytes!` → `fs::write` copy preserves a valid signature. This makes
"sign once in a dedicated PR, embed the committed signed blob" viable and
removes CI ordering complexity. Pitfall: never `strip` after signing.

---

## Considered Options

### Axis A — How the shim binary reaches the running `ocx`

#### A1 — Committed prebuilt blob + arch-gated `include_bytes!` (CHOSEN)

**Description.** Commit `crates/ocx_lib/src/shims/ocx-shim-x86_64.exe` (and
later `-aarch64.exe`) to git. `shim.rs` selects via `cfg(target_arch)` and
`include_bytes!`. Refreshed in a dedicated PR when shim source changes.

| Pros | Cons |
|------|------|
| No CI build-ordering / cross-compile orchestration in the hot path | Binary blob in git history (one per arch, ~128–138 KiB; budget 256 KiB) |
| Authenticode signature embeds with the blob and survives verbatim copy | Blob can drift from `crates/ocx_shim` source if refresh PR skipped — needs a guard test |
| uv / pixi precedent — proven model | Two-step contributor flow (change shim → refresh blob PR) |
| Dev builds reuse the committed signed release blob as a stub | |

#### A2 — `build.rs` cross-compiles `ocx_shim` at `ocx_lib` build time

| Pros | Cons |
|------|------|
| No committed binary; always in sync with source | Requires `x86_64-pc-windows-{gnu,msvc}` toolchain on every build host incl. contributor laptops |
| | Signing cannot happen at `build.rs` time (chicken-egg with CI signer) → would ship unsigned forever or need a post-build re-sign+re-embed loop |
| | `build.rs` that shells a cross-compiler = fragile, slow, breaks `cargo check` ergonomics |

#### A3 — CI downloads the shim as a release artifact at parent-build time

| Pros | Cons |
|------|------|
| No blob in git | Network dependency in the build; breaks offline / hermetic builds |
| | Bootstrapping paradox: the artifact must exist before the build that consumes it |

**Verdict A:** **A1**. A2 spends an innovation token on `build.rs` cross-compile
orchestration and cannot reconcile with the sign-in-CI step. A3 breaks
hermetic builds. A1 matches the proven uv/pixi pattern and makes the signed
blob a reviewable, reproducible artifact.

### Axis B — How the shim learns `pkg_root`

#### B1 — Sibling one-line `.shim` sidecar (CHOSEN)

**Description.** `<name>.shim` next to `<name>.exe`, one line = absolute
`pkg_root`. Shim reads it at runtime, spawns `ocx launcher exec "<pkg_root>" -- "<stem>" <argv>`.

| Pros | Cons |
|------|------|
| `SHIM_BYTES` is byte-identical for every entrypoint/package → signature stable, embed once | Two files per entrypoint instead of one |
| Per-install state lives in the sidecar; binary never mutated per-install (Authenticode-safe) | Sidecar can be deleted/edited independently of the `.exe` (handled by error taxonomy) |
| Mirrors the existing `.cmd` model (pkg_root baked) → no new tamper boundary; `launcher exec` already runs `validate_package_root` against `$OCX_HOME/packages/` | |

#### B2 — `pkg_root` encoded into the `.exe` filename

| Pros | Cons |
|------|------|
| One file | Filename length limits; `pkg_root` has `\`, `:`, spaces — not filename-safe; defeats `%~n0`/stem→entrypoint-name mapping |

#### B3 — `pkg_root` baked into per-install binary copy (PE patching)

| Pros | Cons |
|------|------|
| One file, no sidecar | Per-install binary mutation **invalidates Authenticode** — fatal to the signing story; PE patching is complex and fragile |

**Verdict B:** **B1**. B3 is incompatible with the chosen signing model. B2 is
not representable. B1 keeps `SHIM_BYTES` constant (one signature, one embed)
and reuses the existing pkg_root tamper boundary.

### Axis C — Coexistence vs cutover with `.cmd`

#### C1 — Coexist: emit `.exe` + `.shim` + `.cmd` together (SUPERSEDED — was initially chosen)

| Pros | Cons |
|------|------|
| Default `PATHEXT` (`.COM;.EXE;.BAT;.CMD`) makes `.exe` win automatically — zero migration | Three files per entrypoint on disk |
| Graceful fallback where `.exe` execution is blocked (locked-down AppLocker, etc.) | Orphan `.cmd` retains the residual `%*` risk if a caller invokes it by explicit extension — the *exact vector this feature closes*, left half-open |
| No installed-launcher regen needed | Keeps the entire PATHEXT inject/warn machinery alive solely to keep `.cmd` resolvable — dead weight under `.exe` primacy |

#### C2 — Cutover: emit only `.exe` + `.shim`, stop emitting `.cmd` (CHOSEN — supersedes C1, user decision 2026-05-18)

| Pros | Cons |
|------|------|
| One launcher, **no residual `%*` surface at all** — the orphan `.cmd` vector is fully eliminated, not merely shadowed | Locked-down Windows (AppLocker/WDAC/Smart App Control) blocking unsigned `.exe` has no launcher until Phase-2 SignPath signing |
| `.EXE` is **unconditionally** in the default Windows PATHEXT; cmd.exe / PowerShell resolve `<name>.exe`, Git-Bash resolves `<name>.exe` directly and never consults PATHEXT — so `.cmd` had **no functional dependency** | (pre-1.0) old installed packages keep their previous launchers until re-`install`/`select` regenerates — acceptable mixed estate |
| Makes the entire PATHEXT inject/warn machinery **dead** → removed in-scope (KISS/YAGNI) | |

#### C3 — Flag-gated emission (opt-in `.exe`)

| Pros | Cons |
|------|------|
| Conservative rollout | Needless complexity; security fix should be on by default for a CVE-class issue (YAGNI / parent ADR posture) |

**Verdict C:** **C2** (supersedes the initial C1). The decisive driver is the
primary one — *eliminate the residual `%*` surface*. C1 only shadowed `.cmd`
via PATHEXT order while leaving the vulnerable orphan on disk (resolvable by
explicit extension) and forced a whole PATHEXT inject/warn subsystem to stay
alive purely to keep `.cmd` discoverable. Since `.EXE` is unconditionally in
the default Windows PATHEXT and Git-Bash resolves `<name>.exe` directly, `.cmd`
carried **zero functional load** — removing it closes the vector completely and
deletes the now-dead PATHEXT machinery. The accepted negative (no launcher in
unsigned-`.exe`-blocked locked-down environments until Phase-2 signing) is
narrow, temporary, and outside OCX's CI/automation primary audience. Pre-1.0,
on-disk launchers carry no compat guarantee, so the clean break is sanctioned.

### Axis D — Signing provider / timing

#### D1 — Phase 1 unsigned, Phase 2 SignPath Foundation (CHOSEN)

| Pros | Cons |
|------|------|
| Ships the definitive BatBadBut fix immediately; signing ops do not gate the security fix | Unsigned window: SmartScreen friction for new shim (mitigated: OCX is backend-automation, not interactive download) |
| SignPath Foundation = $0, OSS-eligible, Linux-CI compatible, GitHub-OIDC provenance | Phase 2 prerequisites (policy page, MFA) are real ops work |

#### D2 — Azure Trusted Signing from day one

| Pros | Cons |
|------|------|
| GA, low cost | Requires Windows runner; geo-restricted (EU individuals blocked); legal-entity gating |

#### D3 — Never sign

| Pros | Cons |
|------|------|
| Zero ops | Smart App Control (Win11) / enterprise allow-lists reject unsigned; weakens supply-chain story for the primary audience |

**Verdict D:** **D1**. Decouples the security fix from signing ops; SignPath
Foundation matches OCX's OSS profile. D2 kept as documented fallback if OCX
becomes a US/CA/EU/UK legal entity.

---

## Decision Outcome

**Chosen:** A1 (committed signed blob + arch-gated `include_bytes!`, no
`build.rs`) + B1 (sibling one-line `.shim` sidecar) + **C2 (cutover: emit
`.exe`+`.shim` only, NO `.cmd`)** + D1 (Phase 1 unsigned, Phase 2 SignPath
Foundation).

**Rationale.** This combination ships the definitive BatBadBut fix without
gating it on cross-compile orchestration (rejected A2/A3) or signing ops
(rejected D2/D3), keeps the embedded binary byte-stable so a single signature
embeds verbatim (B1 over B3), and **fully eliminates** the residual `%*`
caller-argv re-parse surface by not emitting `.cmd` at all (C2 over C1/C3 —
C1 only shadowed `.cmd` via PATHEXT order, leaving the vulnerable orphan on
disk). Because `.EXE` is unconditionally in the default Windows PATHEXT and
Git-Bash resolves `<name>.exe` directly, `.cmd` had no functional dependency;
dropping it also makes the PATHEXT inject/warn machinery dead code, removed in
the same change set (KISS/YAGNI). It spends at most one innovation token
(committed-blob embed, already proven by uv/pixi) and stays on the Rust /
`std` / `windows-sys` golden path.

### Weighted trade-off summary

| Axis | Chosen | Decisive driver | Rejected because |
|------|--------|-----------------|------------------|
| A (embed) | Committed blob + `include_bytes!` | CI simplicity + signature survives copy | `build.rs` can't reconcile with CI signer (A2); A3 breaks hermetic build |
| B (pkg_root) | `.shim` sidecar | Constant `SHIM_BYTES` → one signature | PE-patch (B3) invalidates Authenticode |
| C (cutover) | `.exe`+`.shim` only, NO `.cmd` | Fully eliminates residual `%*`; `.EXE` always in default PATHEXT so `.cmd` had no functional load; kills dead PATHEXT machinery | Coexist (C1) leaves the vulnerable `.cmd` orphan on disk + keeps PATHEXT inject/warn alive; flag-gate (C3) = needless complexity for a CVE fix |
| D (signing) | Unsigned v1 → SignPath v2 | Don't block CVE fix on ops | Azure (D2) geo-gated; never-sign (D3) fails SAC/enterprise |

### Quantified Impact

| Metric | Before (`.cmd` only) | After (`.exe`-only cutover) | Notes |
|--------|----------------------|------------------------|-------|
| `%*` caller-argv re-parse surface | Open (residual per parent ADR) | **Fully closed** — no `.cmd` exists at all | The orphan vector is gone, not merely shadowed; this is the security win |
| Files per entrypoint (Windows) | 1 (`.cmd`) | 2 (`.exe`,`.shim`) | + Unix `<name>` unchanged; non-Windows carries zero shim weight |
| PATHEXT inject/warn machinery | present (`emplace_pathext`, `pathext_with_launcher`, `includes_launcher`, `LAUNCHER_EXT`, `warn_if_pathext_missing_launcher`, `synthetic_pathext_entry`) | **removed** | Dead under `.exe`-only — `.EXE` always in default PATHEXT, nothing to inject |
| Shim binary size | n/a | ~138 KiB (x86_64) / ~128 KiB (aarch64); budget **256 KiB** (binding ceiling) | 80 KB was an initial estimate; the `windows-sys` Win32 surface + `dunce` lands the stripped `shim`-profile blob at ~128–138 KiB. `SHIM_SIZE_BUDGET` = 256 KiB is the fail-closed binding budget (≪ the 10 MB whole-`ocx` ceiling). Unchanged by the cutover (shim binary is unaffected) |
| Installed-launcher regen required | n/a | None forced (pre-1.0; re-`install`/`select` regenerates) | Clean break, no migration code; mixed estate acceptable |
| Network in build | none | none | Committed blob; no `build.rs` fetch |

### Consequences

**Positive:**
- **Fully eliminates** the BatBadBut/`%*` CVE-class vector — no `.cmd`
  exists, so there is no orphan to invoke by explicit extension. This is the
  primary security win and the entire point of the cutover.
- Correct Ctrl+C / exit-code / tree-kill behavior (job object) — better than `.cmd`.
- Deletes the now-dead PATHEXT inject/warn subsystem (KISS/YAGNI): smaller
  surface, fewer misleading warnings, one launcher concept on Windows.
- Strengthens OCX's "backend-automation-first, security-conscious" positioning vs Scoop/Choco (still ship unescaped `%*`). **Product-positioning flag raised** — see §Product Positioning.

**Negative:**
- One extra file per entrypoint on disk vs the old `.cmd`-only model
  (`.exe` + `.shim`); still fewer than the rejected C1 three-file model.
- Committed binary blob(s) in git (~128–138 KiB each; budget 256 KiB); requires a blob↔source guard test.
- Unsigned Phase 1 window (SmartScreen friction; low impact for backend audience).
- **Locked-down Windows (AppLocker / WDAC / Smart App Control) that blocks
  unsigned `.exe` execution has no launcher at all** until Phase-2 SignPath
  signing — there is no `.cmd` fallback. Narrow, temporary, and outside OCX's
  CI/automation primary audience; closed by Phase-2 signing.

**Risks:**
- **Blob/source drift** — committed `.exe` falls behind `crates/ocx_shim`. Mitigation: a deterministic-build guard test that fails if the blob does not match a reproducible build of the source (or, minimally, a recorded SHA-256 + manual refresh checklist in the contributor flow).
- **GitHub Actions nested job objects** — the runner sets `JOB_OBJECT_LIMIT_BREAKAWAY_OK`; nested-job behavior must be verified on CI (research_tech finding 2). Mitigation: acceptance test asserts child exit-code propagation on `windows-latest`.
- **`include_bytes!` path is relative to the source file** — must be `crates/ocx_lib/src/shims/…`, not crate root. Mitigation: unit test asserts `SHIM_BYTES.len() > 0` on Windows targets.
- **`std::fs::canonicalize` returns `\\?\` verbatim paths `CreateProcessW` rejects** — mitigation: `dunce::canonicalize` (mandated in component contract).

---

## Error Taxonomy

The shim is a separate binary; it does not use OCX's `Error` enum. It maps each
failure to a process exit code aligned with `quality-rust-exit_codes.md`
(sysexits.h base 64; tool-specific range 79+). The shim never panics in a
released build (`panic = "abort"`; all fallible paths return an exit code).
Diagnostics go to **stderr**, one lowercase line, no trailing period
(`quality-rust-errors.md` `C-GOOD-ERR` style), prefixed `ocx-shim:`.

| # | Condition | Detection | stderr message | Exit code | Rationale |
|---|-----------|-----------|----------------|-----------|-----------|
| E1 | `.shim` sidecar missing | `OpenFileW`/read on `<stem>.shim` fails `ERROR_FILE_NOT_FOUND` | `ocx-shim: sidecar not found: <path> (re-run \`ocx install\` to regenerate the entrypoint)` | **78** (`ConfigError`/`EX_CONFIG`) | Missing per-install config artifact; sidecar is the shim's config. Carries the recovery hint because re-running `ocx install` regenerates the `.exe`+`.shim` pair |
| E2 | `.shim` malformed (empty, > limit, embedded NUL/CR/LF, not UTF-8, not absolute) | Post-read validation | `ocx-shim: malformed sidecar: <reason> (re-run \`ocx install\` to regenerate the entrypoint)` | **78** (`ConfigError`) | Same class as E1 — config artifact unusable; same recovery hint |
| E3 | `pkg_root` resolves outside `$OCX_HOME/packages/` (defense-in-depth; primary check stays in `ocx launcher exec`) | Optional `dunce::canonicalize` + prefix check **only when `OCX_HOME` is readable**; otherwise delegate to `launcher exec` | `ocx-shim: package root outside OCX home: <path>` | **77** (`PermissionDenied`/`EX_NOPERM`) | Tamper / containment violation; least-surprise to refuse |
| E4 | `GetModuleFileNameW` fails or yields no stem | Win32 return value 0 / empty stem | `ocx-shim: cannot determine own path` | **74** (`IoError`/`EX_IOERR`) | OS-level failure obtaining own identity |
| E5 | `ocx` not found | `CreateProcessW` fails `ERROR_FILE_NOT_FOUND`/`ERROR_PATH_NOT_FOUND` | **Two messages, by pin state:** `OCX_BINARY_PIN` unset (literal-`ocx` PATH miss) → `ocx-shim: ocx not found (set OCX_BINARY_PIN or add ocx to PATH)`; `OCX_BINARY_PIN` *defined* but the pinned path is absent → `ocx-shim: pinned ocx not found: {p} (OCX_BINARY_PIN points at a missing path)` | **69** (`Unavailable`/`EX_UNAVAILABLE`) | Required dependency unavailable. The defined-but-missing-pin message names the missing path because the generic "add ocx to PATH" hint would be misleading (PATH was not the resolution route) |
| E6 | `CreateProcessW` fails for any other reason (access denied, etc.) | `CreateProcessW` returns 0, `GetLastError` ∉ {file-not-found, path-not-found} | `ocx-shim: failed to start {program}: win32 error {win32}` | **74** (`IoError`) unless `GetLastError == ERROR_ACCESS_DENIED` → **77** | Spawn failure; permission subcase distinguished. `{program}` is the resolved program (pinned value or literal `ocx`), not a hard-coded `ocx`; `{win32}` is the raw `GetLastError` code. The 77-vs-74 split is derived purely from the carried Win32 code in `exit_code()` — there is one `SpawnFailure` value, not two |
| E7 | Job-object setup fails (`CreateJobObjectW`/`AssignProcessToJobObject`) | Win32 return value | `ocx-shim: job object setup failed: <win32 error>` | proceed **without** job object, log to stderr, do **not** fail | Tree-kill is best-effort hardening, not correctness-critical; failing here would regress vs `.cmd` |
| E8 | Child (`ocx launcher exec`) ran and exited | `GetExitCodeProcess` | (none — transparent) | child's exit code, full i32 passthrough (Windows semantics, mirrors `child_process::exit_code_from_status` non-unix branch) | The shim is transparent; it must not remap the child's code |

**Notes:**
- E3 is **defense-in-depth only**. The authoritative containment check is
  `validate_package_root` inside `ocx launcher exec` (already validates
  `pkg_root` inside `$OCX_HOME/packages/`). The shim performs the check **only
  when it can read `OCX_HOME` cheaply**; otherwise it delegates to
  `launcher exec`. This avoids duplicating policy and avoids a hard failure when
  the shim cannot resolve `OCX_HOME` itself.
- E5/E6: the shim resolves the program exactly as the `.cmd` body does —
  if `OCX_BINARY_PIN` is **defined at all** (present in the environment, even
  as an empty string) the shim takes the pin branch and uses its value;
  **only when `OCX_BINARY_PIN` is unset** does it fall back to the literal
  `ocx` (PATH lookup by `CreateProcessW`). This mirrors the Windows `.cmd`
  `IF DEFINED OCX_BINARY_PIN` semantics exactly — the shim replaces the
  `.cmd`, so Windows-parity governs. (The Unix `.sh` launcher's
  `${OCX_BINARY_PIN:-ocx}` treats empty as unset; that divergence is
  Unix-only and out of scope here.) It must NOT pre-canonicalize the program
  path beyond what the `.cmd` form does, to preserve wire-ABI parity.
- Exit codes 64/65 are intentionally **not** used by the shim: argument parsing
  is `ocx`'s job, not the shim's (the shim forwards argv verbatim).

---

## Component Contracts

Three contracts. All are testable (concrete signatures + pre/postconditions +
edge cases). The shim runtime contract is testable at acceptance level
(Windows runner); the two `ocx_lib` contracts at unit level.

### Contract 1 — Shim runtime (`crates/ocx_shim`, `fn main`)

**Signature (conceptual):** `fn main() -> ! ` (always diverges via
`ExitProcess`/`process::exit`; `panic = "abort"`).

**Preconditions:**
- Invoked as `<dir>/<stem>.exe` with arbitrary trailing argv (may be empty).
- `<dir>/<stem>.shim` *should* exist (absence → E1, well-defined).

**Behavior (ordered):**
1. `GetModuleFileNameW(NULL, …)` → own full path. Derive `stem` = file name
   without final `.exe` (case-insensitive). Failure → **E4**.
2. Compute sidecar path = `<dir>/<stem>.shim`. Read fully. Absence → **E1**.
3. Validate sidecar (Contract: see §`.shim` format below). Invalid → **E2**.
   `pkg_root` = the validated single line, trailing newline stripped.
4. Optionally (only if `OCX_HOME` readable) `dunce::canonicalize(pkg_root)` and
   assert it is under `<OCX_HOME>/packages/`. Violation → **E3**. If `OCX_HOME`
   not resolvable, skip (delegate to `launcher exec`).
5. Resolve program: if env `OCX_BINARY_PIN` is **defined** (present, even
   empty) → that value; **only if unset** → literal `"ocx"`. This mirrors the
   Windows `.cmd` `IF DEFINED OCX_BINARY_PIN` semantics exactly (the shim
   replaces the `.cmd`, so Windows-parity governs; the Unix `${VAR:-ocx}`
   empty-as-unset behaviour differs and is out of scope).
6. Build child command line reproducing the wire ABI **byte-for-byte**:
   `<program> launcher exec "<pkg_root>" -- "<stem>" <forwarded argv...>`
   where `<forwarded argv...>` is the shim's own `argv[1..]` passed through
   `CommandLineToArgvW`-compatible quoting (Rust `std::process::Command`
   argument quoting is the reference; the shim uses the same rules). The shim
   MUST NOT route through `cmd.exe`.
7. Install no-op `SetConsoleCtrlHandler` (handler returns `TRUE`) **before
   the child can run** so the shim survives Ctrl+C until the child exits; the
   child handles its own signal. Do **not** use `CREATE_NEW_PROCESS_GROUP`.
   Registration failure is a logged degraded mode (E7-class), not silent.
   *(Codex finding 4: the child is born running — step 10 drops
   `CREATE_SUSPENDED` — so the handler must be in place before
   `CreateProcessW`, eliminating the Ctrl+C race window.)*
8. `CreateJobObjectW` + set `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` **first**
   (before `CreateProcessW`). Any failure → **E7** (log, continue without job
   object — plain spawn).
9. Build a `STARTUPINFOEXW` attribute list:
   - `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` = the **de-duplicated** set of the
     **valid** standard handles (a console process commonly has stdin == stdout)
     so the child inherits **only** stdio, not every inheritable handle
     *(Codex finding 1, CWE-403)*. `STARTF_USESTDHANDLES` wires the same three
     as the child's std streams, **gated on all three std handles being valid**
     (`core::use_std_handles`): a no-console parent (detached / GUI-subsystem /
     service) yields one or more `NULL`/`INVALID_HANDLE_VALUE` std handles, in
     which case neither `STARTF_USESTDHANDLES` nor the `hStd*` trio is set and
     the OS supplies the child default streams (third degraded mode in
     §Postconditions).
   - `PROC_THREAD_ATTRIBUTE_JOB_LIST` = the job from step 8, so the child is
     born inside the job atomically (no `CREATE_SUSPENDED`→`Assign`→`Resume`
     race window). If the attribute list / either attribute fails, degrade to
     a plain spawn (explicit std handles only, no job) — logged, E7-class.
10. `CreateProcessW` with `bInheritHandles = TRUE` (required for the
    whitelisted HANDLE_LIST set) and `EXTENDED_STARTUPINFO_PRESENT`, **without
    `CREATE_SUSPENDED`** and without a separate
    `AssignProcessToJobObject`/`ResumeThread`. Failure → **E5**
    (file-not-found) or **E6** (other; ACCESS_DENIED → exit 77).
    `DeleteProcThreadAttributeList` is called on **every** exit path.
11. `WaitForSingleObject(child, INFINITE)`; `GetExitCodeProcess(child)`.
12. `ExitProcess(child_exit_code)` — full i32 passthrough (**E8**), matching
    the Windows branch of `child_process::exit_code_from_status`.

**Postconditions:**
- stdin/stdout/stderr inherited (the child writes directly to the real
  console). `bInheritHandles = TRUE` with a `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`
  whitelist of **only** the valid standard handles — no other inheritable
  handle in the shim crosses into the child (CWE-403 closed; the previous
  unconditional blanket inheritance is gone). Three documented degraded modes,
  all strictly smaller surface than the old blanket inherit:
  1. **Attribute list cannot be built** → `STARTF_USESTDHANDLES` with the
     explicit std trio only (no HANDLE_LIST whitelist, no job-at-create).
  2. **HANDLE_LIST attribute fails but the list itself was built** →
     `STARTF_USESTDHANDLES` explicit trio only, no whitelist.
  3. **No valid std handles** (no-console parent: detached / GUI-subsystem /
     service — `GetStdHandle` → `NULL`/`INVALID_HANDLE_VALUE`): the shim sets
     **neither** `STARTF_USESTDHANDLES` **nor** the `hStd*` trio and lets the
     OS supply the child's default streams. The child **still launches** —
     otherwise this would be a no-console regression versus the removed `.cmd`
     launcher. E7-class best-effort; the HANDLE_LIST is built from the unique
     *valid* std handles, so a no-console parent simply yields an empty list.
- Process exit code equals the child's exit code on the success path; equals
  the mapped error code (§Error Taxonomy) on any failure before spawn.
- No orphaned child process if the shim is force-killed (job object
  `KILL_ON_JOB_CLOSE`).
- The embedded bytes are never modified at runtime (Authenticode intact).

**Edge cases (each a test):**
- Empty argv → child line is `<prog> launcher exec "<pkg_root>" -- "<stem>"`.
- argv with spaces / Unicode / `&` / `"` / `%` → forwarded verbatim to the
  child via proper Win32 quoting; **no** `cmd.exe` re-parse (the whole point).
- stem contains `.` (e.g. `clang-format`) → only the final `.exe` is stripped.
- Sidecar with trailing `\r\n` vs `\n` → both accepted; sidecar with embedded
  newline before EOF → **E2**.
- Child killed by Ctrl+C → child's exit code propagated; shim does not
  short-circuit.

### Contract 2 — `launcher::generate()` extension (`generate.rs`)

**Existing signature (unchanged):**
`pub async fn generate(pkg_root: &Path, entries: &Entrypoints, dest: &Path) -> Result<(), crate::Error>`

**Change:** inside the per-entry loop, the Windows `<name>.cmd` write is
**removed**. The Unix `<name>` write is unchanged; on Windows targets emit two
files per entry (and **no `.cmd`**):
- `<dest>/<name>.exe` ← `crate::shim::SHIM_BYTES` (verbatim, no transform).
- `<dest>/<name>.shim` ← sidecar body (see §`.shim` format), derived from the
  already-validated `LauncherSafeString` `pkg_root` (so the unsafe-char check
  at the `generate()` entry boundary already covers it — no second validation).

**Preconditions:**
- `pkg_root` already wrapped in `LauncherSafeString` (existing line 48).
- `crate::shim::SHIM_BYTES` non-empty on Windows targets (Contract 3).

**Postconditions:**
- For each entry on Windows: `<name>.exe` + `<name>.shim` present in `dest`,
  and **`<name>.cmd` absent** (it is never written — Axis C cutover).
- On non-Windows: only `<name>` is written (`SHIM_BYTES` empty, `.exe`/`.shim`
  emission skipped via `cfg(target_os = "windows")`, `.cmd` removed on all
  platforms). Tests assert `<name>` present and `.cmd`/`.exe`/`.shim` absent
  off Windows.
- Write ordering: `.exe` written **before** `.shim` (a present `.shim` without
  its `.exe` is the worse partial state; ordering makes the
  shim-present-sidecar-absent window the recoverable one — E1).
- Idempotent re-run overwrites the two Windows files (matches existing
  `generate_is_idempotent` contract).
- First write error aborts the JoinSet and surfaces the first `crate::Error`
  (unchanged failure contract).

**Edge cases (each a test):**
- Empty `Entrypoints` → no files, `dest` not created (unchanged;
  `generate_empty_entrypoints_creates_nothing`).
- `pkg_root` with `LAUNCHER_UNSAFE_CHARS` → rejected at the `generate()` entry
  boundary before any write, including `.exe`/`.shim`
  (`generate_rejects_pkg_root_with_unsafe_character` also asserts no
  `.exe`/`.shim`/`.cmd`).
- Per-entry: 2 spawned write tasks on Windows (Unix `<name>` + the sequenced
  `.exe`-then-`.shim` task), 1 (Unix `<name>`) elsewhere — JoinSet still
  drains deterministically (first error wins; no ordering dependency between
  tasks).

### Contract 3 — Embed module (`crates/ocx_lib/src/shim.rs`)

**Surface:**
```rust
/// Verbatim bytes of the prebuilt `ocx_shim` executable for the target arch.
/// Empty on non-Windows targets (no shim is emitted there).
pub const SHIM_BYTES: &[u8];

/// Recorded SHA-256 of the committed blob (lowercase hex), for the
/// blob↔source drift guard test. Empty string on non-Windows.
pub const SHIM_SHA256: &str;
```

**Behavior:**
- `#[cfg(all(target_os = "windows", target_arch = "x86_64"))]` →
  `include_bytes!("shims/ocx-shim-x86_64.exe")`.
- `#[cfg(all(target_os = "windows", target_arch = "aarch64"))]` → aarch64 blob
  (follow-on; until then a documented compile-time `compile_error!` or the
  x86_64 blob with a tracked TODO — decided at implementation, not here).
- `#[cfg(not(target_os = "windows"))]` → `SHIM_BYTES: &[u8] = &[]`,
  `SHIM_SHA256 = ""`.
- No `build.rs`. No network. Path is **relative to `shim.rs`**
  (`crates/ocx_lib/src/shims/…`).

**Postconditions / invariants (each a test):**
- On Windows x86_64 build: `SHIM_BYTES.len() > 0` and
  `sha256(SHIM_BYTES) == SHIM_SHA256`.
- On non-Windows build: `SHIM_BYTES.is_empty()`.
- `SHIM_BYTES` is `'static` and never mutated (Authenticode integrity).

**Module registration:** add `pub mod shim;` to `crates/ocx_lib/src/lib.rs`
(crate-root cross-cutting module, peer of `hardlink`, `symlink`,
`child_process` per `arch-principles.md` "Cross-Cutting Modules").

---

## `.shim` Sidecar Format Contract

| Property | Value |
|----------|-------|
| Location | `<dest>/<name>.shim`, sibling of `<name>.exe` |
| Encoding | UTF-8, **no BOM** |
| Content | Exactly one line: the absolute `pkg_root` (the same string the `.sh` launcher body single-quotes as the `launcher exec '<pkg_root>'` literal — the `.cmd` body that previously baked the same string was removed in the Axis C cutover) |
| Terminator | A single trailing `\n` (LF). The shim accepts a trailing `\r\n` or no terminator (tolerant read), but the generator **writes** exactly `<pkg_root>\n` |
| Forbidden in body | `'`, `"`, `\n`, `\r`, `\0` (and, implied by the single-line terminator rule, no interior `\n`/`\r` before the terminator) — already guaranteed absent by `LauncherSafeString` (`LAUNCHER_UNSAFE_CHARS`); the sidecar reuses that guarantee, no second validator. `%` was removed from this set at review-fix R2 (see amendment below) |
| Max length | Shim reads up to 32 KiB; longer → E2 (defends against a corrupt/huge file) |
| Not | a digest, not the target binary path, not config flags (those are env-var territory). Carrying anything but `pkg_root` would lose the `ocx launcher exec` resolution indirection |
| Security boundary | The sidecar is **not** itself a trust boundary. `ocx launcher exec` runs `validate_package_root` against `$OCX_HOME/packages/` — identical to today's `.cmd` |

This is the **One-Way-Door artifact**. Once installed packages carry `.shim`
files, changing the encoding/terminator/semantics requires regenerating every
installed launcher. The format is deliberately minimal (one line, one meaning)
to make it durable.

**Amendment (review-fix R2, 2026-05-18) — `%` removed from the forbidden-byte
set.** `%` is no longer in `LAUNCHER_UNSAFE_CHARS`
(`safety.rs`: `&['\'', '"', '\n', '\r', '\0']`). Rationale: post Axis-C cutover
no consumer of the baked string treats `%` specially — the `.sh` launcher body
**single-quotes** the `pkg_root` literal (no `%VAR%` or parameter expansion
inside a single-quoted POSIX literal), the one-line `.shim` sidecar is read
**verbatim** by the native shim, and the shim spawns through `CreateProcessW`
with **no `cmd.exe`** in the chain (so no `%VAR%` environment-variable
substitution ever occurs). The only producer that treated `%` specially was the
`.cmd` body, which the cutover removed. A real Windows install path can legally
contain `%` (e.g. a user folder named `100%real`); forbidding it would reject
valid roots for no security benefit. **This change is a backward-compatible
*widening* of the admissible set, not a narrowing.** Every `.shim` file written
under the old (`%`-forbidden) grammar is still a member of the new grammar — old
on-disk sidecars are a strict subset of what the new validator accepts — so
**no installed-launcher migration is required** and the One-Way-Door narrowing
direction (which *would* invalidate installed sidecars) is explicitly not taken.
The durable invariants the shim relies on (single line, absolute path, no
embedded `\n`/`\r`/`\0`, UTF-8, no BOM) are unchanged.

---

## UX Scenarios

Each scenario lists the happy path and its error case.

| # | Scenario | Happy path | Error case |
|---|----------|-----------|------------|
| U1 | Resolve from `cmd.exe` with default `PATHEXT` | `cmake --version` → resolver finds `cmake.exe` (`.EXE` before `.CMD`) → shim → `ocx launcher exec` → tool runs | `cmake.shim` deleted → shim E1, stderr `ocx-shim: sidecar not found`, exit 78 |
| U2 | Resolve from PowerShell (`pwsh`) | Same — PowerShell honors `PATHEXT`, picks `.exe` | Malformed `cmake.shim` → E2, exit 78 |
| U3 | Resolve from Git-Bash / MSYS bash on Windows | bash PATH search finds `cmake.exe` (bash does not use `PATHEXT`; explicit `.exe` resolves) | `ocx` not found, no `OCX_BINARY_PIN` → E5, exit 69 |
| U4 | argv with spaces: `cmake "-DCMAKE_INSTALL_PREFIX=C:\Program Files\x"` | Forwarded verbatim through Win32 quoting; `cmake` receives one argument with spaces; **no** `cmd.exe` split | If routed through `.cmd` by explicit extension instead, residual `%*` risk (documented) |
| U5 | argv with Unicode: `tool --name café` | UTF-16 argv → forwarded verbatim | n/a (no failure introduced) |
| U6 | argv with shell metachars: `tool "& whoami"` | `& whoami` passed as a literal argument; **not** executed (the BatBadBut fix) | No `.cmd` exists post-cutover — there is no orphan path to be vulnerable through |
| U7 | Exit-code propagation: tool exits 42 | Shim `ExitProcess(42)`; `%ERRORLEVEL%` / `$LASTEXITCODE` == 42 | Tool crashes with `STATUS_ACCESS_VIOLATION` (0xC0000005) → full i32 passthrough preserved (matches `exit_code_from_status` non-unix) |
| U8 | Ctrl+C while tool runs | No-op `SetConsoleCtrlHandler`; child receives Ctrl+C, cleans up, exits; shim propagates child's exit code | If child is force-killed externally, job object `KILL_ON_JOB_CLOSE` reaps the tree; shim still returns child's code |
| U9 | `OCX_BINARY_PIN` set by outer ocx | Shim spawns `%OCX_BINARY_PIN% launcher exec …` (pinned binary) — parity with `.cmd` `IF DEFINED` branch | `OCX_BINARY_PIN` points to a non-existent path → E5, exit 69, stderr `ocx-shim: pinned ocx not found: {p} (OCX_BINARY_PIN points at a missing path)` (defined-but-missing-pin sub-case) |
| U10 | Missing sidecar but `.exe` present (partial install / manual delete) | n/a | E1, exit 78, actionable stderr; recoverable by `ocx install`/`select` re-running `generate()` |

---

## Reversibility

| Element | Reversible? | Cost to undo |
|---------|-------------|--------------|
| New crate `crates/ocx_shim` | Yes | Delete crate; remove from workspace |
| `crates/ocx_lib/src/shim.rs` + committed blob | Yes | Delete module + blob; revert `lib.rs` |
| `generate()` emitting `.exe`/`.shim` (only) | Yes (code) | Stop emitting; **but** installed packages already on disk carry the files until re-`install`/`select` or `clean` |
| `.shim` on-disk format | **No (One-Way Door)** | Once shipped, every installed package carries `.shim`; format change = migrate-all or accept mixed estate |
| Dropping `.cmd` emission (C2 cutover) | Yes (code), but no fallback while reverted-out | Re-introducing `.cmd` is a pure code change; pre-1.0 there is no on-disk-launcher compat obligation. No `.cmd` safety net exists once cut over — the shim itself is the launcher |
| Removed PATHEXT inject/warn machinery | Yes (code) | Restoring it is a code revert; it is behavior-neutral dead code under `.exe`-only and not needed unless `.cmd` returns |
| Signing (Phase 2) | Yes | Drop the SignPath workflow step; shim still functions unsigned |

**Net:** the binary/crate/embed/cutover mechanics are all reversible as code
(pre-1.0, on-disk launchers carry no compat guarantee). The single
irreversible commitment is the **`.shim` sidecar format** — which is why this
ADR over-specifies it (encoding, BOM, terminator, length cap, forbidden
bytes). Note the cutover removes the `.cmd` safety net: a rollback of the
`.exe` shim has no launcher fallback, so the shim must be correct (mitigated
by the host-runnable unit suite + Windows acceptance suite + corruption
canary).

---

## Migration / Rollout Plan

- **Clean break, pre-1.0, no installed-launcher migration code.** Going
  forward Windows emits `<name>.exe` + `<name>.shim` only — never `.cmd`.
  `.EXE` is unconditionally in the default Windows `PATHEXT`, so new installs
  resolve the shim by bare name with no PATHEXT configuration ever needed.
- **Existing installs:** any pre-cutover `.cmd` (or earlier `.exe`+`.cmd`)
  launchers on disk are simply left as-is until the next `ocx install` /
  `ocx select` / `ocx clean` / re-install re-runs `generate()` (now emitting
  the two-file shim set). No forced migration and no migration code — pre-1.0,
  on-disk launchers carry no compat guarantee, so a re-install regenerates.
  A stale `.cmd` on disk is harmless (it is just never the resolution target
  for a re-installed package).
- **No metadata / OCI manifest change.** Compat guarantee untouched (it covers
  manifests/metadata, not launchers).
- **Phase 1 (this work):** unsigned shim, x86_64 + aarch64. Release note +
  user-guide note (documentation surface list below).
- **Phase 2 (next cycle):** SignPath Foundation signing (see §Out of
  Implementation Scope). Phase 2 does **not** require regenerating Phase 1
  launchers; signed `.exe` replaces unsigned on the next `generate()`.

### Documentation surfaces (must update when this lands)

- `website/src/docs/user-guide.md` — Windows launcher section: `.exe`+`.shim`
  is the *sole* launcher, no `.cmd`, no PATHEXT configuration ever needed
  (`.EXE` is unconditionally in the default Windows PATHEXT).
- `website/src/docs/reference/command-line.md` — DELETE the PATHEXT-warning
  callouts on `install` / `select` / `shell env` / `ci export` / `env`
  entirely (the warnings + synthetic PATHEXT entry are removed).
- `website/src/docs/reference/environment.md` — drop PATHEXT-related notes;
  `OCX_BINARY_PIN` is honored by the shim with Windows `IF DEFINED` semantics.
- `CHANGELOG.md` — `### Security` entry (definitive BatBadBut fix, `.exe`-only)
  + `### Added` (Windows native shim) + note PATHEXT machinery removed; no
  migration prose.
- `adr_windows_cmd_argv_injection.md` — update §Future Work / §Implementation
  Plan to point at this ADR; reconcile #72/#66.
- `.claude/rules/subsystem-package-manager.md` — "OCX Configuration
  Forwarding" + wire-ABI canary note: **two** producers post-cutover — the
  `.sh` `body.rs` golden and the `ocx_shim` `WIRE_SUBCOMMAND` (the `.cmd`
  producer is gone).
- `.claude/rules/subsystem-ci.md` — if a shim build/sign job is added.
- `.claude/rules/arch-principles.md` — "Cross-Cutting Modules" table:
  add `shim` row; ADR Index: add this ADR.
- `crates/ocx_lib/src/package_manager/launcher/safety.rs` doc comment —
  note the sidecar reuses `LAUNCHER_UNSAFE_CHARS`.

---

## Out of Implementation Scope (Signing Ops Prerequisites)

Phase 2 (SignPath Foundation) is **explicitly not implemented** by the feature
that lands this ADR. It is gated on operations work owned by the maintainer,
not the implementing agent:

1. **ocx.sh `/code-signing-policy` page** must exist and be linkable (SignPath
   Foundation onboarding requirement).
2. **Maintainer MFA** enforced on the GitHub org / SignPath account.
3. **GitHub-hosted runners only** (no self-hosted) — SignPath Foundation
   constraint.
4. **PE product/version metadata** populated in `crates/ocx_shim` (product
   name, version, company) so the signed artifact has identity.
5. **RFC3161 timestamping mandatory** in the signing step (else signature dies
   with the short-lived cert).
6. **Strip BEFORE sign** — the build profile strips; signing happens after
   strip and the signed blob is the committed artifact. Never strip post-sign.
7. The `SignPath/github-action-submit-signing-request` workflow step
   (~15 lines, no Windows runner) is added in a **separate PR** that also
   refreshes the committed blob with the signed bytes.
8. **Fallback** (documented, not implemented): Azure Trusted Signing if OCX
   becomes a US/CA/EU/UK legal entity.

The Phase 1 deliverable ships a **functional unsigned shim**; the security fix
(closing `%*`) does not depend on any of the above.

---

## Product Positioning

**Flag raised per `workflow-swarm.md`.** This change strengthens OCX's
positioning ("backend-automation-first, security-conscious") versus Scoop /
Chocolatey, which still ship unescaped `%*`. It does not change target users,
the competitive matrix rows, or product principles — it reinforces
Differentiator #5 (backend-first) and Principle #3 (keep it safe). A
**positioning note (not a matrix change)** for `product-context.md` is
appropriate when this lands: under "Why OCI / security" narrative, OCX's
Windows launchers bypass `cmd.exe` entirely. Defer the actual edit to the
doc-writer at landing; no rebuttal/matrix row needs rewording now.

---

## Implementation Plan (high level — detailed plan is a separate artifact)

1. [x] New crate `crates/ocx_shim` (`std` + `windows-sys` + `dunce`), profile `opt-level="z"`/`lto`/`codegen-units=1`/`panic="abort"`/`strip`. x86_64 + aarch64.
2. [x] Reproducible build → strip → commit blobs to `crates/ocx_lib/src/shims/ocx-shim-{x86_64,aarch64}.exe` + record SHA-256.
3. [x] `crates/ocx_lib/src/shim.rs` (Contract 3) + `pub mod shim;` in `lib.rs`.
4. [x] `launcher::generate()` per Contract 2 (cfg-gated `.exe`/`.shim` writes; `.exe` before `.shim`; **NO `.cmd`** — Axis C cutover).
5. [x] Unit goldens: sidecar body bytes; `SHIM_BYTES` non-empty/empty by cfg; blob↔SHA guard; `.cmd`-never-emitted assertions.
6. [x] Acceptance test on `windows-latest`: resolve via cmd/pwsh/bash, argv with spaces/`&`/unicode, exit-code propagation, Ctrl+C, missing sidecar, `.exe`-resolves-no-`.cmd`. (Registry-independent fixture — Docker registry startup fails on Windows.)
7. [x] **Remove the dead PATHEXT warn/inject machinery in-scope** (same change set as the cutover — it is dead once `.cmd` is gone, not behavior-neutral coexistence cruft).
8. [x] Documentation surfaces (list above).
9. [ ] (Separate PR, Phase 2) SignPath signing step + signed-blob refresh.

## Validation

- [ ] Acceptance suite green on `windows-latest` (registry-independent tests).
- [ ] Shim binary within `SHIM_SIZE_BUDGET` (256 KiB, the binding ceiling). Actual: ~138 KiB (x86_64) / ~128 KiB (aarch64). The earlier "<80 KB" was an initial estimate, not the budget.
- [ ] `task rust:verify` green; existing `body.rs` / `generate.rs` goldens unmodified (additive change).
- [ ] Security review confirms `%*` surface closed for default resolution; residual `.cmd`-by-extension documented.
- [ ] `signtool verify /pa` / `osslsigncode verify` passes on the embedded copy (Phase 2 only).

## Links

- [`adr_windows_cmd_argv_injection.md`](./adr_windows_cmd_argv_injection.md) — parent
- [`adr_package_entry_points.md`](./adr_package_entry_points.md) — launcher design
- [`research_windows_shim_tech.md`](./research_windows_shim_tech.md)
- [`research_windows_shim_patterns.md`](./research_windows_shim_patterns.md)
- [`research_windows_shim_signing.md`](./research_windows_shim_signing.md)
- [`system_design_windows_exe_shim.md`](./system_design_windows_exe_shim.md) — companion system design
- BatBadBut / CVE-2024-24576 — [GHSA-q455-m56c-85mh](https://github.com/rust-lang/rust/security/advisories/GHSA-q455-m56c-85mh)

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-18 | Architect worker (Opus 4.7, issue #66) | Initial draft — Proposed. Options A/B/C/D, error taxonomy, 3 component contracts, `.shim` format, UX scenarios, reversibility, signing-ops out-of-scope |
| 2026-05-18 | Builder (Opus 4.7, Codex gate one-shot) | Contract 1 §Behavior re-sequenced for the combined `STARTUPINFOEXW`(HANDLE_LIST+JOB_LIST, no-suspend) rewrite: console handler before child runs (finding 4); stdio-only handle whitelist (finding 1, CWE-403); job-at-create via attribute list — removed `CREATE_SUSPENDED`/`AssignProcessToJobObject`/`ResumeThread` race window. Postconditions updated. Context fact #4 updated. |
| 2026-05-18 | Builder (Opus 4.7, review-fix R2 doc-reconcile) | **R2 design-record reconciliation to shipped review-fix code (code behavior unchanged except a behavior-preserving dead-arm collapse).** §`.shim` Sidecar Format Contract: removed `%` from the forbidden-byte set (`LAUNCHER_UNSAFE_CHARS` is now `'`,`"`,`\n`,`\r`,`\0`), added the amendment paragraph + this row — rationale (no post-cutover consumer treats `%` specially: `.sh` single-quotes `pkg_root`, one-line `.shim` read verbatim, `CreateProcessW` has no `cmd.exe`) and the explicit statement that this is a **backward-compatible admissible-set widening** (old `.shim` files are a strict subset of the new grammar — no installed-launcher migration; the dangerous narrowing direction is not taken). Fixed the §Content row (was "the same string baked into the `.cmd` body" — `.cmd` removed in the cutover; now references the `.sh` body single-quoted literal). §Error Taxonomy stderr column reconciled to the shipped lines: E1/E2 carry the `(re-run \`ocx install\` …)` recovery hint; E5 has two messages (unset-pin vs defined-but-missing-pin naming the path); E6 is `failed to start {program}: win32 error {win32}` (not the old `<win32 error>` placeholder / hard-coded `ocx`) — exit codes 69/74/77/78 unchanged. Contract 1: added the third degraded mode (no-console parent → neither `STARTF_USESTDHANDLES` nor the std trio, OS default streams, child still launches) to §Postconditions; annotated step 9 that `STARTF_USESTDHANDLES` is gated on all-three-valid via `core::use_std_handles`. Code: collapsed the now-identical `ERROR_ACCESS_DENIED`/`other` `SpawnFailure` match arms in `ocx_shim::run()` into one (77-vs-74 discrimination already lives in `exit_code()`); behavior identical, verified by `ocx_shim` tests. |
| 2026-05-18 | Builder (Opus 4.7, user-directed scope change) | **Axis C C1→C2 cutover (Status → Accepted).** Stop emitting `.cmd` entirely; Windows launcher is `.exe`+`.shim` only. Rewrote Axis C verdict + weighted table + §Decision Outcome + §Quantified Impact (2 files/entry, PATHEXT machinery removed) + §Consequences (residual `%*` orphan GONE; accepted negative now = no launcher in unsigned-`.exe`-blocked locked-down envs) + §Reversibility + §Migration (clean break, pre-1.0, no migration code) + Context facts #2/#6 + Implementation Plan (PATHEXT removal now in-scope/done, not deferred) + doc-surface list. Title + Status updated. Removed the dead PATHEXT inject/warn subsystem (`pathext` module, `warn_if_pathext_missing_launcher`, `emplace_pathext` call sites, `synthetic_pathext_entry`). Shim blob unaffected — not rebuilt. |
