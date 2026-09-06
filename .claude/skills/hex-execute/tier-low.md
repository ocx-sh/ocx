# Tier: low

Inline execution for **trivial two-way-door** changes — one file, ≤30
lines, no structural marker, no security-sensitive or hot path
([`../hex-plan/classify.md`](../hex-plan/classify.md#tier-signal-table)).
**Zero spawns: the orchestrator is the worker** (`adr_0017` C-994). Every
phase below is the orchestrator's own turn on the feature branch; no
worktree is cut, no plan is decomposed, and a plan target is a single
work package. Anything Discover reveals to be larger **stops and
re-runs** as `/hex-execute medium <target>` — the tier never upgrades
mid-flow. Review is still keyed on join level
([`loop.md` § Review by join level](../hex-core/references/loop.md#review-by-join-level)):
`L0` is the orchestrator's own evidence table, and the only spawn this
tier can make is one `L1` backstop.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), work-package mechanics in
[`SKILL.md`](SKILL.md#work-packages), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover

Read the target file and the project rules for its area (project context,
cached in `hex.md › Pointers`,
[`memory.md`](../hex-core/references/memory.md#the-three-sections)). No
explorer. On a plan target, read the single WP's row and its contract
excerpt; on a free-text target, the task description is the contract.

**Gate** — exactly one file changes and the change fits the `low` row; else
stop and re-run at `medium`.

## Phase 2: Stub

Write the public-surface change — a signature, a flag, an error variant —
directly, with a not-implemented body, and **commit it** so the
stub-first ordering is read off the commit graph as at every other tier.
A change with no new surface (a doc line, a constant, a message) has
nothing to stub: say so and continue.

**Gate** — the project's compile/type check passes.

## Phase 3: Verify-Architecture — skipped

One file, two-way door: nothing to verify against an ADR.

## Phase 4: Specify

Write the specification test for each requirement ID the excerpt names;
it fails against the stub. Commit. A doc-only change has no test — its
`L0` check is the grep against the implementation it documents
([`loop.md`](../hex-core/references/loop.md#review-by-join-level)).

**Gate** — the tests fail for the stated reason.

## Phase 5: Implement

Fill the body until the specification tests pass; run the scoped check
([`verify.md`](../hex-core/references/verify.md#verification)). Commit.

**Gate** — the scoped check passes.

## Phase 6: Review-Fix Loop (inline `L0`, optional `L1`)

Write the `L0` evidence table (`<ID> → <path>:<line>`) yourself and grep
every row. Then **answer the composed `spec` + `quality` sections of
[`checklist.md`](../hex-core/references/checklist.md#composition) as the
reviewer**, item by item, before anything else is committed — the inline
row of its composition table. A failed item is fixed inline, then the
scoped check re-runs.

**One `L1` `reviewer` is spawned only when a non-doc file changed** — the
author≠verifier backstop of universal rule 7. It runs exactly as
[`loop.md` § Review by join level](../hex-core/references/loop.md#review-by-join-level)
defines `L1` (`review.l1.*`, delta-only, one round, inside its budget),
its brief carrying the same two checklist sections. A doc-only diff spawns
nothing.

**Gate** — every checklist item answered; the `L1` verdict, when it ran,
has no actionable finding left.

## Phase 7: Cross-model review — skipped

`adversary: off` at this tier. An explicit `--adversary` runs it under the
[adversary contract](../hex-core/references/adversary.md#adversary-contract),
launched in the same batch as the `L1` backstop — or alone when no `L1`
fires — its actionable findings folded into one inline fix pass. Otherwise
log `Cross-model review skipped: tier=low default` and continue.

## Phase 8: Merge and commit

Already on the feature branch — nothing to merge. Trigger (iii), the final
gate, still fires: the project's full documented verification
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).
Commit with a conventional-commit message. Never push.

## Upkeep and handoff

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step).
Mutate the plan's Status block when there is one (`State: review`,
`Updated`, `Next`). Then emit the handoff from [`SKILL.md`](SKILL.md) with:

```
- Tier: low
- Overlays: review=minimal, loop-rounds=1, adversary=off
- Spawns: 0   (or: 1 — L1 backstop)
```

Required artifacts: the commit(s) on the feature branch; the plan's Status
block advanced when a plan exists.
