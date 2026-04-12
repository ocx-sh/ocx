# Bug Fix Workflow

Catalog-only rule. Referenced from [workflow-intent.md](./workflow-intent.md) when work is classified as a bug fix. Enforces root-cause discipline: understand the bug before fixing it.

## Non-Negotiable Sequence

```
Reproduce → Root Cause Analysis → Regression Test → Fix → Verify → Document
```

Every step must complete before the next begins. Skipping steps (especially RCA and regression test) is the #1 cause of incomplete fixes and regressions.

## Phase 1: Reproduce

Confirm the bug exists and capture the exact conditions.

- [ ] Identify the failing behavior (error message, incorrect output, crash)
- [ ] Write down exact reproduction steps (commands, inputs, environment)
- [ ] Confirm the bug is reproducible — if intermittent, note frequency and conditions
- [ ] Identify the scope: which versions, platforms, configurations are affected?

**Gate**: Bug is reproducible with documented steps. If it cannot be reproduced, investigate further before proceeding — do not guess at fixes.

## Phase 2: Root Cause Analysis

Trace the symptom to its actual cause. Do not stop at the first suspicious code.

- [ ] Read the code path that produces the error — trace from symptom to source
- [ ] Identify the root cause vs. the proximate cause (the line that throws vs. the condition that made it throw)
- [ ] Check: is this a single bug or a pattern? Search for similar code that might have the same defect
- [ ] Check git blame / history: was this a regression from a recent change?

**Output**: A clear statement of the root cause: "X happens because Y, introduced by Z" — not "the error is on line N."

**Gate**: Root cause identified and explained. If the cause is unclear, deepen investigation — do not proceed with a speculative fix.

## Phase 3: Regression Test

Write a failing test that proves the bug exists *before* writing the fix.

- [ ] Write a test that exercises the exact reproduction steps from Phase 1
- [ ] The test MUST fail on the current code (proving the bug exists)
- [ ] The test should target the root cause, not just the symptom
- [ ] For acceptance-level bugs: write a pytest test in `test/tests/`
- [ ] For unit-level bugs: write an inline `#[cfg(test)]` test in the affected module

**Gate**: Test exists, compiles, and fails with the expected error. This test becomes the proof that the fix works.

## Phase 4: Fix

Apply the minimal change that addresses the root cause.

- [ ] Fix targets the root cause identified in Phase 2, not just the symptom
- [ ] Change is minimal — no drive-by refactoring, no "while I'm here" improvements
- [ ] If the root cause analysis revealed a pattern of similar bugs (Phase 2), fix all instances
- [ ] If the fix requires architectural changes, escalate to a feature workflow with a plan artifact

## Phase 5: Verify

Confirm the fix works and hasn't introduced regressions.

- [ ] Regression test from Phase 3 now passes
- [ ] All existing tests still pass (subsystem verify for the changed area)
- [ ] Manually verify the reproduction steps from Phase 1 no longer reproduce the bug
- [ ] If the bug was in a hot path or had security implications, check for edge cases

**Gate**: Subsystem verify passes. Regression test passes. Manual verification confirms the fix.

## Phase 6: Review-Fix Loop

Diff-scoped, bounded iterative review before committing. Max 3 rounds.

**Round 1** — review the fix diff. Correctness perspectives run first; if they have actionable findings, fix before running quality/security/performance perspectives.
- **Correctness**: Does the fix address the root cause (Phase 2), not just the symptom?
- **Regression risk**: Could this change break other callers or edge cases?
- **Minimality**: Is every line in the diff necessary for the fix? No drive-by changes?
- **Test coverage**: Does the regression test (Phase 3) adequately prove the fix?

Classify findings as:
- **Actionable** — fix automatically, re-run affected perspectives in Round 2
- **Deferred** — needs human judgment, surfaced in commit summary

**Subsequent rounds** — re-run only perspectives that had actionable findings. Loop exits when no actionable findings remain or after 3 rounds total.

**Cross-model adversarial pass** (optional): After the loop converges, run a single Codex adversarial review against the diff. One-shot — no looping. Skipped gracefully if Codex is unavailable.

**Gate**: No actionable findings remain. `task verify` passes on final state. Deferred findings documented.

## Phase 7: Commit & Document

Close the loop so the fix is traceable.

- [ ] Commit with `fix:` conventional commit type, referencing the root cause in the body
- [ ] If there's an open GitHub issue for this bug, reference it in the commit (`fixes #N`)
- [ ] If the bug was non-trivial and has no GitHub issue, consider creating one for the record
- [ ] If the bug revealed a gap in test coverage, note it for future work

## Plan Artifacts

| Scope | Artifact |
|-------|----------|
| Trivial (obvious cause, < 30 min) | No artifact — follow the phases inline |
| Non-trivial (unclear cause, multi-file, or high risk) | Create `.claude/artifacts/bugfix_plan_[topic].md` from `bugfix_plan.template.md` |
| Post-incident (production impact, security) | Create `.claude/artifacts/postmortem_[topic].md` from `postmortem.template.md` |

## Anti-Patterns

- **Fix without RCA**: "It works now" is not a fix — you need to explain *why* it works
- **Test after fix**: Writing the test after the fix doesn't prove the test catches the bug
- **Symptom fix**: Catching an exception instead of preventing the condition that caused it
- **Scope creep**: Refactoring nearby code during a bug fix — split into separate commits
- **Speculative fix**: "This might be the cause" → investigate until you're certain

## References

- [workflow-intent.md](./workflow-intent.md) — work-type router
- [workflow-git.md](./workflow-git.md) — commit conventions (`fix:` type)
- [quality-core.md](./quality-core.md) — code review checklist
- [workflow-github.md](./workflow-github.md) — issue creation protocol
