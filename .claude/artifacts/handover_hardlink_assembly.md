# Handover: Hardlink-Based Package Assembly

**Date:** 2026-04-10
**Branch:** `goat` (worktree: `ocx`)
**Plan:** `.claude/artifacts/plan_hardlink_assembly.md`
**ADR:** `.claude/artifacts/adr_three_tier_cas_storage.md` (D6 revised)

## Status: All 7 Sub-plans Complete, Nothing Committed

`task verify` passes end-to-end:
- 898 workspace unit tests pass
- 230 acceptance tests pass (5 new in `test_assembly.py` + 2 extensions)
- All lint/format/license/clippy/build gates green

Working tree is uncommitted. User decides commit strategy.

## What Shipped

### Core change

Replaced Plan 8b's directory-level symlink assembly (`packages/{P}/content/` → `layers/{A}/content/`) with a walker that hardlinks files from layers into a real `packages/{P}/content/` directory. Matches pnpm/uv's file-level hardlink pattern. Eliminates the cross-layer relative RPATH failure mode and the filename-dispatch / readlink workarounds from Plan 8b.

### New modules

| Path | Purpose |
|---|---|
| `crates/ocx_lib/src/hardlink.rs` | Low-level hardlink primitives — `create(source, link)`, `update(source, link)`. Mirrors `symlink.rs` template. |
| `crates/ocx_lib/src/utility/fs/assemble.rs` | Assembly walker — `assemble_from_layer(source_content, dest_content) -> Result<AssemblyStats>`. Iterative stack-based walk; hardlinks files, creates real dirs, preserves intra-layer symlinks verbatim on Unix, copies on Windows. |
| `test/tests/test_assembly.py` | I2/I5/I6 acceptance tests (versioned symlink chain, shared-layer inode equality, `du` dedup). |
| `.claude/artifacts/plan_hardlink_assembly.md` | The plan artifact (living design record). |

### Modified files

| File | Change |
|---|---|
| `.claude/artifacts/adr_three_tier_cas_storage.md` | D6 rewritten + simplification follow-up (dropped `create_or_copy`) |
| `.claude/rules/subsystem-file-structure.md` | Added `hardlink.rs` row + walker mention |
| `crates/ocx_lib/src/lib.rs` | `pub mod hardlink;` |
| `crates/ocx_lib/src/symlink.rs` | 2 new rename-survival tests (`symlink_survives_directory_rename`, `symlink_with_temp_path_in_target_breaks_after_rename`) |
| `crates/ocx_lib/src/codesign.rs` | Test at line 475 migrated from `std::fs::hard_link` to `crate::hardlink::create` |
| `crates/ocx_lib/src/package_manager/tasks/pull.rs::setup_owned` | Walker replaces content/ symlink; `link_layers_in_temp` moved BEFORE walker (C2 GC-race closure) |
| `crates/ocx_lib/src/file_structure/package_store.rs::package_dir_for_content` | Reverted from filename-dispatch to `dunce::canonicalize(...).parent()` |
| `crates/ocx_lib/src/utility/fs.rs` | `mod assemble;` + re-export |
| `test/tests/test_find.py` | `.readlink()` → `.resolve()` revert + new `test_find_returns_package_path_not_layer` |
| `test/tests/test_purge.py` | New `test_purge_preserves_shared_layer_inodes` + imports `_make_two_packages_sharing_layer` from `test_assembly.py` |
| `test/tests/test_dependencies.py` | `_list_dep_targets`: `entry.readlink()` → `entry.resolve()` |
| `test/tests/test_package_lifecycle.py` | `.readlink()` → `.resolve()` revert |

### Key design simplification (user directive mid-flight)

Midway through Sub-plan 2, user asked whether `create_or_copy` was necessary. It wasn't:

- `temp → packages/` already required same-fs via atomic rename
- `$OCX_HOME` single-volume constraint is inherited, not new
- Silent copy fallback would hide dedup regressions

**Result:** dropped `create_or_copy`, `LinkMethod` enum, and `COPY_FALLBACK_WARNED`. Cross-device now surfaces `CrossesDevices` as a clear install-time error. ADR and plan both updated.

### Invariants locked in by tests

| Invariant | Test |
|---|---|
| Hardlinks survive temp → packages rename (inode unchanged) | `hardlink::tests::hardlink_survives_directory_rename` |
| Symlink targets byte-preserved through rename | `symlink::tests::symlink_survives_directory_rename` |
| Absolute-into-temp targets break after rename (publisher-bug failure mode) | `symlink::tests::symlink_with_temp_path_in_target_breaks_after_rename` |
| `packages/{P}/content/` is a real directory | All 19 walker tests |
| Cross-device surfaces `CrossesDevices` cleanly | `hardlink::tests::create_errors_on_cross_device` |
| Two packages sharing a layer share inodes | `test_assembly.py::test_shared_layer_files_have_same_inode` |
| `du -s` dedup across two packages | `test_assembly.py::test_shared_layer_disk_usage_is_not_doubled` |
| Symlink chain preserved verbatim through install | `test_assembly.py::test_versioned_symlink_chain_preserved_after_install` |
| Purge preserves shared layer inodes | `test_purge.py::test_purge_preserves_shared_layer_inodes` |
| `find` never returns a layer path | `test_find.py::test_find_returns_package_path_not_layer` |
| C2 GC race closed (`refs/layers/` before walker) | Structural — review verified in `pull.rs::setup_owned` ordering |

## Deferred Findings (for next session)

These were identified during Review-Fix Loops but classified as Deferred / Suggest — not auto-fixed. Organized by where they came from so you can decide what to act on.

### D1 — Multi-layer dest pre-condition tension

**Source:** Sub-plan 3 architect review
**File:** `crates/ocx_lib/src/utility/fs/assemble.rs` lines 98–116 (`dest_content` empty check)
**Problem:** The walker currently errors if `dest_content` is non-empty. For single-layer packages this is correct. For multi-layer (#22) the caller wants to run the walker once per layer into the same `packages/{P}/content/` — which means the second call sees a populated dest and errors.

**Options:**
- (a) Relax to "dest may exist; walker errors on collision (same path contributed by multiple layers)"
- (b) Keep the strict check and have the caller stage each layer into a fresh subdir, then merge separately
- (c) Introduce an "overlay mode" parameter on `assemble_from_layer` that flips the empty-check off

**Not a bug today** — `pull.rs::setup_owned` has `if layer_digests.len() > 1 { unimplemented!("multi-layer package assembly (#22)"); }`. This decision belongs to whoever picks up #22.

### D2 — Partial-dest-on-error contract

**Source:** Sub-plan 3 architect review
**Status:** Fixed as a docstring addition in Round 2 — walker docstring now says "On error, `dest_content` may contain partially-assembled files; callers should assemble into a temp directory they can discard."
**Action:** None needed. Recorded here for completeness.

### S1 — Windows walker path-escape hardening

**Source:** Sub-plan 3 security review (CWE-59, CWE-22)
**File:** `crates/ocx_lib/src/utility/fs/assemble.rs` Windows branch (~line 227)
**Concrete scenario:** A malicious layer publisher crafts a symlink `foo → C:\Windows\System32\config\SAM` or `foo → ..\..\..\..\Windows\System32\config\SAM`. On Unix the walker preserves the symlink verbatim (safe — resolution happens later at access time under the consumer's privileges). On Windows the walker **dereferences** the target at assembly time via `tokio::fs::copy(abs_target, dest_path)` — the OCX process reads the file as the user running `ocx install`. This is a supply-chain vector: the attacker's package silently exfiltrates arbitrary files into the package content.

**Mitigation (proposed):** After computing `abs_target`, canonicalize it (`dunce::canonicalize`) and verify it is within the canonicalized layer root. If it escapes, either error out or fall back to refusing to handle the symlink. Add a Windows test that asserts this.

**Why not fixed in this sub-plan:** The security reviewer classified it as **Suggest** because:
- No Windows CI currently exists in the repo
- The single existing Windows test (`windows_file_symlink_falls_back_to_copy`) uses only intra-layer targets
- User-space attacker has no more reach than the user running `ocx install`

**Status:** Worth a follow-up issue. Real vulnerability when Windows CI lands.

### S2 — TOCTOU between `file_type()` and `hardlink::create()`

**Source:** Sub-plan 3 security review (CWE-367)
**File:** `crates/ocx_lib/src/utility/fs/assemble.rs` line 162 (`entry.file_type()`) vs. line 184 (`hardlink::create`)
**Scenario:** Between the file-type check and the hardlink call, a concurrent writer could swap a regular file for a symlink. On Linux, `link(2)` does not follow symlinks, so the walker would hardlink the symlink inode itself, silently breaking dedup.

**Mitigating control:** The walker only runs inside an exclusively-locked temp directory (via `pull.rs::acquire_temp_dir`). Layer content is immutable once extracted. No concurrent writer exists in normal operation. The race is purely theoretical.

**Action:** Add a one-line comment in `assemble_from_layer` citing the temp-dir lock assumption, so the invariant is reviewable. No code change needed.

**Classification:** Suggest only.

### Minor quality suggestions (all Suggest, Sub-plan 3)

1. **`entry.metadata().await` saves one syscall** vs. `tokio::fs::metadata(&src_path)` at assemble.rs:178. Trivial perf improvement.
2. **`try_exists(...).await.unwrap_or(false)` readability** — works correctly but an explicit match might more clearly signal that IO errors are intentionally swallowed.
3. **Pre-condition helper extraction** — assemble.rs:76–139 is ~40 lines of pre-condition validation. Could be extracted to a `prepare_dest(dest_content) -> Result<()>` helper. Stylistic.
4. **Multi-layer fast-fail** — pull.rs:230 currently extracts all layers *before* the `len() > 1` check at line 272. Could move the check earlier to fail fast. Minor.

### Sub-plan 6 deferrals

- **I1 (Relative RPATH binary test)** — deferred. Requires building a C binary with `$ORIGIN/../lib` RPATH or shipping a pre-compiled binary fixture. Documented in `test/tests/test_assembly.py` module docstring pointing at plan Sub-plan 6 / I1. Not tracked as a GitHub issue; mentioning in handover so a new tracking issue can be created if desired.

## How to Resume

1. **Clear context.**
2. **Re-read this handover** + the plan at `.claude/artifacts/plan_hardlink_assembly.md` + the ADR D6 section.
3. **Check `git status`** on `goat` — all the changes should still be uncommitted.
4. **`task verify`** — confirms the working tree still builds and tests still pass.
5. **Pick from the deferred list above** and address whichever the user wants to tackle first. Candidates in priority order (my read):
   - **D1** (multi-layer dest pre-condition) — has a concrete code location; relaxing the check is ~10 lines. Still needs a design call on (a)/(b)/(c).
   - **S2** (TOCTOU comment) — trivial, just a comment addition.
   - **S1** (Windows path-escape hardening) — ~30-line fix + 2 new Windows tests. Would benefit from a tracking issue first.
   - **Minor quality suggestions** — rollup PR possible, low urgency.
6. **Commit strategy open:** single `feat: hardlink-based package assembly` commit vs. one commit per sub-plan. Previously recommended single commit because sub-plans are cohesive and `task verify` passes as a whole. User's call.

## Files to Reference

- `.claude/artifacts/plan_hardlink_assembly.md` — the plan (living design record; updated mid-flight when `create_or_copy` was dropped)
- `.claude/artifacts/adr_three_tier_cas_storage.md` — ADR D6 (authoritative design)
- `.claude/rules/feature-workflow.md` — swarm workflow reference
- `.claude/rules/rust-quality.md` — standards applied during review rounds
- `crates/ocx_lib/src/hardlink.rs` — simplified primitives (`create`, `update`)
- `crates/ocx_lib/src/utility/fs/assemble.rs` — the walker + 19 unit tests
- `test/tests/test_assembly.py` — new integration tests
