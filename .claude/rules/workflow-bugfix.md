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

# Bug Fix Workflow

Path-scoped rule (auto-loads on source-work surfaces: `crates/**`, `test/**`, `website/**`, `mirrors/**`, `.claude/**`, `Cargo.toml`, `Cargo.lock`). Referenced from [workflow-intent.md](./workflow-intent.md) when work classified as bug fix. Enforce root-cause discipline: understand bug before fix.

## Non-Negotiable Sequence

```
Reproduce → Root Cause Analysis → Regression Test → Fix → Verify → Document
```

Every step complete before next begin. Skip steps (especially RCA + regression test) = #1 cause of incomplete fixes + regressions.

## Phase 1: Reproduce

Confirm bug exist + capture exact conditions.

- [ ] Identify failing behavior (error message, wrong output, crash)
- [ ] Write exact reproduction steps (commands, inputs, environment)
- [ ] Confirm bug reproducible — if intermittent, note frequency + conditions
- [ ] Identify scope: which versions, platforms, configurations affected?

**Gate**: Bug reproducible with documented steps. If cannot reproduce, investigate more before proceed — no guess fixes.

## Phase 2: Root Cause Analysis

Trace symptom to actual cause. No stop at first suspicious code.

- [ ] Read code path that produce error — trace symptom to source
- [ ] Identify root cause vs proximate cause (line that throw vs condition that made it throw)
- [ ] Check: single bug or pattern? Search similar code with same defect
- [ ] Check git blame / history: regression from recent change?

**Output**: Clear root cause statement: "X happens because Y, introduced by Z" — not "error on line N."

**Gate**: Root cause identified + explained. If unclear, dig deeper — no speculative fix.

## Phase 3: Regression Test

Write failing test that prove bug exist *before* writing fix.

- [ ] Write test exercising exact reproduction steps from Phase 1
- [ ] Test MUST fail on current code (prove bug exist)
- [ ] Test target root cause, not just symptom
- [ ] Acceptance-level bugs: pytest test in `test/tests/`
- [ ] Unit-level bugs: inline `#[cfg(test)]` test in affected module

**Gate**: Test exists, compiles, fails with expected error. Test = proof fix works.

## Phase 4: Fix

Apply minimal change that address root cause.

- [ ] Fix target root cause from Phase 2, not symptom
- [ ] Change minimal — no drive-by refactor, no "while I'm here" extras
- [ ] If RCA revealed pattern of similar bugs (Phase 2), fix all instances
- [ ] If fix needs architectural change, escalate to feature workflow with plan artifact

## Phase 5: Verify

Confirm fix work + no new regressions.

- [ ] Regression test from Phase 3 now passes
- [ ] All existing tests still pass (subsystem verify for changed area)
- [ ] Manually verify reproduction steps from Phase 1 no longer reproduce bug
- [ ] If bug in hot path or had security impact, check edge cases

**Gate**: Subsystem verify passes. Regression test passes. Manual verify confirms fix.

## Phase 6: Review-Fix Loop

Apply canonical Review-Fix Loop to bug-fix diff. Bug-fix perspectives run first in Round 1:
- **Correctness**: Fix address root cause (Phase 2), not symptom?
- **Regression risk**: Could change break other callers or edge cases?
- **Minimality**: Every line in diff necessary? No drive-by changes?
- **Test coverage**: Regression test (Phase 3) prove fix adequately?

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

## Phase 7: Commit & Document

Close loop so fix traceable.

- [ ] Commit with `fix:` conventional commit type, reference root cause in body
- [ ] If open GitHub issue for bug, reference in commit (`fixes #N`)
- [ ] If bug non-trivial + no GitHub issue, consider creating one for record
- [ ] If bug revealed test coverage gap, note for future work

## Plan Artifacts

| Scope | Artifact |
|-------|----------|
| Trivial (obvious cause, < 30 min) | No artifact — follow phases inline |
| Non-trivial (unclear cause, multi-file, or high risk) | Create `.claude/artifacts/bugfix_plan_[topic].md` from `bugfix_plan.template.md` |
| Post-incident (production impact, security) | Create `.claude/artifacts/postmortem_[topic].md` from `postmortem.template.md` |

## Red Flags — Recognize Rationalizations Before Acting on Them

If you think anything in left column, stop + apply right column. Most common ways bug-fix session goes wrong.

| Rationalization | Red flag | Correct action |
|---|---|---|
| "I know what's wrong, I'll just fix it" | No Phase 2 RCA written | Write root-cause statement first. If can't, you don't know cause yet. |
| "Test trivial, I'll add after fix" | Planning test after fix | Write failing test first. Test added after fix don't prove fix works. |
| "Clippy warns nearby — I'll fix while I'm here" | Diff contains unrelated changes | Commit fix alone. Separate commit for cleanup. |
| "Catching exception simpler than preventing state" | Fix in `try/except` | Symptom fix. Find condition that produced bad state. |

## Anti-Patterns

- **Fix without RCA**: "It works now" not a fix — must explain *why* works
- **Test after fix**: Test after fix don't prove test catches bug
- **Symptom fix**: Catch exception instead of prevent condition that caused it
- **Scope creep**: Refactor nearby code during bug fix — split into separate commits
- **Speculative fix**: "Might be cause" → investigate until certain

## References

- [workflow-intent.md](./workflow-intent.md) — work-type router
- [workflow-git.md](./workflow-git.md) — commit conventions (`fix:` type)
- [quality-core.md](./quality-core.md) — code review checklist
- [workflow-github.md](./workflow-github.md) — issue creation protocol