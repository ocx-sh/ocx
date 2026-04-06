# Plan: Hardlink-Based Package Assembly

## Context

Plan 8b introduced layer extraction + directory-symlink assembly for three-tier CAS storage. Implementation revealed real complexity cost: `package_dir_for_content` needed filename-based dispatch to avoid following `content/` → `layers/`, test helpers switched from `.resolve()` to `.readlink()`, and `refs/layers/`/`refs/blobs/` targets had to match an implicit "point to child inside entry" convention.

Research and user pushback converged on a better mechanism: **hardlinks** (pnpm/uv pattern). Hardlinks give us layer-level dedup without the symlink-dereferencing semantics that break cross-layer relative RPATH resolution, and they let `packages/{digest}/content/` be a real directory again — eliminating the entire special-casing chain.

**Scope:** Replace `content/` → `layers/.../content/` symlink with a walker that mirrors the layer's directory structure into the package temp dir and creates hardlinks for files. Preserve symlinks within layers verbatim (they're typically versioned shared library chains). Add a new `hardlink.rs` module mirroring the existing `symlink.rs` template. Symlinks remain for GC refs (`refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/`) — the existing symlink module stays in place.

**Persisted as:** `.claude/artifacts/plan_hardlink_assembly.md`
**Supersedes:** Parts of `plan_three_tier_cas.md` Plan 8b (the symlink-based assembly decision)
**ADR impact:** D6 revision — see sub-plan 1

## Execution Model

Each sub-plan is one conventional commit executed via `/swarm-execute` or an equivalent manual orchestration. All Rust agents (builder + reviewer) must read `.claude/rules/rust-quality.md` before writing or reviewing code.

**Branch:** `goat` (current worktree)
**Quality gate:** `task verify` must pass after each commit

### Per-Plan Execution Protocol

```
1. Stub        → worker-builder (focus: stubbing)         Gate: cargo check
2. Verify Arch → worker-reviewer (focus: spec-compliance) Gate: reviewer passes
3. Specify     → N × worker-tester (focus: specification) Gate: tests compile, fail against stubs
4. Implement   → worker-builder (focus: implementation)   Gate: task verify passes
5. Review Loop → N × worker-reviewer (quality + security) Max 3 rounds
                 + worker-architect (high-risk plans)
6. Commit      → conventional commit message
```

**Multi-tester specification phase:** For sub-plans with many edge cases, the specify step fans out across multiple testers in parallel, each owning a category of tests. Testers work on distinct test files or distinct test modules within a file to avoid merge conflicts.

**Multi-reviewer review phase:** Each review round launches reviewers in parallel across perspectives: quality, security, spec-compliance. Findings are classified as actionable (fix automatically) or deferred (report to human).

## Decision Summary

**What changes:**
- `packages/{P}/content/` becomes a real directory containing real subdirectories and hardlinked files (from `layers/{A}/content/`).
- Internal symlinks within a layer (e.g., `libfoo.so` → `libfoo.so.1`) are recreated in the package with the same target string.
- On Windows, file symlinks fall back to copy if privilege is denied (no Developer Mode).
- Cross-volume hardlink attempts surface as a clear `CrossesDevices` error at install time (no silent fallback). `$OCX_HOME` is required to live on a single volume — already implied by the pre-existing `temp → packages/` atomic rename.
- `package_dir_for_content()` reverts to simple `parent()` logic — no filename dispatch.
- Test helpers stop using `.readlink()` / `.resolve()` workarounds.

**What stays:**
- Three-tier CAS (blobs/, layers/, packages/)
- Layer extraction to `layers/{digest}/content/` (unchanged — the race-safe extract-then-rename pattern stays)
- `refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/` (all symlinks; GC uses `read_link`)
- `symlink.rs` module (still used by ReferenceManager and install candidate/current links)
- The entire blob caching and forward-ref wiring from Plan 8b

**What leaves:**
- The `content/` → `layers/.../content/` directory symlink
- The filename-based dispatch in `package_dir_for_content()`
- Test workarounds using `.readlink()`/`.resolve()`

## Edge Cases (Enumerated)

All must have corresponding tests. The Testing Strategy is the test specification.

### Assembly correctness (unit tests on walker)

| # | Case | Expected behavior |
|---|------|-------------------|
| E1 | Empty layer content/ | Package content/ is empty directory |
| E2 | Flat layer (files only, no subdirs) | All files hardlinked at root of package content/ |
| E3 | Nested layer with subdirectories | All subdirs recreated, all files hardlinked |
| E4 | Deeply nested (≥5 levels) | Recursion works without stack overflow |
| E5 | Relative symlink within layer (same dir) | Recreated in package with identical target string |
| E6 | Relative symlink within layer (cross-dir, `../lib/foo`) | Recreated verbatim; resolves correctly via mirrored structure |
| E7 | Absolute symlink within layer | Recreated verbatim (even if pointing outside package) |
| E8 | Chained symlinks (`a` → `b` → `c` → real file) | All three entries recreated; chain resolves in package |
| E9 | Broken symlink in layer | Recreated verbatim; still broken in package (publisher bug, not our fix) |
| E10 | File with special permissions (executable) | Hardlink inherits permissions (same inode) |
| E11 | Empty subdirectory in layer | Empty subdirectory in package |

### Hardlink module (unit tests)

| # | Case | Expected behavior |
|---|------|-------------------|
| H1 | `create()` on same filesystem | Hardlink created, both paths resolve to same inode |
| H2 | `create()` parent dir doesn't exist | Parent created recursively |
| H3 | `create()` target already exists | Error `AlreadyExists` |
| H4 | `create()` source doesn't exist | Error surfacing IO error with path context |
| H5 | `create()` across filesystems | Surfaces `CrossesDevices` as a clear error (no fallback — `$OCX_HOME` must be single-volume) |
| H6 | `update()` overwrites existing hardlink | Old link removed, new hardlink in place |
| H7 | Hardlinked file shares inode (verify via metadata) | `dev+ino` matches between source and hardlink |
| H8 | Ad-hoc signed binary retains signature through hardlink (macOS only) | `codesign -v` passes on both paths |

### Windows-specific (conditional-compiled tests)

| # | Case | Expected behavior |
|---|------|-------------------|
| W1 | File symlink in layer, Developer Mode off | Walker falls back to `tokio::fs::copy` of the dereferenced target (symlink→file conversion, not a hardlink fallback) |
| W2 | Cross-volume attempt | `CrossesDevices` detected, surfaced as a clear error (manual-only test, needs a second NTFS volume) |
| W3 | Hardlink on NTFS | Works — same `FileId` as source |

### Concurrency (unit tests)

| # | Case | Expected behavior |
|---|------|-------------------|
| C1 | Two packages assemble from same layer concurrently | Both succeed, independent package temp dirs |
| C2 | Assembly races with layer GC | Layer extraction is idempotent; hardlinks remain valid even if layer dir vanishes (inodes stay alive) |

### Integration (acceptance tests)

| # | Case | Expected behavior |
|---|------|-------------------|
| I1 | Install package, exec binary with `@loader_path` / `$ORIGIN` relative RPATH | Binary finds its siblings inside the package (not the layer) |
| I2 | Install package with versioned libraries (libfoo.so → libfoo.so.1) | Symlink chain works after assembly |
| I3 | `find` returns the package content path (matches install candidate via `readlink`) | Path equality works without `.resolve()` |
| I4 | `ocx clean --dry-run` reports correct count of packages + layers | Count unchanged from current Plan 8b behavior |
| I5 | Purge package → shared dep layer survives because other package hardlinks its files | Filesystem-level reachability works |
| I6 | `du -sh` on two packages sharing a layer reports dedup (roughly one layer's size, not two) | Inode-level dedup confirmed |

## Sub-plans

### Sub-plan 1: Revise ADR D6

**Objective:** Update ADR to reflect the hardlink-based assembly decision.
**Complexity:** S | **Risk:** Low | **Parallelizable with:** Sub-plan 2

**Scope:**
- Rewrite D6 "Directory-level symlinks for layer assembly" → "Hardlink-based assembly with symlink preservation"
- Update alternatives considered (directory symlinks move from chosen to rejected with rationale)
- Document Windows privilege fallback (copy)
- Document cross-volume fallback (copy)
- Document `$OCX_HOME` single-volume constraint
- Add new invariant: "No file in `packages/{P}/content/` is a symlink to `layers/`"
- Preserve publisher constraint discussion (now moot for RPATH, but keep the `@loader_path` analysis for posterity)
- Update the "Install Flow" section in the ADR Technical Details

**Files:** `.claude/artifacts/adr_three_tier_cas_storage.md`

**Agents:** `worker-architect` (writes revision), `worker-doc-reviewer` (validates consistency)

---

### Sub-plan 2: New `hardlink.rs` module

**Objective:** Introduce a cross-platform hardlink module mirroring `symlink.rs` structure.
**Complexity:** M | **Risk:** Low | **Parallelizable with:** Sub-plan 1

**Scope:**
- New file `crates/ocx_lib/src/hardlink.rs`
- Public API (minimal — no copy fallback):
  - `create(source: &Path, link: &Path) -> Result<()>` — creates a hardlink; creates parent dirs; fails if target exists or filesystems differ
  - `update(source: &Path, link: &Path) -> Result<()>` — creates or replaces atomically
- **No `create_or_copy`, no `LinkMethod` enum, no silent copy fallback.** `$OCX_HOME` is already required to live on a single volume (the pre-existing `temp → packages/` atomic rename demands it). Cross-device surfaces as a clear error at install time — operators fix their layout, callers get a clean contract.
- Private helpers with platform-conditional compilation where necessary
- All errors wrapped via `Error::InternalFile(path, io_error)`
- Tests mirror symlink.rs structure:
  - `setup()` helper
  - `// ── create ──` section
  - `// ── update ──` section
  - `// ── hardlinks survive rename ──` section — locks in the temp→packages inode-preservation invariant
  - `#[cfg(windows)] mod windows { ... }` for NTFS-specific tests
  - Conditional `#[cfg(unix)]` test for inode sharing
- Register module in `lib.rs`
- Clean up `codesign.rs:475` to use `hardlink::create` (migrate the test's direct `std::fs::hard_link` call)

**Files:** `crates/ocx_lib/src/hardlink.rs` (create), `crates/ocx_lib/src/lib.rs` (register), `crates/ocx_lib/src/codesign.rs` (consumer update)

**Contract:**
```rust
pub fn create(source: &Path, link: &Path) -> Result<()>;
pub fn update(source: &Path, link: &Path) -> Result<()>;
```

**Single-volume invariant rationale:**
- `temp/` and `packages/` are already same-fs (pre-existing atomic-rename requirement in `pull.rs::move_temp_to_object_store`).
- `layers/`, `blobs/`, and `temp/` are all direct subdirs of `$OCX_HOME`, so they are same-fs in every realistic deployment.
- A user who bind-mounts one tier to a separate volume gets a loud error at install time (`CrossesDevices`), not a silent dedup regression.
- `rename(2)` is inode-preserving on POSIX and `MoveFileEx` preserves `FileId` on same-volume NTFS, so hardlinks created in `temp/` survive the atomic rename into `packages/` — the invariant is verified by a unit test (`hardlink_survives_directory_rename`).

**Testing Strategy (specify phase fans out):**
- **Tester A** (happy path): H1, H2, H3, H6, H7
- **Tester B** (error paths): H4, H5 (Linux: self-skipping cross-device error test using `/dev/shm` vs `/tmp`)
- **Tester C** (platform-specific): H8 (macOS inode proxy), W1/W2/W3 (Windows conditional)
- Cross-cutting: `hardlink_survives_directory_rename` — verifies the temp→packages invariant with a unit test

**Review:**
- **Reviewer A** (quality): API surface matches symlink template; error handling; naming
- **Reviewer B** (security): TOCTOU between `remove` and `create` in `update()`; symlink attack vectors
- **Reviewer C** (spec-compliance): Tests cover the contract above

---

### Sub-plan 3: Assembly walker utility

**Objective:** Create a utility that walks a layer's `content/` tree and mirrors it into a target directory, hardlinking files and preserving symlinks.
**Complexity:** M | **Risk:** Medium | **Dependencies:** Sub-plan 2

**Scope:**
- New free function or `struct` in `crates/ocx_lib/src/utility/fs/` (placement to be confirmed during stub phase; candidate name: `assemble.rs`)
- Function contract:
  ```rust
  pub async fn assemble_from_layer(
      source_content: &Path,       // layers/{digest}/content/
      dest_content: &Path,         // temp dir's content/ being built
  ) -> Result<AssemblyStats>
  ```
- `AssemblyStats` struct returned for observability:
  ```rust
  pub struct AssemblyStats {
      pub files_hardlinked: usize,
      pub files_copied: usize,      // Windows-only: file-symlink copy count (see W1)
      pub dirs_created: usize,
      pub symlinks_recreated: usize,
      pub bytes_hardlinked: u64,
      pub bytes_copied: u64,        // Windows-only: bytes copied from symlink targets
  }
  ```
- Walks `source_content` depth-first
- For each entry:
  - **Directory**: create a real directory at the corresponding path in `dest_content`
  - **Regular file**: `hardlink::create(source, dest)` — any error propagates, including `CrossesDevices`
  - **Symlink**: `read_link(source)`, then create a new symlink at `dest` with the same target string (use existing `crate::symlink::create`). On Windows, if symlink creation fails with permission error, fall back to `std::fs::copy` with the symlink's resolved target (one-level dereference only).
- Async where possible (tokio::fs), sync where necessary (hardlink is sync in stdlib — use `spawn_blocking` if it dominates latency; measure first)
- No use of `DirWalker` — the existing walker classifies for GC purposes and doesn't fit this read+write pipeline cleanly. A purpose-built walker is simpler than fighting the classify contract.

**Files:** `crates/ocx_lib/src/utility/fs/assemble.rs` (create), `crates/ocx_lib/src/utility/fs.rs` or `mod.rs` (register)

**Testing Strategy (specify phase fans out):**
- **Tester A** (structure): E1, E2, E3, E4, E11 — basic directory mirroring
- **Tester B** (symlinks): E5, E6, E7, E8, E9 — symlink preservation
- **Tester C** (file properties): E10 — permissions, and verify hardlink inode sharing
- **Tester D** (error paths): source path doesn't exist, dest parent doesn't exist, dest already populated (should error or handle gracefully per contract)

**Review:**
- **Reviewer A** (quality): Async patterns, error propagation, walker structure
- **Reviewer B** (security): Path traversal via malicious symlinks (e.g., a layer with a symlink `foo` → `../../../etc/passwd`); TOCTOU on directory creation
- **Reviewer C** (performance): Measure latency on a realistic layer (~1000 files); confirm hardlink is fast enough without `spawn_blocking`
- **worker-architect**: Verify walker design aligns with ADR assembly invariants

---

### Sub-plan 4: Wire walker into pull pipeline

**Objective:** Replace the `content/` symlink creation in `pull.rs` with a call to the assembly walker.
**Complexity:** S | **Risk:** Low | **Dependencies:** Sub-plans 2, 3

**Scope:**
- In `setup_owned()`, replace the `crate::symlink::create(&layer_content, pkg.content())` call with:
  ```rust
  let layer_content = fs.layers.content(pinned.registry(), &layer_digests[0]);
  crate::utility::fs::assemble_from_layer(&layer_content, &pkg.content()).await
      .map_err(PackageErrorKind::Internal)?;
  ```
- For multi-layer (future), iterate over `layer_digests` and call `assemble_from_layer` for each, with merging logic — but keep as `unimplemented!()` for now with a clear comment. Current packages are single-layer in practice; multi-layer merging is a separate follow-up (#22).
- Revert `package_dir_for_content()` in `package_store.rs` to the pre-plan-8b form: simple `dunce::canonicalize(content_path).parent()`. The filename dispatch becomes unnecessary because `content/` is a real directory again.

**Files:** `crates/ocx_lib/src/package_manager/tasks/pull.rs`, `crates/ocx_lib/src/file_structure/package_store.rs`

**Testing Strategy:**
- Existing unit tests for `package_store::package_dir_for_content` should continue to pass (with the old assertions restored)
- No new unit tests; acceptance tests in sub-plan 6 cover the integration

**Review:**
- **Reviewer A** (quality): The `unimplemented!()` for multi-layer has a TODO pointing to #22
- **Reviewer B** (spec-compliance): Revert is complete — no stray filename dispatch remaining

---

### Sub-plan 5: Revert test workaround helpers

**Objective:** Simplify test helpers that used `.readlink()` / `.resolve()` as workarounds for the symlinked content/.
**Complexity:** S | **Risk:** Low | **Dependencies:** Sub-plan 4

**Scope:**
- `test/tests/test_dependencies.py::_find_content_path` — no change needed (already returns `Path(result[pkg.short])` from last session)
- `test/tests/test_dependencies.py::_list_dep_targets` — replace `readlink()` back to `resolve()` (since dep symlink targets point to real directories again, `.resolve()` is the idiomatic choice)
- `test/tests/test_purge.py::_find_content_path` — same as test_dependencies
- `test/tests/test_find.py::test_find_returns_content_path` — the `candidate.readlink()` comparison can go back to `candidate.resolve()` (the resolve now stops at the package content/ dir)
- `test/tests/test_package_lifecycle.py::test_create_push_install_find` — same
- `test/tests/test_dependencies.py::test_clean_dry_run_transitive_chain` — the count stays at 6 (3 packages + 3 layers) because we still have layers as separate CAS entries. No change needed.

**Files:** ~5 test files in `test/tests/`

**Testing Strategy:** Running the existing tests IS the validation.

**Review:**
- **Reviewer A** (quality): Comments explain that the helpers are simple again; no stale workaround comments left
- **Reviewer B** (spec-compliance): All workarounds from Plan 8b fully reverted

---

### Sub-plan 6: Edge case acceptance tests

**Objective:** Add acceptance tests covering real-world behavior that's hard to test at the unit level.
**Complexity:** M | **Risk:** Medium | **Dependencies:** Sub-plans 4, 5

**Scope:**
- New test file `test/tests/test_assembly.py` containing:
  - **I1: Relative RPATH binary** — create a test package with a binary using `$ORIGIN/../lib/libfoo.so`, install it, exec it, assert it finds libfoo in the package (not in a layer path). This is the key test proving hardlinks fix the cross-layer RPATH problem.
  - **I2: Versioned shared library chain** — package contains `libfoo.so` → `libfoo.so.1` → `libfoo.so.1.2.3`. Install it, verify the chain works after assembly. Read the symlinks and confirm targets are preserved verbatim.
  - **I5: Layer dedup verified via inode sharing** — install two packages that happen to share a layer (construct this artificially). Verify that a specific file in both packages has the same inode (Python `os.stat().st_ino`).
  - **I6: du reports dedup correctly** — roughly — install two packages sharing a layer, compare `du -sL` (logical) vs `du -s` (physical). The physical size should be roughly one layer's worth.
- Extensions to existing tests:
  - `test_purge.py::test_purge_preserves_shared_layer` — new test: purge package A, assert package B's content (hardlinked from the same layer) still resolves and works
  - `test_find.py::test_find_returns_package_path_not_layer` — new assertion: `.resolve()` on the find result doesn't enter `layers/`

**Testing Strategy (specify phase fans out):**
- **Tester A** (binaries with RPATH): I1, I2 — these require building small C programs or using a preexisting binary
- **Tester B** (inode and disk dedup): I5, I6 — uses `os.stat()` and `du` subprocess
- **Tester C** (GC and find integration): `test_purge_preserves_shared_layer`, `test_find_returns_package_path_not_layer`

**Review:**
- **Reviewer A** (quality): Tests are deterministic, use existing fixtures, follow naming conventions in `subsystem-tests.md`
- **Reviewer B** (spec-compliance): Every edge case in the table above has a corresponding test

---

### Sub-plan 7: Documentation

**Objective:** Update subsystem docs to reflect the hardlink assembly.
**Complexity:** S | **Risk:** Low | **Dependencies:** Sub-plans 1-6

**Scope:**
- `.claude/rules/subsystem-file-structure.md` — update three-store description with hardlink assembly; add `hardlink.rs` alongside `symlink.rs` in the module map
- `.claude/rules/subsystem-package-manager.md` — update any references to the symlink-based assembly in the pull flow description
- Optional: `website/src/docs/` — update user-guide if it describes the store layout

**Files:** `.claude/rules/subsystem-file-structure.md`, `.claude/rules/subsystem-package-manager.md`, possibly website docs

**Agents:** `worker-doc-writer` (focus: reference), `worker-doc-reviewer` (final check)

## Dependency Graph

```
Sub-plan 1 (ADR revision) ──┐
                            │
Sub-plan 2 (hardlink mod) ──┤──► Sub-plan 3 (walker) ──► Sub-plan 4 (wire) ──► Sub-plan 5 (revert tests) ──► Sub-plan 6 (edge tests) ──► Sub-plan 7 (docs)
```

- Sub-plans 1 and 2 are fully parallel (different files, no shared state)
- Sub-plans 3-7 are sequential (each builds on the previous)
- Sub-plan 5 can start as soon as sub-plan 4 is committed; sub-plan 6 can start in parallel with 5 if the test authors work on different files

## Execution Sequence

| Order | Sub-plan | Parallelizable? | Primary Team |
|-------|----------|-----------------|--------------|
| 1a | 1 (ADR revision) | Yes (with 1b) | architect + doc-reviewer |
| 1b | 2 (hardlink module) | Yes (with 1a) | builder + 3 testers + 3 reviewers |
| 2 | 3 (walker) | After 1b | builder + 4 testers + 3 reviewers + architect |
| 3 | 4 (wire into pull.rs) | After 2 | builder + 2 reviewers |
| 4 | 5 (revert workarounds) | Parallel with 5a | builder + 2 reviewers |
| 5 | 6 (edge acceptance tests) | Parallel with 4 | 3 testers + 2 reviewers |
| 6 | 7 (docs) | After 5 | doc-writer + doc-reviewer |

## Per-Plan Review Loop Configuration

Every sub-plan uses the bounded iterative review loop from `.claude/rules/feature-workflow.md`:

- **Round 1:** All reviewers run in parallel
- **Round 2+:** Only re-run perspectives that produced actionable findings
- **Max rounds:** 3
- **Classification:** Each finding is marked actionable (fix automatically) or deferred (report to human)
- **Exit condition:** No actionable findings remain in the last round

## Verification

After each sub-plan commit: `task verify` (fmt + clippy + build + unit + acceptance tests).

After sub-plan 6 (implementation complete):
- `ocx install cmake:3.28` → verify `packages/.../content/` is a real directory with hardlinked files (check via `stat`)
- `stat -c %i packages/.../content/bin/cmake layers/.../content/bin/cmake` — confirm same inode
- `ls -lL ~/.ocx/packages/.../content/bin/` — no symlinks in package content (symlinks only inside packages if the original layer had them)
- `ls ~/.ocx/` — blobs/, layers/, packages/, tags/, symlinks/, temp/
- Optional: create a small C program with `$ORIGIN/../lib` RPATH, package and install, exec to verify RPATH resolution

## Open Questions for Execution

1. **Reflink tier?** Research suggests `reflink → hardlink → copy` is the pnpm pattern. Do we want the reflink tier in the first implementation, or defer as an optimization? **Recommendation: defer** — adds a dependency, and reflinks give identical semantics to hardlinks for our immutable-after-extraction use case. Add later if benchmarks justify it.

2. **Multi-layer merging?** The walker handles single-layer. Multi-layer packages need directory merging logic (layer A contributes `bin/`, layer B contributes `lib/`). **Recommendation: keep `unimplemented!()` with a comment pointing to #22 for now.** The single-layer case is what ships today; multi-layer is future work and needs its own conflict-resolution design.

3. **Concurrent assembly GC race?** If two packages assemble from the same layer while GC is running, can the layer vanish mid-walk? **Analysis:** GC holds no lock, but it only deletes unreferenced entries. A layer about to be hardlinked from is referenced (via the in-progress package's eventual `refs/layers/` — but that's created AFTER assembly). There's a small race window where the layer is extracted but no package has referenced it yet. **Mitigation options:** (a) Create `refs/layers/` entries before the walker runs (not after), (b) use a lease/grace period like containerd, (c) accept the race and retry the pull if the layer vanishes. **Recommendation for this plan: (a) — move `link_layers_in_temp` earlier in the pull flow so the refs exist before assembly.** Document this as an invariant.

## Risks

- **Hardlink walk latency** on layers with many small files (thousands). Mitigation: measure on a realistic layer during implementation; if slow, use `spawn_blocking` or batched `rayon` fan-out.
- **Cross-volume `$OCX_HOME`** (user bind-mounts one tier to a separate volume). Hardlink will fail with `CrossesDevices`, surfaced as a clear error. This is not new risk — the pre-existing `temp → packages/` atomic rename already requires same-fs, so this constraint is inherited rather than introduced. Mitigation: document the single-volume requirement in the user guide; the error message should point at it directly.
- **Windows Developer Mode** not enabled → all file symlinks in layers fall back to copy. Not a correctness issue, just inefficient for specific layer shapes. Mitigation: document the recommendation in the Windows installation guide.
- **macOS codesign breakage** if any step modifies a hardlinked binary after the initial signing. Mitigation: we only sign during extraction (layer creation), never after. Hardlink into package inherits the signature.

## Non-Goals

- **Reflink support** (deferred — see open questions)
- **Multi-layer assembly with directory merging** (deferred to #22)
- **Policy-based blob eviction** (still #35)
- **Removing blob skip from GC** (still requires #35 — index-cache blobs remain unreferenced by design)
- **`ocx store optimize` pass** to dedup identical files across non-shared layers (future feature)

## Review round (2026-04-11)

After the initial implementation the user flagged six concerns during
review. All of them were addressed in a single review-round pass tracked by
`plan_review_round_hardlink_assembly.md`:

1. **Lexical path helper duplication** (`normalize_path` + `validate_symlinks_in_dir` scattered across `archive.rs` and `symlink.rs`) — consolidated into `crates/ocx_lib/src/utility/fs/path.rs` (`lexical_normalize`, `escapes_root`, `validate_symlinks_in_dir`).
2. **Windows symlink→copy fallback** — dropped. The walker now returns `io::ErrorKind::Unsupported` for both file and directory layer symlinks on Windows. `AssemblyStats` no longer has `files_copied` / `bytes_copied`. Follow-up: file an issue for native Windows symlink support once Developer Mode / `SeCreateSymbolicLinkPrivilege` is available in CI.
3. **Transitional "Plan 8c" comments** — stripped from `package_store.rs` and `pull.rs`.
4. **Walker constants scattered** — `MAX_WALK_ENTRIES`, `MAX_STACK_DEPTH` (renamed to `MAX_WALK_DEPTH`) and `DEFAULT_CONCURRENCY` now live together at the top of `assemble.rs`. Decision: keep as `const` (compile-time), not env-var configurable.
5. **Sequential walker** — parallelized. The walker now spawns one directory-level task per `(src_dir, dest_dir)` pair through a semaphore-bounded `JoinSet`. `entries_seen` uses an `Arc<AtomicUsize>`; stats are return-and-summed (no shared mutex). New tests E14 (100×10 file fanout) and E15 (entry cap under parallelism) lock in the behaviour.
6. **Layer extraction atomicity gap** — addressed. Added `TempStore::layer_path` (keyed by `(registry, __layer__, digest)` with null-byte delimiters) and a new `extract_layer_atomic` helper in `pull.rs`. Each layer now goes through the same singleflight → find-plain → exclusive lock → post-lock recheck → atomic rename pipeline as package install, via a dedicated `LayerGroup = singleflight::Group<(String, Digest), ()>` (30-minute timeout). Two concurrent installs pulling packages that share a layer will now extract the layer exactly once in-process, while still tolerating cross-process races at the final rename.

Out of scope for this round and still deferred: multi-layer assembly
(issue #22), Windows layer symlink support.

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-04-10 | mherwig / Claude | Initial plan from hardlink-vs-symlink design discussion |
| 2026-04-11 | mherwig / Claude | Review round: path helper consolidation, Windows fallback dropped, parallel walker, layer singleflight |
