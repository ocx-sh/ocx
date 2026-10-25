# Tier: xhigh

The full treatment for **one-way-door-high** work — a new module or package,
a breaking API, a protocol or storage-layout change. When the plan carries a
new one-way-door decision no accepted ADR covers, it adds an architect with
an ADR, the review panel and the cross-model plan review. Three-axis research
runs only when the user asks for it (this tier named explicitly, or
`--research=3`). A plan built from an accepted ADR never re-runs research or
an architect on that ADR's decisions.

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

## Phase 2: Research (0 or 1 axis; 3 when asked)

Default as [`tier-high.md` § Phase 2](tier-high.md#phase-2-research-01-axis):
skip when an accepted ADR or persisted research already covers the topic,
else **1** `researcher` on the most relevant axis. Research is never repeated.

**Three axes only when the user asks** — this tier named explicitly, or
`--research=3`. Launch **3** `researcher` workers in a single concurrent
batch, one per axis (the project's product knowledge — research keywords,
comparable tools, located via `hex.md › Pointers` — seeds the search terms
when present):

- **Technology / tools** — trending libraries, competing tools.
- **Design patterns** — emerging approaches, best practices, known pitfalls.
- **Domain knowledge** — the domain's specs, security considerations,
  algorithm choices (grounded in project context, never assumed).

Each returns an opinionated recommendation with trend analysis, adoption
signals, and citations. Persist each as a research artifact.

**Gate** — research persisted (or an explicit "skipped — covered by <ADR /
artifact>" note); when three axes ran, state-of-the-art and adoption signals
are checked on all of them.

## Phase 3: Classify (sequential)

Confirm **one-way-door high** in the plan header. If the classification fails
(the change is actually medium), **downgrade and re-run** as
`/hex-plan high "…"` — never silently over-specify a medium change or
under-specify a large one.

Required artifacts this tier:

- the **plan** (executable phases);
- an **ADR** — written when the plan carries a new one-way-door decision no
  accepted ADR covers; otherwise the accepted ADR is cited;
- **persisted research** (from Phase 2, when it ran).

Formats follow the project's documented conventions; the `/hex-init`
templates are the fallback.

**Gate** — scope, reversibility, and all required artifacts listed in the
plan header.

## Phase 4: Design (architect only for a new one-way door)

Launch **1** `architect` **only** when the plan carries a new one-way-door
decision no accepted ADR covers; its model class is `deep`
([`models.md`](../hex-core/references/models.md)). It produces an ADR and,
when scope warrants, a system-design doc. A downward `--architect` override is
honored but never silent — the announce block flags it ("a new one-way door
recommends a delegated architect — running inline per user flag"). A plan
built from an accepted ADR (or a discussion handed off with one) designs
inline and cites the ADR — no architect on its decisions.

Design must include everything the `high` tier requires, plus:

- **Trade-off analysis** — at least **3** options (not 2), weighted criteria,
  risks, reversibility, and a recommendation with rationale. Owned by the
  ADR; an inline design cites the accepted ADR's analysis instead.
- **Migration / rollout plan** — how existing code, data, and users move to
  the new shape without breakage, or with explicit breakage communicated.

When project context names a constitution (cached in `hex.md › Pointers`),
checking the design against it is **mandatory** — record every deviation in
the plan's Constitution Deviations table
([constitution gate](../hex-core/references/protocol.md#constitution-gate)).
An ADR at this tier that violates the constitution without a recorded row is
incomplete, not merely unreviewed.

**Gate** — the design is written (an ADR when one was due), design artifacts
exist, and the contracts are testable.

## Phase 5: Decompose (sequential)

As [`tier-high.md` § Phase 5](tier-high.md#phase-5-decompose-sequential).
At this tier the pipeline cut is itself a design output — a cross-area
change that decomposes into one pipeline usually means the boundaries were
cut as feature slices, or no contract was fixed up front; re-cut before
shipping the plan.

**Gate** — as `high`.

## Phase 6: Review (one reviewer; panel + cross-model for a new one-way door or when asked)

Run the [Review-Fix Loop](../hex-core/references/loop.md#the-review-fix-loop)
on the draft plan — **plan-artifact scope: one round**; fix application,
re-validation (only after a fixed Block finding), and escalation follow the
canonical loop's artifact-scope rule, never restated here. Each decision is
reviewed once across the chain.

**One reviewer, always** — `reviewer` (focus `spec`, phase `post-stub`): are
the contracts testable? Do they match the user-experience section?
**Mandatory mechanical check**: every C-/S- ID maps to at least one pipeline
Scope cell and at least one test step; an uncovered ID is an actionable
finding, no exceptions at this tier
([traceability IDs](../hex-core/references/protocol.md#traceability-ids)).
For a plan built from an accepted ADR it checks only the decomposition —
contracts testable, pipelines cut cleanly along contracts, steps sized for a
fresh agent.

**Panel and cross-model plan review** — only when the plan itself carries a
new one-way-door decision no accepted ADR covers, or the user asks (this tier
named explicitly, or `--adversary`). Launch concurrently with the reviewer:

- `architect` — are the trade-offs honest, the alternatives considered, any
  boundary violations introduced?
- `researcher` — does the plan miss a trending pattern, a known pitfall, or a
  state-of-the-art approach?
- the **cross-model plan review**, launched **in the Round 1 batch, last** —
  never after the panel — and run once in `plan-artifact` scope on the plan
  file (`adr_0016` C-987). One-shot, no loop; 4-way triage (actionable /
  deferred / stated-convention / trivia); its actionable findings join the
  same fix application as the panel's
  ([adversary contract](../hex-core/references/adversary.md#adversary-contract)).
  If the adversary produces no review — the skill is unavailable, or it ran
  and did not complete one — log `Cross-model plan review skipped: <reason>`
  and continue — but **surface the skip prominently in the handoff**, since
  one review layer was missed.

An unjustified constitution violation is flagged as an actionable finding —
never waved through at this tier
([constitution gate](../hex-core/references/protocol.md#constitution-gate)).

**Gate** — the plan is ready for `/hex-execute`; panel and cross-model
deferred findings, when they ran, are documented. Then run the
[upkeep step](../hex-core/references/protocol.md#upkeep-step) and emit the
handoff from [`SKILL.md`](SKILL.md).
