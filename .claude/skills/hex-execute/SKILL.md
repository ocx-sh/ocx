---
name: hex-execute
description: Multi-agent execution orchestrator — implements an approved hex-plan artifact (or a free-text task) as parallel pipelines of serial steps cut along contracts, merges them onto a feature branch, runs one integration gate concurrent with one review, and hands to /hex-finalize. Use to execute, implement, build, or run an approved plan, resume an interrupted execution, or turn a design into tested, reviewed, committed code.
license: Apache-2.0
metadata:
  summary: Swarm execution — plan artifact to reviewed, gated branch
  keywords: execution,swarm,multi-agent,tdd,implementation,pipelines,worktrees
  repository: https://github.com/michael-herwig/arcana
---

# hex-execute — Execution Orchestrator

Thin dispatcher. A plan is a few **pipelines** cut along contracts; a pipeline
is a chain of **steps** in one worktree. This file resolves the target, runs
the **contract wave**, dispatches steps, merges, runs the **integration gate**
concurrent with the **review call**, fixes, and hands to `/hex-finalize`.
Vocabulary lives in `hex-core` and is **linked here, never copied**.

Shared contracts:
[`protocol.md`](../hex-core/references/protocol.md) ·
[`workers.md`](../hex-core/references/workers.md) ·
[`models.md`](../hex-core/references/models.md) ·
[`memory.md`](../hex-core/references/memory.md) ·
[`worktree.md`](../hex-core/references/worktree.md) ·
[`verify.md`](../hex-core/references/verify.md).
If `hex-core` is not installed: `grim add ghcr.io/michael-herwig/arcana/hex-core:latest`.

## Argument syntax

```
/hex-execute [target] [--dry-run]
```

- **target** (optional): a plan artifact path; a free-text task (no plan
  artifact, one pipeline); or omitted — resolve the active-plan pointer from
  `hex.md › Memory`
  ([`memory.md`](../hex-core/references/memory.md#the-three-sections)).
- `--dry-run` — print the announce block and stop; no worker launches.

## The run

Counts are fixed ([`DESIGN.md`](../DESIGN.md)): **two** full gates (integration,
release), **≤ 3** review calls, workers **never** wait, escalation by the
orchestrator only.

### 1. Resolve memory and target

Locate `.agents/memory/hex.md` searching upward; a missing file is normal
([`memory.md`](../hex-core/references/memory.md#location-and-resolution)) —
use shipped defaults and note that `/hex-init` can create one. When present,
read it whole: `Pointers` (verification, hub/generated files, fresh-build
switch, worktree location), `Preferences` (model-class agent definitions,
limits), `Memory` (active-plan pointer). A `Federation lead:` bullet means this
repo is a satellite: **halt** per
[`memory.md`](../hex-core/references/memory.md#location-and-resolution).

Resolve the target: explicit plan path (read it whole), active-plan pointer
(none recorded: stop and ask), or free text (scope contracts inline).
A plan carrying a `Repo` column: follow [`federation.md`](federation.md) before
anything else is written.

Plan `State` decides entry: `plan-approved` is normal. `executing` is a prior
run — resume from the pipeline table's `Status` column
(`pending | active | merged | failed`), resetting an unfinished pipeline to its
last committed step; branches and worktrees are evidence only, the Implementation
Steps checkboxes are the finer progress, and a resuming run **never reads the
per-run scratch root**. `landing` is a finalize-only re-entry
([`federation.md`](federation.md#landing-re-entry-and-upkeep)). `review` or
`done`: stop and report; the human decides whether to re-execute.

### 2. Announce

No approval gate — print the resolved config and proceed (the user can abort):

```
hex-execute
  Target:      .agents/plans/plan_cache.md
  Pipelines:   3 (A, B, C) — contract wave first, then A ∥ B ∥ C
  Jobs:        3 live × 4 = 12            (project single-build jobs: 12)
  Classes:     steps standard; B step 4 standard-high (plan mark)
  Review:      1 call after all pipelines land
  Gates:       integration (with review) · release (/hex-finalize)
  Branches:    feature hex/plan-cache; per pipeline hex/plan-cache--<pipeline>
```

Plus the federation pre-flight lines, when federated. A client that cannot spawn
subagents announces `Degraded: inline workers` and runs each brief inline,
sequentially ([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)).
Then set the plan's `State: executing`.

### 3. Contract wave

Resolve the feature branch — the non-trunk branch already checked out, else
`hex/<plan-slug>` from the trunk
([`worktree.md`](../hex-core/references/worktree.md#pipeline-worktree-mechanics)).
A **single pipeline** (small task) skips the wave and the worktree: its steps
run directly on the feature branch.

Otherwise one or a few `standard` steps write the **stubs plus contract tests**
for every pipeline — public surface with not-implemented bodies, and the tests
that pin each cross-pipeline contract, derived from the plan's component
contracts, not from the stubs. Commit once. That commit is the frozen base of
every pipeline.

### 4. Pipelines and steps

Each pipeline gets a branch `hex/<plan-slug>--<pipeline-slug>` and a worktree
([`worktree.md`](../hex-core/references/worktree.md#pipeline-worktree-mechanics)),
all created from the wave commit; all pipelines start together. Pipelines own
disjoint file sets. Inside a pipeline: serial steps, shared worktree, no merge,
no review, no gate between steps. The orchestrator spawns every step itself
(foreground results reach it; there is no driver agent in between) and starts a
pipeline's next step the instant the previous returns.

**Step.** A fresh agent of class `standard` (`standard-high` only where the plan
marks the step — at most 1 pipeline in 4), one small brief
([`workers.md`](../hex-core/references/workers.md#universal-worker-protocol)
rule 8: the excerpt, not the plan body). The step does contract-first TDD
inside itself — specify, implement — and commits with `--no-verify`. Brief
states:

- **Step feedback is the only inner check.** A behavioural step runs the tests
  it wrote or touched, once, with the narrowest command it picks (e.g.
  `cargo test -p x`), never the project's gate wrapper. A non-behavioural step
  (docs, taskfiles, config, renames, comments) runs **nothing** — "will this be
  exercised later anyway?" means skip.
- **Every commit and merge on hex-owned branches uses `--no-verify`.** This
  run's integration and release gates satisfy the project's
  "verify before commit" instructions; do not run the full gate.
- **Never wait** on a lock, a gate or a poll — return instead. No worker-side
  locks. A heavy or exclusive tool (bazel server, docker acceptance project) is
  gate-only; a step that needs a one-off exclusive check hands it to the
  orchestrator and does not wait.
- **Never commit hub or generated files** (lockfiles, baselines, goldens; the
  `hex.md › Pointers` list).
- Parallel builds: use at most the pipeline's `jobs` allotment.

**Job budget.** live pipelines × jobs per pipeline ≤ the project's single-build
jobs. The orchestrator sets the allotment in every brief.

**Contract drift.** At each step return, `git diff <wave-commit>` over the
contract-wave files. A touched contract: the orchestrator applies the edit to
the feature branch, merges it into the affected pipelines' branches, and states
the change in their next brief. No review is triggered.

**Failure and escalation** (orchestrator only,
[`models.md`](../hex-core/references/models.md)): a step has failed when it
returned incomplete or its output was rejected; a red TDD phase is not a failure.
Same step failing again: retry at the same class with the failure attached →
one class up → defer as residue and keep the loop going.

**Pipeline done** = its steps all returned. Merge it onto the feature branch
with `--no-verify`, one pipeline at a time in dependency order, never a batch;
no gate on the merge. Set its table `Status: merged`. Merge-time file-set
re-validation and the conflict playbook are
[`worktree.md`](../hex-core/references/worktree.md#pipeline-worktree-mechanics)'s.

### 5. Integration gate ∥ review call

When every pipeline has merged, **in the background and concurrently**:

- **Integration gate** — the project's full documented verification on the
  feature branch ([`verify.md`](../hex-core/references/verify.md#verification)),
  after regenerating the hub and generated files once, minimally (no
  `cargo update`).
- **Review call** — one autonomous `/hex-review` over `anchor..HEAD`: staged
  panel and codex, seats split per pipeline plus one for the seams.
  `/hex-review` owns seats, rounds and the
  [last-reviewed anchor](../hex-core/references/loop.md#the-last-reviewed-anchor).

Mid-run review calls: [`loop.md` § Review calls](../hex-core/references/loop.md#review-calls).

**One fix pass serves both**: gate failures and the review's findings go to
`standard` fixers per pipeline, in parallel, then the gate runs again. Red →
one fix pass → gate again; at most 2 fix passes, then stop and hand the
residue to the user. A gate that failed on infrastructure is rerun once.

### 6. Hand off

Set the plan's `State: review`, `Next: /hex-finalize`, `Updated` refreshed (a
free-text target has no Status block — say so in the handoff). Run the
[upkeep step](../hex-core/references/protocol.md#upkeep-step). Never push.
Emit the handoff
([contract](../hex-core/references/protocol.md#handoff-contract)):

```markdown
## Execution Complete: <feature | plan name>

### Run
- Pipelines: <n> merged, <n> deferred · contract wave <sha>
- Integration gate: green | red after 2 fix passes (<cause>)
- Review calls: <n> · anchor <sha>

### Artifacts
- <plan path> (Status block advanced to `review`)
- <feature branch and tip>

### Deferred findings (need human judgment)
- Review: …
- Residue: … (steps deferred after escalation)

### Next step
    /hex-finalize
```

A plan carrying a `Repo` column adds the per-repo landing order and
back-pointer list ([`federation.md`](federation.md#handoff-addendum)).

## Plan artifact

Co-owned with `/hex-plan` and `/hex-review`; no external state file.

```markdown
## Status
- State:   executing      <!-- planning → plan-approved → executing → review → done -->
- Updated: 2026-07-19
- Next:    /hex-execute <this plan path>
```

- **Living design record.** When execution reveals a behaviour the plan did not
  specify, update the plan first, then the test, then the code.
- **Spec deltas (C-419).** On each pipeline merge, append its delta entries to
  the plan's single `## Spec Deltas` block — one `Target:` for the plan
  (C-415), append-only, never rewriting another pipeline's entry. A `MODIFIED`
  entry's `Base:` is the **pasted output** of archive.md's stale-base sub-ID scan
  ([C-405](../hex-core/references/archive.md#safety-envelope)) over the **live**
  destination. A satellite-delivered entry (`Repo` ≠ `.`) defers rather than
  folding (C-415 clause 5). Grammar and guards:
  [`archive.md` § Delta grammar](../hex-core/references/archive.md#delta-grammar).

## Constraints

- Project rules, verification and artifact conventions come from project context
  and the `hex.md › Pointers`, never a hardcoded table; no documented
  verification means detect one command once and suggest `/hex-init`.
- The orchestrator's main loop only dispatches and merges; anything slow runs in
  the background. Event-driven on step returns, with a fallback tick of at
  least 20 minutes. Two levels: `/hex-loop` above, this run below.
- Never exceed the concurrency cap
  ([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)).
- **Never push to remote.** Never leave finished work uncommitted.
- No mid-flow questions; a scope surprise stops and re-routes to `/hex-plan`.

$ARGUMENTS
