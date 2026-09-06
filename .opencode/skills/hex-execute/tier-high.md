# Tier: high

The **default** execution tier — one-way-door-medium plans: a new command, a
new index or storage layout, work spanning 1–2 areas. Preserves the
contract-first TDD skeleton (Stub → Specify → Implement → Review-Fix) with
the join-level Review-Fix Loop with a `full` `L2` checklist. That skeleton is
the shape at effective tier `high`: in a plan carrying the generation marker
a WP whose [effective
tier](../hex-core/references/decompose.md#the-effective-tier)
resolves `medium` runs the collapsed builder and skips Verify-Architecture
instead ([`loop.md`](../hex-core/references/loop.md#the-review-fix-loop)).

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), work-package mechanics in
[`SKILL.md`](SKILL.md#work-packages), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover

Read the plan artifact in full: Status block, component contracts, UX
scenarios, and the Parallelization table (work packages, DAG, merge order).
In parallel, read the project rules for every area the plan's work packages
touch (project context, cached in `hex.md › Pointers`,
[`memory.md`](../hex-core/references/memory.md#the-three-sections)).
**Federation — a plan carrying a `Repo` column:** "every area" resolves **per
owning repo** — a satellite WP's rules are read by an explicit `Read` of **that
repo's** project context, never ambient, since `--add-dir` does not load a
satellite's `CLAUDE.md` (C-306, C-318). Absent a `Repo` column, unchanged.

When the plan defines 2+ file-disjoint work packages, WPs launch
dependency-ready per [`SKILL.md`](SKILL.md#schedule)'s Schedule step:
resolve the feature branch, prepare the ready-set's worktrees before Stub
begins, and run each WP's Phases 2–7 as its dependencies allow.

**Resume.** When the Status block already reads `State: executing` (a
prior run was interrupted), resume at the WP level from the table's
`Status` column rather than re-deriving from branches
([`SKILL.md`](SKILL.md#work-packages)); preparing a ready WP's worktree sets
it to `active`.

**Gate** — the plan's phases and work packages are parsed; project rules for
every touched area are read.

## Phase 2: Stub

For each work package (or the single implicit one), launch **1** `builder`
(focus `stub`) — in a single concurrent batch across the ready work
packages, each scoped to its own declared file set — to create the public
surface with not-implemented bodies. No business logic. Each brief carries
the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).

**A WP whose [effective
tier](../hex-core/references/decompose.md#the-effective-tier)
resolves `medium` runs the collapsed builder instead — Stub, Specify and Implement
in one spawn**
([`loop.md`](../hex-core/references/loop.md#the-review-fix-loop)).
Every other WP, and every WP in a plan without the generation marker, runs
this phase unchanged.

**Gate** — the project's compile/type check passes for every work package.

## Phase 3: Verify-Architecture

Launch **1** `reviewer` (focus `spec`, phase `post-stub`) per work package to
validate stubs against the plan's component contracts: signatures match,
module boundaries align, error variants cover the documented failure modes.
*Optional when the whole plan touches ≤3 files. A WP skips this phase only
by deriving `medium` ([the effective
tier](../hex-core/references/decompose.md#the-effective-tier)).*
Each brief carries the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).

**Gate** — every reviewer reports pass.

## Phase 4: Specify

For each work package, launch **1** `tester` (focus `specification`) to
write unit and acceptance tests from the plan's component-contracts and
user-experience sections — NOT from the stubs. Tests cite the `C-`/`S-`
IDs they cover
([`protocol.md`](../hex-core/references/protocol.md#traceability-ids)). Each
brief carries the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).
Tests MUST fail against the stubs.

**Skipped for a WP that ran the collapsed builder** — its tests were written
there ([Phase 2](#phase-2-stub)).

**Gate** — tests compile/parse and fail with not-implemented against the
stubs, for every work package; every plan ID has at least one failing
test.

## Phase 5: Implement

For each work package, launch **1** `builder` (focus `implement`) to fill
stub bodies until its specification tests pass. Each brief carries the
excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).

**Skipped for a WP that ran the collapsed builder** — its implementation was
written there ([Phase 2](#phase-2-stub)), which paid this gate.

**Gate** — the [scoped check](../hex-core/references/verify.md#scoped-check)
passes, per work package.

## Phase 6: Review-Fix Loop (by join level, `full` checklist)

Run the [Review-Fix Loop](../hex-core/references/loop.md#the-review-fix-loop)
at the join levels [`loop.md` § Review by join
level](../hex-core/references/loop.md#review-by-join-level) defines — the
sole definition, never restated here:

- **`L0`** on every builder return — the evidence table, grep-verified by
  this orchestrator; an unverifiable row goes back to the builder once.
- **`L1`** at every leaf join, per WP in its own worktree before merge —
  **1** `reviewer` (focus `spec`, phase `post-implementation`), its brief
  carrying the `spec` + `quality` sections of
  [`checklist.md`](../hex-core/references/checklist.md#composition), reading
  `git diff <base>..<head>` and the excerpt only
  ([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
  rule 8), **1 round**, inside its budget.
- **`L2`** once, at this run's end-of-run join over the feature branch,
  only when **two or more** WPs landed — **1** deep-reasoning `reviewer`
  carrying the `review=full` sections of
  [`checklist.md`](../hex-core/references/checklist.md#composition)
  ([`overlays.md`](overlays.md#review-axis)): `spec` and `quality`, plus
  `security` when the diff touches security-sensitive paths, `performance`
  when it touches a hot path or async code, `docs` when doc-drift triggers
  match, and any `perspectives.always` rule that matches. Leaf verdicts are
  inputs. Skipped at `N = 1`.

A `sec`, `hot` or `door` flag, or a `risk` cell, raises the WP one level,
never a round. A WP arriving under a decomposing coordinator is already
reviewed at `L1` per sub-WP and `L2` at its join; it is not re-reviewed
at `L1` here.

**Cross-model code-diff review** (when `adversary=on` — auto-on for
one-way-door signals, or explicit `--adversary`): launched **in the same
batch as the terminal join's native seat** — the `L2` seat, or the sole
leaf's `L1` at `N = 1` — last in that batch, never after it. One-shot,
4-way triage; its actionable findings join that join's single `builder`
(focus `implement`) fix pass, one re-verify; graceful skip when unavailable
([adversary contract](../hex-core/references/adversary.md#adversary-contract)).

**Gate** — the loop's
[exit gate](../hex-core/references/loop.md#the-review-fix-loop) at each
level's `rounds`; deferred findings, budget residue and cross-model
findings are documented.

## Phase 7: Merge and commit

When the plan ran 2+ work packages in worktrees: merge each onto the plan's
feature branch, serialized in a valid topological order per
[`SKILL.md`](SKILL.md#work-packages) — one WP at a time. Before each merge,
run the merge-time file-set re-validation;
a **scoped check** runs after every merge — the project's full documented
verification only on the merge-triggered ones
[`worktree.md` § Worktree work-package mechanics](../hex-core/references/worktree.md#worktree-work-package-mechanics)
names — and a merge conflict or a failed post-merge verification follows
the merge-conflict / post-merge-failure playbook
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).
Set the table's `Status` column to `merged` on a successful merge, `failed`
on a playbook halt; ephemeral branch deleted and worktree removed once its
WP merges. Commit per completed work package (or once, for a
single-package plan) with a conventional-commit message on the feature
branch. Never push — landing the feature branch on the trunk is the
human's step.

**Federation — a WP whose `Repo` is a satellite key (C-306/C-307).** Its merge
is `git -C <repo> merge` onto **that repo's** `hex/<plan-slug>`; the commit
carries the `Hex-Plan:` trailer, and the post-merge verification is the **owning
repo's**, read by an explicit `Read` of that repo's project context — never
ambient ([`SKILL.md` § Work packages](SKILL.md#work-packages)). Merge order is
one global topological sequence across all repos, one at a time
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).
Absent a `Repo` column this is inert.

**Gate** — trigger (iii), the final gate, fires once at the end of the run,
however many work packages merged — a single-package plan included
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).

## Upkeep and handoff

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step):
re-point any `hex.md › Pointers` entry this run revealed as drifted (a
changed verification command, a new worktree location), and update
`hex.md › Memory` with anything worth persisting (e.g. a review
perspective that mattered).
**Federation — a plan carrying a `Repo` column:** upkeep additionally offers to
confirm each `Repos:`-ledger row's landing, advancing a
`landing` plan to `done` only when every row is landed (C-324), and on `done`
removes the plan's slug from each satellite's `Federation lead:` bullet —
deleting the bullet, and the file if it held nothing else (C-313). Both are
defined in full in [`protocol.md` § Upkeep step](../hex-core/references/protocol.md#upkeep-step).
When the target is a plan artifact, mutate its Status block:
`State: review`, `Updated` refreshed, `Next: /hex-review <plan path>` (see
[`SKILL.md`](SKILL.md#the-plan-artifact)). Then emit the handoff from
[`SKILL.md`](SKILL.md) with:

```
- Tier: high
- Overlays: review=full, loop-rounds=1, adversary=<on|off>
```

The handoff block also prints the **six-figure rollup** of the run's timings,
per [`protocol.md` § Handoff contract](../hex-core/references/protocol.md#handoff-contract).

Required artifacts: the plan (Status block advanced to `review`), the
commit(s) on the feature branch, and any research artifact Implement
uncovered a surprise worth persisting (rare at this tier — an ADR-level
surprise means the plan was underspecified; re-route through
`/hex-plan xhigh` rather than inlining one here).
