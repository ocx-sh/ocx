# Master Plan: Three-Tier CAS Storage Redesign

## Context

The current `objects/` store conflates content-addressed blobs, extracted layers, and assembled packages into one directory tree. This causes four problems: repo in CAS path breaks dedup (#27), no place for shared layers (#22), manifests outside CAS (#33), and no place for non-extracted blobs (#24). The ADR at `.claude/artifacts/adr_three_tier_cas_storage.md` designs a three-tier replacement: `blobs/` (raw OCI data), `layers/` (extracted content), `packages/` (assembled packages). This plan decomposes the implementation into 10 sequential commits.

**Related:** ADR `adr_three_tier_cas_storage.md`, Research `research_content_addressed_storage.md`
**Persisted as:** `.claude/artifacts/plan_three_tier_cas.md`

## Execution Model

Each sub-plan is one autonomous commit executed via `/swarm-execute` with contract-first TDD.

**Branch:** `goat` (current worktree)
**Quality gate:** `task verify` must pass after each commit
**Workflow:** Swarm (primary) per `.claude/rules/feature-workflow.md`

### Per-Plan Execution Protocol

Every plan follows the full contract-first TDD cycle with review loop:

```
1. Stub        → worker-builder (focus: stubbing)         Gate: cargo check
2. Verify Arch → worker-reviewer (focus: spec-compliance)  Gate: reviewer passes
3. Specify     → worker-tester (focus: specification)      Gate: tests compile, fail against stubs
4. Implement   → worker-builder (focus: implementation)    Gate: task verify passes
5. Review Loop → worker-reviewer (quality + security)      Max 3 rounds
                 worker-architect (for Plans 5, 8, 9)      Actionable → fix; deferred → report
6. Commit      → conventional commit message
```

Step 2 (Verify Arch) is optional for plans touching ≤3 files (Plans 1, 3, 4).
Step 5 includes `worker-architect` for high-risk plans (5, 8, 9) to validate design alignment with the ADR.

**Mandatory context for all Rust agents (builder + reviewer):** Prompts must instruct agents to read `.claude/rules/rust-quality.md` before writing or reviewing code. Key rules to enforce:
- No `pub(crate)` / `pub(super)` — use module nesting for visibility
- No `.unwrap()` in library code
- No `std::fs::*` — all filesystem I/O must use `tokio::fs::*` (async). Use existing `utility::fs::DirWalker` for directory traversal where applicable
- `thiserror::Error` derive on error types with `#[source]` on inner errors
- Explicit `match` for exhaustiveness where new variants must cause compile errors
- `#[allow(dead_code)]` on foundation modules until consumers are wired (remove once consumed)
- Return `&Path` not `&PathBuf` from accessor methods

### Agent Teams Per Plan

| Plan | Builder | Tester | Reviewer | Architect | Doc Reviewer |
|------|---------|--------|----------|-----------|-------------|
| 1 (CAS helpers) | Yes | Yes | Yes (quality) | No | No |
| 2 (3 stores) | Yes | Yes | Yes (quality + spec) | No | No |
| 3 (TagStore) | Yes | Yes | Yes (quality) | No | No |
| 4 (SymlinkStore) | Yes | Yes | Yes (quality) | No | No |
| 5 (Big Bang) | Yes | Yes | Yes (quality + spec) | **Yes** | Yes |
| 6 (RefManager) | Yes | Yes | Yes (quality + spec) | No | No |
| 7 (LocalIndex) | Yes | Yes | Yes (quality) | No | Yes |
| 8 (Pull pipeline) | Yes | Yes | Yes (quality + security) | **Yes** | Yes |
| 9 (GC) | Yes | Yes | Yes (quality + spec) | **Yes** | No |
| 10 (Docs) | No | No | No | No | Yes (writer + reviewer) |

## Dependency Graph

```
Plan 1 (CAS helpers) ──┐
Plan 3 (TagStore)    ───┤──► Plan 5 (Big Bang) ──► Plan 6 (RefManager)
Plan 4 (SymlinkStore)──┤         │                      │
                        │         ▼                      ▼
Plan 2 (3 stores) ──────┘   Plan 7 (LocalIndex) ──► Plan 8 (Pull pipeline)
                                                         │
                                                         ▼
                                                    Plan 9 (GC) ──► Plan 10 (Docs)
```

Plans 1, 3, 4 can be parallelized (separate worktrees). Plan 2 depends on Plan 1. Plans 5-10 are sequential.

---

## Plan 1: CAS Path Helpers and Digest File Contract

**Objective:** Shared path construction and digest-file utilities for all three new stores.
**Artifact:** `.claude/artifacts/plan_cas_path_helpers.md`
**Complexity:** S | **Risk:** Low | **Acceptance tests:** No

**Scope:**
- New file `crates/ocx_lib/src/file_structure/cas_path.rs`
- Git-style 2-level sharding: `{algorithm}/{2hex}/{remaining_hex}`
- `cas_shard_path(digest) -> PathBuf`
- `write_digest_file(dir, digest)` / `read_digest_file(dir) -> Digest`
- `is_valid_cas_path(dir) -> bool` validator
- `CasTier` enum: `Package`, `Layer`, `Blob`
- Register module in `file_structure.rs`

**Files:** `file_structure/cas_path.rs` (create), `file_structure.rs` (add mod)

**Review loop:** Round 1: quality review. No architecture review (≤3 files).

---

## Plan 2: BlobStore, LayerStore, PackageStore Structs

**Objective:** Three new CAS store types with path methods, using helpers from Plan 1.
**Artifact:** `.claude/artifacts/plan_cas_stores.md`
**Complexity:** M | **Risk:** Low | **Acceptance tests:** No
**Dependencies:** Plan 1

**Scope:**
- `BlobStore`: `path(registry, digest)`, `data(...)`, `digest_file(...)`, `list_all() -> Vec<BlobDir>`
- `LayerStore`: `path(registry, digest)`, `content(...)`, `digest_file(...)`, `list_all() -> Vec<LayerDir>`
- `PackageStore`: mirrors `ObjectStore` API — `path(id)`, `content(id)`, `metadata(id)`, `manifest(id)`, `resolve(id)`, `install_status(id)`, `digest_file(id)`, `*_for_content(path)`, `list_all() -> Vec<PackageDir>`
- `PackageDir` with consolidated refs: `refs_symlinks_dir()`, `refs_deps_dir()`, `refs_layers_dir()`, `refs_blobs_dir()`
- No repo in any CAS path
- Accepts `PinnedIdentifier` (extracts registry + digest, ignores repo)
- Register modules (do NOT add to FileStructure yet)

**Files:** `file_structure/blob_store.rs`, `layer_store.rs`, `package_store.rs` (create), `file_structure.rs` (add mods)

**Review loop:** Round 1: quality + spec-compliance review (verify API surface matches ADR D2, D4, D5, D7, D11).

---

## Plan 3: TagStore

**Objective:** Standalone tag store replacing the tags portion of IndexStore.
**Artifact:** `.claude/artifacts/plan_tag_store.md`
**Complexity:** S | **Risk:** Low | **Acceptance tests:** No
**Dependencies:** None

**Scope:**
- `TagStore`: `tags(identifier) -> PathBuf`, `list_repositories(registry) -> Vec<String>`
- Layout: `{root}/{registry}/{repo_path}.json`
- Port from IndexStore tags methods

**Files:** `file_structure/tag_store.rs` (create), `file_structure.rs` (add mod)

**Review loop:** Round 1: quality review. No architecture review (≤3 files).

---

## Plan 4: SymlinkStore

**Objective:** Renamed InstallStore with `symlinks/` root directory.
**Artifact:** `.claude/artifacts/plan_symlink_store.md`
**Complexity:** S | **Risk:** Low | **Acceptance tests:** No
**Dependencies:** None

**Scope:**
- `SymlinkStore`: identical API to `InstallStore` — `current()`, `candidate()`, `candidates()`, `symlink(kind)`
- Root at `{root}/symlinks/` instead of `{root}/installs/`
- Move `SymlinkKind` enum to new file

**Files:** `file_structure/symlink_store.rs` (create), `file_structure.rs` (add mod)

**Review loop:** Round 1: quality review. No architecture review (≤3 files).

---

## Plan 5: FileStructure Migration (Big Bang)

**Objective:** Replace FileStructure's 4 sub-stores with 6 new sub-stores. Update all consumers. Remove old stores.
**Artifact:** `.claude/artifacts/plan_file_structure_migration.md`
**Complexity:** L | **Risk:** High | **Acceptance tests:** Yes
**Dependencies:** Plans 1, 2, 3, 4

**Scope:**
- Change `FileStructure` fields: remove `objects`, `index`, `installs` → add `blobs`, `layers`, `packages`, `tags`, `symlinks`
- Update every consumer (verified list):
  - `reference_manager.rs` — `objects.*` → `packages.*`
  - `tasks/common.rs`, `find.rs`, `find_symlink.rs`, `install.rs`, `uninstall.rs`, `deselect.rs`, `pull.rs`, `resolve.rs` — store field renames
  - `garbage_collection/*.rs` — `objects.list_all()` → `packages.list_all()`
  - `profile/snapshot.rs` — `objects.path()` → `packages.path()`
  - `oci/index/local_index.rs` — `IndexStore` → `TagStore` (tags only; manifest migration in Plan 7)
  - CLI `context.rs` — FileStructure construction
  - All unit test fixtures
- Delete `object_store.rs`, `index_store.rs`, `install_store.rs`

**Files:** ~20 files across `file_structure/`, `package_manager/tasks/`, `oci/index/`, `profile/`, CLI

**Review loop:**
- Round 1: `worker-reviewer` (quality + spec-compliance), `worker-architect` (verify migration completeness against ADR, no missed consumers)
- Round 2+: re-run only perspectives with findings. Max 3 rounds.

**Doc impact:** Spawn `worker-doc-reviewer` to identify all docs referencing old paths/store names.

---

## Plan 6: ReferenceManager Consolidated refs/

**Objective:** Adapt ReferenceManager to `refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/`.
**Artifact:** `.claude/artifacts/plan_reference_manager.md`
**Complexity:** M | **Risk:** Medium | **Acceptance tests:** Yes
**Dependencies:** Plan 5

**Scope:**
- `link()` → back-ref in `refs/symlinks/` (was `refs/`)
- `link_dependency()` → forward-ref in `refs/deps/` (was `deps/`)
- New: `link_layer()` → ref in `refs/layers/`
- New: `link_blob()` → ref in `refs/blobs/`
- `unlink()` / `unlink_dependency()` adapt to new paths
- `broken_refs()` checks `refs/symlinks/` only

**Files:** `reference_manager.rs` (update)

**Review loop:** Round 1: quality + spec-compliance (verify ref naming matches ADR D4, verify back-ref/forward-ref direction).

---

## Plan 7: LocalIndex Reads from BlobStore

**Objective:** LocalIndex uses TagStore for tags and BlobStore for cached manifests.
**Artifact:** `.claude/artifacts/plan_local_index.md`
**Complexity:** M | **Risk:** Medium | **Acceptance tests:** Yes
**Dependencies:** Plan 5

**Scope:**
- `LocalIndex` fields: replace `IndexStore` with `TagStore` + `BlobStore`
- `get_tags()` / `persist_tags()` → `TagStore::tags()` paths
- `get_manifest()` → read from `BlobStore::data()` (JSON manifest)
- `update_manifest()` → write to `BlobStore` path + digest file
- `index update` → tags-only (remove manifest fetching from `sync_tag()`)
- Update `LocalIndex::Config` and CLI `context.rs`

**Files:** `oci/index/local_index.rs`, `local_index/cache.rs`, `local_index/config.rs`, CLI `context.rs`

**Review loop:** Round 1: quality review. Spawn `worker-doc-reviewer` for `subsystem-oci.md`.

---

## Plan 8: Pull Pipeline Three-Tier Storage

**Objective:** Pull flow stores manifests in blobs/, extracts layers to layers/, assembles packages with directory symlinks.
**Artifact:** `.claude/artifacts/plan_pull_pipeline.md`
**Complexity:** L | **Risk:** High | **Acceptance tests:** Yes
**Dependencies:** Plans 5, 6, 7

**Scope:**
- **Surface full resolution chain**: `resolve()` must return both the image index digest (if multi-platform) and the platform-specific manifest digest. Currently the image index digest is discarded inside `select()`. The pull pipeline needs both to create proper blob refs.
- Fetch manifest/index → store in `blobs/` (with digest file)
- For each layer: check `layers/` exists → skip; else fetch, extract, write digest (parallel per layer)
- Assembly per ADR D6: single-layer → symlink `content/`; multi-layer → real dirs at shared parents, per-subtree symlinks
- Write `manifest.json` into package dir
- **Create `refs/blobs/` forward-refs** from package to image index blob + manifest blob. This fixes the current gap where blobs are cached but unreferenced — enabling GC to properly collect orphaned blobs without the `CasTier::Blob` skip in `unreachable_objects`.
- **Create `refs/layers/` forward-refs** from package to each extracted layer
- Create `refs/deps/` via ReferenceManager (already working)
- Write `metadata.json`, `resolve.json`, `install.json`, `digest`
- **Remove blob skip from GC** `unreachable_objects` — once blob refs are wired, BFS determines reachability correctly for all tiers
- **Refactor ReferenceManager**: Introduce `PackageRefs` struct that abstracts ref directory management for a specific package. Currently `pull.rs` hardcodes path knowledge (e.g., `temp_dir.join("refs").join("deps")`). The new API: `reference_manager.for_package(content_path)` returns a `PackageRefs` with methods like `link_dep()`, `link_layer()`, `link_blob()`, `deps_dir()` — encapsulating all ref directory structure so consumers don't leak path internals.

**Files:** `tasks/pull.rs` (major rewrite), `tasks/common.rs`, `tasks/install.rs`, `reference_manager.rs` (PackageRefs abstraction), `oci/index.rs` or `resolve.rs` (surface index digest)

**Review loop:**
- Round 1: `worker-reviewer` (quality + security), `worker-architect` (verify pull flow matches ADR install flow, verify assembly strategy matches D6)
- Round 2+: re-run findings. Max 3 rounds.

**Doc impact:** Spawn `worker-doc-reviewer` for `subsystem-package-manager.md`.

---

## Plan 9: GC Three-Tier Reachability

**Objective:** GC walks all three tiers with CasTier-aware nodes.
**Artifact:** `.claude/artifacts/plan_gc_three_tier.md`
**Complexity:** M | **Risk:** Medium | **Acceptance tests:** Yes
**Dependencies:** Plans 5, 6, 8

**Scope:**
- `CasNode { path, tier }` replaces raw `PathBuf` in graph
- `build()` walks `packages.list_all()`, `layers.list_all()`, `blobs.list_all()`
- Roots: packages with live `refs/symlinks/` + profile content-mode
- BFS: `refs/deps/` → packages, `refs/layers/` → layers, `refs/blobs/` → blobs
- Tier-aware deletion + reporting
- Blob retention gap (#35): skip unreferenced blobs (conservative)

**Files:** `garbage_collection/reachability_graph.rs`, `garbage_collection.rs`, `clean.rs`

**Review loop:**
- Round 1: `worker-reviewer` (quality + spec-compliance), `worker-architect` (verify GC correctness — no reachable entry deleted, no unreachable entry retained except blobs per #35)
- Round 2+: re-run findings. Max 3 rounds.

---

## Plan 10: Documentation and Cleanup

**Objective:** Update all docs, finalize ADR, clean dead code.
**Artifact:** `.claude/artifacts/plan_documentation.md`
**Complexity:** S | **Risk:** Low | **Acceptance tests:** No
**Dependencies:** All previous plans

**Scope:**
- Rewrite `subsystem-file-structure.md` for 6-store layout
- Update `architecture-principles.md` Key Concepts + ADR index
- Update `product-context.md` Three-Store Architecture section
- Update `subsystem-oci.md` LocalIndex section
- Update `subsystem-package-manager.md` store references
- Update `website/src/docs/user-guide.md` if applicable
- ADR status: Proposed → Accepted
- Memory file updates

**Review loop:** `worker-doc-writer` produces drafts, `worker-doc-reviewer` validates consistency with code.

---

## Execution Sequence

| Order | Plan | Parallel? | Team |
|-------|------|-----------|------|
| 1a | Plan 1 (CAS helpers) | Yes (with 1b, 1c) | builder + tester + reviewer |
| 1b | Plan 3 (TagStore) | Yes (with 1a, 1c) | builder + tester + reviewer |
| 1c | Plan 4 (SymlinkStore) | Yes (with 1a, 1b) | builder + tester + reviewer |
| 2 | Plan 2 (3 CAS stores) | After 1a | builder + tester + reviewer |
| 3 | Plan 5 (Big Bang) | After 1a-2 | builder + tester + reviewer + **architect** + doc-reviewer |
| 4 | Plan 6 (RefManager) | After 3 | builder + tester + reviewer |
| 5 | Plan 7 (LocalIndex) | After 3 | builder + tester + reviewer + doc-reviewer |
| 6 | Plan 8 (Pull pipeline) | After 4, 5 | builder + tester + reviewer + **architect** + doc-reviewer |
| 7 | Plan 9 (GC) | After 6 | builder + tester + reviewer + **architect** |
| 8 | Plan 10 (Docs) | After 7 | doc-writer + doc-reviewer |

## Verification

After each commit: `task verify` (fmt + clippy + build + unit + acceptance tests).

After Plan 9 (full implementation complete):
- `ocx install cmake:3.28` → verify three-tier layout on disk
- `ocx exec cmake:3.28 -- cmake --version` → verify content works through symlinks
- `ocx clean --dry-run` → verify tier-aware reporting
- `ls -la ~/.ocx/` → confirm `blobs/`, `layers/`, `packages/`, `tags/`, `symlinks/`, `temp/`
- Verify no `objects/`, `index/`, `installs/` directories exist
