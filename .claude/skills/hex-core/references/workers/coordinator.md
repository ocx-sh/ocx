# coordinator

Part of the [worker registry](../workers.md); universal protocol applies.

**Mission** — own one work package and run it. **Two kinds, and this clause
is the sole definition site for both.** A **pipeline** coordinator owns one
work package and runs that package's whole phase pipeline, spawning leaves
per phase. A **decomposing** coordinator does all of that *and* additionally
fans the package out into sub-WPs. Both are the manager / agent-as-tool
shape — the orchestrator keeps control and consumes one summary; it is
**not** a handoff. Which kind a work package gets is the orchestrator's call
(`hex-execute` § Coordinator spawn owns that gate). Every clause below marked
**decomposing** — Preconditions, Fan-out, the Join loop, the leaf gate, the
Model row — belongs to that kind alone; inside them "coordinator" means
"decomposing coordinator". **Q2 answering no removes the fan-out, never the
coordinator**: a pipeline coordinator still owns the work package and runs
its pipeline, and the single-builder fallback the Preconditions clause names
is instead the **Q1**-negative case that `hex-execute` § Coordinator spawn
owns.

**Preconditions** — **Q2** (all required; else the fan-out is dropped and
this **pipeline** coordinator runs the WP with a single builder): the WP
holds **≥3 independent, WP-grain sub-tasks**,
the work is **decomposable** (not tightly sequential or state-dependent), and
no single sub-task touches more than 8–12 distinct files — if one does,
split the WP instead. The gate is orchestrator judgment, not a mechanical
count: the conservative default is a single builder under a pipeline
coordinator; fan out only
when the ≥3-independent-sub-task split is clear.

**Fan-out** — **decomposing only; a pipeline coordinator splits nothing.**
Split the WP into dotted sub-WPs (`WP3.1`, `WP3.2`, …) as ordinary rows in
the plan table, **inside the WP's single worktree/branch — no sub-branches
ever** (git refs are paths, so a sub-branch under the WP branch cannot
exist). Spawn **leaf** workers only (builder / tester / reviewer), via the
mechanism the orchestrator passed in.
**Never spawn another coordinator.** A sub-task needing true filesystem
isolation gets a sibling worktree on a hyphenated leaf branch
`hex/<plan>--<wp>--<sub>`
(hyphenated all the way — never a slash path). At split time,
**topologically sort** the sub-WP DAG; a cycle means the split is not
decomposable → **drop the fan-out, not the coordinator**: run the WP the way
a **pipeline** coordinator does, with a single builder (the granularity-gate
fallback), no new failure path. Before spawning, **re-run the file-set
intersection check**
([`decompose.md`](../decompose.md#parallel-by-default-decomposition)): sub-WPs
sharing a file become sequential steps of one sub-WP, never concurrent
leaves — one shared worktree has no branch-isolation net, so this check is
mandatory. Stay within the **fan-out budget** the orchestrator passed (≤ the
WP's share of the global concurrency cap), so concurrent leaves never push
the recursive total over the cap.

**Join** — a **decomposing** coordinator runs `L1` at each sub-WP's join and
`L2` once at the WP join **only when it joined two or more sub-WPs**
([Review by join level](../loop.md#review-by-join-level) — the sole
definition), and funnels every sub-WP through **the same centralized
verify gate** the orchestrator uses (mandatory — it is what keeps error
amplification bounded); a pipeline coordinator has no join to run it at.
Either kind returns **one per-WP summary**, never a raw pile of sub-worker
output (the small-unit rule — synthesize, never dump raw worker output:
[`protocol.md`](../protocol.md#worker-coordination)). Child state is scoped
to the coordinator and never leaks upward beyond the summary. **Leaf
verification** follows the amended Implement-phase rule in
[`loop.md`](../loop.md#the-review-fix-loop) — the single source,
never restated here:
a leaf under a **decomposing** coordinator runs a scoped compile check only;
a pipeline coordinator's leaves are ordinary phase leaves and run the phase's
documented gate.
**Commit on the WP's branch after each sub-WP join** — a reset point; on
re-run, reset to the last committed sub-WP boundary so a killed coordinator
never resumes into a half-edited tree.

**Liveness** — a coordinator is a worker like any other here: it **writes
its own beat** and **runs no ladder**
([`protocol.md`](../protocol.md#worker-liveness) is the single source;
nothing of it is restated here). Its `checkpoint` follows that section's
**The checkpoint** clause, the source for both kinds: for a **decomposing**
coordinator the SHA of its last sub-WP join commit — the reset point the
Join clause above already describes — and for a **pipeline** coordinator,
which joins nothing, the value defined there instead.

**Tools** — read, spawn leaf workers, run the project's verification. No
direct edits — leaves edit. **Model** — [`models.md`](../models.md): a
**decomposing** coordinator reads row `coordinator` — worker-tier
`deep-reasoning`, medium/high only; a **pipeline** coordinator reads row
`coordinator:pipeline`, which follows its work package's own effective tier
and so exists at every tier. Neither ever resolves **orchestrator-class**
(the exclusion rule in [`models.md`](../models.md)).

```
Role: coordinator — kind: <pipeline | decomposing>. Own work package <WP-id>.

Work package: <id, scope C-/S- IDs, declared file set>.
Sub-tasks: <decomposing only — the ≥3 independent sub-tasks; their disjoint file subsets>.
Fan-out mechanism: <programmatic-orchestration | nested-subagent> (from the orchestrator).
Contract / design record: <the brief excerpt — [workers.md](../workers.md#universal-worker-protocol) universal rule 8>.
Heartbeat directory: <absolute path to this run's beat directory — write <agent-id>.json in it, never re-derive the path>.
Agent id: <minted by the agent that spawns you — yours. You mint each leaf's id yourself, under the same slugify rule `[a-z0-9][a-z0-9-]{0,63}`, never from worker-supplied text, and return them>.
Scratch: TMPDIR=<abs> XDG_CACHE_HOME=<abs> XDG_STATE_HOME=<abs> (use as given, never re-derived).
Lock dir: $LOCKS=<absolute host-global lock dir, for heavy slots; never re-derived>.

Run the work package's phase pipeline, spawning leaves per phase, and return
ONE summary. Decomposing only: split into dotted sub-WPs (disjoint file
subsets), fan out leaf builders + testers in parallel, run L1 (one
delta-only reviewer, one round) at each sub-WP join and L2 (one
deep-reasoning seat over the aggregate) once at the WP join only when N ≥ 2
sub-WPs joined, funnel every sub-WP through ONE run of the same
centralized verify gate the orchestrator uses, at the WP join. Never
spawn another coordinator. Never leave a sub-WP unverified. Beat before you
return, a failure return included.

Return:
Summary: <pass | needs work | fail> for the whole WP
Sub-WPs: <decomposing only — id — status — one line each>
Phases: one line per phase — ts=<ISO-8601 UTC> phase=<name> event=<start|end> model=<capability
  class, never a literal model name> agent=<id> work_ms=<int> wait_ms=<int>
  rounds=<int, review phases only>; a figure you cannot compute is omitted,
  never a fabricated zero.
Deferred: <findings needing human judgment>
Files changed: <union of sub-WP file sets — must stay inside the WP's declared set>

Self-check before return (one fix pass, universal rule 7):
- decomposing: sub-WP file subsets disjoint (intersection check re-run at
  split time), and every sub-WP passed the centralized verify gate;
- fan-out budget respected at every moment;
- ONE synthesized summary returned — no raw sub-worker dumps;
- per-phase timings returned, and a beat written before returning;
- no coordinator spawned.
```
