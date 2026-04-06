# Plan: Multi-Layer Assembly Walker

<!--
Implementation Plan
Filename: artifacts/plan_multi_layer_assembly.md
Owner: Builder (/builder)
Handoff to: Builder (/builder), QA Engineer (/qa-engineer)
Related Skills: builder, qa-engineer
-->

## Overview

**Status:** Draft
**Author:** Claude (swarm-plan)
**Date:** 2026-04-12
**Scope:** Small (1-3 days) | Two-Way Door
**GitHub Issue:** ocx-sh/ocx#22 (Multi-Layer Packages)
**Related ADR:** [adr_three_tier_cas_storage.md](./adr_three_tier_cas_storage.md)

## Objective

Add an `assemble_from_layers` function to the freestanding assembly walker module that merges multiple overlap-free OCI layers into a single package `content/` directory using a **single merged walk**. This is the assembly primitive that the pull pipeline will use for multi-layer packages (#22).

## Scope

### In Scope

- New `assemble_from_layers` public function in `utility/fs/assemble.rs`
- Merged directory walker (`process_directory_merged`) that reads entries from all contributing layers at each directory level
- Overlap detection (fail-on-overlap: two layers contributing the same non-directory entry is an error)
- Layer pruning on recursion (layers that don't contribute a subdirectory are dropped)
- Layer ordering preservation (array index = layer order, for error messages and future shadowing)
- Comprehensive unit tests for multi-layer assembly
- Re-export from `utility/fs.rs`

### Out of Scope

- Pull pipeline integration (remains gated at N=1 in `pull.rs` until #22 lands)
- Acceptance tests (no user-facing behavior change — the pull pipeline doesn't call this yet)
- Changing the existing `assemble_from_layer` single-layer function (stays as-is, optimized path)
- Whiteout / layer shadowing semantics (overlap-free only)

## Research

**Research artifact:** N/A (findings inline — too small for standalone artifact)

**Prior art:** Nix, Docker overlay2, and OverlayFS all apply layers sequentially, not via merged fan-in. The sequential approach works (the existing walker tolerates `AlreadyExists` on dirs), but relies on filesystem-level `EEXIST` for overlap detection — errors surface mid-walk after partial writes.

**Why merged walk instead:** The merged-walk approach reads all layers' entries for a given directory *before* writing anything, detecting overlaps in-memory, and then producing exactly one write per destination path. This is architecturally cleaner:
- Overlap errors are detected before any syscalls for that directory level
- No TOCTOU races on concurrent `create_dir` calls
- Layer pruning happens naturally (layers that don't contribute a subdirectory are dropped from recursion)
- Single entry counter and depth tracker across all layers

**BTreeMap vs k-way merge:** `readdir` order is inode-allocation order (not lexical), so k-way merge via `BinaryHeap` would require pre-sorting each layer's entries anyway. `BTreeMap<OsString, Vec<...>>` is the right choice — readable, naturally deduplicates by name, and the `entry()` API gives overlap detection in one pass. Memory per directory level is bounded by entry count (typically dozens to hundreds).

**`file_type()` performance:** On Linux (ext4/btrfs/xfs), `tokio::fs::DirEntry::file_type()` reads from the buffered `getdents64` result's `d_type` field — no additional `stat()` syscall. The `symlink::is_link()` check adds one `lstat()` per entry for Windows junction detection; acceptable overhead on Linux.

## Technical Approach

### Architecture Changes

```
Before (sequential, one call per layer):
  for layer in layers:
    assemble_from_layer(layer/content/, dest/content/)
    # TOCTOU races, no unified overlap detection

After (single merged walk):
  assemble_from_layers(&[layer_0/content/, layer_1/content/, ...], dest/content/)
    # At each directory level:
    #   1. Read entries from ALL contributing layers
    #   2. Merge by name, detect overlaps
    #   3. Files/symlinks: place from the single contributing layer
    #   4. Directories: recurse with only layers that have that subdir
```

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| Keep `assemble_from_layer` unchanged | Optimized for the common single-layer case (no BTreeMap overhead). Avoids touching 20+ existing tests. |
| Separate `process_directory_merged` function | The merged walk has fundamentally different logic (collect-then-process vs. stream-and-process). Sharing code via a unified function would add branching complexity for no benefit. |
| `BTreeMap<OsString, Vec<...>>` for entry merging | Sorted iteration gives deterministic output. Memory per directory level is bounded by entry count (typically dozens to hundreds, not millions). |
| `EntryKind` enum for classification | Classify entries once during collection, then pattern-match during processing. Avoids redundant `file_type()` syscalls. |
| Layer ordering via array index | Preserves publisher intent. With overlap-free semantics, order doesn't affect the result, but it provides deterministic error messages ("layer 0 and layer 2 both contribute `bin/tool`") and enables future shadowing if the invariant is relaxed. |
| Fail-on-overlap (not shadow/overwrite) | This is the #22 invariant: overlap-free layers. Two layers contributing the same file is a publisher error, surfaced at assembly time. |

## Component Contracts

### `assemble_from_layers`

```rust
/// Assembles multiple overlap-free layers into a single destination tree.
pub async fn assemble_from_layers(
    sources: &[&Path],   // ordered: layer[0] is base, layer[N] is top
    dest_content: &Path,  // created by walker if missing; parent must exist
) -> Result<AssemblyStats>
```

**Behavior:**
- Empty `sources` slice: returns `Ok(AssemblyStats::default())` (no-op)
- Single source: equivalent to `assemble_from_layer` (but uses merged-walk code path)
- Multiple sources: merged walk, overlap detection, layer pruning on recursion
- `dest_content` created if missing; parent must exist (same pre-condition as `assemble_from_layer`)
- On error, `dest_content` may be partially populated (caller should use a temp directory)

**Error cases:**
- `AssemblyError::LayerOverlap` — two layers contribute same non-directory entry
- `AssemblyError::LayerOverlap` — type mismatch (dir in one layer, file in another)
- `AssemblyError::SourceNotDirectory` — any source path is not a directory
- `AssemblyError::DestinationParentMissing` — `dest_content.parent()` doesn't exist
- `AssemblyError::EntryLimitExceeded` / `DepthExceeded` — resource caps
- `AssemblyError::WindowsSymlinksUnsupported` — symlink in any layer on Windows

### `EntryKind` (private)

```rust
enum EntryKind {
    Dir,
    File { size: u64 },
    Symlink,
    // No `Other` variant — FIFOs, sockets, char/block devices are skipped
    // during collection (matching existing process_directory behavior which
    // silently skips "other entry types" at line 438-440).
}
```

During entry collection, entries that are not dir/file/symlink are skipped silently (not inserted into the BTreeMap). This matches the existing single-layer walker's behavior.

### `MultiSpawnRequest` (private)

```rust
struct MultiSpawnRequest {
    src_dirs: Vec<PathBuf>,  // contributing layers for this directory
    dest_dir: PathBuf,
    depth: usize,
}
```

### `process_directory_merged` (private)

**Algorithm:**
1. Read entries from all `src_dirs` into `BTreeMap<OsString, Vec<(layer_index, PathBuf, EntryKind)>>`
2. Count entries against the atomic cap
3. For each unique entry name:
   - Partition contributors into dirs vs non-dirs
   - If >1 non-dir → `LayerOverlap` error
   - If mix of dir + non-dir → `LayerOverlap` error
   - Single non-dir → hardlink (file) or recreate (symlink) via existing `handle_symlink_entry`
   - Dirs only → `create_dir`, post `MultiSpawnRequest` with filtered `src_dirs`
4. Return per-directory `AssemblyStats`

## Execution Model

Follows the **Swarm Workflow** from `.claude/rules/workflow-feature.md` with contract-first TDD. Worker assignments per `.claude/rules/workflow-swarm.md`.

## Implementation Steps

> **Contract-First TDD**: Stub → Verify Architecture → Specify → Implement → Review-Fix Loop → Cross-Model Adversarial Pass → Commit.

### Phase 1: Stub

**Worker:** `worker-builder` (focus: `stubbing`)

- [ ] **Step 1.1:** Add `EntryKind` enum and `MultiSpawnRequest` struct
  - Files: `crates/ocx_lib/src/utility/fs/assemble.rs`
  - Public API: private types only

- [ ] **Step 1.2:** Add `assemble_from_layers` function stub
  - Files: `crates/ocx_lib/src/utility/fs/assemble.rs`
  - Public API: `pub async fn assemble_from_layers(sources: &[&Path], dest_content: &Path) -> Result<AssemblyStats>`
  - Body: `unimplemented!()`

- [ ] **Step 1.3:** Add `process_directory_merged` function stub
  - Files: `crates/ocx_lib/src/utility/fs/assemble.rs`
  - Public API: private async fn with same signature pattern as `process_directory`
  - Body: `unimplemented!()`

- [ ] **Step 1.4:** Add re-export
  - Files: `crates/ocx_lib/src/utility/fs.rs`
  - Public API: `pub use assemble::assemble_from_layers`

**Gate:** `cargo check` passes.

### Phase 2: Verify Architecture

**Worker:** `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`)

Review stubs against this design record. Verify:
- `assemble_from_layers` signature matches documented contract
- `EntryKind` covers all entry types the walker handles (Dir/File/Symlink, unknown types skipped)
- `MultiSpawnRequest` carries all data needed for recursion
- Existing code (`assemble_from_layer`, `process_directory`, `handle_symlink_entry`) is not modified

**Gate:** Reviewer passes. *This feature touches 2 files — review is lightweight.*

### Phase 3: Specify

**Worker:** `worker-tester` (focus: `specification`)

Write tests from the contracts above, NOT from the stubs. Tests must fail against stubs with `unimplemented!()`. All tests in `crates/ocx_lib/src/utility/fs/assemble.rs` (inline `#[cfg(test)]`).

Tests follow the existing convention: doc-comment per test explaining the invariant it locks in, organized by category.

#### Step 3.1: Boundary / degenerate inputs

| Test | Invariant |
|------|-----------|
| `ml_empty_sources_is_noop` | Empty `&[]` returns `AssemblyStats::default()`, does not create `dest_content` |
| `ml_single_source_matches_single_layer` | `assemble_from_layers(&[a], dest)` produces byte-identical tree to `assemble_from_layer(a, dest)` — validates API equivalence |
| `ml_creates_dest_if_missing` | `dest_content` absent but parent exists → walker creates it |
| `ml_empty_layers_produce_empty_dest` | Two layers, both with empty `content/` → dest exists but is empty, stats are zero |

#### Step 3.2: Two-layer merging — structure

| Test | Invariant |
|------|-----------|
| `ml_disjoint_files_flat` | Layer A: `a.txt`; Layer B: `b.txt` → both present in dest root |
| `ml_disjoint_subtrees` | Layer A: `lib/a.so`; Layer B: `bin/b` → disjoint subdirectories, no interleaving |
| `ml_shared_directory_merges_files` | Layer A: `bin/tool_a`; Layer B: `bin/tool_b` → shared `bin/` contains both files |
| `ml_deep_shared_tree` | Layer A: `a/b/c/x.txt`; Layer B: `a/b/c/y.txt` → 3-level deep shared directory tree merges correctly |
| `ml_mixed_shared_and_disjoint` | Layer A: `bin/a`, `lib/liba.so`; Layer B: `bin/b`, `share/doc.txt` → `bin/` merged, `lib/` and `share/` disjoint |
| `ml_layer_pruned_on_recursion` | Layer A: `lib/a.so`; Layer B: `bin/b` (no `lib/`) → `lib/` recurses with only layer A. Verify by checking layer B's files are NOT in `lib/` (obviously), but also that `dirs_created` count matches expectation |

#### Step 3.3: Three-layer merging

| Test | Invariant |
|------|-----------|
| `ml_three_layers_all_disjoint` | A: `a/`, B: `b/`, C: `c/` → three disjoint trees merged |
| `ml_three_layers_shared_root_dir` | A: `bin/a`; B: `bin/b`; C: `bin/c` → single `bin/` with three files |
| `ml_three_layers_partial_overlap` | A: `bin/a`, `lib/liba.so`; B: `bin/b`, `share/doc.txt`; C: `lib/libc.so`, `share/man.txt` → `bin/` has {a,b}, `lib/` has {liba,libc}, `share/` has {doc,man} |
| `ml_three_layers_progressive_pruning` | A: `a/b/c/x`; B: `a/b/y`; C: `a/z` → at depth `a/b/c/` only layer A contributes; at `a/b/` layers A+B; at `a/` all three |

#### Step 3.4: Hardlink + inode invariants (Unix, `#[cfg(unix)]`)

| Test | Invariant |
|------|-----------|
| `ml_every_file_shares_inode_with_source` | Two layers, verify every assembled file's `(dev, ino)` matches its layer source — the core correctness guarantee |
| `ml_inode_stable_across_layers` | Three layers contributing files to shared `bin/`. Each assembled file's inode matches its specific source layer, not any other layer |
| `ml_hardlink_survives_dest_rename` | After assembly into temp dir, rename to final location. All inodes preserved (hardlinks survive directory rename) |
| `ml_different_layers_different_inodes` | `bin/a` from layer A and `bin/b` from layer B share `bin/` but have different inodes (they're different files) |

#### Step 3.5: Symlinks across layers (Unix, `#[cfg(unix)]`)

| Test | Invariant |
|------|-----------|
| `ml_symlink_in_one_layer_files_in_another` | Layer A: `lib/libfoo.so → libfoo.so.1`; Layer B: `bin/tool` → symlink target preserved verbatim, hardlink in `bin/` correct |
| `ml_symlinks_in_both_layers_disjoint` | Layer A: `lib/libfoo.so → libfoo.so.1`; Layer B: `lib/libbar.so → libbar.so.2` → both symlinks present in shared `lib/`, both targets verbatim |
| `ml_relative_symlink_across_shared_dir` | Layer A: `lib/libfoo.so.1` (real file); Layer B: `lib/libfoo.so → libfoo.so.1` (symlink targeting A's file). NOT an overlap — different entry names (`libfoo.so.1` vs `libfoo.so`). Verify the symlink resolves correctly in assembled dest because both land in the same `lib/` |
| `ml_symlink_target_survives_temp_rename` | Assemble into temp, rename to final. Relative symlink target string is byte-identical after rename |

#### Step 3.6: Stats accumulation

| Test | Invariant |
|------|-----------|
| `ml_stats_sum_files_across_layers` | Layer A: 3 files; Layer B: 2 files → `files_hardlinked == 5` |
| `ml_stats_sum_bytes_across_layers` | Layer A: 100 bytes total; Layer B: 50 bytes total → `bytes_hardlinked == 150` |
| `ml_stats_count_dirs_correctly` | Layer A: `bin/`, `lib/`; Layer B: `bin/`, `share/` → shared `bin/` created once, `dirs_created == 3` (bin, lib, share) |
| `ml_stats_count_symlinks_across_layers` | Layer A: 1 symlink; Layer B: 2 symlinks (disjoint paths) → `symlinks_recreated == 3` |
| `ml_stats_zero_length_files_counted` | Zero-byte files are still counted in `files_hardlinked` but contribute 0 to `bytes_hardlinked` |

#### Step 3.7: Error paths — overlap detection

| Test | Invariant |
|------|-----------|
| `ml_file_overlap_two_layers` | A: `bin/tool`; B: `bin/tool` → `LayerOverlap` error, path contains `bin/tool` |
| `ml_file_overlap_three_layers` | A: `x`; B: (nothing); C: `x` → still detected even with non-contributing middle layer |
| `ml_symlink_overlap` | A: `lib/link → target_a`; B: `lib/link → target_b` → `LayerOverlap` error |
| `ml_type_mismatch_dir_vs_file` | A: `foo/` (dir with children); B: `foo` (regular file) → `LayerOverlap` error |
| `ml_type_mismatch_file_vs_dir` | A: `foo` (regular file); B: `foo/` (dir) → same error, reversed order |
| `ml_type_mismatch_dir_vs_symlink` | A: `foo/` (dir); B: `foo → target` (symlink) → `LayerOverlap` error |
| `ml_overlap_detected_before_writes` | A: `bin/a`, `shared/x`; B: `bin/b`, `shared/x` → error on `shared/x`, but `bin/` may or may not have been partially created (error is per-directory-task, not globally pre-checked). Document the partial-write behavior. |

#### Step 3.8: Error paths — pre-conditions

| Test | Invariant |
|------|-----------|
| `ml_source_not_existing` | One source path doesn't exist → error wrapping `NotFound` |
| `ml_source_is_file` | One source path is a regular file → `SourceNotDirectory` error |
| `ml_mixed_valid_invalid_sources` | First source valid, second doesn't exist → error (validates all sources before walking) |
| `ml_dest_parent_missing` | `dest_content.parent()` doesn't exist → `DestinationParentMissing` error |

#### Step 3.9: Resource limits

| Test | Invariant |
|------|-----------|
| `ml_entry_limit_across_layers` | Layer A: 5 files; Layer B: 5 files; cap set to 7 via `_with_cap` → `EntryLimitExceeded` (combined count crosses cap) |

#### Step 3.10: Concurrency stress

| Test | Invariant |
|------|-----------|
| `ml_wide_fanout_two_layers` | Layer A: 50 dirs × 5 files; Layer B: 50 dirs × 5 files (same dir names, different file names) → 50 shared dirs, 500 files total, all hardlinked correctly |
| `ml_deep_tree_two_layers` | Layer A: 20-level deep `a/b/c/.../x.txt`; Layer B: same 20-level deep path with `y.txt` → deep shared tree merges without stack overflow or depth error |

**Gate:** Tests compile and fail with `unimplemented!()`.

### Phase 4: Implement

**Worker:** `worker-builder` (focus: `implementation`)

- [ ] **Step 4.1:** Implement `assemble_from_layers` scheduler loop
  - Files: `crates/ocx_lib/src/utility/fs/assemble.rs`
  - Details: Same semaphore-bounded JoinSet pattern as `assemble_from_layer_with_cap`, but using `MultiSpawnRequest` with `src_dirs: Vec<PathBuf>`. Pre-conditions: validate all sources are directories, validate dest parent exists.

- [ ] **Step 4.2:** Implement `process_directory_merged`
  - Files: `crates/ocx_lib/src/utility/fs/assemble.rs`
  - Details: Collect entries from all src_dirs into BTreeMap, classify with `EntryKind`, partition dirs/non-dirs, detect overlaps, place files/symlinks, post subdirectory requests with pruned layer list. Reuse `handle_symlink_entry` and `hardlink::create` directly.

**Gate:** All 19 specification tests pass. `task verify` succeeds.

### Phase 5: Review-Fix Loop

**Diff-scoped, bounded iterative review (max 3 rounds).** Per `.claude/rules/workflow-feature.md` step 9.

**Round 1 — all perspectives (parallel):**
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-implementation`) — design record ↔ tests ↔ implementation traceability
- `worker-reviewer` (focus: `quality`) — code review checklist: naming, style, patterns, Rust quality rules
- `worker-reviewer` (focus: `security`) — symlink validation, path traversal, resource exhaustion caps

Each reviewer classifies findings as:
- **Actionable** — `worker-builder` fixes automatically
- **Deferred** — needs human judgment, reported in summary

**Round 2+ (selective):** Re-run only perspectives that had actionable findings. Loop exits when no actionable findings remain or after 3 rounds.

**Gate:** No actionable findings remain.

### Phase 6: Cross-Model Adversarial Pass

Per `.claude/rules/workflow-feature.md` step 10.

Single Codex adversarial review against the full diff. Actionable findings fold into one final `worker-builder` pass. Deferred findings go to the completion summary. One-shot — no looping. Skipped gracefully if Codex is unavailable.

**Gate:** Adversarial pass complete (or skipped).

### Phase 7: Commit

All changes committed on feature branch with conventional commit message. Deferred findings from review-fix loop and adversarial pass printed as summary. Human decides when to push.

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `crates/ocx_lib/src/utility/fs/assemble.rs` | Modify | Add `assemble_from_layers`, `process_directory_merged`, `EntryKind`, `MultiSpawnRequest`, 41 tests across 10 categories |
| `crates/ocx_lib/src/utility/fs.rs` | Modify | Add `assemble_from_layers` to re-exports |

## Dependencies

### Code Dependencies

No new dependencies. Uses existing:
- `tokio` (async, JoinSet, Semaphore, fs)
- `std::collections::BTreeMap` (entry merging)
- `crate::hardlink::create` (file placement — uses blocking `std::fs::hard_link`, existing convention)
- `crate::symlink::{create, is_link, validate_target}` (symlink handling)

**Note:** `AssemblyStats::merge()` is currently private but lives in the same module (`assemble.rs`), so `assemble_from_layers` can call it directly — no visibility change needed.

## Testing Strategy

> Tests are the executable specification, written from this design record in Phase 3.
> Each test traces back to a contract or invariant documented above.

### Test Summary

| Category | Count | Focus |
|----------|-------|-------|
| Boundary / degenerate | 4 | Empty, single, equivalence |
| Two-layer structure | 6 | Disjoint, shared, mixed, pruning |
| Three-layer structure | 4 | Progressive pruning, partial overlap |
| Hardlink / inode (Unix) | 4 | Core correctness: dev+ino sharing |
| Symlinks across layers (Unix) | 4 | Target preservation, cross-layer resolution |
| Stats accumulation | 5 | files, bytes, dirs, symlinks, zero-length |
| Overlap detection | 7 | File, symlink, type mismatch, three-layer |
| Pre-condition errors | 4 | Missing source, file source, missing parent |
| Resource limits | 1 | Entry cap across layers |
| Concurrency stress | 2 | Wide fanout (500 files), deep tree (20 levels) |
| **Total** | **41** | |

### Acceptance Tests

N/A — no user-facing behavior change. The pull pipeline remains gated at N=1 layers until #22 integrates.

## Risks

| Risk | Mitigation |
|------|------------|
| BTreeMap overhead for large directories | Bounded by directory entry count (not file count across tree). Directories with >10k entries are pathological. Single-layer path (`assemble_from_layer`) remains the optimized default. |
| Existing tests affected | No changes to existing code or tests. New function is purely additive. |
| Future shadowing semantics conflict | Layer ordering preserved by array index. Current overlap-free semantics are a strict subset of any future shadowing model. |
| macOS case-insensitive FS | `BTreeMap<OsString>` uses byte-level ordering. On HFS+/APFS, `Bin/tool` and `bin/tool` from different layers would not be detected as an overlap by the BTreeMap — the collision would surface as `EEXIST` from `hardlink::create` instead of a clean `LayerOverlap`. Known limitation; OCX's primary target is Linux CI. Document in function doc-comment. |
| FIFOs/devices in layers | OCI tar layers can contain special file types. The walker skips them silently during collection, matching existing `process_directory` behavior. No `EntryKind` variant needed. |

## Verification

```sh
cargo nextest run -p ocx_lib assemble      # all assembly tests (existing + new)
cargo clippy --workspace                     # lint
cargo fmt --check                            # format
task verify                                  # full quality gate
```
