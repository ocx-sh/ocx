# Tier: high

The **default** planning tier — medium-scope, one-way-door-medium work: a new
command, a new index or storage layout, a change spanning 1–2 areas. This is
the baseline any caller gets without an explicit tier. Preserve the
contract-first TDD skeleton (Stub → Specify → Implement → Review).

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover (parallel)

Launch in a **single batch** so they run concurrently
([`protocol.md`](../hex-core/references/protocol.md#worker-coordination)):

- **1** `architecture-explorer` — map the current architecture, trace
  dependencies, find reusable code and active patterns.
- **2–4** `explorer` workers — one per involved area. Identify the areas from
  the project's own rules and structure (project context, cached in
  `hex.md › Pointers`), not a hardcoded table.

In parallel, read directly: the project rules for the areas touched, and any
prior plans/ADRs/research in the convention-resolved artifact home (project
conventions, else `.agents/`,
[`memory.md`](../hex-core/references/memory.md#location-and-resolution)) for
overlap.

This step also reads any `Federation:` bullets under `hex.md › Pointers`,
alongside every other pointer (C-314). When they are present, an `explorer`
scoped to a satellite's area reads **that repo's** rules by explicit
`Read` — ambient context covers the lead only, and `--add-dir` does not
load a satellite's `CLAUDE.md` (C-306, C-318). Absent `Federation:`
bullets, this step is unchanged.

GitHub: when the target resolved to a PR/issue, use its fetched context in
place of a broad scan; a PR's file list is the explicit scope for the
`architecture-explorer`. Fall back to the client's GitHub MCP list tools or
`gh` only when the target is free text.

**Gate** — worker reports are in; the architecture is mapped, reusable
components are identified, prior artifacts are checked for overlap.

## Phase 2: Research (parallel, 1 axis)

Launch **1** `researcher` on the single most relevant axis (technology *or*
patterns *or* domain), paired with at least one explorer's output so
external findings stay grounded in local code. The project's product
knowledge (research keywords, comparable tools — located via
`hex.md › Pointers`) seeds the axis choice and search terms when present.
Findings longer than a paragraph **must** persist as a research artifact in
the convention-resolved location for reuse.

Override: `--research=3` launches all three axes in a single concurrent batch.
The researcher's model class is `fast-balanced`
([`models.md`](../hex-core/references/models.md)).

**Gate** — research findings persisted (or an explicit "no new signals"
note); adoption trends and recent work checked.

## Phase 3: Classify (sequential)

Determine reversibility and scope; record it in the plan header:

| Scope | Reversibility | Artifacts |
|---|---|---|
| Small (1–3 days) | Two-way door | plan |
| Medium (1–2 weeks) | One-way door (medium) | plan + ADR (when a boundary decision is made) |
| Large (2+ weeks) | One-way door (high) | plan + ADR + persisted research |

Artifact formats follow the project's documented conventions; the templates
shipped with `/hex-init` are the fallback. If this resolves to Large, **stop
and re-run** as `/hex-plan xhigh "…"` — no silent upgrade mid-pipeline.

**Gate** — scope and reversibility documented in the plan header.

## Phase 4: Design (delegated for one-way-door, inline otherwise)

For **one-way-door medium** or cross-area work, launch an `architect`
(`--architect=on`) to produce an ADR or system design; its model class is
`deep-reasoning` ([`models.md`](../hex-core/references/models.md)). For two-way-door
scope, design inline in the plan.

Design must include:

- **Component contracts** — public API (types, signatures) with expected
  behavior per component.
- **User-experience scenarios** — action → expected outcome → error cases for
  each user-facing behavior.
- **Error taxonomy** — documented failure modes with remediation guidance.
- **Edge cases** — boundary and corner cases enumerated.
- **Trade-off analysis** — at least 2 options, weighted criteria, risks,
  reversibility, and a recommendation with rationale.

When project context names a constitution (cached in `hex.md › Pointers`),
check the design against it and record every deviation in the plan's
Constitution Deviations table
([constitution gate](../hex-core/references/protocol.md#constitution-gate)).

Write design artifacts to the convention-resolved location. **Gate** — the
contracts are testable: a tester could write failing tests from them without
reading any code.

## Phase 5: Decompose (sequential)

Break the design into right-sized tasks for contract-first TDD execution,
**decomposed to maximize parallelism**
([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition)):

- Each task maps to a Stub → Specify → Implement → Review cycle.
- Cut along structural boundaries, never feature slices; every WP declares
  its expected file set, and two tasks needing the same file become
  sequential steps of one WP.
- Waves are computed from the dependency graph (topological levels); the
  critical path is identified and marked.
- A WP the author knows is riskier than its file set shows gets a `risk`
  hint in its `Review` cell (otherwise empty), and a WP below the overhead
  floor **folds into its nearest sibling** as sequential steps
  ([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition)).
- The plan's Parallelization section carries the WP table (id, scope,
  expected files, size, wave, depends-on, review, verify, status — status
  initialized `pending`), the wave-grouped mermaid `graph TD`, a
  "Shippable after wave: N — <what ships>" line, and the serialized
  topological-order merge plan (waves derived)
  ([`worktree.md`](../hex-core/references/worktree.md#worktree-work-package-mechanics)).
  Fewer parallel WPs than file-disjointness allows → one-line
  justification; an isolated sub-overhead WP carries one too.
- **Federation:** when `hex.md › Pointers` carries `Federation:` bullets,
  offer **per-repo WP decomposition** — a WP whose scope lies in a
  satellite gets that satellite's key in the plan's `Repo` column — and add
  the mandatory integration WP that depends on every satellite WP it joins
  (C-311). Wave-cutting compares `(Repo, path)` pairs, not bare paths, when
  applying the file-disjointness key (C-316). `/hex-plan` never runs the
  C-303 pre-flight and never writes into a satellite — it only proposes the
  column (C-314). Absent `Federation:` bullets, no offer, no column, plan
  unchanged.

Print the **effective-tier histogram** at this gate, linking rather than restating
its grammar
([`decompose.md`](../hex-core/references/decompose.md#parallel-by-default-decomposition)).

**Gate** — the plan holds executable phases `/hex-execute` can run without
further decomposition, and the Parallelization section shows the widest
wave structure the file sets permit.

## Phase 6: Review (parallel panel, bounded loop)

Run the [Review-Fix Loop](../hex-core/references/loop.md#the-review-fix-loop)
on the draft plan — **plan-artifact scope: one panel round**; fix
application, conditional re-validation, and escalation follow the
canonical loop's artifact-scope rule, never restated here.

**Round 1** — launch concurrently:

- `reviewer` (focus `spec`, phase `post-stub`) — are the contracts testable?
  Do they match the user-experience section? Mechanically verifies every
  C-/S- ID maps to at least one WP Scope cell and at least one test step; an
  uncovered ID is an actionable finding
  ([traceability IDs](../hex-core/references/protocol.md#traceability-ids)).
  Also checks the Parallelization table for an unjustified sub-overhead WP.
- `architect` — are the trade-offs honest, the alternatives considered, any
  boundary violations introduced? *(required for one-way-door decisions)*
- `researcher` — does the plan miss a trending pattern, a known pitfall, or a
  state-of-the-art approach?

The panel flags an unjustified constitution violation as an actionable
finding
([constitution gate](../hex-core/references/protocol.md#constitution-gate)).

**Cross-model plan review** (when `adversary=on` — auto-on for one-way-door
signals, or explicit `--adversary`): launched **in the Round 1 panel batch,
last** — never after the panel — and run once in `plan-artifact` scope
(`adr_0016` C-987). One-shot, 4-way triage; its actionable findings join
the same fix application as the panel's and are re-validated by the same
single `reviewer` (focus `spec`) pass; graceful skip when unavailable
([adversary contract](../hex-core/references/adversary.md#adversary-contract)).

**Gate** — the plan is ready for `/hex-execute`; deferred findings are
documented. Then run the
[upkeep step](../hex-core/references/protocol.md#upkeep-step) and emit the
handoff from [`SKILL.md`](SKILL.md).
