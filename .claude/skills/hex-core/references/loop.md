# hex Review-Fix Loop

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## The Review-Fix Loop

The canonical contract-first loop. **This is the only copy in the bundle —
every other file links here.** Diff-scoped, bounded, tier-scaled.

**Contract-first TDD phases:**

1. **Stub** — a builder (focus `stub`) creates the public surface; gate on
   the project's compile/type check.
2. **Specify** — a tester (focus `specification`) writes tests from the
   design record; they MUST fail against the stubs.
3. **Implement** — a builder (focus `implement`) fills bodies until the
   tests pass; run the [scoped check](verify.md#scoped-check) — the WP's own contract
   tests plus the project's cheapest documented assembly gate —
   **unconditionally, at every tier**. This gate is **not** coupled to the
   WP's `Verify` cell, which budgets the merge boundary only — that WP's
   Review-Fix-Loop exit gate and the merge that immediately follows it
   ([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)).
   **The backstop is stated:** tier `xhigh` (and `max`) thereby gives up its pre-merge
   proof over untouched modules, and what catches a defect in a module no WP
   touched is [merge rule](worktree.md#worktree-work-package-mechanics) trigger (ii), a
   [checkpoint](verify.md#checkpoints) — `M = 3` merges, a cleared dependency level,
   or a high-risk merge, whichever fires first —
   with the bounded bisection of the post-merge-failure playbook (C-904,
   [Worktree work-package mechanics](worktree.md#worktree-work-package-mechanics))
   attributing the failure across at most three merges, trigger (i) at a
   coordinator join, and trigger (iii), the final gate.
   **Carve-out for a leaf under a decomposing coordinator:** it runs a
   scoped compile/parse check only — concurrent full verification in the
   coordinator's shared worktree would race on shared build artifacts — and
   the coordinator runs the one authoritative verification at the WP join.
   **The kind is load-bearing:** a pipeline coordinator splits its WP into no
   sub-WPs and holds no join, so the carve-out's own backstop does not
   exist there and a WP under one pays this gate in full.
4. **Review-Fix** — the loop below.

**The collapse at effective tier `medium`.** The four-phase list above is the
shape at effective `high` and above; at effective tier `medium`, Stub +
Specify + Implement collapse into one builder spawn ([the effective
tier](decompose.md#the-effective-tier)). One builder writes the public surface, then the
failing tests, then the implementation, in a single turn, and
**Verify-Architecture does not run**. **The collapsed builder still pays the
Implement gate's scoped check** ([Scoped check](verify.md#scoped-check)) — that gate
is unconditional at every tier and nothing here removes it.

**The ordering is checked by the orchestrator, not reported by the
builder**: the collapsed builder commits the stubs and the specification
tests as its first commit on the WP branch, before the implementation
commit, the builder's output contract names that commit's SHA, and the
orchestrator runs the project's test command at that commit and requires it
to fail — a pass at that commit is a violation and the WP does not proceed,
and the builder's own prose is not evidence. **That check is budgeted**: it
runs the WP's [scoped check](verify.md#scoped-check) command **warm**, reusing the
tree already built, so it costs a test run rather than a build; where the
older commit forces a cold rebuild, that rebuild **runs once** and is
recorded in the schedule log
([Parallel-by-default decomposition](decompose.md#parallel-by-default-decomposition)).

**What is given up is stated**: the *temporal* property is recovered —
surface before tests, tests before implementation, read off the branch's
commit graph by a party that did not write it — but **author≠verifier is
not**, and its backstop is the `L1` leaf review, which runs at every tier
([Review by join level](#review-by-join-level)). **Model-cell
resolution**: the collapsed spawn resolves all three source cells
(`builder:stub`, `builder:implement`, `tester`) and reads the highest, never
the lowest, disclosed like any other override-driven raise
([`models.md`](models.md#rules)).

At effective `high` and above the four-phase list is unchanged in every
byte.

**The loop:**

- **Round 1** — run the join level's seat on the diff ([Review by join
  level](#review-by-join-level)); where a level resolves to more than one
  seat, run them concurrently, blockers-first (spec, correctness).
  Classify each finding:
  - **Actionable** — a builder fixes it; re-run only the affected
    perspectives next round.
  - **Deferred** — surface it in the summary with context; never block the
    loop on it.
- **Subsequent rounds** — re-run only the perspectives with actionable
  findings from the prior round. A finding that surfaces two rounds running
  (oscillating) auto-defers.
- **Loop cap** — keyed on the **join level** ([Review by join
  level](#review-by-join-level)), never on tier: the level's `rounds`
  value, shipped `L0` 0 · `L1` 1 · `L2` 1. **Plan-artifact scope** (a
  draft plan or ADR under review by its own orchestrator): **one** panel
  round → the orchestrator applies actionable fixes → **one** re-validation
  pass by `reviewer` (focus `spec`) *whenever any actionable fix was
  applied* (skipped only on a clean panel) → anything still actionable
  escalates to the user. Artifacts are re-checked downstream anyway
  (Specify and Verify-Architecture gates), so multi-round artifact loops
  buy little; re-enabling them takes an **explicit** `artifact loop
  rounds: N` limit in `hex.md › Preferences` — the generic loop-rounds
  ceiling does not. If actionable findings remain when a cap
  is hit, **stop and escalate to the user** with the outstanding list —
  do not loop past the cap.
- **A `loop rounds` value in `hex.md › Preferences` is a ceiling, never a
  default and never a raise.** It caps every level's `rounds` *and* any
  `--loop-rounds` flag: the effective cap at a level is the **lowest of**
  the stored value, the run's resolved request — `--loop-rounds` when
  passed, the level's `review.<level>.rounds` otherwise — and the hard
  maximum of 3. The stored value never raises a level's `rounds`; a
  `--loop-rounds` flag may still loosen a run up to (never past) the
  stored ceiling. `limits.*` sit **outside** the later-wins
  [spawn-selection precedence](protocol.md#spawn-selection-precedence) — a
  user flag may lower a limit, never raise it past the stored ceiling. The
  stored value never affects plan-artifact scope — that scope moves only
  via the explicit `artifact loop rounds: N` limit named above. Both limits
  are announced at the gate with their source, like every other resolved
  axis.
- **Seats, class, input scope and budget** are set per join level, once,
  in [Review by join level](#review-by-join-level) below — there is no
  per-WP review budget and no tier-scaled perspective panel in this loop.
- **Adversary seat** (optional, tier-scaled) — when `adversary=on`, the
  cross-model adversary **launches in the same batch as the native seat of
  the join it gates**, never after it (`adr_0016` C-987), and its actionable
  findings join that join's single builder fix pass (C-988); one-shot, never
  loops. Batch order, clocks, triage and the failure path are the
  [Adversary contract](adversary.md#adversary-contract)'s, restated nowhere
  else.
- **Exit gate** — no actionable findings remain, the **WP's resolved
  verification** passes on the final state, and deferred findings are
  documented for handoff. The resolved verification is what that WP's
  `Verify` cell sets — grammar, defaults and the scoped/full determination
  live in [Parallel-by-default
  decomposition](decompose.md#parallel-by-default-decomposition), stated once there and
  linked, never restated, here. **This is not the run's final gate** — it
  fires **once per work package that runs the loop, in that WP's own
  worktree, before merge**; the plan's terminal verification is a
  separate, un-lowerable gate enumerated by the merge rule
  ([Worktree work-package mechanics](worktree.md#worktree-work-package-mechanics)).

### Review by join level

**Review depth is keyed on where a diff joins, never on tier.** Tier scales
execution — phases and model class ([the effective
tier](decompose.md#the-effective-tier)) — and nothing else. Every diff is
reviewed once at the level where it joins, by one seat, reading the diff
and nothing prose-shaped. Four levels, closed and versioned (`adr_0015`
C-980):

| Level | Fires at | Seats | Class | Rounds | Budget | Input |
|---|---|---|---|---|---|---|
| `L0` inline | every builder return | 0 spawns | — | 0 | — | the builder's evidence table, verified mechanically |
| `L1` leaf | a leaf's join: a WP or sub-WP branch lands | 1 `reviewer` | fast-balanced | 1 | 10 min | `git diff <base>..<head>` + the WP's contract excerpt |
| `L2` aggregate | a node joins **N ≥ 2** leaves | 1 `reviewer` | deep-reasoning | 1 | 20 min | the aggregate diff + the leaf verdicts |
| `L3` trunk | `/hex-review`, on explicit invocation only | that skill's staged panel | that skill's | that skill's | — | the feature branch |

Shipped defaults; the `L1` and `L2` cells, and the checklist sections each
brief carries (`review.<level>.checklist`), are the `review.<level>.*` keys
in `hex.md › Preferences` ([`config.md`](config.md#key-vocabulary)), and a
project overrides them **per level, never per role**. **Every diff passes
`L1` once, at the join nearest the builder that wrote it; every aggregate
passes `L2` once, at the node that assembled it.** A node whose child
already ran its own `L2` takes that verdict as input and does not re-run
`L1` over the child's diff — the rule is the same at every nesting depth.

- **`L0`** — the builder returns, with its files-changed list, an
  **evidence table**: one row per requirement ID its excerpt carried,
  `<ID> → <path>:<line>`, naming the line that satisfies it (a test, a
  symbol, a doc line). The orchestrator verifies each row **mechanically**
  — the path is in the diff and the line matches the ID's contract text by
  grep — and an unverifiable row is an actionable finding sent straight
  back to the builder, one fix pass. No reviewer is spawned. **A WP is
  `L0`-only — it runs no `L1` — when its actual diff is documentation
  only**: every path in `git diff --name-only <base>..<head>` is a markdown
  file or lies under the project's documented docs convention (`hex.md ›
  Pointers`), a mechanical read of the file list re-validation already
  produces. **A doc WP is checked by grep against the implementation it
  documents, never by a prose panel.** Every other WP runs `L0` and then
  `L1` at its join. `L0` carries no checklist — a mechanical grep takes no
  judgement list ([`checklist.md`](checklist.md#composition)).
  Universal rule 7 is unchanged — the self-check still carries no weight;
  the evidence table is verified by a party that did not write it, which
  is what gives it weight.
- **`L1`** — fires once per leaf join for every WP that is not `L0`-only,
  at every tier and in every plan shape. One `reviewer` (focus `spec`, phase
  `post-implementation`), its brief carrying the composed `spec` + `quality`
  sections of [`checklist.md`](checklist.md#composition), reads the leaf's diff
  against its recorded base and the contract excerpt — never the plan
  body, never a summary, never the tree. **A finding must sit on a diff
  line or name a contradiction the diff introduced**; anything else is
  out of scope and dropped, not deferred. One round: actionable findings
  get one `builder` fix pass, re-verified by the WP's resolved
  verification, and the loop ends. The Verify-Architecture reviewer at
  effective `high` and above is untouched — it is a phase gate, not a join.
- **`L2`** — fires when a coordinator, the orchestrator, or any
  sub-orchestrator between them joins **two or more** leaves whose `L1`
  passed. One deep-reasoning seat reads the aggregate diff of the join
  and the leaf verdicts as inputs, looking for what no leaf could see:
  semantic conflicts between independently correct leaves, a shared
  symbol changed on one side and called on the other, contract coverage
  across the set. **At `N = 1` the level is skipped** — there is no
  aggregate, and the `L1` verdict stands (C-981). The orchestrator's own
  `L2` fires **once, at the end of the run**, over `<base>..HEAD` of the
  feature branch with `N` = the WPs merged — never once per merge. The
  run's `review` overlay axis selects which
  [`checklist.md`](checklist.md#composition) sections this seat's brief
  carries ([`hex-execute/overlays.md`](../../hex-execute/overlays.md#review-axis)):
  the checklist grows, the seat count does not.
- **`L3`** — the feature branch to the trunk. **Only `/hex-review`, only
  when invoked.** Nothing in `/hex-execute` arms it, requires it, or
  records a precondition for it; a plan reaches its terminal review state
  through `/hex-review` because that skill is the state's sole writer,
  not because a lower level owed it a backstop. The execution handoff
  names what `L1`/`L2` deferred and every budget residue, so a trunk
  pass, if the user runs one, starts from the residue rather than the
  whole branch.

**Risk raises one level, never the round count** (C-983). A WP whose
`sec`, `hot` or `door` flag reads `true` ([the effective
tier](decompose.md#the-effective-tier)) — at spawn time, or at the
merge-time re-derivation over the actual diff — reviews **one level above**
the join it is at, in place of that join's own level: `L0 → L1` (a docs-only
WP gets a leaf reviewer after all); `L1 → L2` (its leaf join runs the `L2`
seat instead of the `L1` one — the one single-leaf `L2` that runs at
`N = 1`); `L2 → L2` with the `security` and `performance` checklists forced
on. It never adds a round and never reaches `L3`. The
plan's `Review` cell is an author-declared fifth source: `risk` raises the
same way, legacy `panel` reads `risk`, `self` and `light` are inert, a
missing column or cell is no hint.

**The budget ends the loop** (C-984). Every level carries a wall-clock
budget (`review.<level>.budget-minutes`); a seat that has not returned
inside it is stopped, `review budget expired: <level> <WP> — residue:
<what was not reviewed>` goes to the handoff's deferred list, and the WP
**proceeds** — it merges with residue recorded, never waits. Expiry is a
deferred finding, never a failure and never a re-run.

**Retired, stated so no reader looks for it** (C-986): the per-WP
`self | light | panel` budget and its guard, the `panel` escape hatch,
the branch-review precondition (the `adr_0012` backstop), and the
three-scope "review grows by diversity" model. The loop's other sections
— anchor, validation, delta scope, the diminishing-returns stop — are
unchanged and read "round cap" as this section's per-level `rounds`.

### The last-reviewed anchor

**One rule, two scopes, one of them persisted.**
A review round reads `<last-reviewed>..HEAD`, where `<last-reviewed>` is the
SHA the previous round **of the same scope** reviewed.

- **WP scope** — inside a Review-Fix Loop in a WP worktree, the anchor is
  the SHA round N−1 reviewed. It is held in the orchestrator's session state
  and **is not persisted**, because it never outlives the ephemeral branch
  it names and therefore cannot go stale.
- **Branch scope** — across `/hex-review` invocations on the feature branch,
  the anchor must survive the session and **is persisted** as one
  Status-block line: `- Reviewed: <full 40-char SHA>`, following the
  `Repos:` ledger's full-SHA precedent (C-324) for exactly the same reason —
  a short SHA or a ref name is not a stable identity. **Placement:
  immediately after `Next:` and *before* the `Repos:` ledger** — this line and
  the optional `- Verify-default:` line alike, because the `Repos:` ledger is
  multi-row and unbounded, so a line placed behind it has no stable position.
- **One writer rule** — whoever completes a review pass over a diff whose
  head is `<sha>` writes `Reviewed: <sha>`. It means precisely *"every
  commit reachable from this SHA has been through at least one review
  pass"* — nothing about verdicts, and nothing about whether findings
  remain. A WP-scope round **never** writes the field. Absent field ⇒ never
  reviewed ⇒ full-branch review.

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

**The mandatory full pass is what makes it safe.**
Round N ≥ 2 reads **`<last-reviewed>..HEAD` plus finding-adjacent files** —
the files named by the prior round's actionable findings, in full, even
where the delta does not touch them, because a fix's correctness is judged
against its surroundings. Round 1 reads the anchor's range where one is
valid, the full diff otherwise — **except at a level whose `rounds` is 1, where a
valid anchor never narrows round 1**: the 1-round cap makes that single
round the whole loop, so it reads the full scope and *is* the mandatory
converged pass — the shipped `L1` and `L2` case. **One
full pass is mandatory at the converged gate** — after actionable findings
reach zero and before the exit gate — **never delta-scoped, never skipped, not lowerable by any
config key.** It is a pass, not a second read: a converging round that already
read the full scope **satisfies** it, and a further read is owed only where
that round was delta-scoped. An `L0`-only WP runs no loop and has no
converged gate; its evidence table is its whole review, and the run's `L2`
aggregate, when one fires, reads its diff like any other. **"Full" resolves per the two scopes above and is not the feature
branch in both:** a **WP-scope** loop's converged pass reads
**the WP branch's own full diff against its recorded base** — the scope that
loop has reviewed all along — and a **branch-scope** pass reads the
**whole feature branch**. Reading the feature branch at the end of every
per-WP loop would re-review every already-merged WP once per subsequent WP,
which is `O(N²)`. The pass absorbs the three miss classes delta
scoping cannot: **review non-determinism on unchanged code**; **semantic
conflicts** (two independently correct changes combining broken with zero
textual overlap); and **collateral breakage through a shared symbol** — a
fix that changes a signature, contract or invariant and breaks an
*unchanged* caller, which is in neither the delta nor the finding-adjacent
set, since that caller is neither touched nor named by the finding. The
per-finding oscillation rule above is unchanged, and so is the round-N
perspective-shrinking rule — **delta scoping shrinks the diff *in addition
to*, never instead of, shrinking the perspective set**.

### The diminishing-returns stop

**A second exit condition, severity-aware.**
Let `A(N)` be the count of **actionable findings graded `Block` or `High`**
at the end of round N, after the per-finding auto-defer rule has been
applied. Severity is orthogonal to the actionable/deferred class
([Finding severity](severity.md#finding-severity)), so the stop names both axes or it
counts a naming nit against a data-loss bug. `Warn` and `Suggest` are
excluded: a round that converts one `Block` into three `Warn`s has
converged, and a count blind to that would call it oscillation. **Below tier
`high` the severity ladder is not applied** and the tag is absent, so `A(N)`
there counts all actionable findings — the same degrade every other
severity consumer takes. **The stop fires when both hold:**
`A(N) ≥ A(N−1)` for `N ≥ 2` — the `Block`/`High` count did not **strictly**
shrink — **and** round N introduced **no new `Block` or `High`** that was
not present in round N−1. The second clause is what keeps the stop from
firing on genuine progress: a round that surfaces a *new* serious defect is
doing its job, and stopping there would escalate a loop that had just found
something. When it fires the loop **stops and escalates to the user with the
outstanding list**, byte-for-byte the terminal behaviour hitting the loop
cap already produces — no new escalation path, no new message shape. The
strictly-decreasing expectation was derived from the constant-input case,
where every round re-read the *same* full diff; under delta scoping the
input shrinks too, so part of any observed decrease is an artifact of the
scope, and the rule is an **expectation, not a law**. It ships anyway
because its failure direction is benign: a scope-artifact decrease makes the
stop fire **late** — the loop runs to its cap, which is the pre-existing
behaviour — never early, and the severity floor biases it later still.
`A(N) = 0` is the **exit gate**, not this stop; the stop can only fire
**earlier** than the loop cap and never raises it, and the
`hex.md › Preferences` `loop rounds` ceiling is untouched.

## Convergence contract

The post-implementation drift check: does delivered code cover every
requirement ID the plan carries? Run by the review orchestrator when its
target traces to a plan artifact.

- **4-way gap taxonomy, keyed by [Traceability IDs](protocol.md#traceability-ids):**
  **missing** (nothing delivered for the ID), **partial** (delivered but
  incomplete against the contract), **contradicts** (delivered behavior
  conflicts with the contract), **unrequested** (delivered behavior no ID
  asked for — the reverse gap).
- **Append-only growth**: the orchestrator appends gaps as new WP **rows**
  at the end of the plan's Parallelization table, with matching new
  Implementation Steps entries, `Depends on` the delivered WPs; their
  **wave derives** as the next topological level (no explicit wave to
  assert). Existing WPs, sub-WPs, steps, and IDs are never rewritten or
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
  to the change. A gap delivered in a WP whose `Repo` is **not** `.` is
  **not** folded into the lead's spec — it is reported "delivered in
  `<repo>`, fold by hand" and left in the plan, because a fold has no correct
  destination across a repo boundary (the [fold-back](archive.md) phase is
  lead-scoped and never crosses it).

