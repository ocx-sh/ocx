---
paths:
  - "crates/**"
  - "test/**"
  - "website/**"
  - "mirrors/**"
  - ".claude/**"
  - "Cargo.toml"
  - "Cargo.lock"
---

# Refactoring Workflow

Path-scoped rule (auto-loads on source-work surfaces: `crates/**`, `test/**`, `website/**`, `mirrors/**`, `.claude/**`, `Cargo.toml`, `Cargo.lock`). Referenced from [workflow-intent.md](./workflow-intent.md) when work classified as refactoring. Enforces Two Hats Rule: change structure, no behavior change.

## Core Principle

> **Two Hats Rule** (from `quality-core.md`): Never mix refactor + behavior change in same session. Refactor change structure, NOT behavior. Tests pass unchanged. Commit before switch hats.

## Non-Negotiable Sequence

```
Safety Net → Scope → Transform → Verify → Repeat
```

Each transformation = one cycle. Multiple transformations = multiple cycles, each own commit.

## Phase 1: Safety Net

Verify tests exist to catch unintended behavior change.

- [ ] Check test coverage for code being refactored
- [ ] If coverage inadequate, write **characterization tests** first — tests document current behavior (even if ugly), so know if accidentally change it
- [ ] Characterization tests committed separately before refactor begin

**Gate**: Tests exist exercising behavior of code being refactored. If can't write characterization tests (code untestable), signal refactor higher risk — consider plan artifact.

## Phase 2: Scope

Define exactly one transformation. Refactor = sequence of small, named transformations — not "clean up this module."

| Transformation | Example | Scope |
|---------------|---------|-------|
| Rename | Rename `foo` to `bar` across the codebase | Single symbol |
| Extract | Extract method/function/module from inline code | One extraction |
| Move | Move function/struct to a different module | One move |
| Inline | Inline a function/variable that adds no clarity | One inlining |
| Simplify | Replace complex conditional with simpler equivalent | One simplification |
| Dedup | Extract shared logic from 2+ genuinely duplicated call sites | One extraction |

**Rule**: Can't name transformation in 2-3 words = too broad. Split.

**Gate**: Transformation named, scoped to specific files/symbols, expected outcome clear.

## Phase 3: Transform

Apply single transformation.

- [ ] Make structural change
- [ ] Use LSP refactor tools (rename, find references) when available — prefer over regex
- [ ] Do NOT change behavior, fix bugs, add features this phase
- [ ] Do NOT update tests to match new structure — tests pass as-is (that proof)

## Phase 4: Verify

Confirm behavior unchanged.

- [ ] All existing tests pass without modification (subsystem verify for changed area)
- [ ] Any test fails = transformation changed behavior — revert + investigate
- [ ] Review diff: every change serve named transformation? Remove anything unrelated

**Gate**: Subsystem verify pass. No test mods needed. Diff clean + focused.

## Phase 5: Review-Fix Loop

Apply canonical Review-Fix Loop to each transformation's diff. Refactor-specific perspectives run first in Round 1:
- **Behavior preservation**: Diff change only structure, never behavior?
- **Scope discipline**: Every line serve named transformation from Phase 2?
- **Test integrity**: Any tests modified? (If so, behavior likely changed — flag)
- **Code quality**: Transformation improve clarity without new smells?

<!-- REVIEW_FIX_LOOP_CANONICAL_BEGIN -->
Diff-scoped, bounded iterative review. Tier-scaled: 1 round at `low`, up to 3 rounds at `high`/`max`.

**Round 1** — run every perspective on diff. Perspectives most likely find blockers run first (e.g. spec-compliance, correctness, behavior-preservation); if surface actionable findings, fix before remaining perspectives in same round.

Classify each finding:

- **Actionable** — fix automatically, re-run affected perspectives next round.
- **Deferred** — needs human judgment; surface in commit summary with context.

**Subsequent rounds** — re-run only perspectives with actionable findings prior round. Loop exits when no actionable findings remain or tier's round cap hit. Oscillating findings (same issue surfaced two rounds) auto-defer.

**Cross-model adversarial pass** (optional, tier-scaled): after Claude loop converges, run single Codex adversarial review against diff as final gate. One-shot, no looping — two-family stylistic thrash = failure mode. Skipped gracefully if Codex unavailable.

**Gate to exit**: no actionable findings remain, verification passes on final state, deferred findings documented for handoff.
<!-- REVIEW_FIX_LOOP_CANONICAL_END -->

## Phase 6: Commit & Repeat

Commit transformation, start next cycle if more transformations.

- [ ] Commit with `refactor:` conventional commit type
- [ ] Deferred findings from review loop in commit summary
- [ ] Each commit = one named transformation — reviewable in isolation
- [ ] Start next transformation from Phase 2

## Plan Artifacts

| Scope | Artifact |
|-------|----------|
| Single transformation | No artifact — follow phases inline |
| Multi-step refactor (3+ transformations) | Create `.claude/state/plans/plan_refactor_[topic].md` from `plan.template.md` — list transformations in order |
| Cross-subsystem refactor | Use `/swarm-plan` — multiple subsystem rules may apply |

## Red Flags — Recognize Rationalizations Before Acting on Them

| Rationalization | Red flag | Correct action |
|---|---|---|
| "The existing tests cover the general area, we're good" | No explicit check that the specific code path is tested | Write a characterization test for the exact function or branch being transformed. |
| "I'll do the rename AND extract the helper in one pass" | Diff contains 2+ named transformations | Split. Commit each transformation separately — Two Hats Rule at the transformation level too. |
| "The test started failing, so I updated it to match" | Test file modified during refactor | Revert the test change. If the transformation is behavior-preserving, tests pass unchanged. |
| "The cleanup is small, I'll sneak in a bug fix" | Refactor commit fixes a bug | Split. `refactor:` and `fix:` are different commit types — keep them separate. |

## Anti-Patterns

- **"Refactor this module"**: Too broad — name specific transformations
- **Behavior change during refactor**: Find bug, commit refactor first, fix bug in separate commit
- **Skipping characterization tests**: "Code has tests" — check *specific code being changed* tested
- **Giant refactor commits**: Each transformation own commit — reviewable, revertible, bisectable
- **Modifying tests during refactor**: Tests need changes = changing behavior (exception: updating import paths after move)

## References

- [workflow-intent.md](./workflow-intent.md) — work-type router
- [workflow-git.md](./workflow-git.md) — commit conventions (`refactor:` type)
- [quality-core.md](./quality-core.md) — Two Hats Rule, reusability assessment