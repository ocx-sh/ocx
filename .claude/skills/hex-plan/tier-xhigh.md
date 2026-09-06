# Tier: xhigh

The full treatment for **one-way-door-high** work — a new module or package,
a breaking API, a cross-area refactor, a protocol or storage-layout change.
Preserve contract-first TDD, and add a mandatory architect, mandatory 3-axis
research, and the cross-model plan review as a default gate before handoff.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

**Meta-plan preview is mandatory.** At `xhigh`, the gate in
[`SKILL.md`](SKILL.md) step 5 always blocks for explicit approval — this tier
is expensive, and the preview catches a misclassification before workers
launch.

## Phase 1: Discover (parallel, full)

Same shape as `high`, launched in a single concurrent batch:

- **1** `architecture-explorer` — map the architecture, trace dependencies,
  find reusable code and patterns.
- **2–4** `explorer` workers — one per involved area, scoped from the
  project's own rules and structure.

In parallel, read directly the project rules for **every** area touched, and
all prior ADRs/plans/research in the convention-resolved artifact home. A PR's
file list feeds the Discover scope directly.

This step also reads any `Federation:` bullets under `hex.md › Pointers`,
alongside every other pointer (C-314). When they are present, an `explorer`
scoped to a satellite's area reads **that repo's** rules by explicit
`Read` — ambient context covers the lead only, and `--add-dir` does not
load a satellite's `CLAUDE.md` (C-306, C-318). Absent `Federation:`
bullets, this step is unchanged.

**Gate** — a full architecture map is produced; every prior decision record
in the domain is enumerated.

## Phase 2: Research (parallel, 3 axes — mandatory)

Launch **3** `researcher` workers in a single concurrent batch, one per axis
(the project's product knowledge — research keywords, comparable tools,
located via `hex.md › Pointers` — seeds the search terms when present):

- **Technology / tools** — trending libraries, competing tools.
- **Design patterns** — emerging approaches, best practices, known pitfalls.
- **Domain knowledge** — the domain's specs, security considerations,
  algorithm choices (grounded in project context, never assumed).

Each returns an opinionated recommendation with trend analysis, adoption
signals, and citations. Persist each as a research artifact — mandatory at
this tier; substantial findings are expected.

**Gate** — three research artifacts persisted; state-of-the-art and adoption
signals checked on all axes.

## Phase 3: Classify (sequential)

Confirm **one-way-door high** in the plan header. If the classification fails
(the change is actually medium), **downgrade and re-run** as
`/hex-plan high "…"` — never silently over-specify a medium change or
under-specify a large one.

Required artifacts this tier:

- the **plan** (executable phases);
- an **ADR** (mandatory — the architecture decision record);
- **persisted research** (from Phase 2).

Formats follow the project's documented conventions; the `/hex-init`
templates are the fallback.

**Gate** — scope, reversibility, and all required artifacts listed in the
plan header.

## Phase 4: Design (architect mandatory, ADR mandatory)

Launch **1** `architect`; its model class is `deep-reasoning`
([`models.md`](../hex-core/references/models.md)). A downward `--architect`
override is honored but never silent — the announce block flags it ("high
tier recommends a delegated architect — running inline per user flag").
Produce an ADR and, when scope warrants, a system-design doc.

Design must include everything the `high` tier requires, plus:

- **Trade-off analysis** — at least **3** options (not 2), weighted criteria,
  risks, reversibility, and a recommendation with rationale.
- **Migration / rollout plan** — how existing code, data, and users move to
  the new shape without breakage, or with explicit breakage communicated.

When project context names a constitution (cached in `hex.md › Pointers`),
checking the design against it is **mandatory** — record every deviation in
the plan's Constitution Deviations table
([constitution gate](../hex-core/references/protocol.md#constitution-gate)).
An ADR at this tier that violates the constitution without a recorded row is
incomplete, not merely unreviewed.

**Gate** — the ADR is written, design artifacts exist, and the contracts are
testable.

## Phase 5: Decompose (sequential)

Break the design into right-sized tasks, each mapping to a Stub → Specify →
Implement → Review cycle so `/hex-execute` runs unchanged — **decomposed to
maximize parallelism**
([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition)):
cut along structural boundaries, declare every WP's expected file set and
optional `Review` risk hint (`risk` on a WP the author knows is riskier
than its file set shows; a sub-overhead WP folds into its nearest sibling), compute waves from the
dependency graph, and mark the critical path. The
Parallelization section carries the WP table (id, scope, expected files,
size, wave, depends-on, review, verify, status — status initialized
`pending`), the wave-grouped mermaid `graph TD`, a "Shippable after wave:
N — <what ships>" line, and the serialized topological-order merge plan
(waves derived)
([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics));
fewer parallel WPs than file-disjointness allows → one-line justification.
At this tier the wave structure is itself a design output — a cross-area
change that decomposes into one sequential chain usually means the
boundaries were cut as feature slices; re-cut before shipping the plan.

**Federation.** When `hex.md › Pointers` carries `Federation:` bullets,
Decompose offers per-repo WP decomposition — a WP whose scope lies in a
satellite gets that satellite's key in the `Repo` column — and adds the
mandatory integration WP that depends on every satellite WP it joins
(C-311); wave-cutting applies the file-disjointness key over `(Repo, path)`
pairs, not bare paths (C-316). `/hex-plan` never runs the C-303 pre-flight
and never writes into a satellite — it only proposes the column (C-314).
Absent `Federation:` bullets, none of this fires and the plan is unchanged.

Print the **effective-tier histogram** at this gate, linking rather than restating
its grammar
([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition)).

**Gate** — the plan holds executable phases `/hex-execute` can run without
further decomposition, and the Parallelization section shows the widest
wave structure the file sets permit.

## Phase 6: Review (parallel panel + mandatory cross-model)

Run the [Review-Fix Loop](../hex-core/references/loop.md#the-review-fix-loop)
on the draft plan — **plan-artifact scope: one panel round**; fix
application, conditional re-validation, and escalation follow the
canonical loop's artifact-scope rule, never restated here.

**Round 1** — launch concurrently:

- `reviewer` (focus `spec`, phase `post-stub`) — are the contracts testable?
  Do they match the user-experience section? **Mandatory mechanical check**:
  every C-/S- ID maps to at least one WP Scope cell and at least one test
  step; an uncovered ID is an actionable finding, no exceptions at this tier
  ([traceability IDs](../hex-core/references/protocol.md#traceability-ids)).
  Also checks the Parallelization table for an unjustified sub-overhead WP.
- `architect` — are the trade-offs honest, the alternatives considered, any
  boundary violations introduced?
- `researcher` — does the plan miss a trending pattern, a known pitfall, or a
  state-of-the-art approach?

An unjustified constitution violation is flagged by the panel as an
actionable finding — never waved through at this tier
([constitution gate](../hex-core/references/protocol.md#constitution-gate)).

**Cross-model plan review — a default part of this tier's flow.** Launched
**in the Round 1 panel batch, last** — never after the panel — and run once
in `plan-artifact` scope on the plan file (`adr_0016` C-987). One-shot, no
loop; 4-way triage (actionable / deferred / stated-convention / trivia);
its actionable findings join the same fix application as the panel's and
are re-validated by the same single `reviewer` (focus `spec`) pass
([adversary contract](../hex-core/references/adversary.md#adversary-contract)).
If the adversary produces no review — the skill is unavailable, or it ran and
did not complete one — log
`Cross-model plan review skipped: <reason>` and continue — but **surface the
skip prominently in the handoff**, since one review layer was missed.

**Gate** — the plan is ready for `/hex-execute`; both panel and cross-model
deferred findings are documented. Then run the
[upkeep step](../hex-core/references/protocol.md#upkeep-step) and emit the
handoff from [`SKILL.md`](SKILL.md).
