# ADR: Hermetic Windows shim cross-build via cargo-zigbuild

## Metadata

**Status:** Accepted (revised path — gnullvm + chkstk stub + build-std; see addendum 2026-05-19)
**Date:** 2026-05-19
**Deciders:** Michael Herwig, Claude
**Beads Issue:** N/A (follows issue #66 Windows shim work)
**Related PRD:** [`prd_windows_exe_shim.md`](./prd_windows_exe_shim.md)
**Tech Strategy Alignment:**
- [x] Rust golden path; introduces Zig as a *build-only* hermetic toolchain (no runtime dependency)
**Domain Tags:** devops, infrastructure, security
**Supersedes:** the cargo-xwin reproducibility approach in [`adr_windows_exe_shim.md`](./adr_windows_exe_shim.md) §reproducibility
**Related:** [`system_design_windows_exe_shim.md`](./system_design_windows_exe_shim.md)

## Context

`build-windows-shims.yml` enforces a byte-equality gate: a fresh cross-rebuild of `crates/ocx_shim` must be `cmp -s`-identical to the committed `ocx-shim-{x86_64,aarch64}.exe` blobs. The build used `cargo xwin` (cargo-xwin + Microsoft VS manifest). The MSVC SDK/CRT and cargo-xwin itself were unpinned, so `cargo xwin` resolved *latest in manifest* at download time. Result: a dev-machine blob refresh passed locally then failed CI's hermetic rebuild — a recurring "refresh drifted blobs" treadmill (commits `93ab95c1`, `72ee86ed`, run 26085901842). Pinning cargo-xwin + `XWIN_SDK_VERSION` + `--xwin-version` major was **verified insufficient** (CI x86_64 `07c6b27e…` ≠ local `f4052a2f…`): CRT-within-major and CI clang/lld remain unpinned. `XWIN_CRT_VERSION` cannot be pinned offline (wants a manifest package version, not the `_VC_CRT_BUILD` header triple; external fetch routes policy-denied).

The msvc-vs-gnu target choice was already flagged an open arch-review item in `rust-toolchain.toml`.

## Decision Drivers

- Cross-machine byte reproducibility (kill the treadmill)
- Minimal dependency surface; dynamically link the OS-provided Windows runtime, not Microsoft's static CRT
- Size budget: `SHIM_SIZE_BUDGET = 256 KiB` fail-closed `const assert` (`crates/ocx_lib/src/shim.rs`)
- Keep the published blob + SLSA-attestation contract intact

## Industry Context & Research

PoC 2026-05-19 (cargo-zigbuild 0.22.3, zig 0.16-dev). Zig bundles its own hermetic toolchain (clang + mingw-w64 + libc); output depends only on pinned-rustc + pinned-zig + source — no floating Microsoft manifest. context7 `/rust-cross/cargo-zigbuild` confirms first-class `x86_64-pc-windows-gnu` support.

**Key insight:** the cross-machine drift is structural to cargo-xwin (external floating SDK). A hermetic toolchain removes the root cause rather than chasing pins.

## Considered Options

### Option 1: Keep cargo-xwin, pin everything (SDK+CRT+clang+cargo-xwin)

| Pros | Cons |
|------|------|
| Smallest binary (~140K) | CRT manifest string not derivable offline; clang pin fragile; verified-insufficient so far; treadmill persists on every MS bump |

### Option 2: cargo-zigbuild + `x86_64-pc-windows-gnu` (+ aarch64 equivalent)

| Pros | Cons |
|------|------|
| Hermetic → true cross-machine reproducibility | gnu binary 280K > 256K budget (mingw overhead) — needs budget bump |
| Resolves the open msvc-vs-gnu arch TODO | aarch64 gnu/gnullvm unproven (`gnullvm` link-fails: `___chkstk_ms`) |
| Zig already on PATH; pinnable | Adds Zig as a build-only toolchain dependency in CI |

### Option 3: Redesign the gate (provenance, not byte-cmp)

**Description:** Committed blob authoritative (CI-built + SLSA-attested); gate fails only if build inputs (shim src, Cargo.lock, toolchain, build flags) changed without a blob refresh — `build-windows-shims.yml` already path-filters exactly these.

| Pros | Cons |
|------|------|
| Cheapest; keeps cargo-xwin + 140K blobs; no toolchain change | Weaker integrity than byte-identical rebuild; doesn't deliver true reproducibility |

## Decision Outcome

**Chosen Option:** Option 2 (cargo-zigbuild + windows-gnu), with the size budget raised as a recorded decision.

**Rationale:** User direction — accept the ~+140 KiB ("doable for now, note it as a decision"), prefer the hermetic toolchain and dynamic OS-runtime linkage over Microsoft's static CRT, minimise dependencies. Removes the drift root cause instead of perpetually chasing pins, and closes the long-open msvc-vs-gnu question.

### Quantified Impact

| Metric | Before (xwin/msvc) | After (zig/gnu) | Notes |
|--------|--------|-------|-------|
| Blob size x86_64 | ~140 KiB | ~280 KiB | Raise `SHIM_SIZE_BUDGET` 256→512 KiB (headroom) |
| Cross-machine reproducible | No | Yes (pinned rustc+zig) | Eliminates treadmill |
| Build-tool deps | cargo-xwin + MS manifest | zig (pinned, hermetic) | No external floating SDK |

### Addendum 2026-05-19 — aarch64 spike failed (BLOCKER)

PoC outcomes: `x86_64-pc-windows-gnu` ✅ (280K, hermetic). `x86_64-pc-windows-gnullvm` ❌ `___chkstk_ms` undefined (persists with `-Zbuild-std` + `-Cpanic=immediate-abort`). `aarch64-pc-windows-gnu` ❌ not a real rustc target. `aarch64-pc-windows-gnullvm` ❌ `__chkstk` undefined. zig+`-nolibc` does not supply the stack-probe builtin for the *-gnullvm targets; mingw `-gnu` works but has no aarch64 variant.

**Resolution (chosen):** supply the stack-probe builtins inline — `crates/ocx_shim/src/chkstk.rs`, upstream libgcc `___chkstk_ms` (x86_64) + compiler-rt `__chkstk` (aarch64), `core::arch::global_asm!`, cfg-gated to `target_abi="llvm"` (gnullvm discriminator). Combined with `-Zbuild-std=std,panic_abort` + `-Cpanic=immediate-abort` (RUSTC_BOOTSTRAP on the pinned 1.95 stable), this links **both** gnullvm arches and sheds std backtrace (gimli/addr2line/object/miniz):

| Target | Size | vs 256K budget |
|---|---|---|
| x86_64-pc-windows-gnullvm | 66 560 B | well under |
| aarch64-pc-windows-gnullvm | 64 000 B | well under |

Locally deterministic across clean rebuilds. **Supersedes the gnu/280K plan: no `SHIM_SIZE_BUDGET` bump needed; result is smaller than the old msvc 140K; near-zero deps (windows-sys + dunce only).** Quantified-impact + size-budget rows below are obsolete (kept for history).

### Addendum 2 2026-05-19 — gate redesign (byte-equality abandoned)

The hermetic toolchain (pinned Zig + rustc, no MS manifest) shipped, but the **byte-equality gate is not achievable**: cargo-zigbuild's `*-pc-windows-gnullvm` PE link is nondeterministic run-to-run even with pinned Zig 0.16.0 + pinned rustc + `/Brepro` + no `-Zbuild-std` (~10 CI runs; lld embeds a per-link build id `/Brepro` does not zero). `-Zbuild-std` additionally non-reproducible and was dropped.

**Resolution — provenance/inputs gate** (the deciders' originally-mused model): the committed blob is authoritative, CI-built + SLSA-attested. `build-windows-shims.yml` no longer `cmp -s`; it now asserts the committed blob is a valid PE, within `SHIM_SIZE_BUDGET`, and matches the in-tree `SHIM_SHA256` (corruption canary), and the job's path filter forces a successful fresh build + a blob-refresh PR (from the uploaded `ocx-shim-fresh-*` artifact) whenever build inputs change. Integrity = SLSA attestation + SHA canary + refresh discipline, not cross-run byte identity. Size budget raised to 512 KiB (no-build-std blob ≈ 235–329 KiB). Net: hermetic *toolchain* (no floating Microsoft SDK — the original treadmill is gone) without an impossible byte-identity requirement.

### Consequences

**Positive:** deterministic blob; treadmill gone; msvc/gnu TODO resolved; smaller attack/repro surface.
**Negative:** larger binary (budget bump); Zig pinned in CI; one-way door (published toolchain + committed bytes change).
**Risks:**
- aarch64 windows-gnu/gnullvm not yet building → *mitigation:* spike aarch64 first; fall back to keeping aarch64 on xwin only if blocked, or `build-std` to shed backtrace deps.
- Size lever if 512 KiB unacceptable later: `build-std` + `panic_immediate_abort` drops std backtrace (gimli/addr2line/object/miniz — the actual bloat, not user deps).

## Implementation Plan (follow-up PR — NOT PR #134)

1. [ ] Spike `aarch64-pc-windows-gnu` (and `gnullvm` with `-lc`/compiler-rt flags) to a working link.
2. [ ] Pin Zig version (`CARGO_ZIGBUILD_ZIG_PATH` / documented pinned zig) + cargo-zigbuild version.
3. [ ] `taskfiles/rust.taskfile.yml` `shim:build` → `cargo zigbuild`; drop xwin pins; PE timestamp determinism flag; double-build determinism check in-task.
4. [ ] `rust-toolchain.toml` targets → gnu; `crates/ocx_lib/src/shim.rs` `include_bytes!` paths + raise `SHIM_SIZE_BUDGET` to 512 KiB; update `SHIM_SHA256`.
5. [ ] Refresh committed blobs from the hermetic build; `build-windows-shims.yml`: install pinned Zig, drop cargo-xwin, keep byte-equality gate (now CI-vs-CI deterministic) + SLSA attestation.
6. [ ] Validate `test_windows_shim.py` on the gnu binary (Windows runner).
7. [ ] Update `adr_windows_exe_shim.md` cross-ref; memory `project_shim_blob_nonreproducible`, `project_windows_shim_arch_size`.

## Validation

- [ ] Two independent machines produce byte-identical blobs
- [ ] Size within raised budget; assert still fail-closed
- [ ] Windows acceptance suite green on gnu binary
- [ ] SLSA attestation verifies on refreshed blob

## Links

- [adr_windows_exe_shim.md](./adr_windows_exe_shim.md)
- [system_design_windows_exe_shim.md](./system_design_windows_exe_shim.md)
- Memory: project_shim_blob_nonreproducible, project_windows_shim_arch_size

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-19 | Claude | Initial draft — decision Accepted per user direction |
