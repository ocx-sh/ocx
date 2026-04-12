# Refactoring Workflow

Catalog-only rule. Referenced from [workflow-intent.md](./workflow-intent.md) when work is classified as a refactoring. Enforces the Two Hats Rule: change structure without changing behavior.

## Core Principle

> **Two Hats Rule** (from `quality-core.md`): Never mix refactoring and behavior changes in the same session. Refactoring changes structure, NOT behavior. Tests must pass unchanged. Commit before switching hats.

## Non-Negotiable Sequence

```
Safety Net → Scope → Transform → Verify → Repeat
```

Each transformation is one cycle. Multiple transformations are multiple cycles, each with its own commit.

## Phase 1: Safety Net

Verify that adequate tests exist to catch unintended behavior changes.

- [ ] Check test coverage for the code being refactored
- [ ] If coverage is inadequate, write **characterization tests** first — tests that document current behavior (even if ugly), so you'll know if you accidentally change it
- [ ] Characterization tests are committed separately before the refactoring begins

**Gate**: Tests exist that exercise the behavior of the code being refactored. If you can't write characterization tests (code is untestable), that's a signal the refactoring is higher risk — consider a plan artifact.

## Phase 2: Scope

Define exactly one transformation. Refactoring is a sequence of small, named transformations — not "clean up this module."

| Transformation | Example | Scope |
|---------------|---------|-------|
| Rename | Rename `foo` to `bar` across the codebase | Single symbol |
| Extract | Extract method/function/module from inline code | One extraction |
| Move | Move function/struct to a different module | One move |
| Inline | Inline a function/variable that adds no clarity | One inlining |
| Simplify | Replace complex conditional with simpler equivalent | One simplification |
| Dedup | Extract shared logic from 2+ genuinely duplicated call sites | One extraction |

**Rule**: If you can't name the transformation in 2-3 words, it's too broad. Split it.

**Gate**: Transformation is named, scoped to specific files/symbols, and the expected outcome is clear.

## Phase 3: Transform

Apply the single transformation.

- [ ] Make the structural change
- [ ] Use LSP refactoring tools (rename, find references) when available — prefer over regex
- [ ] Do NOT change any behavior, fix any bugs, or add any features during this phase
- [ ] Do NOT update tests to match new structure — tests should pass as-is (that's the proof)

## Phase 4: Verify

Confirm behavior is unchanged.

- [ ] All existing tests pass without modification (subsystem verify for the changed area)
- [ ] If any test fails, the transformation changed behavior — revert and investigate
- [ ] Review the diff: does every change serve the named transformation? Remove anything unrelated

**Gate**: Subsystem verify passes. No test modifications needed. Diff is clean and focused.

## Phase 5: Review-Fix Loop

Diff-scoped, bounded iterative review of each transformation. Max 3 rounds.

**Round 1** — review the transformation diff. Behavior-preservation and scope-discipline perspectives run first; if they have actionable findings, fix before running code-quality perspectives.
- **Behavior preservation**: Does the diff change only structure, never behavior?
- **Scope discipline**: Does every line serve the named transformation from Phase 2?
- **Test integrity**: Were any tests modified? (If so, behavior likely changed — flag it)
- **Code quality**: Does the transformation improve clarity without introducing new smells?

Classify findings as:
- **Actionable** — fix automatically, re-run affected perspectives in Round 2
- **Deferred** — needs human judgment, surfaced in commit summary

**Subsequent rounds** — re-run only perspectives that had actionable findings. Loop exits when no actionable findings remain or after 3 rounds total.

**Cross-model adversarial pass** (optional): After the loop converges, run a single Codex adversarial review against the diff. One-shot — no looping. Skipped gracefully if Codex is unavailable.

**Gate**: No actionable findings remain. `task verify` passes on final state. Deferred findings documented.

## Phase 6: Commit & Repeat

Commit the transformation, then start the next cycle if there are more transformations.

- [ ] Commit with `refactor:` conventional commit type
- [ ] Deferred findings from the review loop included in commit summary
- [ ] Each commit is one named transformation — reviewable in isolation
- [ ] Start next transformation from Phase 2

## Plan Artifacts

| Scope | Artifact |
|-------|----------|
| Single transformation | No artifact — follow the phases inline |
| Multi-step refactoring (3+ transformations) | Create `.claude/artifacts/plan_refactor_[topic].md` from `plan.template.md` — list transformations in order |
| Cross-subsystem refactoring | Use `/swarm-plan` — multiple subsystem rules may apply |

## Anti-Patterns

- **"Refactor this module"**: Too broad — name specific transformations
- **Behavior change during refactoring**: If you find a bug, commit the refactoring first, then fix the bug in a separate commit
- **Skipping characterization tests**: "The code has tests" — check that the *specific code being changed* is tested
- **Giant refactoring commits**: Each transformation should be its own commit — reviewable, revertible, bisectable
- **Modifying tests during refactoring**: If tests need changes, you're changing behavior (exception: updating import paths after a move)

## References

- [workflow-intent.md](./workflow-intent.md) — work-type router
- [workflow-git.md](./workflow-git.md) — commit conventions (`refactor:` type)
- [quality-core.md](./quality-core.md) — Two Hats Rule, reusability assessment
