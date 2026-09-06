# Tier: low

Inline decision note for **trivial two-way-door** calls — a naming or
local-style choice inside one file, no contract of any kind
([`classify.md`](classify.md#tier-signal-table)). **Zero spawns: the
orchestrator decides itself** (`adr_0017` C-994), and still shows a
two-option table — a single unweighed opinion is not a design. Anything
Discover reveals to carry a contract stops and re-runs as
`/hex-architect medium <decision>`. A discussion dossier never arrives
here ([`SKILL.md`](SKILL.md#a-discussion-dossier-as-decision)).

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md) and the outer contracts
in [`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover (inline)

Read the file the decision touches and the project's conventions for its
area (project context, cached in `hex.md › Pointers`); grep for the
nearest precedent — the same choice made elsewhere in the tree decides
by itself.

**Gate** — precedent found, or its absence noted.

## Phase 2: Research (skip)

## Phase 3: Classify (inline)

One line: two-way door, one file, no contract — why `low` holds.

## Phase 4: Reason & Design (inline)

Two options, one weighted criterion each way, a recommendation with its
rationale, and the precedent it follows. Verify any claim about existing
code by grep before asserting it.

## Phase 5: Review (inline self-challenge)

Before handoff, challenge the recommendation as its own reviewer: is the
rejected option dismissed fairly or only less familiar; does the choice
contradict a rule the area's conventions state. Fix inline.

**Gate** — the note is defensible in two sentences.

## Upkeep and handoff

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step),
then emit the handoff from [`SKILL.md`](SKILL.md) with:

```
- Blast radius: one file
- Reversibility: two-way
- Tier: low
- Overlays: research=skip, adversary=off, artifact=inline
- Spawns: 0
```

Its only artifact is the trade-off table in the handoff itself — never an
ADR at this tier.
