---
name: fix
description: Use when a bug ticket or GitHub issue should go from report to an approved fix plan — "plan a fix", "fix this issue". Root-causes it via rca, then plans in plan mode with the failing regression test first. Routes to hex-plan instead when the fix is large or moves an interface contract.
user-invocable: true
argument-hint: "issue-number | url | description"
triggers:
  - "plan a fix"
  - "fix issue"
  - "fix this issue"
  - "fix ticket"
---

# Fix — RCA, then a regression-test-first plan

## 1. Root cause

Read [`../rca/SKILL.md`](../rca/SKILL.md) and follow it in full against `$ARGUMENTS`. Do not shortcut its gates — a fix plan built on an unverified cause is the failure this skill exists to prevent.

## 2. Verdict gate

Verdict ≠ `bug` → output the RCA report and stop. No plan.

## 3. Escalation gate

Stop and do **not** enter plan mode when any holds:

- size **L** or **XL** ([`../rca/references/sizing.md`](../rca/references/sizing.md))
- any interface surface is touched (same file, list)
- confidence **low**, or repro **not reproduced** with no observed causal chain

Output the RCA header, the one-line reason, and:

```
## Actions
- Run `/hex-plan <issue>` with this RCA as input   (XL → `/hex-architect` first)
```

## 4. Plan

Call `EnterPlanMode` (load it via `ToolSearch` `select:EnterPlanMode` if deferred). The plan file holds, in order:

1. **RCA header** — the five-line block from the report, plus the causal chain's key `file:line`.
2. **Regression test — first.** Level per `workflow-bugfix.md` Phase 3: acceptance bug → pytest in `test/tests/`; unit bug → inline `#[cfg(test)]` in the faulty module. Name the test, what it asserts (the root cause, not the symptom), and the command that shows it **red on current HEAD** (`task test:parallel --force -- <file>` or the Bazel/cargo unit target). One per blast-radius site when the sites differ in behaviour.
3. **Fix** — at the root-cause layer; every site from the blast radius covered or explicitly excluded with reason. No drive-by refactor.
4. **Verify** — regression test green, `task verify:scoped --force`. No local full `task verify`.
5. **Commit** — `fix(<scope>): <user-facing sentence>`; the subject is the changelog line. `Closes #<n>` in the body.

Then `ExitPlanMode`. After approval, implement per `workflow-bugfix.md`: test red first, then fix, then green. If the fix grows past **M** mid-implementation, stop and re-enter step 3.

$ARGUMENTS
