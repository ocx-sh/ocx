# ADR: Shared OCX store across DevContainer instances — store zoning, cross-device assembly, concurrency-safe GC

## Metadata

**Status:** Proposed
**Date:** 2026-06-13
**Deciders:** Architect (/architect), repository owner
**Beads Issue:** N/A
**Related PRD:** N/A
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust 2024, Tokio; no new runtime tech)
- [x] One new crate considered (`reflink-copy`) — justified in Technical Details, spends ≤1 innovation token
**Domain Tags:** infrastructure, data
**Supersedes:** N/A
**Superseded By:** N/A

## Context

OCX stores everything under a single root `$OCX_HOME` (default `~/.ocx`), fanned out into seven stores by `FileStructure::with_root` → `root.join("<name>")` (`file_structure.rs:65-76`): `blobs/`, `layers/`, `packages/`, `tags/`, `symlinks/`, `state/`, `temp/`, plus the `projects/` GC ledger.

Operators want to **share as much of this as possible across multiple DevContainer instances via one mounted volume** — download once, reuse everywhere; and on CI, persist only the expensive-to-produce tiers. Today this is blocked by structural coupling between immutable content and mutable per-environment state. Three concrete blockers, all confirmed at code level (see [`research_shared_store.md`](./research_shared_store.md)):

1. **Hardlinks force one volume.** `packages/.../content/` is hardlinked from `layers/.../content/`; `hardlink.rs` errors `CrossesDevices` with no fallback. So packages and layers must share a filesystem. This kills the CI use case "cache blobs+layers on a persistent mount, keep packages ephemeral on local disk" — different volumes → hard fail.
2. **Install symlinks are per-environment state.** `symlinks/.../{candidate,current}` encode each instance's version pins. They cannot be shared: instance A pinning `cmake:3.28` and B pinning `cmake:3.31` would clobber each other's `current`.
3. **GC is blind across instances.** A `ocx clean` in instance A roots reachability on (a) install back-refs whose targets live in *another* instance's filesystem view, and (b) project locks whose `ocx.toml`/dir is not mounted in A. Both fail open (collect what is actually live). And there is no store-wide lock — `delete_objects` itself documents "Do not run `clean` while other OCX operations are in progress."

Adversarial verification refuted all three "is it safe?" claims (concurrent same-digest publish; concurrent GC; NFS/volume guarantees) with high confidence. The package tier's publish primitive (`move_dir` = `remove_dir_all(dst)` then `rename`, `fs.rs:63-68`) is the single most dangerous element: it destructively replaces a live directory while readers traverse it unlocked.

This ADR decides how to make the store shareable: how to split it, how to relax the single-volume constraint, and what concurrency/GC hardening is mandatory once content is shared.

## Decision Drivers

- **KISS / few knobs.** Operators should reach for one or two env vars, not a 7-way matrix of footguns.
- **Two real use cases.** (UC1) DevContainer fleet sharing the whole content store, install state per-instance. (UC2) CI caching only blobs+layers, packages ephemeral on a *different* volume.
- **Correctness under concurrency.** Multiple `ocx` processes on the same shared store must not corrupt it or delete each other's live objects.
- **Backward compatibility of on-disk metadata + OCI manifest** (project invariant). Storage *layout under the root* is not in that frozen set and may change.
- **Honest substrate boundaries.** Network filesystems silently void flock and atomic rename; the design must state where guarantees end rather than pretend.

## Industry Context & Research

**Research artifact:** [`research_shared_store.md`](./research_shared_store.md)

**Trending / proven approaches.** Every mature shared artifact store separates an **immutable, content-addressed layer** (safe to share concurrently with *no* locking — collisions impossible by identity) from a **mutable per-environment layer**, and confines concurrency control to the latter:

- **containerd** — content store (CAS, immutable) vs snapshots (per-container, copy-on-write). Concurrent pulls serialize via `OpenWriter`/`ErrLocked` + digest-verifying `Commit` (`ErrAlreadyExists` instead of overwrite).
- **Nix** — immutable `/nix/store`; multi-user writes funnel through a single-writer daemon; GC uses a global lock + live socket so in-flight builds register roots mid-collection.
- **Bazel** — CAS (content) split from the Action Cache (results); `--disk_cache` shareable because content-addressed; idle, size/age-based GC.
- **pnpm** — global CAS; import-method fallback **clone (reflink/CoW) → hardlink → copy**; store on a different drive → copy; `pnpm store prune` tracks live projects via symlinks under the store (the same shape as OCX's project ledger).

**Key insight.** OCX already *has* this separation latent in its tiers: blobs and layers are content-addressed and publish non-destructively (safe to share today); packages are content-addressed but one destructive primitive away from safe; symlinks/state/projects are inherently per-instance. The design should make the latent split explicit rather than invent a new model. For cross-drive relaxation, `reflink-copy::reflink_or_copy` is the proven primitive (CoW with automatic copy fallback). For shared-store GC, follow Nix/Bazel: reference-count from roots, never delete anything reachable from a live environment, serialize collection, and prefer idle/age-gated sweeps.

## Considered Options

### Option 1: Documentation only — "put `$OCX_HOME` on the shared volume, single host, never clean concurrently"

**Description:** No code change. Document that the whole root may live on one shared local-FS volume, that install state will be shared (so instances see each other's `current`), and that concurrent `clean` is unsupported.

| Pros | Cons |
|------|------|
| Zero engineering cost | Does not solve per-environment pins — shared `symlinks/` means instances clobber each other's selections |
| No new surface | Does not enable UC2 (cross-drive blobs+layers-only cache) — hardlink still single-volume |
| | Leaves the `move_dir` destructive-replace hazard live |
| | Leaves GC fail-open; "never clean concurrently" is unenforceable across instances |

### Option 2: Full per-store env granularity — `OCX_BLOBS_DIR`, `OCX_LAYERS_DIR`, `OCX_PACKAGES_DIR`, `OCX_TAGS_DIR`, `OCX_SYMLINKS_DIR`, `OCX_STATE_DIR`, `OCX_TEMP_DIR`

**Description:** One env var per store, each defaulting to `$OCX_HOME/<name>`, routed through a resolver. Plus cross-device assembly fallback and GC hardening.

| Pros | Cons |
|------|------|
| Maximally flexible — any split possible | Footgun matrix: inter-store invariants (temp must co-locate with the tier it stages; packages need layers same-FS *or* fallback) are invisible to the operator |
| Directly expresses every use case | 7 vars to document, validate, and forward to subprocesses |
| | Violates KISS / YAGNI — most combinations are nonsense and need guard rails |

### Option 3 (recommended): Two/three-zone overrides + cross-device assembly fallback + concurrency-safe GC, delivered in phases

**Description:** Group the stores into **content** (shareable, content-addressed) and **state** (per-instance, mutable), each overridable by one env var, with packages independently relocatable for the CI case. Harden the package publish primitive and GC so a shared store is actually safe.

- `OCX_CACHE_DIR` → `blobs/` + `layers/` (+ their staging temp). The shareable, regenerable bulk. Default: under `$OCX_HOME`.
- `OCX_STATE_DIR` → `symlinks/` + `state/` + `projects/`. Per-instance, never shared. Default: under `$OCX_HOME`.
- `OCX_PACKAGES_DIR` → `packages/` (+ package staging temp). Default: with cache zone (so hardlink assembly stays intra-volume) unless overridden onto another volume, which engages the reflink→copy assembly fallback.
- `OCX_INDEX` (existing) → `tags/` index; folded into the same resolver.

| Pros | Cons |
|------|------|
| Two env vars cover UC1; a third covers UC2 — KISS surface | Requires the `move_dir` → non-destructive publish fix and GC hardening to be *safe* (but those are correctness fixes worth doing regardless) |
| Mirrors the proven content-vs-state split (containerd/Bazel/Nix/pnpm) | `temp/` must split per zone (implementation detail) |
| Zoning keeps invariants enforceable (each zone is internally one volume) | Cross-device fallback adds one dependency (`reflink-copy`) |
| Symlinks cross volumes natively, so state separation is free | Phased — full CI cross-drive support lands in phase 2 |

## Decision Outcome

**Chosen Option:** Option 3, delivered in three phases so each phase is independently shippable and the risky parts are isolated.

**Rationale.** Option 1 doesn't solve per-environment pins or the CI split and leaves two correctness hazards. Option 2 is a footgun matrix that violates KISS. Option 3 makes the *already-latent* content-vs-state split explicit with a minimal knob set, matches every reference system, and — crucially — pairs the new sharing capability with the concurrency fixes that make it safe. The package-publish and GC fixes are not optional extras: they are pre-existing correctness gaps (the `dest_override` path can already trigger a destructive `move_dir` race single-host) that sharing would amplify, so doing them now pays double.

### Phasing

| Phase | Scope | Unblocks | Risk |
|-------|-------|----------|------|
| **P1 — Zoning + package-publish fix** | `OCX_STATE_DIR` (and `OCX_CACHE_DIR` as alias for content) via a single layout resolver; make package publish non-destructive (first-writer-wins, like layers); state separation so symlinks/projects/state stay per-instance | UC1 (DevContainer fleet sharing content, isolating install state) on same-host local-FS volume | Medium — touches publish path; covered by characterization tests |
| **P2 — Cross-device assembly** | `OCX_PACKAGES_DIR` independent volume; `reflink-copy` fallback in `hardlink`/`assemble` when layer and package dirs differ in filesystem; split `temp/` per zone | UC2 (CI caches blobs+layers only) | Medium |
| **P3 — Concurrency-safe GC** | store-wide GC lock (exclusive `clean` vs shared install); fail-closed cross-instance roots; mtime grace period; network-FS detection + best-effort posture | safe concurrent `clean` on a shared store | High — semantics change; ADR-worthy sub-decisions |

### Quantified Impact

| Metric | Before | After | Notes |
|--------|--------|-------|-------|
| Env vars for store layout | 2 (`OCX_HOME`, `OCX_INDEX`) | 4 (+`OCX_CACHE_DIR`/`OCX_STATE_DIR`/`OCX_PACKAGES_DIR`) | KISS surface, one resolver |
| Cross-drive package assembly | hard fail (`CrossesDevices`) | reflink (CoW) or copy fallback | enables UC2 |
| Disk per DevContainer (UC1, N instances) | N × full store | 1 × content + N × (small state) | dedup of blobs/layers/packages |
| Safe concurrent `clean` | no (documented unsafe) | yes on local-FS, same host (P3) | best-effort on network FS |

### Consequences

**Positive:**
- DevContainer fleets share blobs/layers/packages on one volume; each instance keeps independent pins/selections.
- CI persists only the expensive tiers; packages stay ephemeral on a different drive.
- Package publish becomes safe-by-construction (matches blob/layer tiers); a pre-existing `dest_override` race is closed.
- GC gains a real concurrency guard and stops fail-open over-collection across instances.

**Negative:**
- More env surface (3 new vars) to document and forward via `OcxConfigView`/`apply_ocx_config`/`environment.md`.
- One new dependency (`reflink-copy`).
- P3 changes GC semantics (root liveness three-state for install back-refs; `NotFound`→`Unknown` for shared stores) — needs its own focused review.

**Risks:**
- **Network filesystem (NFS/SMB/cross-host).** flock degrades to a no-op and `rename(2)` is not atomic; mutable-namespace locks (tags, `ocx.toml`, `install.json`, auth) lose mutual exclusion. *Mitigation:* document as best-effort, add P3 network-FS detection that warns (or refuses) on mutating ops + `clean`. Content tiers stay correct via content-addressing regardless.
- **Named-volume root ownership (UID 0).** Non-root container users hit permission-denied on a fresh named volume. *Mitigation:* document the match-UID/GID or entrypoint-chown pattern (DevContainer norm).
- **Shared install back-refs would be unreliable GC roots.** Because install state is per-instance (state zone), shared-store GC must root on the **shared project ledger**, not on per-instance install symlinks. *Mitigation:* P3 places `projects/` decision explicitly; lockfile-driven installs are the rooting source for shared stores.

## Technical Details

### Architecture — zones

```
                    ┌─────────────────────────── shared volume (one filesystem) ───────────────────────────┐
  OCX_CACHE_DIR ──► │  blobs/      (CAS, raw OCI; atomic file rename; safe multi-writer)                    │
                    │  layers/     (CAS, extracted; non-destructive dir rename; safe multi-writer)          │
  OCX_PACKAGES_DIR  │  packages/   (CAS, assembled; hardlinked from layers/ — same volume OR reflink/copy)  │
  (default: cache)  │  temp/       (staging; MUST co-locate per zone for intra-volume rename)               │
                    └─────────────────────────────────────────────────────────────────────────────────────┘

  OCX_STATE_DIR ──► ┌──── per-instance (instance-local disk / workspace; symlinks cross volumes fine) ──────┐
                    │  symlinks/   (candidate / current — each instance's version pins)                     │
                    │  state/      (update-check timestamps, runtime prefs)                                 │
                    │  projects/   (GC ledger — see P3 for shared-store rooting)                            │
                    └─────────────────────────────────────────────────────────────────────────────────────┘
```

UC1 (DevContainer fleet): `OCX_CACHE_DIR=OCX_PACKAGES_DIR=/vol/ocx-shared`, `OCX_STATE_DIR=~/.ocx-local`.
UC2 (CI): `OCX_CACHE_DIR=/cache/ocx` (persistent), packages + state under ephemeral `$OCX_HOME` → cross-device assembly fallback engages.

### Env-var contract (resolution: flag ▸ env ▸ root-default)

| Var | Controls | Default |
|-----|----------|---------|
| `OCX_HOME` | root for any zone not overridden | `~/.ocx` |
| `OCX_CACHE_DIR` | `blobs/` + `layers/` (+ layer temp) | `$OCX_HOME` |
| `OCX_PACKAGES_DIR` | `packages/` (+ package temp) | `$OCX_CACHE_DIR` (keep hardlink intra-volume) |
| `OCX_STATE_DIR` | `symlinks/` + `state/` + `projects/` | `$OCX_HOME` |
| `OCX_INDEX` (existing) | `tags/` index | `$OCX_CACHE_DIR` tags |

All new vars are resolution-affecting → must be added to `env::keys`, `env::OcxConfigView`, `Env::apply_ocx_config`, and `website/src/docs/reference/environment.md` (per the CLI forwarding rule in `subsystem-cli.md`).

### Code seam — one resolver, two seams collapsed

Introduce a `StoreLayout` resolver in `ocx_lib` consumed by a new `FileStructure::with_layout(layout)`; `with_root` stays as the no-override path. The resolver picks `override-or-root.join(name)` per store and is the single place `OCX_CACHE_DIR`/`OCX_STATE_DIR`/`OCX_PACKAGES_DIR`/`OCX_INDEX` are applied — folding in the currently-divergent tag-root override at `context.rs:129-138`. `BlobStore::new`/`LayerStore::new`/etc. already take a `PathBuf`, so they are untouched (existing seam that makes this tractable). Resolution lives in `ocx_lib`, not the CLI (`feedback_lib_hosts_substance_cli_thin.md`).

### Package publish fix (P1) — make it non-destructive

Replace the package tier's `move_dir` (`remove_dir_all(dst)` + `rename`) with the layer tier's pattern (`layer_staging.rs:40-48`): bare `rename`; if it fails because `dst` exists with an OK `install.json`, discard our temp and treat as success (first-writer-wins). For the legitimate re-pull-over-broken-install case, either publish under a fresh temp and swap, or guard the destructive replace + all package reads behind the per-package `install.json` lock (L4) so no reader observes a remove window. This eliminates the reader-visible `ENOENT` window and the `dest_override` concurrent-destructive-publish race — a correctness fix independent of sharing.

### Cross-device assembly (P2)

Add `reflink-copy` (`reflink_or_copy()` = CoW clone with automatic byte-copy fallback). In the assembly walker (`assemble_from_layer`) / `hardlink`, choose: same-filesystem (via `same_filesystem`) → hardlink; else → `reflink_or_copy`. CoW preserves the space/speed benefit on btrfs/XFS/APFS; copy is the universal fallback. Note CoW files are independent inodes — re-validate the macOS codesign-through-shared-inode assumption (only hardlinks share inodes); copied/cloned package content must be signed in place.

### GC hardening (P3)

- **Store-wide advisory lock**: `clean()` takes an exclusive `$OCX_STATE_DIR`/store-scoped flock; install/pull take it shared. Serializes GC vs mutators on a local-FS volume (Nix's global-GC-lock pattern).
- **Fail-closed cross-instance roots**: classify `dunce::canonicalize` `NotFound` on a project-ledger target as `Unknown` (retain) when the store may be shared, not `Dead` (`registry.rs:125`); give install back-ref liveness an `Unknown` state (retain when `path_exists_lossy` cannot positively confirm vs a clean ENOENT) (`reachability_graph.rs:286-289`).
- **mtime grace period**: skip objects younger than N seconds to narrow the write-before-ref TOCTOU window (Bazel's age-gated GC).
- **Shared-store rooting**: root GC on the **shared project ledger** (lockfile pins), since per-instance install symlinks are not globally visible. Place `projects/` to be reachable by all sharers (decision recorded in P3).
- **Network-FS posture**: detect NFS/SMB at store init; warn (or refuse) on mutating ops + `clean`; document `$OCX_HOME` on network FS as best-effort (consistent with `adr_lock_file_locking_strategy.md`).

### "Are we safe?" — verified matrix (P3 as-built)

| Substrate | Content tiers (blob/layer/package*) | Mutable state (tags/symlinks/projects/install/auth) | Concurrent `clean` |
|-----------|-------------------------------------|------------------------------------------------------|--------------------|
| Same host, local-FS shared volume | Safe (*after P1 package fix) | Safe — state per-instance; shared locks enforced by host kernel | **Safe (P3 landed)** — GC lock serializes `clean` vs install/pull on same instance; grace window + content-addressing cover cross-instance. |
| Network FS / cross-host | Content stays correct (content-addressed); rename non-atomic on NFS is a residual risk | **Unsafe** — flock degrades silently on NFS v2/v3 | **Best-effort.** P3 detects NFS and warns; `OCX_NETWORK_FS=refuse` enforces hard block (exit 81). Cross-instance concurrent clean on NFS-hosted store is still unsafe due to silent lock degradation. |

**P3 hardening facts (cross-model review, as-built):**

1. **Three-state liveness** — `read_link` I/O errors (e.g. NFS stale-handle, EPERM) on install back-refs → `RefLiveness::Unknown` → retain. Only clean `ENOENT`/`NotFound` → `Dead`. `registry.rs::probe_live_target` was already three-state pre-P3; P3 confirmed `reachability_graph.rs` uses the same semantics for back-refs.
2. **Shared-roots fail-closed on unreadable peer** — `SharedRootsVersion` is a `serde_repr` u8 enum (V1=1); unknown version → `SharedRootsUnion::RetainAll`. Unreadable committed peer ledger file (non-`NotFound` I/O error) → `RetainAll`. NotFound race (file vanished between readdir and read) → skip with debug log (benign).
3. **Shared-root resolution fail-closed** — when a peer's digest appears in the shared ledger but the corresponding blob/layer/package is absent from the cleaner's own cache, the cleaner retains the entry (the object is simply missing from this instance's view; it may be present on the peer's zone).
4. **GC lock ordering** — GC lock (`$OCX_STATE_DIR/gc.lock`) acquired BEFORE any L1 object-store lock (state-zone is coarser than object-level). This ordering is enforced structurally: `clean()` calls `GcLock::acquire_exclusive()` at the start, before any `PackageStore`/`LayerStore`/`BlobStore` operation.
5. **Cross-instance scope** — the GC lock is per-instance (lives in per-instance `$OCX_STATE_DIR`). Cross-instance safety relies solely on: content-addressing (idempotent writes) + mtime grace window (time-bounded) + shared-roots ledger (opt-in, fail-closed).

## Implementation Plan

1. [ ] **P1.1** `StoreLayout` resolver + `FileStructure::with_layout`; fold in `OCX_INDEX`; add `OCX_CACHE_DIR`/`OCX_STATE_DIR`; mirror in `OcxConfigView`/`apply_ocx_config`/`environment.md`.
2. [ ] **P1.2** Characterization tests for package publish; replace `move_dir` with non-destructive first-writer-wins; close `dest_override` race; verify launcher/`find`/`clean` never see a delete window.
3. [ ] **P1.3** Acceptance test: two `ocx` processes sharing content zone, separate state zones, independent pins.
4. [ ] **P2.1** Add `reflink-copy`; `OCX_PACKAGES_DIR`; cross-device branch in assembly; split `temp/` per zone; re-validate codesign on CoW/copied content.
5. [ ] **P2.2** Acceptance test: blobs+layers on FS-A, packages on FS-B → reflink-or-copy assembly succeeds.
6. [ ] **P3.1** Store-wide GC lock (`$OCX_STATE_DIR/gc.lock`, exclusive clean / shared install) + acceptance test (concurrent clean vs install). **Scope: same-`$OCX_HOME` only** — the state zone is per-instance, so this lock does NOT exclude across instances sharing only the content zone; cross-instance correctness rests on content-addressing (P1) + mtime grace (P3.3) + shared-roots rooting (P3.5) + network-FS refusal (P3.6).
7. [ ] **P3.2** Install back-ref three-state liveness (`RefLiveness::{Live,Dead,Unknown}` in `reachability_graph.rs`; I/O-error → `Unknown` → retain, not fail-open dead). **NOTE (design correction):** `registry.rs::probe_live_target` is **already** three-state (`NotFound`→`Dead`, other `Err`→`Unknown`→retain) and `collect_project_roots` **already** fail-closed (`RetainAll`) — do NOT re-implement. The original "`NotFound`→`Unknown`" wording was stale.
8. [ ] **P3.3** mtime grace period (`OCX_GC_GRACE_SECONDS`, default 600s; skip entries younger than N before delete; clock-skew/future-mtime → retain).
9. [ ] **P3.4** Delete-objects audit log — append-only JSONL at `$OCX_STATE_DIR/gc-log.jsonl` (`schema_version`, `instance_id`, `run_id`, `object_path`, `digest`, `tier`, `action` {Deleted|WouldDelete|Skipped}, `reason`, `bytes_freed`, `dry_run`, `force`); one-generation rotation at `OCX_GC_LOG_MAX_BYTES` (10 MiB); best-effort (WARN, never fatal); `OCX_GC_LOG=off` disables. Forensics for "did instance A delete instance B's object". Per-instance (each logs its own deletions; correlate by `instance_id`).
10. [ ] **P3.5** Shared-store rooting (opt-in `OCX_SHARED_STORE`): digest-only shared roots ledger `$OCX_PACKAGES_DIR/roots/<instance_id>/<project_hash>` written on lock-save (best-effort); shared-mode `clean` unions all instances' shared roots. `projects/` symlink ledger stays per-instance (state zone). **One-way-door sub-decision — needs architect sign-off (new GC-root authority + on-disk structure); cross-ref `adr_project_gc_symlink_ledger.md`.** Also: scoped `NotFound`→non-pruning for the project ledger only when `OCX_SHARED_STORE=true` (default unchanged — departed projects still prune).
11. [ ] **P3.6** Network-FS detection (`statfs`/`GetVolumeInformationW` → `Local|Network(name)|Unknown`) + posture knob `OCX_NETWORK_FS` ∈ {`warn` (default), `refuse`, `allow`}; `refuse` returns `ExitCode::PolicyBlocked` (81, reused — record overload in `quality-rust-exit_codes.md`). Testing seam `__OCX_TESTING_FORCE_FS_KIND`.

## Validation

- [ ] Perf: UC1 disk usage ≈ 1×content + N×state (not N×full).
- [ ] Correctness: concurrent same-digest publish exposes no `ENOENT`/half-state to readers (P1).
- [ ] Correctness: concurrent `clean` + install never deletes a live object on local-FS, same host (P3).
- [ ] Cross-device assembly succeeds with reflink (btrfs/XFS) and copy (ext4) (P2).
- [ ] Security review: network-FS best-effort posture; named-volume UID guidance; codesign-on-CoW.
- [ ] `task verify` green; `environment.md` updated; subsystem rules (`subsystem-file-structure.md`, `subsystem-package-manager.md`) updated.

## Links

- [research_shared_store.md](./research_shared_store.md) — full findings + citations
- [adr_three_tier_cas_storage.md](./adr_three_tier_cas_storage.md) — the blob/layer/package tier model
- [adr_file_lock_unification.md](./adr_file_lock_unification.md) — blob lock-free rationale, cross-process race posture
- [adr_lock_file_locking_strategy.md](./adr_lock_file_locking_strategy.md) — flock semantics, NFS best-effort
- [adr_project_gc_symlink_ledger.md](./adr_project_gc_symlink_ledger.md) — project GC ledger + Live/Dead/Unknown
- [adr_global_toolchain_tier.md](./adr_global_toolchain_tier.md) — implicit global lock GC root

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-06-13 | Architect (/architect) | Initial draft — zoning + cross-device assembly + concurrency-safe GC, phased |
| 2026-06-13 | Architect (/architect) | Design-hardening pass (5-mechanism workflow). Corrections: `registry.rs` already three-state + `collect_project_roots` already fail-closed (P3.2 rewritten); GC lock scoped same-`$OCX_HOME` (P3.1); added delete-objects audit log (P3.4), opt-in shared-roots ledger (P3.5, one-way-door sign-off), network-FS posture (P3.6). Backing detail in [`system_design_shared_store.md`](./system_design_shared_store.md). |
