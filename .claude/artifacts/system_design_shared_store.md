# System Design: Shareable OCX store across DevContainer instances

**Status:** Draft → ready for plan
**Date:** 2026-06-13
**Author:** Architect (/architect)
**Decision record:** [`adr_devcontainer_shared_store.md`](./adr_devcontainer_shared_store.md)
**Research:** [`research_shared_store.md`](./research_shared_store.md)
**Backing detail:** five mechanism designs (M1–M5) produced by a design-hardening workflow; this document is the self-contained synthesis.

This is the authoritative "how it works" record. The execution breakdown (phases, tasks, acceptance criteria) lives in [`plan_shared_store.md`](../state/plans/plan_shared_store.md).

---

## 1. Goal & use cases

Make the OCX store shareable across multiple DevContainer instances on one mounted volume, and let CI cache only the expensive tiers — without corruption or cross-instance GC deletion.

- **UC1 — DevContainer fleet.** N containers share the content store (blobs/layers/packages) on one volume; each keeps its own install state (pins/selections/projects). Download/extract/assemble once; dedup to `1×content + N×state`.
- **UC2 — CI cache split.** Persist blobs+layers on a cache mount; keep packages ephemeral on a *different* volume (per-job pins). Requires cross-device assembly.

## 2. Constraints (verified at code level)

| Constraint | Source |
|---|---|
| Package `content/` is hardlinked from `layers/content/`; `hardlink.rs` errors `CrossesDevices`, no fallback → layers+packages same FS today. | `hardlink.rs:57-62`, `assemble.rs:481` |
| Package publish (`move_dir`) is **destructive** (`remove_dir_all(dst)` then rename); package reads take no lock → reader-visible delete window. | `utility/fs.rs:63-68`, `pull.rs:653` |
| Blob publish = atomic file rename (safe); layer publish = non-destructive first-writer-wins rename (safe). | `blob_store.rs:132-143`, `layer_staging.rs:40-48` |
| Single root: `FileStructure::with_root` → `root.join(name)` for 7 stores; only `OCX_HOME`+`OCX_INDEX` exist; a second divergent override seam at `context.rs:129-138`. | `file_structure.rs:65-76`, `context.rs:129-138` |
| All OS locks = `fs4` flock (advisory Unix, mandatory Windows); host-local cross-process OK; **silently degrades on NFS**; singleflight in-process only. | `file_lock.rs`, `research_lockfile_locking_primitives.md:52` |
| GC: install back-ref liveness fail-open (no `Unknown`); `registry.rs` probe **already three-state**; `collect_project_roots` **already fail-closed**; no store-wide lock, no mtime grace. | `reachability_graph.rs:279-294`, `registry.rs:116-133`, `garbage_collection.rs:104-105` |

## 3. Architecture — zones

Separate **immutable content** (shareable, safe by content-addressing — the containerd/Bazel/Nix/pnpm lesson) from **mutable per-instance state** (never shared).

```
 OCX_CACHE_DIR ──►  blobs/  layers/   (+ layer staging temp)     content zone — one volume, shareable
 OCX_PACKAGES_DIR ► packages/         (+ package staging temp)   default = cache zone; separate volume → reflink/copy
 (default: cache)   tags/  (OCX_INDEX, defaults under cache)
 OCX_STATE_DIR ──►  symlinks/  state/  projects/                 per-instance — NEVER shared (symlinks cross volumes fine)
```

- UC1: `OCX_CACHE_DIR=OCX_PACKAGES_DIR=/vol/shared`, `OCX_STATE_DIR=~/.ocx-local`.
- UC2: `OCX_CACHE_DIR=/cache` (persistent), packages+state under ephemeral `$OCX_HOME` → cross-device assembly fallback engages.
- Defaults preserve today's single-root layout exactly (every zone collapses to `$OCX_HOME`).

## 4. Cross-mechanism interaction map (the gaps that matter)

The five mechanisms are not independent. These interactions are load-bearing:

1. **temp-split (M2) × cross-device assembly (M3).** Each tier's staging temp must co-locate with that tier (`layer_temp`→cache, `temp`→packages) so every *publish* is an intra-volume rename. The only remaining inter-zone op is layer→package *assembly* (the hardlink), which is exactly what M3's reflink→copy fallback targets. P1 splits temp; P2 adds the fallback. → P1 must land the temp split even though the fallback is P2.
2. **GC store-wide lock (M4) × non-destructive publish (M1).** The lock is same-`$OCX_HOME` only (state zone is per-instance). For the true cross-instance case the lock provides nothing; safety then rests on M1 (non-destructive publish), M4 mtime grace, and M4 shared-roots. → M1 is a *correctness prerequisite* for sharing, not just a nicety.
3. **projects/ placement tension (M4).** `symlinks/` and `projects/` live in the per-instance state zone → invisible to peers. So shared-store GC cannot root on them cross-instance. Resolved by M4's **digest-only shared roots ledger** in the packages zone (opt-in `OCX_SHARED_STORE`). This is the one new one-way-door structure.
4. **codesign × cross-device (M3).** Hardlinks share inodes (layer signed once, package inherits). Reflink/copy produce independent inodes → macOS package content must be re-signed in place when any file was placed cross-device (`AssemblyStats::independent_inode_files > 0`).
5. **named-volume UID (security/ops).** Docker named volumes are root-owned (UID 0); a non-root container user hits permission-denied. → P1 must surface a clean `PermissionDenied` (77) + doc the chown/UID guidance, not panic.
6. **env forwarding (M2) × launcher re-entry.** Zone vars are resolution-affecting; a launcher re-entering `ocx launcher exec` must inherit them or a fleet member silently falls back to `$OCX_HOME`. → forward via `OcxConfigView`/`apply_ocx_config`.

## 5. Mechanism designs (synthesis)

### M1 — Non-destructive package publish (P1; also a standalone correctness fix)

**Problem.** `move_temp_to_object_store` → `move_dir` does `remove_dir_all(output_path)` then `rename`. Readers (`find_in_store` `common.rs:46-71`, live launchers, `clean` BFS `reachability_graph.rs:250-294`) traverse `packages/{digest}/` with no lock. The re-pull-over-broken-install path (`pull.rs:303-308`) and `pull_local` `dest_override` reach `move_dir` on a *live* dir → reader-visible `ENOENT`/half-state.

**Design.** Mirror the proven layer-tier `finalize_layer_dir` pattern; add a guarded swap for the one case the layer tier never hits (replacing a live broken install):

- New `finalize_package_dir(fs, pinned, temp, output_path)` for the CAS path (`dest_override == None`):
  1. `create_dir_all(parent)`; **bare** `rename(temp, output_path)`.
  2. `Ok` → first writer wins.
  3. `Err` & dest is a **committed OK install** (`check_install_status(output_path/install.json) == true`) → discard our temp, reuse winner (stricter than the layer tier's `path_exists_lossy`: requires a committed install, so a half-written loser can't masquerade as a winner).
  4. `Err` & dest exists but **not OK** (broken/partial) → **stash→swap under lock** (below).
  5. `Err` & no dest → propagate.
- **Broken-install replacement (the only live-dir replace):** never `remove_dir_all(output_path)`. Instead, holding the per-digest **TempStore lock already held by the pull path** (`acquire_temp_dir` at `pull.rs:317`, keyed registry+digest, lives *outside* the dir so it survives the rename — `temp_store.rs:38-39`):
  ```
  stash = <temp_root>/__stale_<pid>_<rand>      // under temp zone → reclaimed by existing stale sweep
  rename(output_path, stash)                     // old live dir out (atomic; open fds survive)
  rename(temp, output_path)                      // new dir into canonical name (atomic)
  remove_dir_all(stash)  (best-effort)
  ```
  Post-lock recheck collapses a second concurrent writer's swap to a no-op. (Dir-over-non-empty-dir rename fails `ENOTEMPTY`; the stash step frees the target name — the standard atomic-directory-replace idiom.)
- `move_temp_to_object_store` branches: CAS dest → `finalize_package_dir`; `dest_override` dest → keep `move_dir` (caller-owned, empty-by-contract, not a shared CAS target).
- `move_dir` retained for override path only; doc narrowed to "destructive; empty/override dest only."
- Windows: route the swap renames through a shared `with_windows_rename_retry` (extract the backoff already in `persist_temp_file`) — a live launcher may hold a handle open (`ERROR_SHARING_VIOLATION`).

**Invariant INV-M1.** No lock-free reader ever observes `packages/{digest}/` missing or half-deleted during publish/re-pull. Established by: happy path is rename-only (no `remove_dir_all` of the canonical name ever); broken-install replace only ever `rename`s the canonical name (old inode unlinked via stash after the new dir is in place; open fds survive); two writers serialized by the held digest lock.

**Tests.** Characterization first (pre-change safety net): `test_repull_replaces_package_dir_observably`. Then U1–U8 unit + **C1 `concurrent_repull_vs_find_never_observes_missing_dir`** (the load-bearing test — tight reader loop during a paused re-pull swap; never `None`/`Err`) + C2 `concurrent_repull_vs_clean_bfs`.

### M2 — Store-layout resolver + zone env vars (P1; `OCX_PACKAGES_DIR` readiness for P2)

**Design.** New `StoreLayout` value struct (`file_structure/store_layout.rs`) resolving zone roots once (flag ▸ env ▸ default), and `FileStructure::with_layout(layout)`; `with_root` becomes `with_layout(StoreLayout::from_root(root))` (all ~25 existing call sites untouched). Mapping:
```
blobs,layers ← cache.join(..)   packages ← packages_root.join("packages")
tags ← OCX_INDEX | cache.join("tags")   symlinks,state,projects ← state.join(..)
temp ← packages.join("temp")   layer_temp ← cache.join("temp")   (NEW second temp store)
```
Defaulting order: packages defaults to **resolved** cache; tags defaults under cache; resolve once at construction.

**Temp split.** `FileStructure` gains `layer_temp: TempStore`. `pull.rs:803` layer acquire → `fs.layer_temp.layer_path`; `pull.rs:508` package acquire → `fs.temp.path` (now packages-zone-rooted); `clean.rs:496` sweeps **both** (idempotent when zones coincide). When zones unified, `temp`/`layer_temp` point at the same dir — identical to today.

**Env keys** (new, all env-only — no new flags; `OCX_INDEX` keeps `--index`): `OCX_CACHE_DIR` (default `$OCX_HOME`), `OCX_PACKAGES_DIR` (default `$OCX_CACHE_DIR`), `OCX_STATE_DIR` (default `$OCX_HOME`). Empty string = unset. `OCX_INDEX` default shifts to `$OCX_CACHE_DIR/tags` (identical when cache unset).

**Resolution-affecting forwarding** (subsystem-cli rule, four surfaces): `env::keys` consts; `OcxConfigView` fields `cache_dir/packages_dir/state_dir` (+ `new()` init); `Env::apply_ocx_config` set-when-present / remove-when-absent arms; `ContextOptions::as_view` populates from env; `environment.md` sections. Collapses the `context.rs:129-138` divergence — `LocalIndex` consumes `file_structure.tags`/`.blobs` directly.

**Independent `OCX_HOME` readers:** `FileStructure::new()` must become env-aware (resolve zones from env, not `from_root`) so `self activate`'s install-symlink bin path resolves under `OCX_STATE_DIR`. `config/loader.rs` (config tier) and `about.rs` (cosmetic) stay `OCX_HOME`-keyed; `ocx_shim` out of scope.

**Tests.** Resolver precedence/defaulting; `with_root == with_layout(from_root)` parity; temp split + collapse; forwarding round-trip; index-coherence regression; `file_structure_new_honors_state_dir`; acceptance `test_state_dir_isolates_install_symlinks`, `test_cache_dir_shared_no_redownload`.

### M3 — Cross-device assembly fallback (P2)

**Design.** Probe `same_filesystem(layer_content, dest_content)` **once per source layer** (not per file), cache verdict in `Arc<Vec<AssemblyMode>>` indexed by `layer_idx` (the walker already carries `layer_idx` per entry). File-placement branch: `Hardlink` → `hardlink::create` (intra-volume, dedup); `Reflink` → `reflink::create` (new module mirroring `hardlink.rs`/`symlink.rs`, wrapping `reflink-copy::reflink_or_copy` = CoW where supported, byte copy otherwise). `reflink_or_copy` is blocking → wrap in `spawn_blocking`.

**codesign.** Add `AssemblyStats.independent_inode_files` (summed in `merge`); after `assemble_from_layers`, if `> 0` and macOS, re-sign `pkg.content()` in place via the existing `sign_extracted_content` (no-op off-macOS / under `OCX_NO_CODESIGN`; dedups by inode). Hardlink path unchanged (inode inheritance). Rename `AssemblyStats.files_hardlinked`→`files_placed` (Two-Hats refactor, separate commit).

**Dependency.** `reflink-copy = { version = "0.1.29", default-features = false }`. License MIT/Apache (already allowed). Expect a non-blocking `multiple-versions = "warn"` for the `windows` family on Windows targets — note in commit, no `deny.toml` change.

**Tests.** `reflink::create` R1–R5 (incl. `/dev/shm` cross-device, independent-inode assertion); assembly A1–A8 (cross-device success, same-device still hardlinks, mixed-FS, exec-bit under copy, symlink, `merge` sums); codesign C1–C4 (macOS-gated); acceptance ACC1 (cache FS-A + packages FS-B install).

### M4 — Concurrency-safe GC + delete-objects audit log + network-FS posture (P3)

**ADR corrections (build on current code, don't redo):** `registry.rs::probe_live_target` is already three-state (`NotFound`→`Dead`, other `Err`→`Unknown`→retain); `collect_project_roots` already fail-closed (`RetainAll`). The genuinely missing pieces:

1. **Store-wide GC lock.** `$OCX_STATE_DIR/gc.lock`; `clean()` exclusive, mutators (install/pull/uninstall/select) shared. RAII via `LockedFile`; new `LockedFile::open_shared_create_with_timeout` (current `open_shared` returns `Ok(None)` when file absent — a gap). Timeouts: clean 120s → `TempFail` (75); mutators 10s then proceed without lock (debug log) so a stuck clean never blocks installs. `OCX_GC_LOCK_TIMEOUT` knob. Order GC-before-L1 (no deadlock). **Same-`$OCX_HOME` only** — explicitly not cross-instance.
2. **Install back-ref three-state.** `has_live_refs` → `RefLiveness::{Live,Dead,Unknown}` (max over refs); `try_exists` `Ok(false)`/`NotFound`→Dead, other `Err`→Unknown→**retain as root**. Do NOT change `path_exists_lossy` elsewhere (surgical).
3. **mtime grace.** In `delete_objects`, skip entry-dir mtime younger than `OCX_GC_GRACE_SECONDS` (default 600); future/zero mtime → retain (clock-skew guard); dry-run honors grace. Primary TOCTOU defense in the cross-instance case where the lock doesn't apply.
4. **Shared-roots rooting (opt-in `OCX_SHARED_STORE`, one-way-door).** Digest-only ledger `$OCX_PACKAGES_DIR/roots/<instance_id>/<project_hash>` written best-effort on lock-save (same trigger as `register_project_dir_best_effort`); `instance_id` from `$OCX_STATE_DIR/instance-id`. Shared-mode `clean` unions all instances' shared roots. `projects/` symlink ledger stays per-instance. Scoped: under `OCX_SHARED_STORE=true`, project-ledger `NotFound` → retain-don't-prune (default unchanged).
5. **Delete-objects audit log.** Append-only JSONL `$OCX_STATE_DIR/gc-log.jsonl`; schema in ADR P3.4; one-generation rotation at `OCX_GC_LOG_MAX_BYTES` (10 MiB); best-effort (WARN, never fatal); `OCX_GC_LOG=off`; dry-run logs `WouldDelete`. Per-instance; correlate by `instance_id`.
6. **Network-FS posture.** `utility::fs::filesystem_kind` (pure `classify_magic(u64)→FsKind` seam + `statfs`/`GetVolumeInformationW` syscall); `OCX_NETWORK_FS` ∈ {`warn` default, `refuse`, `allow`}; `refuse` → `Error::NetworkFsRefused` → `ExitCode::PolicyBlocked` (81, reused). Check content/packages zone (rename atomicity) + state zone (flock). Testing seam `__OCX_TESTING_FORCE_FS_KIND`.

**Tests.** GC-lock 7 unit + concurrency acceptance; back-ref three-state; grace predicate (incl. future-mtime); shared-roots read/write/union + default-mode-ignores; audit-log append/rotation/dry-run/disabled/failure-isolation; network-FS `classify_magic` + posture + forced-NFS acceptance.

### M5 — Test strategy (cross-cutting)

- **Two-instance simulation.** Unit: two `FileStructure::with_layout` sharing `cache`, distinct `state`. Acceptance: `OcxRunner` gains `extra_env`; `shared_store` fixture (one `OCX_CACHE_DIR`, two `OCX_STATE_DIR`).
- **Deterministic races** via extending the existing `OCX_TEST_FAULT` stage enum + new `__OCX_TESTING_*` pause hooks (publish-pause; post-write-pre-ref pause) — barriers, not sleeps. Reuse `ThreadPoolExecutor` spawn pattern + `EXIT_TEMP_FAIL=75` from `test_project_concurrency.py`.
- **Cross-device** via `/dev/shm` runtime device-number self-skip (the `hardlink.rs:183-225` pattern); `separate_tmpfs_device()` guard in `test/src/helpers.py`.
- **Characterization before M1** (refactor Phase 1): `test_package_publish_characterization.py` — lock current behavior incl. the destructive replace being fixed.
- **Cannot test in CI:** NFS flock degradation, real reflink on btrfs/XFS, NFS rename non-atomicity → document + posture tests on the *detection* (mocked fs-kind), `#[ignore]`+`OCX_TEST_REFLINK_FS` opt-in for real reflink.
- **Placement rule:** predicates (grace, liveness, reflink branch, fs-kind classify) → Rust unit; multi-process invariants (publish race, clean-vs-install, cross-device assembly) → pytest. One behavior, one harness.

## 6. Consolidated env-var surface

| Var | Zone/behavior | Resolution-affecting (forwarded)? | Default | Phase |
|---|---|---|---|---|
| `OCX_CACHE_DIR` | blobs+layers (+layer temp) | **Yes** | `$OCX_HOME` | P1 |
| `OCX_STATE_DIR` | symlinks+state+projects | **Yes** | `$OCX_HOME` | P1 |
| `OCX_PACKAGES_DIR` | packages (+package temp) | **Yes** | `$OCX_CACHE_DIR` | P1 (override), P2 (cross-device) |
| `OCX_INDEX` (existing) | tags index | Yes (existing) | `$OCX_CACHE_DIR/tags` | P1 (fold into resolver) |
| `OCX_SHARED_STORE` | shared-mode GC rooting + non-pruning | No (behavioral) | false | P3 |
| `OCX_GC_LOCK_TIMEOUT` | GC lock timeout | No | 120s clean / 10s mutators | P3 |
| `OCX_GC_GRACE_SECONDS` | mtime grace | No | 600 | P3 |
| `OCX_GC_LOG` | audit log on/off | No | on | P3 |
| `OCX_GC_LOG_MAX_BYTES` | audit rotation cap | No | 10 MiB | P3 |
| `OCX_NETWORK_FS` | warn\|refuse\|allow | No | warn | P3 |
| `__OCX_TESTING_FORCE_FS_KIND` | test seam | No | unset | P3 (test) |

## 7. Documentation & rule surfaces (must update with the code)

- `website/src/docs/reference/environment.md` — all new vars (P1: 3 zone vars; P3: 6 GC/FS vars).
- `website/src/docs/user-guide.md` + product-context Storage Layout — zone model, UC1/UC2 recipes, named-volume UID guidance, network-FS best-effort note.
- `.claude/rules/subsystem-file-structure.md` — zone model, `StoreLayout`, two temp stores, `state/gc.lock`, `state/gc-log.jsonl`, `state/instance-id`, `packages/roots/`.
- `.claude/rules/subsystem-package-manager.md` — clean lock + shared-mode rooting; non-destructive publish.
- `.claude/rules/subsystem-cli.md` — forwarding set gains 3 zone vars.
- `.claude/rules/arch-principles.md` — Utility Catalog row `reflink::create`; ADR index amendments.
- `.claude/rules/quality-rust-exit_codes.md` — record `PolicyBlocked` (81) overload for network-FS refuse.
- `crates/ocx_lib/src/...` doc-comments: `hardlink.rs` (soften single-volume), `codesign.rs` (second caller), `garbage_collection.rs` (drop "don't run concurrently"), `move_dir` (destructive/override-only).

## 8. Open decisions needing sign-off

1. **Shared-roots ledger (P3.5)** — new GC-root authority + on-disk structure in the packages zone. One-way-door. Cross-ref `adr_project_gc_symlink_ledger.md`. **Recommend Option 4-A** (digest-only ledger, packages zone) over mounting `projects/` shared.
2. **`PolicyBlocked` (81) reuse** for network-FS `refuse` vs a new exit code — recommend reuse (semantics fit: deliberate local policy refusal).
3. **Audit-log default on** — recommend on (forensics is the point); opt-out via `OCX_GC_LOG=off`.
