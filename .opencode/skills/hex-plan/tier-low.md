# Tier: low

Inline plan for **trivial two-way-door** changes — one file, ≤30 lines
([`classify.md`](classify.md#tier-signal-table)). **Zero spawns: the
orchestrator writes the single-WP plan itself** (`adr_0017` C-994). The
contract-first skeleton (Stub → Specify → Implement → Review) is kept so
`/hex-execute low` runs it unchanged. Anything Discover reveals to be
larger stops and re-runs as `/hex-plan medium <target>`.

`Read` this file from [`SKILL.md`](SKILL.md) after the config is announced.
Shared vocabulary is linked, not restated: roles in
[`workers.md`](../hex-core/references/workers.md), the plan template via
`hex.md › Pointers`, and the outer contracts in
[`protocol.md`](../hex-core/references/protocol.md).

## Phase 1: Discover (inline)

Read the target file the prompt names and the project rules for its area
(project context, cached in `hex.md › Pointers`,
[`memory.md`](../hex-core/references/memory.md#the-three-sections)). No
explorer. GitHub: a PR's file list is the scope.

**Gate** — the one file is identified and the change fits the `low` row.

## Phase 2: Research (skip)

## Phase 3: Classify (inline)

One line in the plan: reversibility (two-way), blast radius (one file),
and why `low` holds.

## Phase 4: Design (inline)

One paragraph: what changes and the requirement ID(s) it satisfies, with
the acceptance criterion each test will assert.

## Phase 5: Decompose (inline)

A single work package, filled from the template with `Size: S`, the one
expected file, `Verify` empty (inherits `scoped`), `Review` empty. The
Status block carries `Tier: low`, `- Tier-grammar: 5` and
`- Effective-tier: derived` ([`SKILL.md`](SKILL.md#the-plan-artifact)).

## Phase 6: Review (inline self-check)

No reviewer. Before handoff, answer the `spec` section of
[`checklist.md`](../hex-core/references/checklist.md#composition) against
the plan as its own reviewer — every requirement has an ID, every ID has
a test the WP will write, nothing unrequested — and the template's
required sections are present or deliberately deleted. When project
context names a constitution (cached in `hex.md › Pointers`), apply the
[constitution gate](../hex-core/references/protocol.md#constitution-gate)
inline.

**Gate** — the plan is ready for `/hex-execute low`.

## Upkeep and handoff

Run the [upkeep step](../hex-core/references/protocol.md#upkeep-step),
then emit the handoff from [`SKILL.md`](SKILL.md) with:

```
- Scope: trivial (one file)
- Tier: low
- Overlays: (none)
- Spawns: 0
```

Its only artifact is the plan (Status block initialized, active-plan
pointer recorded in `hex.md › Memory`).
