# Tier: medium

Minimal review for **two-way-door** diffs — a flag or option change, a doc
edit, a fixture, a single-area tweak of ≤3 files. The adversarial stance
still applies (question every assumption, name failure conditions) but the
panel shrinks to one reviewer — no RCA, no cross-model pass. Matches what a
quick check against `--base=HEAD~1` or a close sibling branch should feel
like.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover (inline, no worker)

Read the diff against the resolved baseline (already computed in
[`SKILL.md`](SKILL.md) step 2). Identify the single area touched by
matching changed paths against the project's own module/area boundaries
(project context, cached in `hex.md › Pointers`,
[`memory.md`](../hex-core/references/memory.md#the-three-sections)). Read
that area's quality rules directly — the diff is small enough that no
worker is needed for this.

**Gate** — the diff is fetched and the single area's rules are loaded.

## Phase 2: Stage 1 — Correctness (single reviewer)

Launch **1** `reviewer` (focus `spec`, phase `post-implementation`) on the
diff. Model class per [`models.md`](../hex-core/references/models.md) row
`reviewer:spec`, tier `medium`. Its brief carries the composed `spec` +
`quality` sections of
[`checklist.md`](../hex-core/references/checklist.md#composition) — the
single-reviewer case the checklist exists for. It applies both jobs a
larger tier splits across two reviewers:

- design/contract compliance — does the change match the project's stated
  patterns and conventions;
- quality — naming, style, tests present, duplication;
- convergence + ID-coverage — when the target traces to a plan, the
  [convergence check](../hex-core/references/loop.md#convergence-contract)
  against its C-/S- IDs.

`post-implementation` phase because this reviews **Implement**-phase
output: the code already exists (not Stub scaffolding, not Specify-phase
tests reviewed in isolation). The design-record anchors this phase checks
cover the whole Stub → Specify → Implement trajectory, so one pass is
enough at this size.

**Gate** — the reviewer is done; every finding is classified actionable or
deferred.

## Phase 3: Stage 2 — skipped

Two-way-door scope (`breadth=minimal`): no security, performance,
documentation, architect, or researcher perspective. If Discover surfaced a
signal the classifier should have caught (e.g. a dependency-manifest change
hiding inside a diff that looked doc-only), **stop and re-run** as
`/hex-review high <target>` — never silently upgrade mid-pipeline.

**Gate** — the skip is logged in the output; proceed to verdict.

## Phase 4: Root-cause analysis — skipped

`rca: off`. The reviewer reports findings with proximate cause and
remediation only. A systemic-smelling finding is still flagged **deferred**
with a reason — a human can escalate to `/hex-review high` for Five Whys.

## Phase 5: Cross-model — skipped

`adversary: off` by default. If the user explicitly passes `--adversary`,
run the pass anyway (user override) in the scope the target implies
(`code-diff` for a diff, `plan-artifact` for a markdown target) — launched
in the same batch as the Phase 2 reviewer, last, and triaged here
([adversary contract](../hex-core/references/adversary.md#adversary-contract)).
Otherwise log `Cross-model review skipped: tier=medium default` and continue.

## Phase 6: Verdict & Output

Produce the review report using the skeleton from
[`SKILL.md`](SKILL.md#the-review-report):

```markdown
## Code Review: [target]
### Summary
- Verdict: Approve | Needs Work | Request Changes
- Tier: medium
- Baseline: <base>
- Diff: N files, +L / -L lines, 1 area
### Stage 1 — Correctness
[findings with file:line, description, remediation]
### Convergence   <!-- when the target traces to a plan -->
### Fold-Back     <!-- mandatory when the target traces to a plan; full block per SKILL.md § The review report -->
### Deferred Findings
[each with: what it is, why human judgment is needed]
```

Omit Stage 2, Cross-Model, and Root-Cause sections — absence is the tier
contract, not a bug. Omit Convergence too unless the target traces to a
plan.

**Gate** — the report is presented. No commits.

## Upkeep and handoff

**Fold-Back runs identically at every tier — see
[`SKILL.md`](SKILL.md#the-review-report).**

Run the
[upkeep step](../hex-core/references/protocol.md#upkeep-step): update any
`hex.md › Pointers` entry this run revealed as drifted. Then emit the handoff
from [`SKILL.md`](SKILL.md) with:

```
- Scope: small (two-way door)
- Tier: medium
- Baseline: <base>
- Overlays: breadth=minimal, rca=off, adversary=off
```

If actionable findings exist and the caller wants them applied:

```
/hex-execute "apply medium-tier review findings"
```
