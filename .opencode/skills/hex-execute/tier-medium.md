# Tier: medium

Minimal execution for **two-way-door** plans — a flag or option, a doc edit
plus code, a single-area tweak of ≤3 files. A WP whose [effective
tier](../hex-core/references/decompose.md#the-effective-tier)
resolves `medium` runs the **collapsed** pipeline — Stub, Specify and Implement
in one builder spawn, under the conditions
[`loop.md`](../hex-core/references/loop.md#the-review-fix-loop) states.
In a plan without the generation marker the four-phase skeleton below runs
unchanged; either way only the worker count scales down — review depth is
keyed on join level, not tier.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), work-package mechanics in
[`SKILL.md`](SKILL.md#work-packages), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover

Read the plan artifact (or, for a free-text target, the task description
directly) and its Parallelization table — medium tier is normally a single work
package. In parallel, read the project rules for the area touched (project
context, cached in `hex.md › Pointers`,
[`memory.md`](../hex-core/references/memory.md#the-three-sections)) and the
specific code region the plan or prompt names. On resume (`State:
executing`), read the table's `Status` column for the WP's progress
([`SKILL.md`](SKILL.md#work-packages)).
**Federation — a plan carrying a `Repo` column:** the area touched resolves
**per owning repo** — a satellite WP's rules are read by an explicit `Read` of
**that repo's** project context, never ambient, since `--add-dir` does not load
a satellite's `CLAUDE.md` (C-306, C-318). Absent a `Repo` column, unchanged.

**Gate** — the target files are identified; the work package (if the plan
defines one) is confirmed single.

## Phase 2: Stub

Launch **1** `builder` (focus `stub`) to create the public surface — types,
signatures, error variants — with not-implemented bodies. No business logic.
Its brief carries the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).

**A WP whose [effective
tier](../hex-core/references/decompose.md#the-effective-tier)
resolves `medium` runs the collapsed builder instead — Stub, Specify and Implement
in one spawn**
([`loop.md`](../hex-core/references/loop.md#the-review-fix-loop)).
Every other WP, and every WP in a plan without the generation marker, runs
this phase unchanged.

**Gate** — the project's compile/type check passes.

## Phase 3: Verify-Architecture — skipped

Two-way door, ≤3 files: skip the `reviewer` architecture pass. If Discover
revealed a larger scope, **stop and re-run** as `/hex-execute high <target>`
rather than silently upgrading mid-flow.

**Gate** — the skip is logged in the announcement; proceed to Specify.

## Phase 4: Specify

Launch **1** `tester` (focus `specification`) to write unit tests from the
plan's component contracts (or, free-text, from the stated behavior),
citing the `C-`/`S-` IDs each test covers
([`protocol.md`](../hex-core/references/protocol.md#traceability-ids)). Its
brief carries the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).
Acceptance tests are optional at this tier — add one only when the change is
user-visible. Tests MUST fail against the stubs.

**Skipped for a WP that ran the collapsed builder** — its tests were written
there ([Phase 2](#phase-2-stub)).

**Gate** — tests compile/parse and fail with not-implemented against the
stubs; every plan ID is covered by at least one failing test.

## Phase 5: Implement

Launch **1** `builder` (focus `implement`) to fill stub bodies until the
specification tests pass. Its brief carries the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).

**Skipped for a WP that ran the collapsed builder** — its implementation was
written there ([Phase 2](#phase-2-stub)), which paid this gate.

**Gate** — the [scoped check](../hex-core/references/verify.md#scoped-check)
passes.

## Phase 6: Review-Fix Loop (by join level)

Run the [Review-Fix Loop](../hex-core/references/loop.md#the-review-fix-loop)
at the join levels [`loop.md` § Review by join
level](../hex-core/references/loop.md#review-by-join-level) defines — the
sole definition, never restated here. At this tier that is `L0` on the
builder's return (evidence table, grep-verified, no spawn) and `L1` at the
WP's join: **1** `reviewer` (focus `spec`, phase `post-implementation`),
its brief carrying the `spec` + `quality` sections of
[`checklist.md`](../hex-core/references/checklist.md#composition),
delta-only, **1 round**, inside its budget. Each brief carries the excerpt,
not the plan body ([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8). `L2` fires only when this run joins **two or more** WPs — the
usual single-WP `medium` plan skips it. A `sec`, `hot` or `door` flag, or a
`risk` cell, raises the WP one level, never a round.

**Gate** — the loop's
[exit gate](../hex-core/references/loop.md#the-review-fix-loop), bounded
by the level's `rounds`; budget residue goes to the handoff.

## Phase 7: Cross-model review — skipped

Two-way door: skip (`adversary: off` at this tier). If the user passes
`--adversary` explicitly, run it anyway (user override) under the
[adversary contract](../hex-core/references/adversary.md#adversary-contract)
— launched in the same batch as the `L1` seat, last, its actionable findings
joining that seat's single fix pass; otherwise log
`Cross-model review skipped: tier=medium default` and continue.

## Phase 8: Merge and commit

Single work package — no worktree to merge. Commit the change on the plan's
feature branch
([`SKILL.md`](SKILL.md#work-packages)) with a conventional-commit message.
Never push. The merge-time file-set re-validation and merge-conflict /
post-merge-failure playbook apply only when a worktree merge happens — n/a
on this tier's single-WP direct path
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).
Print the Deferred Findings summary even when empty — it confirms the
pipeline ran to completion.

**Gate** — trigger (iii), the final gate, still fires on this single-WP
direct path
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).

**Federation — a WP whose `Repo` is a satellite key (C-306/C-307).** Commit and
merge run in the owning repo via `git -C <repo>`, onto that repo's
`hex/<plan-slug>`; the commit carries the `Hex-Plan:` trailer, and the
post-merge verification is the **owning repo's**, read by an explicit `Read` of
that repo's project context — never ambient
([`SKILL.md` § Work packages](SKILL.md#work-packages);
[`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).
Absent a `Repo` column this is inert.

## Upkeep and handoff

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step):
re-point any `hex.md › Pointers` entry this run revealed as drifted, and
update `hex.md › Memory` with anything worth persisting.
**Federation — a plan carrying a `Repo` column:** upkeep additionally offers to
confirm each `Repos:`-ledger row's landing, advancing a
`landing` plan to `done` only when every row is landed (C-324), and on `done`
removes the plan's slug from each satellite's `Federation lead:` bullet —
deleting the bullet, and the file if it held nothing else (C-313). Both are
defined in full in [`protocol.md` § Upkeep step](../hex-core/references/protocol.md#upkeep-step).
When the target is a plan artifact, mutate its Status block:
`State: review`, `Updated` refreshed, `Next: /hex-review <plan path>` (see
[`SKILL.md`](SKILL.md#the-plan-artifact)); a free-text target has no Status
block to mutate — note that instead. Then emit the handoff from
[`SKILL.md`](SKILL.md) with:

```
- Tier: medium
- Overlays: (none)
```

The handoff block also prints the **six-figure rollup** of the run's timings,
per [`protocol.md` § Handoff contract](../hex-core/references/protocol.md#handoff-contract).

Its only required artifacts are the commit itself and, when a plan exists,
its advanced Status block. No ADR or research artifact at this tier — if the
pipeline reveals a need for either, stop and re-route through
`/hex-plan high`.
