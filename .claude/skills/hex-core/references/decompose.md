# hex Decomposition

A topic file of the hex swarm protocol; the spine is
[`protocol.md`](protocol.md).

## Parallel-by-default decomposition

A plan is a few **pipelines** cut along contracts. A pipeline is an ordered
chain of **steps** in one worktree; each step is a fresh agent with a small
brief. Inside a pipeline: serial, shared state, no merge, no review, no gate
between steps. Across pipelines: parallel, sharing only contracts.

- **Cut pipelines along contracts** — a directory, module, package or
  data-model boundary whose interface can be fixed up front — never by
  user-facing feature slice. Where no contract can be fixed up front, the
  work is steps of **one** pipeline. Splitting into steps is cheap; splitting
  into pipelines buys parallelism and is done only where a contract allows it.
- **Step sizing is by context, never by risk.** A step is what one fresh agent
  holds comfortably in a small brief. Split a pipeline into more steps to keep
  contexts small, never to add a check.
- **Every pipeline declares its expected file set at planning time.** Two
  pipelines may run together only if their file sets are disjoint; two tasks
  that need the same file are steps of **one** pipeline. **When the plan
  carries a `Repo` column** the comparison is over `(Repo, path)` pairs
  (C-316) — satellites routinely declare textually identical repo-relative
  paths that are disjoint across repos; with no `Repo` column every pair is
  `(., p)` (same key, worktree side:
  [Pipeline worktree mechanics](worktree.md#pipeline-worktree-mechanics)).
- **Hub and generated files belong to no pipeline** — lockfiles, baselines,
  goldens (`/hex-init` lists them). They are regenerated once, minimally, at
  integration.
- **Contract wave first.** Stubs **plus contract tests** for every pipeline,
  committed once. Then all pipelines start together. A step that edits a
  contract-wave file is detected by `git diff` against that commit when it
  returns, and the orchestrator re-briefs the affected pipelines; no review is
  triggered.
- **Small task = one pipeline.** No contract wave; one review call; the
  integration gate is the only gate before `/hex-finalize`.
- **Marks, rare.** The plan's `Marks` cell is empty, `hard` (the pipeline's
  steps start at `standard-high`; at most 1 pipeline in 4) or `review` (the
  pipeline is reviewed on its own before it lands). Everything else runs at
  `standard`; only the orchestrator escalates ([`models.md`](models.md)).
- **The Parallelization table** carries one row per pipeline:
  `Pipeline | Repo | Scope | Expected Files | Wave | Depends on | Marks | Status`.
  `Repo` is present only in a federated plan; `Scope` cites the C-/S-IDs the
  pipeline delivers; the pipeline's steps are an ordered list in the plan.
  Statuses are `pending | active | merged | failed`. Legacy `Size`, `Review`
  and `Verify` columns and dotted sub-rows are read and ignored.
- **Launch on dependency-ready; waves are a derived reporting view.** A
  pipeline is in wave N iff every pipeline it depends on sits in an earlier
  wave and N is minimal; wave 1 = no dependencies. A pipeline becomes eligible
  the instant every pipeline in its `Depends on` has Status `merged`. The
  orchestrator keeps a **ready-set**, ordered critical-path-first, launches
  within the cap ([Worker coordination](protocol.md#worker-coordination) owns
  the cap and the build-job budget; nothing here restates them), and recomputes
  on every merge. Merge stays serialized in a valid topological order.
- **The critical path** — the longest dependency chain — is identified and
  marked; it bounds wall-clock time no matter how wide the waves are.
- **Under-parallelization is justified, never silent**: fewer parallel
  pipelines than file-disjointness allows carries a one-line justification in
  the plan's Parallelization section.
- **Progress surface.** Live state comes from the Status column: one rollup
  line per pipeline (`P3: step 2/4`); a pipeline `active` far beyond its peers
  carries a staleness flag. Surface only, never speculative re-execution.
- **The schedule log — one append-only plan section.** The plan carries a
  `## Schedule log`, never edited or reordered. Two line kinds, told apart by
  the first word after the first `·`:
  `- <ISO-8601 UTC> · merged <pipeline> @ <post-merge SHA> · ready: <ids | —> · blocked: <id (<blocker>), … | —>` and
  `- <ISO-8601 UTC> · step <pipeline>/<n> · model <class> · work <elapsed> [· wait <elapsed>]`.
  `<class>` is a capability class ([`models.md`](models.md)), never a literal
  model name. `wait` is the interval from a step becoming runnable to its
  worker beginning work; where the start is unknown the field is absent, never
  zero. The post-merge SHA is mandatory — it is what makes bisection free
  ([Pipeline worktree mechanics](worktree.md#pipeline-worktree-mechanics)).
  Elapsed is wall-clock from the orchestrator's own `date -u +%FT%TZ` brackets,
  best-effort and never gated on. A consumer reads only the kind it knows; a
  section without `step` lines is a run that predates them.
- **Failure cascade — a `failed` pipeline blocks its dependents without
  stopping the run.** The ready-set rule already makes a dependent of a
  `failed` pipeline ineligible, and independent siblings keep flowing.
  Strandedness is derived, never stored: one pass over the static `Depends on`
  edges at report time, no fifth status. Report the failed pipeline(s) first,
  then one line per stranded pipeline naming its **direct** blocker —
  `P7 — blocked by P4 (stranded) ← P2 (failed)` — plus what did complete, read
  from the plan's `Shippable after wave` line. **A run that ends with a
  non-empty stranded set never presents a green final gate as plan completion
  and never reaches the plan's terminal review state** — `done`, or `landing`
  for a plan carrying a `Repo` column; `/hex-review` remains the sole writer
  of that state and gains this precondition.
