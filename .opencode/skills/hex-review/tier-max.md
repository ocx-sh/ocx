# Tier: max

**Everything in [`tier-xhigh.md`](tier-xhigh.md), plus three additions**
(`adr_0017` C-995): every configured adversary, one more researcher in the
Stage 2 batch, and usage simulation reported in its own section. Explicit
only — `--tier=max`; the classifier never lands here. Phases are
`xhigh`'s, linked, never restated; the headings exist so
`tiers.hex-review.max.counts` has identifiers to name.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.

## Phase 1: Discover (inline, no worker)

As [`tier-xhigh.md` § Phase 1](tier-xhigh.md#phase-1-discover-inline-no-worker).

## Phase 2: Stage 1 — Correctness (parallel, 2 workers)

As [`tier-xhigh.md` § Phase 2](tier-xhigh.md#phase-2-stage-1--correctness-parallel-2-workers).

## Phase 3: Stage 2 — Adversarial panel (parallel, every adversary + simulators)

As [`tier-xhigh.md` § Phase 3](tier-xhigh.md#phase-3-stage-2--adversarial-panel-parallel-up-to-6-workers),
with the batch widened:

- **every configured adversary** launches last in the batch — a
  list-valued `adversary` key fans out one call per skill, each under its
  own bound ([adversary contract](../hex-core/references/adversary.md#adversary-contract));
  one configured entry announces `max: 1 adversary configured`;
- **one more `researcher`** (focus `ecosystem`, known-pitfall framing) —
  input to the panel's `pitfalls` section;
- **4 `simulator` workers** in artifact mode, one per shipped pattern
  plus one per project `.agents/workers/simulator-<pattern>.md`, each in
  its own scratch worktree off the target
  ([`workers/simulator.md`](../hex-core/references/workers/simulator.md)).

## Phase 4: Root-cause analysis (`rca=on`, all findings above Suggest)

As `tier-xhigh.md` § Phase 4.

## Phase 5: Cross-model pass (mandatory, every adversary)

As [`tier-xhigh.md` § Phase 5](tier-xhigh.md#phase-5-cross-model-pass-mandatory),
triaging the **union** of every adversary's return with duplicate merge;
a finding two adversaries raise is one finding with two attributions.

## Phase 6: Verdict & Output

As [`tier-xhigh.md` § Phase 6](tier-xhigh.md#phase-6-verdict--output), the
report gaining one section after Stage 2:

```markdown
### Usage simulation
[per pattern: verdict (succeeds unaided | with friction | fails), then the
findings with repro — actionable and deferred]
```

## Upkeep and handoff

As [`tier-xhigh.md` § Upkeep and handoff](tier-xhigh.md#upkeep-and-handoff),
the handoff carrying `Tier: max` and the adversary count that ran.
