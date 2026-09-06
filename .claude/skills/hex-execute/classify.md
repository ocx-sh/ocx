# Classification Signals

Signal-to-tier map for `/hex-execute` when `tier=auto`, plus the overlay
triggers that stack on the chosen tier. The primary signal is the target
plan's Status block; a free-text target (no plan artifact) falls back to
`/hex-plan`'s signal table. The classifier emits a tier — **only `low`,
`medium`, `high`, or `xhigh`** — with zero or more overlays.

The classifier **never emits `max`** — that tier is explicit only,
`--tier=max` or a plan whose Status block says `Tier: max`
([`protocol.md`](../hex-core/references/protocol.md#tier-grammar)). When
signals split across adjacent tiers, or the overlay mix is unusual, mark
**low-confidence** — that forces the meta-plan gate in
[`SKILL.md`](SKILL.md) step 5. Never fire a mid-flow question; ambiguity is
resolved at the single gate.

## Primary: plan Status block

When the target resolved to a plan artifact
([`SKILL.md`](SKILL.md) Dispatch step 2), read its Status block:

| Field | Mapping |
|---|---|
| `Tier: low \| medium \| high \| xhigh \| max` | Use verbatim — overrides the classifier entirely |
| A reversibility note in the plan's Classify-phase table (two-way door / one-way-door medium / one-way-door high), if the project's plan convention records one | one-way-door medium or xhigh → suggest `adversary=on` |

If the Status block is present but `Tier:` is blank, or the plan carries no
reversibility note, fall back to free-text classification for that axis
only — no guess.

## Fallback: free-text targets

When no plan artifact resolves (a bare free-text task, no path, no
active-plan pointer), apply the same signal table as `/hex-plan`'s
[`classify.md`](../hex-plan/classify.md) — same target language, same scope
cues.

> **Read `/hex-plan`'s `classify.md` "Tier signal table" section directly**
> when classifying a free-text execute target. This file does not duplicate
> that table — sibling skills cross-read to stay lockstep.

Execute-specific deltas from the plan-side table:

- Execute's `low` and `medium` tiers still need ≥1 concrete file to change. A pure doc
  edit alone is not an execute target.
- Execute's `xhigh` and `max` tiers expect an existing plan artifact as
  target — the product of `/hex-plan xhigh` (or `max`). A free-text `xhigh`
  or `max` target should **stop and route** through `/hex-plan xhigh "…"`
  first; hex-execute does not decompose
  a large change from scratch.

## Confidence rules

- **Confident** — the plan's Status-block `Tier:` is set, or a free-text
  target has ≥2 matching signals with no competing signal from an adjacent
  tier. Announce and proceed (the gate is a fast announce-and-go).
- **Low-confidence** — the Status block is partial or absent **and**
  free-text signals split across adjacent tiers, or the target is terse with
  no discriminating cue. Flag it; [`SKILL.md`](SKILL.md) routes into the
  blocking meta-plan gate.

Never manufacture a question when confident: *announce and proceed*, or *let
the gate handle it*.

## Overlay triggers

Overlays adjust a single axis on top of the chosen tier and stack — several
may fire. Axis definitions and per-tier defaults are in
[`overlays.md`](overlays.md).

| Overlay | Triggered by signals |
|---|---|
| `review=adversarial` | Status-block `Tier: xhigh` or `Tier: max`; the diff touches security-sensitive paths (auth/crypto/signing, a new dependency manifest, a CI workflow file); the plan or prompt mentions "novel algorithm", "cross-area", "protocol change" |
| `adversary=on` | one-way-door medium or high signal from the plan's Classify-phase note; breaking-change signals in the plan or prompt; an explicit `Overlays: adversary=on` note in the plan |
| `loop-rounds=1` | every tier — rounds are a per-level setting; the axis only caps ([`overlays.md`](overlays.md#loop-rounds-axis)) |

Project hints in the Preferences section of
`.agents/memory/hex.md` (always-on perspectives, path-triggered
escalations) fold into these suggestions before the gate
([spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence)).

## Examples

1. `/hex-execute .agents/plans/plan_flag.md` — Status block reads
   `Tier: medium` → tier **medium**, no overlays, confident. Status block wins.
2. `/hex-execute .agents/plans/plan_cache.md` — Status block
   `Tier: xhigh` + a Classify-phase note "one-way door (medium)" → tier
   **xhigh** + `adversary=on` (reversibility signal), confident.
3. `/hex-execute "add a --json flag to the list command"` (free text, no
   plan) → falls back to `/hex-plan`'s medium-tier signal → tier **medium**,
   confident.
4. `/hex-execute "refactor the entire pull pipeline"` (free text, no plan)
   → the classifier infers **xhigh**, but `xhigh` expects a plan artifact;
   [`SKILL.md`](SKILL.md) announces and stops, pointing at
   `/hex-plan xhigh "…"`.
5. `/hex-execute .agents/plans/plan_x.md` — Status block present but
   `Tier:` blank, free-text fallback signals split between `medium` and
   `high` → **low-confidence** → the gate fires.
