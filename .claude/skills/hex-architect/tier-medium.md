# Tier: medium

Minimal inline trade-off for **two-way-door** decisions — a builder-vs-
constructor call, sync-vs-async for an internal helper, a naming convention.
Keep a Component-level sketch, an NFR touch note, and a real (if small)
trade-off table so the decision is defensible even though nothing is
delegated. Five phases, not six — hex-architect never decomposes into
executable tasks; that is `/hex-plan`'s job downstream.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), model classes in
[`models.md`](../hex-core/references/models.md), and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover (single worker)

Launch **1** `explorer` scoped to the single area the decision touches — no
`architecture-explorer`, the scope is too small to earn one. In parallel,
read directly the project's architectural conventions for the area (from
project context, cached in `hex.md › Pointers`,
[`memory.md`](../hex-core/references/memory.md#the-three-sections)) and any
prior decision covering similar ground.

**Gate** — the area is mapped; existing precedent for a similar decision is
identified, or its absence is noted.

## Phase 2: Research (skip)

No `researcher`. The orchestrator may make a brief inline check (e.g. a
feature-detected docs tool) if an option involves an external library or
API.

**Gate** — the skip is logged in the record header
(`Research: skipped — two-way door`). If the inline check surfaces a
surprise that makes this not a two-way door, **stop and re-run** as
`/hex-architect high "…"` rather than silently upgrading mid-flow.

## Phase 3: Classify (inline)

Confirm the two-way-door reversibility and single-area blast radius inline
in the record header. If Discover revealed the decision is *not* two-way (it
touches an external contract, a stored format, or spans areas), **stop and
re-run** as `/hex-architect high "…"` — never silently upgrade
mid-pipeline.

## Phase 4: Reason & Design (inline)

Draft directly in the conversation and the handoff — no `architect` worker,
no file (an ADR is written only if the user asked via `--artifact=adr`, see
[`overlays.md`](overlays.md)):

- **Component sketch** — one paragraph naming the component(s) touched and
  the public surface, if any, this decision affects.
- **NFR touch** — a one-line note on any of scalability, availability,
  latency, security, cost, or operability this decision affects; say nothing
  about the ones it doesn't (no checklist essay).
- **Trade-off table** — at least 2 options, brief pros/cons, a
  recommendation with a one-sentence rationale. This is not optional even at
  this tier — a single unweighed opinion is not a design.
- **Reversibility** — name concretely what makes this two-way (how to undo
  it later, and roughly what that costs).

## Phase 5: Review (single reviewer, single pass)

Launch **1** `reviewer` (focus `quality`) for a single pass challenging the
recommendation — is the rejected option dismissed fairly, or just less
familiar? No adversarial design panel, no cross-model adversary at this
tier (`adversary: off`). Run the
[Review-Fix Loop](../hex-core/references/loop.md#the-review-fix-loop)
capped at **one round**: the orchestrator edits the record directly on
actionable findings; only a Block-tier finding earns one reviewer re-run
(2 passes total max, then stop).

**Gate** — the decision record is ready for handoff.

## Upkeep and handoff

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step):
re-point any `hex.md › Pointers` entry this run revealed as drifted.
Then emit the handoff from [`SKILL.md`](SKILL.md) with:

```
- Blast radius: single area
- Reversibility: two-way
- Tier: medium
- Overlays: research=skip, adversary=off, artifact=inline (or adr, if asked)
```

Its only required artifact is the trade-off table in the handoff itself — no
ADR, no research artifact at this tier, unless the user explicitly requested
one. If the classifier's signals called for more, it should have picked a
higher tier; re-run if so.
