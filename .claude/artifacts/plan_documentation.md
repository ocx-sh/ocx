# Plan 10: Documentation and Cleanup

## Context

Three-tier CAS implementation (Plans 1–9) is complete and committed on `goat`. The last remaining work is:

1. **Dead code** — old `object_store.rs`, `index_store.rs`, `install_store.rs` are unused but still `mod`-declared and re-exported. Plan 5 specified deleting them; this wasn't done.
2. **AI config docs** — still describe the old 4-store layout.
3. **User-facing docs** — `website/src/docs/user-guide.md` may still reference the old layout.
4. **ADR status** — flip `adr_three_tier_cas_storage.md` from Proposed → Accepted.
5. **Memory** — `MEMORY.md` describes the old `FileStructure`.

**Related:** `plan_three_tier_cas.md`, `adr_three_tier_cas_storage.md`

## Execution Model

This plan is documentation + mechanical dead-code deletion. Contract-first TDD does not apply — there is no new behavior to specify. Instead:

```
1. Cleanup    → worker-builder (refactoring)   Gate: task verify passes
2. Write docs → worker-doc-writer (parallel)   Gate: drafts produced
3. Review     → worker-doc-reviewer            Round 1: full audit
                worker-reviewer (quality)      Review dead-code deletion diff
4. Fix loop   → Max 3 rounds per swarm-execute protocol
5. Commit     → conventional commit: chore(docs)/refactor mix
```

**Review-fix loop** follows `.claude/skills/personas/swarm-execute/SKILL.md`:
- Diff-scoped (`git diff main...HEAD --name-only`)
- Severity-gated (Block/Warn drive loop; Suggest → deferred)
- Max 3 rounds; selective re-review (only perspectives with findings re-run)
- On exit: `task verify` + deferred findings summary

## Phase 1 — Dead Code Deletion

**Agent:** `worker-builder` (focus: refactoring)
**Scope:**
- Delete `crates/ocx_lib/src/file_structure/object_store.rs`
- Delete `crates/ocx_lib/src/file_structure/index_store.rs`
- Delete `crates/ocx_lib/src/file_structure/install_store.rs`
- Remove their `mod` declarations and `pub use` re-exports from `crates/ocx_lib/src/file_structure.rs`
- Grep for any stray references to `ObjectStore`, `IndexStore`, `InstallStore`, `ObjectDir` as types (not in historical doc comments) and fix
- Keep `SymlinkKind` export alive (it lives in the dead `install_store.rs` currently — move to `symlink_store.rs` if still needed, or verify `symlink_store.rs` already has its own and delete duplicate)

**Gate:** `task verify` passes.

## Phase 2 — Documentation Updates (parallel)

**Agent:** `worker-doc-writer` (focus: narrative + reference)

Spawn in parallel — each targets one file:

### 2a. `.claude/rules/subsystem-file-structure.md`
Rewrite for 6-store layout (`blobs`, `layers`, `packages`, `tags`, `symlinks`, `temp`). Update module map, `FileStructure` struct snippet, per-store sections, and GC paragraph. Reference the new consolidated `refs/` layout (`refs/symlinks`, `refs/deps`, `refs/layers`, `refs/blobs`).

### 2b. `.claude/rules/architecture-principles.md`
Fix the "Composite root" row (old: `ObjectStore + IndexStore + InstallStore + TempStore`). Update "Key Concepts" table: `Object` → `Package`/`Layer`/`Blob` entries. Add ADR `adr_three_tier_cas_storage.md` to the ADR index.

### 2c. `.claude/rules/product-context.md`
Rewrite "Three-Store Architecture" heading and diagram to the six-store layout. Keep the quick-context framing.

### 2d. `.claude/artifacts/adr_three_tier_cas_storage.md`
Flip `Status: Proposed` → `Status: Accepted`. Add an "Implementation" note pointing at the commit range on `goat`.

### 2e. `website/src/docs/user-guide.md`
Audit for references to `~/.ocx/objects/`, `~/.ocx/installs/`, `~/.ocx/index/` and rewrite to new layout. Keep user framing — this is end-user doc, not internal architecture.

### 2f. Auto-memory `~/.claude/projects/-home-mherwig-dev-ocx/memory/MEMORY.md`
Update "Key Architecture" and "FileStructure Layout (implemented)" sections to reflect the six-store layout, new `PackageDir` accessors, and consolidated `refs/`.

## Phase 3 — Review Loop

**Round 1:**
- `worker-doc-reviewer` — full trigger-matrix audit across all changed docs + cross-reference check against current code
- `worker-reviewer` (focus: quality) — review the dead-code deletion diff only

Classify findings Actionable / Deferred / Suggest per swarm-execute protocol.

**Round 2+:** `worker-builder` (fresh) fixes actionable findings; re-run only perspectives that had findings. Max 3 rounds. `task verify` between rounds.

## Phase 4 — Commit

Single conventional commit on `goat`:

```
chore(docs): document three-tier CAS and delete legacy stores
```

NEVER push — human decides.

## Files Touched

- `crates/ocx_lib/src/file_structure.rs` (remove mods)
- `crates/ocx_lib/src/file_structure/object_store.rs` (delete)
- `crates/ocx_lib/src/file_structure/index_store.rs` (delete)
- `crates/ocx_lib/src/file_structure/install_store.rs` (delete)
- `.claude/rules/subsystem-file-structure.md`
- `.claude/rules/architecture-principles.md`
- `.claude/rules/product-context.md`
- `.claude/artifacts/adr_three_tier_cas_storage.md`
- `website/src/docs/user-guide.md`
- `~/.claude/projects/-home-mherwig-dev-ocx/memory/MEMORY.md`

## Verification

- `task verify` passes
- No grep hits for `ObjectStore|IndexStore|InstallStore` as types in `crates/` (except in git history)
- `task lint:ai-config` passes
- Doc-reviewer reports clean
