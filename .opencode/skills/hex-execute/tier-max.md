# Tier: max

**Everything in [`tier-xhigh.md`](tier-xhigh.md), plus three additions**
(`adr_0017` C-995): every configured adversary, one more researcher on the
aggregate join, and usage simulation after merge. Explicit only —
`--tier=max` or a plan whose Status block says `Tier: max`; the classifier
never lands here. Phases 1–7 are `xhigh`'s, linked, never restated; the
headings below exist so `tiers.hex-execute.max.counts` has identifiers to
name.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.

## Phase 1: Discover

As [`tier-xhigh.md` § Phase 1](tier-xhigh.md#phase-1-discover).

## Phase 2: Stub

As [`tier-xhigh.md` § Phase 2](tier-xhigh.md#phase-2-stub).

## Phase 3: Verify-Architecture (reviewer + architect)

As [`tier-xhigh.md` § Phase 3](tier-xhigh.md#phase-3-verify-architecture-reviewer--architect).

## Phase 4: Specify

As [`tier-xhigh.md` § Phase 4](tier-xhigh.md#phase-4-specify).

## Phase 5: Implement

As [`tier-xhigh.md` § Phase 5](tier-xhigh.md#phase-5-implement).

## Phase 6: Review-Fix Loop (by join level, `adversarial` checklist, every adversary)

As [`tier-xhigh.md` § Phase 6](tier-xhigh.md#phase-6-review-fix-loop-by-join-level-adversarial-checklist),
with two changes to the **terminal join's batch**:

- **Every configured adversary launches**, not the first entry: a
  list-valued `adversary` key fans out one call per skill, each last in
  the batch behind the native seat, each under its own resolved bound;
  their findings are union-triaged with the same duplicate merge and join
  the same single `builder` fix pass
  ([adversary contract](../hex-core/references/adversary.md#adversary-contract)).
  One configured entry runs it and announces `max: 1 adversary configured`.
- **One `researcher`** (focus `ecosystem`, known-pitfall framing) joins the
  `L2` batch; its return is an input to the `L2` seat's `pitfalls`
  section, never a seat of its own.

## Phase 7: Merge and commit

As [`tier-xhigh.md` § Phase 7](tier-xhigh.md#phase-7-merge-and-commit).

## Phase 8: Usage simulation

After the final gate, before handoff. Launch **4** `simulator` workers in
one batch — one per shipped pattern (`first-time`, `power-user`,
`adversarial`, `automation`) plus one per project
`.agents/workers/simulator-<pattern>.md` — in artifact mode, each in its
own scratch worktree off the feature branch
([`workers/simulator.md`](../hex-core/references/workers/simulator.md)).
Each writes its scenario first, runs it, returns findings with repros.

Triage the union: actionable findings get **one** merged `builder` (focus
`implement`) fix pass on the feature branch, re-verified by the project's
full documented verification (the final gate re-runs); deferred findings
go to the handoff as product questions. One pass, never a loop — a fix
that fails verification is reverted and its findings promoted to deferred.

**Gate** — every simulator returned or timed out under its budget
(`review.l2.budget-minutes` bounds the batch); the fix pass, when it ran,
passes the final gate.

## Upkeep and handoff

As [`tier-xhigh.md` § Upkeep and handoff](tier-xhigh.md#upkeep-and-handoff),
the handoff carrying `Tier: max`, the adversary count that ran, and a
**Usage simulation** section: per pattern, the verdict and the findings
fixed or deferred.
