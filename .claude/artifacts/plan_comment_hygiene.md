# Plan: Comment Hygiene Cleanup

## Classification

- **Scope**: Small (1-2 days)
- **Reversibility**: Two-Way Door — comments can always be re-added
- **Work type**: Refactoring (structure change, no behavior change)

## Problem Statement

AI-assisted development has introduced ~35-50 noisy comments across ~10 files. Two dominant anti-patterns:
1. **Next-line narration** (~20-30 instances) — comments restating what the next line does
2. **Redundant doc comments** (~15-20 instances) — `///` that just restate the function name

The codebase is broadly healthy (1.8% comment density, zero AI reasoning comments, zero dead comments). This is a targeted cleanup, not a rewrite.

## Design: Comment Quality Standard

### Decision: Add comment quality rules to `quality-rust.md` + CLAUDE.md

**Rationale**: The root cause is AI tools generating narration by default. Two levers:
1. **Prevention** — CLAUDE.md instruction so AI stops generating noise
2. **Detection** — quality rule so reviewers catch it

### Comment Quality Rules (to add to `quality-rust.md`)

**Block-tier:**
- Commented-out code — delete it; version control preserves history
- `unsafe` without `// SAFETY:` — already exists, re-confirmed

**Warn-tier:**
- **Obvious/narration comments** — comments a reader could reconstruct by reading the code
- **Tautological doc comments** — `///` that restates the function name without adding information
- **Closing brace comments** — `} // end if`

**Positive requirements (doc comments):**
- Public items: `///` with at minimum a one-sentence summary that adds information beyond the name
- Functions returning `Result`: `# Errors` section
- Modules: `//!` inner doc at top

**The Ousterhout test**: "If someone unfamiliar with the code could write your comment just by reading it, it adds no value."

### CLAUDE.md Addition (2 lines in Core Principles)

```
Comments explain *why*, never *what*. Do not add comments that restate code.
Assume a senior Rust engineer as the reader.
```

### Patterns to PRESERVE (not clean up)

- `// ── Section Name ──` dividers in long files
- Phase/step comments in multi-step orchestration (`// Phase 1:`, `// Step 1:`)
- `// SAFETY:` blocks
- Module-level `//!` documentation
- Parenthetical qualifications (`// tolerate failure (stale ref or GC'd object)`)
- Issue references (`// NOTE: issue #23`)
- Comments explaining non-obvious constraints or "why this looks wrong but is correct"

## Phases

### Phase 1: Update Quality Rules

Update `quality-rust.md` to add comment quality standards under a new `## Comment Quality` section. Add the 2-line CLAUDE.md instruction.

**Files:**
- `.claude/rules/quality-rust.md` — add Comment Quality section
- `CLAUDE.md` — add comment guidance to Core Principles section

### Phase 2: Clean Up Existing Noise (file by file)

Each file is one refactoring transformation (per workflow-refactor.md). All existing tests must pass unchanged after each file.

**Priority order** (highest noise first):

| # | File | Issue | Action | Est. Changes |
|---|------|-------|--------|-------------|
| 1 | `crates/ocx_lib/src/oci/client.rs` | Next-line narration (~9) | Remove narration comments, keep phase/step comments | ~9 deletions |
| 2 | `crates/ocx_lib/src/file_structure/object_store.rs` | Tautological doc comments (~6) | Remove first sentence where it restates fn name; keep sentences with actual info | ~6 edits |
| 3 | `crates/ocx_lib/src/oci/identifier.rs` | Tautological doc comments (~5) | Same treatment — keep examples, remove restatements | ~5 edits |
| 4 | `crates/ocx_lib/src/oci/digest.rs` | Tautological doc comments (~2) | Remove tautological first sentences | ~2 edits |
| 5 | `crates/ocx_lib/src/package_manager/tasks/garbage_collection/reachability_graph.rs` | Next-line narration (~3) | Remove narration | ~3 deletions |
| 6 | `crates/ocx_lib/src/shell.rs` | Next-line narration (~2) | Remove or condense | ~2 edits |
| 7 | `crates/ocx_mirror/src/command/sync.rs` | Next-line narration (~2, incl. bare `// Execute`) | Remove | ~2 deletions |
| 8 | `crates/ocx_mirror/src/pipeline/package.rs` | Narration (~1) | Remove | ~1 deletion |
| 9 | `crates/ocx_cli/src/app/version.rs` | Tautological doc (~1) | Remove | ~1 deletion |

### Phase 3: Verify

- `task rust:verify` passes
- Spot-check `cargo doc --no-deps` to ensure public API docs still render correctly
- No test modifications needed (pure comment removal)

## Executable Phases (for /swarm-execute)

- **Phase 1** (rules): Update `quality-rust.md` + `CLAUDE.md` with comment quality standards
- **Phase 2** (cleanup): 9 files, each a single refactoring transformation — remove/trim comments per the table above
- **Phase 3** (verify): `task rust:verify` + `cargo doc --no-deps`
- **Review**: Scope discipline (only comments changed, no code), behavior preservation (tests unchanged)

## Testing Strategy

No new tests needed. This is a pure comment cleanup — all existing tests must pass unchanged. That passing state IS the proof that behavior is preserved.

## Research Artifact

`.claude/artifacts/research_comment_best_practices.md` — full research findings with sources.
