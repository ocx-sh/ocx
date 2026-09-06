# Overlay Axis Definitions

Overlays are single-axis adjustments layered on the tier
[`classify.md`](classify.md) chose. [`classify.md`](classify.md) decides
*when* an overlay fires from signals; this file defines *what each axis
means* and how it changes the pipeline. Grammar only lives here — the shared
overlay rules are in
[`protocol.md`](../hex-core/references/protocol.md#overlay-grammar).

## Axis grammar (flag values)

Matches the [`SKILL.md`](SKILL.md) parser:

```
--research=skip|1|3
--axes=<comma-separated>
--adversary / --no-adversary
--artifact=inline|adr|system-design
```

## research axis

Controls the Research phase worker count. The axis **count** is set here;
the axis **selection** is a gate interaction, not a flag default — see
[`SKILL.md`](SKILL.md) step 4.

| Value | Effect |
|---|---|
| `skip` | No `researcher` launched. The orchestrator may still make a brief inline check against project context or a feature-detected docs tool. |
| `1` | One `researcher` on the single axis picked at the gate from [`classify.md`](classify.md)'s ranked candidates (default: the top-ranked candidate on plain approval). |
| `3` | Three `researcher` workers in parallel, one per axis picked at the gate (default: the top-3 candidates). |

Per-tier defaults: `low` → `skip`, `medium` → `skip`, `high` → `1`,
`xhigh` → `3` (mandatory), `max` → `5` (mandatory, one axis
`competitive-research`; `adr_0017` C-995).
`--research=3` at `high` promotes research breadth alone — the rest of the
tier (single architect worker, ADR-only artifact, bounded review) stays at
`high`'s rules. `--axes=<a,b,c>` names axes explicitly, bypassing the
interactive picker; it is still echoed at the gate for confirmation, and its
length must match the resolved count (extra names beyond the count are
dropped with a note, fewer names fall back to classifier candidates for the
remainder). Researcher model class is `fast-balanced` at every tier
([`models.md`](../hex-core/references/models.md)); literal model choices
live in `hex.md › Preferences`, never in a flag.

## adversary axis (plan-artifact scope)

Controls whether the configured cross-model adversary skill runs against the
**design artifact** (the ADR, or the ADR + system-design doc), launched
**inside the Round 1 design-panel batch, last** — never after the panel
([`adversary.md`](../hex-core/references/adversary.md#adversary-contract),
`adr_0016` C-987). Distinct from the *adversarial
design panel* itself (in-harness `reviewer` perspectives, a tier-baseline
behavior described in each tier file's Review phase) — this axis is only the
cross-model pass. The skill name is read from the Preferences section of
`.agents/memory/hex.md` (`codex-adversary` is only an example
value); the full contract — scopes, one-shot rule, 4-way triage, graceful skip,
stall bound and backstop — is in
[`adversary.md`](../hex-core/references/adversary.md#adversary-contract). This
is the `plan-artifact` scope; `/hex-execute` runs the same skill in
`code-diff` scope on implementation later.

| Value | Effect |
|---|---|
| `off` | No cross-model design review. |
| `on` | Invoke the adversary skill once in `plan-artifact` scope on the ADR (and system-design doc, if produced), launched last in the Round 1 panel batch. One-shot, no loop. Triage its findings 4-way (actionable / deferred / stated-convention / trivia); actionable fixes are applied with the panel's and validated by the same single `reviewer` (focus `spec`) pass. |

Per-tier defaults:

| Tier | adversary default |
|---|---|
| low | `off` (inline tier); explicit `--adversary` runs it alone |
| medium | `off` (two-way door — cost outweighs value; no design panel either) |
| high | `off`, auto-on when [`classify.md`](classify.md) fires `adversary=on` for one-way-door signals, and auto-on on dossier fast-path input ([`SKILL.md`](SKILL.md#a-discussion-dossier-as-decision)); explicit via `--adversary` |
| xhigh | `on` (a default part of the flow; a skip is surfaced prominently) |
| max | `on` — **every** entry of a list-valued `adversary` key, in the Round 1 batch; below `max` the first entry only (`adr_0017` C-995) |

When the adversary produces no review — the named skill is unavailable, or it
ran and did not complete one — log
`Cross-model design review skipped: <reason>` and continue — a gate, not a
blocker ([`adversary.md`](../hex-core/references/adversary.md#adversary-contract)).

## artifact axis

Controls what the design produces.

| Value | Effect |
|---|---|
| `inline` | Trade-off table and recommendation written directly in the conversation and the handoff block; no design-record file. |
| `adr` | An Architecture Decision Record is produced at the convention-resolved location ([`SKILL.md`](SKILL.md) › The design artifact). |
| `system-design` | A fuller system-design section (C4 levels, NFR detail, migration plan) is produced **in addition to** the ADR, not instead of it — for high-blast-radius or new-module decisions. |

Per-tier defaults:

| Tier | artifact default |
|---|---|
| low | `inline` (a decision note in the handoff; never an ADR) |
| medium | `inline` (an ADR is written only if the user asks — `--artifact=adr`) |
| high | `adr` |
| xhigh | `adr`, with `system-design` added when [`classify.md`](classify.md)'s cross-area / external-contract / new-module signal fires |
| max | `adr` + `system-design` (both mandatory) |

## Precedence

User-supplied flags always override classifier-inferred overlays, which in
turn fold in `hex.md › Preferences` hints on top of the tier
baseline — later wins
([spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence)).
When [`classify.md`](classify.md) infers `adversary=on` but the user passes
`--no-adversary`, the user wins. `tiers.hex-architect.<tier>.overlays` sets a
per-project default for an overlay axis — it rewrites this axis's tier baseline (a layer-1 rewrite, not a project hint — see [`config.md` § tiers](../hex-core/references/config.md#tiers)),
below any user flag.
[`SKILL.md`](SKILL.md) step 5 prints the final resolved config with each
axis's source.
