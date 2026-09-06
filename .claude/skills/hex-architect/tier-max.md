# Tier: max

**Everything in [`tier-xhigh.md`](tier-xhigh.md), plus three additions**
(`adr_0017` C-995): five research axes with `competitive-research`
mandatory, every configured adversary in the design-review batch, and
usage simulation of the design's user-facing surface. Explicit only —
`--tier=max`; the classifier never lands here. Phases are `xhigh`'s,
linked, never restated; the headings exist so
`tiers.hex-architect.max.counts` has identifiers to name.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.

## Phase 1: Discover (single worker, full)

As [`tier-xhigh.md` § Phase 1](tier-xhigh.md#phase-1-discover-single-worker-full).

## Phase 2: Research (5 axes, gate-selected — mandatory, one `competitive-research`)

As [`tier-xhigh.md` § Phase 2](tier-xhigh.md#phase-2-research-3-axes-gate-selected--mandatory)
with **5** `researcher` workers: four axes from the gate's ranked
candidates, the fifth always focus `competitive-research`
([`workers/researcher.md`](../hex-core/references/workers/researcher.md)),
seeded from the project's product knowledge via `hex.md › Pointers`.

## Phase 3: Classify (sequential)

As [`tier-xhigh.md` § Phase 3](tier-xhigh.md#phase-3-classify-sequential).

## Phase 4: Reason & Design (architect mandatory, ADR + system-design)

As [`tier-xhigh.md` § Phase 4](tier-xhigh.md#phase-4-reason--design-architect-mandatory-adr--system-design);
`system-design` is mandatory here, not signal-gated.

## Phase 5: Review (adversarial design panel + every adversary + simulators)

As [`tier-xhigh.md` § Phase 5](tier-xhigh.md#phase-5-review-adversarial-design-panel--mandatory-cross-model),
the Round 1 batch widened:

- **every configured adversary** launches last in the batch, one call per
  entry of a list-valued `adversary` key, findings union-triaged with
  duplicate merge ([adversary contract](../hex-core/references/adversary.md#adversary-contract));
- **4 `simulator` workers** in design mode — one per shipped pattern plus
  one per project `.agents/workers/simulator-<pattern>.md` — each walking
  the ADR's or system design's user-facing contract as that user
  ([`workers/simulator.md`](../hex-core/references/workers/simulator.md)).
  Actionable findings become proposed changes at the review gate, applied
  in the same single fix application the panel's findings get; deferred
  findings are product questions in the handoff.

The handoff carries `Tier: max`, the axes that ran and the adversary count.
