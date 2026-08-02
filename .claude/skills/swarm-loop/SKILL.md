---
name: swarm-loop
description: Use for autonomous delivery of one issue off a long-living branch: design, plan, TDD build, bounded review-fix loop, merge, consistency check. /swarm-loop.
user-invocable: true
disable-model-invocation: false
argument-hint: "[issue-or-task] [--onto=<long-living-branch>] [--max-review=3]"
triggers:
  - "swarm loop"
  - "autonomous feature loop"
  - "one issue end to end"
  - "loop this issue"
---

# swarm-loop — Autonomous single-feature delivery

Delivers **one** issue (or scoped task) end-to-end on a throwaway branch cut
from a long-living feature branch, then folds it back with a merge commit and a
single entirety-consistency check. This is the inner primitive; `swarm-x`
repeats it across a milestone. Designed to run **inside one Opus subagent** so
the calling orchestrator's context stays lean — the subagent returns only a
completion report.

## Inputs

- **issue-or-task** — a GitHub issue number (`#194`) or free-text scope.
- `--onto=<branch>` — long-living feature branch to cut from and merge back
  into. Default: current branch.
- `--max-review=N` — cap on the review-fix loop (default 3).

## Preconditions (fail fast)

1. `--onto` branch exists, working tree clean (`git status --porcelain` empty).
2. Not on `main`. Never commit on `main` (Principle #6).
3. Read the issue body + linked master plan; extract acceptance criteria,
   dependency edges, and doc surfaces. If a dependency issue is unmerged, stop
   and report — do not proceed out of order.

## Phases

### 0. Branch
`git checkout <onto> && git checkout -b wip/<n>-<slug>`. One issue = one branch.
Slug from issue title, kebab-case. **Do not** prefix the child branch with
`<onto>` — a `feat/x` ref and a `feat/x/…` ref cannot coexist in git.

### 1. Design — `/architect`
Invoke `/architect` (or `worker-architect`, opus) for one-way-door decisions:
error taxonomy, seams, config schema, CLI contract. Skip only for pure
two-way-door edits (≤3 files, no new API). Persist ADR/design-spec to
`.claude/artifacts/` when a decision is hard to reverse.

### 2. Plan — `/swarm-plan`
`/swarm-plan <tier> #<n>` → dependency-ordered plan artifact under
`.claude/state/plans/plan_*.md` with a `## Status` block and testable
component contracts. Tier from scope (`auto` classifies; critical-path or
cross-subsystem issues → `high`/`max`). Enumerate every doc surface the change
touches (environment.md, configuration.md, exit codes, user guide).

### 3. Build — `/swarm-execute`
`/swarm-execute <tier> <plan>` → contract-first TDD: Stub → Verify Arch →
Specify (tests fail on stubs) → Implement → subsystem verify green. Builder
model scales with tier (opus at max). Reuse existing utilities and the
`fake_sigstore.py` fixture stack; never hand-roll what the codebase already has.

### 4. Bounded review-fix loop — `/swarm-review` ↔ `/swarm-execute` (≤ --max-review)
```
round r in 1..=max_review:
  /swarm-review <tier> HEAD --base=<onto>     # diff-scoped adversarial review
  if no actionable findings: break
  /swarm-execute <tier> "<actionable findings>"   # fix, re-verify
```
Actionable → fix and re-review only affected perspectives next round. Deferred /
oscillating (same finding twice) → auto-defer, carry to the report. Exit on
clean review **or** cap hit — never unbounded. Run subsystem verify (not full
`task verify`) each round; full gate runs once at the end.

### 5. Merge
`git checkout <onto> && git merge --no-ff wip/<n>-<slug>` with a merge commit
`Merge #<n>: <title>`. Resolve conflicts in-branch (spawn a subagent per
non-trivial conflict — never inline). Delete the issue branch after merge.

### 6. Entirety-consistency pass (one-shot, deferred, NO loop)
On `<onto>`, spawn **one subagent** to run a single `/swarm-review` focused on
the *whole* long-living branch's coherence (naming, error taxonomy, CLI
surface, docs, cross-issue seams) and to apply any low-risk fixes via
`/swarm-execute` in that same subagent. **This does not loop.** Findings needing
judgment are deferred to the report. Execution stays in the subagent so the
orchestrator context is untouched.

## Exit / report

Return a compact report (no file dumps): branch merged y/n, review rounds used,
`task verify` result, acceptance-criteria coverage, deferred findings, doc
surfaces updated, and any dependency/network limits hit (e.g. real-network TUF
paths left behind `#[ignore]`). Update the master plan's `## Status` block and
check off the issue in the tracker.

## Guardrails

- Never push to remote — human decides (Principle #6).
- Honest verification only — cite the command/test that proves each claim
  (`quality-core.md` Verification Honesty). "Should work" is banned.
- If genuinely blocked (missing dep, unreachable service, out-of-order
  dependency), stop and report — do not fabricate green.
- Keep the loop bounded. A finding that survives two rounds is deferred, not
  re-attempted a third time.
