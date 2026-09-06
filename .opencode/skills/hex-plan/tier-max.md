# Tier: max

**Everything in [`tier-xhigh.md`](tier-xhigh.md), plus three additions**
(`adr_0017` C-995): five research axes with `competitive-research`
mandatory, every configured adversary in the review batch, and usage
simulation of the plan's user-facing section. Explicit only —
`--tier=max`; the classifier never lands here. Phases are `xhigh`'s,
linked, never restated; the headings exist so `tiers.hex-plan.max.counts`
has identifiers to name.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.

## Phase 1: Discover (parallel, full)

As [`tier-xhigh.md` § Phase 1](tier-xhigh.md#phase-1-discover-parallel-full).

## Phase 2: Research (parallel, 5 axes — mandatory, one `competitive-research`)

As [`tier-xhigh.md` § Phase 2](tier-xhigh.md#phase-2-research-parallel-3-axes--mandatory)
with **5** `researcher` workers: the gate's ranked candidates fill four
axes, and the fifth is always focus `competitive-research`
([`workers/researcher.md`](../hex-core/references/workers/researcher.md)),
seeded from the project's product knowledge via `hex.md › Pointers`.

## Phase 3: Classify (sequential)

As [`tier-xhigh.md` § Phase 3](tier-xhigh.md#phase-3-classify-sequential).

## Phase 4: Design (architect mandatory, ADR mandatory)

As [`tier-xhigh.md` § Phase 4](tier-xhigh.md#phase-4-design-architect-mandatory-adr-mandatory).

## Phase 5: Decompose (sequential)

As [`tier-xhigh.md` § Phase 5](tier-xhigh.md#phase-5-decompose-sequential).

## Phase 6: Review (parallel panel + every adversary + simulators)

As [`tier-xhigh.md` § Phase 6](tier-xhigh.md#phase-6-review-parallel-panel--mandatory-cross-model),
the Round 1 batch widened:

- **every configured adversary** launches last in the batch, one call per
  entry of a list-valued `adversary` key, findings union-triaged with
  duplicate merge ([adversary contract](../hex-core/references/adversary.md#adversary-contract));
- **4 `simulator` workers** in design mode — one per shipped pattern plus
  one per project `.agents/workers/simulator-<pattern>.md` — each walking
  the plan's user-facing section as that user
  ([`workers/simulator.md`](../hex-core/references/workers/simulator.md)).
  Their actionable findings become **proposed requirement IDs** presented
  at the review gate, accepted or dropped by the orchestrator in the same
  single fix application the panel's findings get; deferred findings are
  product questions in the handoff.

The handoff carries `Tier: max`, the axes that ran and the adversary count.
