# hex Run Loop

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## The Review-Fix Loop

The run loop. **This is the only copy in the bundle — every other file
links here.** Bounded by count, never by tier.

**Contract wave → pipelines → integration gate ‖ review call → fix →
release.**

1. **Contract wave** — stubs **plus contract tests** for every pipeline,
   committed once on the feature branch. Contract-first TDD starts here:
   the tests are red against the stubs, and the wave checks only that the
   surface builds ([step feedback](verify.md#step-feedback)). A **small
   task is one pipeline** and has no contract wave.
2. **Pipelines** — all start together from the contract-wave commit. A
   pipeline is an ordered chain of **steps** in one worktree
   ([pipeline worktrees](worktree.md#pipeline-worktree-mechanics)); each
   step is a fresh `standard` agent with a small brief. Inside a pipeline:
   serial, shared state, no merge, no review, no gate between steps.
   Across pipelines: parallel, sharing only the contracts. Inside a step:
   failing test first, then the body until green; a red phase is not a
   failure. The only inner check is [step feedback](verify.md#step-feedback).
   Commits use `--no-verify`, and the step brief says the run's gates
   satisfy the project's verify-before-commit instructions
   ([commits and hooks](verify.md#commits-and-hooks)).
   - A step that edits a contract-wave file shows in `git diff` at its
     return; the orchestrator re-briefs the affected pipelines. No review is
     triggered.
   - A pipeline reviews itself only where the plan marks it.
3. **Integration** — once every pipeline has landed
   ([merge and regenerate](worktree.md#pipeline-worktree-mechanics)), the
   [integration gate](verify.md#the-two-gates) and the review call run
   **concurrently**.
4. **Fix** — one fix pass serves both: the review's actionable findings
   and the gate's failures, run per pipeline in parallel.
5. **Release** — `/hex-finalize` runs the
   [release gate](verify.md#the-two-gates).

**Workers never wait** on a lock, a gate or a poll; a worker that would
wait returns. The orchestrator only dispatches and merges; anything slow
runs in the background, event-driven with a ≥ 20 min fallback.

**Escalation is the orchestrator's alone**, on repeated failure of the same
step ([`models.md`](models.md)): retry at the same class with the failure
attached, then one class up, then defer as residue and keep the loop going.

### Review calls

- **Default: one `/hex-review` after all pipelines land.** The orchestrator
  may add at most two more mid-run, for a very big finished chunk with much
  still to come: **three calls per run at most**. Each reads `anchor..HEAD`
  ([the last-reviewed anchor](#the-last-reviewed-anchor)), so later edits to
  reviewed code are included next time.
- **Seats are `standard` at every tier**; tier scales the seat count only.
  Seats are split per pipeline, plus one for the seams between pipelines.
  `deep` seats run only when the user asks.
- **Adversary seat** (cross-model, tier-scaled) — launches in the same batch
  as the native seats, never after them; its actionable findings join the
  same fix pass; one-shot, never loops. Unavailable → log a skip line and
  never wait for its quota. Batch order, clocks, triage and the failure path
  are the [Adversary contract](adversary.md#adversary-contract)'s, restated
  nowhere else.

### The loop

- **Round 1** — the review call runs its seats concurrently,
  blockers-first (spec, correctness). Classify each finding:
  - **Actionable** — a fixer fixes it; re-run only the affected
    perspectives next round.
  - **Deferred** — surface it in the summary with context; never block the
    loop on it.
- **Fix rounds** — fix, then re-check the **fix delta only**
  ([delta scope](#delta-round-scope)), re-running only the perspectives
  with actionable findings. A finding that surfaces two rounds running
  (oscillating) auto-defers.
- **Cap: 2 fix rounds** (a red gate re-runs after its fix pass under the
  same cap, [the two gates](verify.md#the-two-gates)). Actionable findings
  left after the cap: **stop and escalate to the user** with the outstanding
  list. A `loop rounds` value in `hex.md › Preferences` or a `--loop-rounds`
  flag may lower the cap, never raise it.
- **Plan-artifact scope** (a draft plan or ADR under review by its own
  orchestrator): **one** round → the orchestrator applies actionable
  fixes → **one** re-validation pass by `reviewer` (focus `spec`) *only when
  a Block-severity finding was fixed* → anything still actionable escalates
  to the user. **Each decision is reviewed once across the chain**: a plan
  built from an accepted ADR reviews only its decomposition, never the
  ADR's decisions again ([`/hex-plan`](../../hex-plan/SKILL.md)). Re-enabling multi-round artifact loops takes an **explicit**
  `artifact loop rounds: N` limit in `hex.md › Preferences`.
- **Exit** — no actionable findings remain, the integration gate is green
  on the final state, and deferred findings are documented for handoff.

### The last-reviewed anchor

**One rule, two scopes, one of them persisted.**
A review round reads `<last-reviewed>..HEAD`, where `<last-reviewed>` is the
SHA the previous round **of the same scope** reviewed.

- **Round scope** — across the fix rounds inside one review call, the
  anchor is the SHA round N−1 reviewed. It is held in the orchestrator's
  session state and **is not persisted**.
- **Branch scope** — across review calls and `/hex-review` invocations on
  the feature branch, the anchor must survive the session and **is
  persisted** as one Status-block line: `- Reviewed: <full 40-char SHA>`,
  following the `Repos:` ledger's full-SHA precedent (C-324) for exactly the
  same reason — a short SHA or a ref name is not a stable identity.
  **Placement: immediately after `Next:` and *before* the `Repos:`
  ledger**, because the `Repos:` ledger is multi-row and unbounded, so a
  line placed behind it has no stable position.
- **One writer rule** — whoever completes a review pass over a diff whose
  head is `<sha>` writes `Reviewed: <sha>`. It means precisely *"every
  commit reachable from this SHA has been through at least one review
  pass"* — nothing about verdicts, and nothing about whether findings
  remain. A round-scope round **never** writes the field. Absent field ⇒
  never reviewed ⇒ full-branch review.

### Anchor validation

**One predicate, fail-safe.** Before a persisted anchor
is used, assert that it lies **inside the range this review is about**;
reachability alone is not enough. **Validation runs in two steps, in this
order:** the fallback baseline is resolved first — the PR's fetched base ref,
else `main` — the anchor is validated against *that* value, and only a valid
anchor then substitutes as the round's baseline. **Two tests, both required:**
`git merge-base --is-ancestor <anchor> <HEAD>` **must pass** (the anchor is
reachable), **and**
`git merge-base --is-ancestor <anchor> <resolved-baseline>`
**must fail** (the anchor is not already behind the baseline). The second
test closes a fail-open hole the first cannot: a trunk SHA, or any common
ancestor, is a perfectly good ancestor of HEAD, so a one-test check would
accept it and review only `trunk..HEAD` **minus the feature-branch commits
that precede it** — silently omitting reviewed-looking work nobody reviewed.
An anchor **equal to the merge-base** fails the second test and is treated
as valid-and-degenerate: its range is the whole branch, which is a full
review anyway. On a miss of either test the anchor is invalid: **fall back
to a full-branch review**, announce the fallback with its reason, and
rewrite the anchor at the end. This is a degrade, never a halt — a redundant
full review costs time, while a wrong-scope review silently reports on a
diff it did not read. **A missing object is a miss, not a crash**: where the
anchor SHA no longer resolves (garbage-collected after a rewrite), the
command's failure is treated as a failed ancestry test. **This is the sole
staleness predicate, and it is sufficient by construction** —
`/hex-finalize`'s recomposition is explicitly not SHA-stable
([`finalize.md`](finalize.md#re-entry)) and is explicit-invocation-only rather
than gated on plan State, so a finalize run **will** invalidate a stored
anchor with no signal in the plan; a rebase, a reset and a force-push all
manifest identically as a failed reachability test, and an out-of-range
anchor as a failed range test, so enumerating causes separately would add
predicates that can disagree. The `backup/<branch>-…` ref is a
**diagnostic, never a second predicate**: an *inert* ref for this branch
explains *why* the ancestry test failed and is named in the fallback
announcement, while an *armed* ref already forbids acting on the branch at
all under the shipped hex-state rule, so there is no interaction left to
design.

### Delta round scope

**Round 1 is the full pass.** It reads the anchor's range where one is
valid (see above), the full branch diff otherwise. Round N ≥ 2 reads **the
fix delta plus finding-adjacent files** — the files named by the prior
round's actionable findings, in full, even where the delta does not touch
them, because a fix's correctness is judged against its surroundings. Round
1 absorbs what delta scoping cannot: **review non-determinism on unchanged
code** and **semantic conflicts** (two independently correct changes
combining broken with zero textual overlap). **Collateral breakage through a
shared symbol** — a fix changes a signature and breaks an *unchanged*
caller, in neither the delta nor the adjacent set — is what the gates catch. Delta scoping shrinks the diff *in
addition to*, never instead of, shrinking the perspective set.

## Convergence contract

The post-implementation drift check: does delivered code cover every
requirement ID the plan carries? Run by the review orchestrator when its
target traces to a plan artifact.

- **4-way gap taxonomy, keyed by [Traceability IDs](protocol.md#traceability-ids):**
  **missing** (nothing delivered for the ID), **partial** (delivered but
  incomplete against the contract), **contradicts** (delivered behavior
  conflicts with the contract), **unrequested** (delivered behavior no ID
  asked for — the reverse gap).
- **Append-only growth**: the orchestrator appends gaps as new pipeline **rows**
  at the end of the plan's Parallelization table, with matching new
  Implementation Steps entries, `Depends on` the delivered pipelines; their
  **wave derives** as the next topological level (no explicit wave to
  assert). Existing pipelines, steps, and IDs are never rewritten or
  renumbered.
- **Byte-identical when clean**: nothing unmet → the plan file is not
  touched at all and the report states "Converged".
- **Verdict interplay**: unconverged gaps cap the review verdict at
  Needs Work — never Approve — with `Next: /hex-execute <plan path>`.
- **Composition with fold-back (C-412)**: convergence runs **first and
  unconditionally**; the [spec fold-back](archive.md) phase runs **only** on
  a `Converged` result. They are mirrors on the same `C-###`/`S-###` join
  key — convergence asks *does the delivered code cover the plan's IDs* and
  appends to the plan (plan ← code), fold-back asks *does the spec describe
  what the plan delivered* and appends to the spec (spec ← plan); neither
  rewrites what the other wrote. A `Needs Work` verdict means fold-back never
  runs at all.
- **Federation — a plan carrying a `Repo` column (C-310):** the mechanism
  above is unchanged; coverage is evaluated against the **union diff** (the
  lead plus every distinct `Repo` value, `/hex-review`'s C-309 scope),
  satellite delivery is located via the `Hex-Plan:` commit trailer
  (`git -C <repo> log --grep`), and an appended gap row carries a `Repo` value
  like any other row. `C-`/`S-` IDs stay plan-scoped and are therefore global
  to the change. A gap delivered in a pipeline whose `Repo` is **not** `.` is
  **not** folded into the lead's spec — it is reported "delivered in
  `<repo>`, fold by hand" and left in the plan, because a fold has no correct
  destination across a repo boundary (the [fold-back](archive.md) phase is
  lead-scoped and never crosses it).

