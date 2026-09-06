# Tier: xhigh

The full treatment for **one-way-door-high** plans — a new module or
package, a breaking API, a cross-area refactor, a protocol or
storage-layout change. Preserves contract-first TDD, and adds the
`adversarial` `L2` checklist (ADR-compliance and known-pitfall checks) and a
mandatory cross-model code-diff gate before commit. That skeleton is the shape
at effective tier `xhigh`: in a plan carrying the generation marker a WP whose
[effective
tier](../hex-core/references/decompose.md#the-effective-tier) resolves `medium`
runs the collapsed builder and skips Verify-Architecture instead
([`loop.md`](../hex-core/references/loop.md#the-review-fix-loop)).

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), work-package mechanics in
[`SKILL.md`](SKILL.md#work-packages), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

**Meta-plan preview is mandatory.** At `xhigh`, the gate in
[`SKILL.md`](SKILL.md) step 5 always blocks for explicit approval — this
tier is expensive, and the preview catches a misclassification before
workers launch.

## Phase 1: Discover

Read the plan artifact in full: Status block, the linked ADR (if the plan
references one), component contracts, and every work package. In parallel,
read the project rules for **every** area the plan's work packages touch,
and any linked research artifacts.
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

**Gate** — the plan, its ADR (if any), and every touched area's project
rules are read.

## Phase 2: Stub

For each work package, launch **1** `builder` (focus `stub`); model class
resolves through [`models.md`](../hex-core/references/models.md)
(`builder:stub` — the fast-balanced baseline, escalated on judgment for
cross-area or new-module scaffolding, announced with its reason). Each brief
carries the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).

**A WP whose [effective
tier](../hex-core/references/decompose.md#the-effective-tier)
resolves `medium` runs the collapsed builder instead — Stub, Specify and Implement
in one spawn**
([`loop.md`](../hex-core/references/loop.md#the-review-fix-loop)).
Every other WP, and every WP in a plan without the generation marker, runs
this phase unchanged.

**Gate** — the project's compile/type check passes **across the whole
workspace**, not just the touched work packages — cross-area implications
must surface immediately. In a federated run "the whole workspace" is the
workspace of the repo the gate runs in, never a cross-repo aggregate (C-321).

## Phase 3: Verify-Architecture (reviewer + architect)

Launch **in a single concurrent batch**, per work package (a WP skips this
phase only by deriving `medium` ([the effective
tier](../hex-core/references/decompose.md#the-effective-tier))). Each brief
carries the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8) — except the `architect`, which reads in full the ADR or design
document its brief names as its compliance target, per that rule's
architect carve-out:

- **1** `reviewer` (focus `spec`, phase `post-stub`).
- **1** `architect` — validates stubs against the plan's ADR: are the
  boundaries honored, the trade-offs implemented as decided, any area
  boundary violated?

Architect findings here are first-class: if stubs diverge from the ADR,
**stop and re-stub** before writing any tests.

**Gate** — both report pass; ADR compliance confirmed.

## Phase 4: Specify

For each work package, launch **1** `tester` (focus `specification`); model
class resolves through
[`models.md`](../hex-core/references/models.md) (`tester` — deep-reasoning class
at this tier). Cover edge cases exhaustively: boundary conditions,
concurrent access, failure modes, cross-area interactions. Unit and
acceptance tests both required, each citing the `C-`/`S-` IDs it covers
([`protocol.md`](../hex-core/references/protocol.md#traceability-ids)). Each
brief carries the excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).

**Skipped for a WP that ran the collapsed builder** — its tests were written
there ([Phase 2](#phase-2-stub)).

**Gate** — tests compile/parse, fail with not-implemented against the
stubs; coverage matches the plan's documented edge-case list; every plan
ID has at least one failing test.

## Phase 5: Implement

For each work package, launch **1** `builder` (focus `implement`); model
class deep-reasoning at this tier
([`models.md`](../hex-core/references/models.md)). Each brief carries the
excerpt, not the plan body
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8).

**Skipped for a WP that ran the collapsed builder** — its implementation was
written there ([Phase 2](#phase-2-stub)), which paid this gate.

**Gate** — the [scoped check](../hex-core/references/verify.md#scoped-check)
passes.

## Phase 6: Review-Fix Loop (by join level, `adversarial` checklist)

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
  carrying the `review=adversarial` sections of
  [`checklist.md`](../hex-core/references/checklist.md#composition)
  ([`overlays.md`](overlays.md#review-axis)): the `full` set, plus
  `architecture` and `pitfalls`, and any `perspectives.always` rule that
  matches. For the `architecture` section this seat reads in full the ADR
  or design document its brief names, per rule 8's architect carve-out.
  Leaf verdicts are inputs. Skipped at `N = 1`.

A `sec`, `hot` or `door` flag, or a `risk` cell, raises the WP one level,
never a round; at `L2` it forces the security and performance checklists
on. A WP arriving under a decomposing coordinator is already reviewed at
`L1` per sub-WP and `L2` at its join; it is not re-reviewed at `L1` here.

**Cross-model code-diff review — a default part of this tier's flow.**
Launched **in the same batch as the terminal join's native seat** — the
`L2` seat, or the sole leaf's `L1` at `N = 1` — last in that batch, never
after it. One-shot, no loop; 4-way triage (actionable / deferred /
stated-convention / trivia); its actionable findings join that join's
single `builder` (focus `implement`) fix pass, one re-verify. If that
merged pass fails verification, **revert it, re-run it native-only, and
promote every cross-model finding to deferred** rather than looping
([adversary contract](../hex-core/references/adversary.md#adversary-contract)).
If the adversary produces no review — the skill is unavailable, or it ran and
did not complete one — log
`Cross-model review skipped: <reason>` and continue — but **surface the skip
prominently in the handoff**, since one review layer was missed.

**Gate** — the loop's
[exit gate](../hex-core/references/loop.md#the-review-fix-loop) at each
level's `rounds`; deferred findings, budget residue and cross-model
findings are documented.

## Phase 7: Merge and commit

Merge work packages onto the plan's feature branch, serialized in a valid
topological order per [`SKILL.md`](SKILL.md#work-packages) — one WP at a
time. Before each merge, run the merge-time file-set re-validation;
a **scoped check** runs after every merge — the project's full documented
verification only on the merge-triggered ones
[`worktree.md` § Worktree work-package mechanics](../hex-core/references/worktree.md#worktree-work-package-mechanics)
names — and a merge conflict or a failed post-merge verification follows
the merge-conflict / post-merge-failure playbook
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).
Set the table's `Status` column to `merged` on a successful merge, `failed`
on a playbook halt; ephemeral branch deleted and worktree removed once its
WP merges. Commit per completed work package with conventional-commit
messages on the feature branch. Never push — landing the feature branch on
the trunk is the human's step.

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

Surface prominently in the handoff:

- whether the cross-model gate ran or skipped (with reason);
- any ADR-compliance concern the `architect` flagged as deferred;
- any SOTA gap the `researcher` flagged as deferred.

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
Mutate the plan's Status block: `State: review`, `Updated` refreshed,
`Next: /hex-review <plan path>` (see
[`SKILL.md`](SKILL.md#the-plan-artifact)). Then emit the handoff from
[`SKILL.md`](SKILL.md) with:

```
- Tier: xhigh
- Overlays: review=adversarial, loop-rounds=1, adversary=on
```

The handoff block also prints the **six-figure rollup** of the run's timings,
per [`protocol.md` § Handoff contract](../hex-core/references/protocol.md#handoff-contract).

Required artifacts: the plan (Status block advanced to `review`), the
commit(s) on the feature branch, and — if implementation revealed a decision
the original ADR didn't cover — a follow-up ADR request escalated to
`/hex-plan xhigh` rather than inlined here.
