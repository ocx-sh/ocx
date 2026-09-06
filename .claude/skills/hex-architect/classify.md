# Classification Signals

Signal-to-tier map for `/hex-architect` when `tier=auto`, plus the candidate
research axes and overlay triggers that stack on the chosen tier. The
classifier reads the free-text decision and emits a tier — **only `low`,
`medium`, `high`, or `xhigh`** — with a confidence flag and a ranked list of candidate
research axes.

The classifier **never emits `max`** — that tier is explicit only,
`--tier=max` or a plan whose Status block says `Tier: max`
([`protocol.md`](../hex-core/references/protocol.md#tier-grammar)). When
signals split across adjacent tiers, mark **low-confidence** — that forces
the meta-plan gate in [`SKILL.md`](SKILL.md) step 4. Never fire a mid-flow
question; ambiguity is resolved at the single gate.

## Decision-weight signals

Four signal categories feed the tier call — read them together, not in
isolation; a decision can be low on one axis and high on another (e.g. a
single-area change to an external contract):

- **Reversibility** — two-way door (cheap to undo later) vs. one-way door
  (expensive or impossible to undo once shipped).
- **Blast radius** — single area (one module/component) vs. cross-area
  (multiple modules/subsystems) vs. external contract (a public API, wire
  format, or storage format another consumer depends on).
- **Novelty** — an established pattern already used elsewhere in the project
  vs. a genuinely new approach with no local precedent.
- **Compliance / security touch** — none vs. the decision affects auth,
  crypto, data handling, or a regulated domain.

## Tier signal table

| Tier | Signals | Examples |
|---|---|---|
| **low** | Trivial two-way door; a naming or local-style call inside one file; no contract of any kind, no precedent needed | `snake_case or camelCase for this private helper`, `keep the early return or nest it` |
| **medium** | Two-way door; single area; established pattern; no external contract; no compliance/security touch | `builder vs constructor for this type`, `sync or async for this internal helper`, `naming convention for the new module` |
| **high** | One-way-door medium; single area or an internal contract; established pattern applied in a new way, or a bounded novel approach | `storage layout for the new index`, `caching strategy for the lookup path`, `internal event bus vs direct calls` |
| **xhigh** | One-way-door high; cross-area or an external/public contract; genuinely novel approach; compliance or security-critical | `public plugin API shape`, `wire protocol for the new sync mechanism`, `auth boundary redesign`, `data model migration with no rollback` |

Pick the **highest** tier with at least one clear signal. A single `low` or `medium`
keyword does not demote a decision whose body describes a one-way-door
change. `high` is the default working tier — most architecture decisions
land here.

## Confidence rules

- **Confident** — one tier has ≥2 matching signals and no competing signal
  from an adjacent tier. Announce and proceed (the gate is a fast
  announce-and-go, though axis selection is still offered — see
  [`SKILL.md`](SKILL.md) step 4).
- **Low-confidence** — signals split across adjacent tiers, or the decision
  is terse with no discriminating cue (e.g. "should we cache this?" with no
  scope given). Flag it; [`SKILL.md`](SKILL.md) routes into the blocking
  meta-plan gate.

Never manufacture a question when confident: *announce and proceed*, or *let
the gate handle it*.

## Research axis candidates

The classifier proposes a ranked subset of this catalog based on keywords in
the decision text; the tier baseline in [`overlays.md`](overlays.md) sets how
many actually run (`high` 1, `xhigh` 3) — **the user selects which** at the
gate. This is the signature interaction at `/hex-architect`, more central
here than in any other hex skill.

| Axis | Surfaced when the decision mentions… |
|---|---|
| Technology / tooling | a library, framework, protocol, or "what should we use for X" |
| Design-pattern precedent | "how do others solve this", "is there a standard pattern for X" |
| Performance & scale | throughput, latency, "at scale", concurrency, large data volumes |
| Security & compliance | auth, crypto, PII, a regulated domain, "attack surface" |
| Operability & cost | infra spend, on-call burden, observability, "who maintains this" |
| Data model / compatibility | schema, migration, wire format, backward compatibility |
| Product / competitive landscape | "how do similar tools solve this", a comparable tool named, a user-facing behavior change |

Fewer than 3 candidates surface clearly → the classifier still proposes 3 by
including the next-best generic matches, flagged as lower-confidence
suggestions the user can swap out. `hex.md › Preferences` research axes of
interest fold into these candidates before the gate, ranked alongside the
keyword-derived ones ([spawn-selection
precedence](../hex-core/references/protocol.md#spawn-selection-precedence));
the project's product knowledge (located via `hex.md › Pointers`) — research
keywords and comparable tools — seeds the product/competitive axis (run by
a `researcher` in `competitive-research` focus) and sharpens the others'
search terms.

## Product / requirement clarification

When the decision text is silent on **who it serves, what constrains it,
or what success looks like**, the classifier emits up to 3
`[NEEDS CLARIFICATION: <question>]` markers — each with a
`Recommended: <answer> — <reason>` line — covering the missing slots
(users/context, constraints, success criteria). They ride the **existing
single gate** exactly like artifact markers
([`protocol.md`](../hex-core/references/protocol.md#the-meta-plan-approval-gate));
never a separate question round. The project's product knowledge (located
via `hex.md › Pointers`) answers the users/context slot by default —
suppress a question whose answer it already holds, and fold that answer
into the recommendation instead.
Markers that survive the gate become the ADR's Open questions — one
shared pool under the same hard cap of 3 ([`SKILL.md`](SKILL.md)); the
classifier never adds a second budget.

## Overlay triggers

| Overlay | Triggered by signals |
|---|---|
| `adversary=on` | one-way-door high; external/public contract; compliance/security touch; label `breaking-change` or `security` |
| `artifact=system-design` (adds to `adr`, does not replace it) | cross-area or external-contract blast radius; "new module/package/subsystem" phrasing |

## Examples

1. `/hex-architect "sync or async for the new internal retry helper"` → tier
   **medium**, no overlays, confident.
2. `/hex-architect "storage layout for the new tag-lock cache"` → tier
   **high** (one-way-door medium, internal contract), research candidates:
   technology/tooling, data model/compatibility, performance & scale.
3. `/hex-architect "should we cache this?"` → **low-confidence** (no scope,
   no reversibility cue). The gate fires.
4. `/hex-architect "public plugin API for third-party extensions"` → tier
   **xhigh** (external contract, novel) + `adversary=on` (public API change),
   research candidates: design-pattern precedent, security & compliance,
   data model/compatibility.
