# Classification Signals — /swarm-execute

Signal-to-tier mapping used when `/swarm-execute` runs with tier=`auto`.
Also defines overlay triggers that stack on top of the chosen tier.

**Primary signal: plan-artifact header.** Plans produced by tiered
`/swarm-plan` carry their classification in the handoff block. That is the
#1 classifier input — read it first, apply it verbatim, fall back to text
signals only when the target is a free-text task description.

When signals are genuinely split across adjacent tiers, or the overlay mix
is unusual, mark the classification **low-confidence** — this forces the
meta-plan gate in SKILL.md. Do **not** fire a mid-flow `AskUserQuestion`;
ambiguity is always resolved at the single approval gate.

## Primary: plan-artifact header

When the target is a file path ending `.md` (typically under
`.claude/artifacts/`), grep the plan for the handoff block emitted by
`/swarm-plan`:

| Field | Mapping |
|---|---|
| `Tier: low \| high \| max` | Use verbatim (overrides classifier) |
| `Scope: Small` | → `low` (only when `Tier:` absent) |
| `Scope: Medium` | → `high` |
| `Scope: Large` | → `max` |
| `Reversibility: One-Way Door Medium` | force `--codex` on (auto-on trigger for `high`) |
| `Reversibility: One-Way Door High` | force `--codex` on (mandatory for `max`) |
| `Overlays: codex=on` / `codex=off` | adopt verbatim |
| `Overlays: architect=opus` | suggest `--builder=opus` (novel architecture → complex implementation) |

If the plan header is missing any field, fall back to free-text
classification for just that axis — don't guess.

## Fallback: free-text targets

When no plan file is passed (the argument is a free-text task description),
apply the same signal table as `/swarm-plan`'s classify.md — same target
language, same scope cues.

> **Read `.claude/skills/swarm-plan/classify.md` "Tier signal table" section
> directly** when classifying a free-text execute target. This file does
> not duplicate that table — sibling skills cross-read to stay in lockstep.

Execute-specific deltas from the plan-side table:

- Execute's `low` tier still requires at least one concrete file/test to
  change. A pure doc edit on its own is not an execute target — point
  the user to `/commit` instead.
- Execute's `max` tier requires an existing plan artifact at the target.
  Free-text `max` targets should be re-routed through `/swarm-plan max`
  first to produce the plan — announce this and stop.

## Confidence rules

- **Confident**: plan-header `Tier:` set, or free-text target has ≥2
  matching signals with no competing signal from an adjacent tier.
  Proceed without the meta-plan gate.
- **Low-confidence**: plan header partial/absent AND free-text signals
  split across adjacent tiers. Flag; SKILL.md routes it into the
  meta-plan gate.

Never manufacture a question when confident. *Announce and proceed*, or
*let the meta-plan gate handle it*.

## Overlay triggers

Overlays adjust a single axis on top of the chosen tier. They stack —
multiple triggers may fire. Axis definitions live in `overlays.md`.

| Overlay | Triggered by |
|---|---|
| `--builder=opus` | Plan "Subsystems Touched" lists ≥2; plan or prompt mentions "novel algorithm", "new trait hierarchy", "cross-subsystem", "protocol change" |
| `--tester=opus` | tier=max (mandatory — reflects the exhaustive edge-case coverage work documented in `tier-max.md` Phase 4) |
| `--reviewer=haiku` | tier=low AND NO structural markers from `swarm-review/classify.md:48-61` present in the diff |
| `--reviewer=opus` | tier=max AND `--breadth=adversarial` |
| `--doc-reviewer=haiku` | Diff touches ≤2 doc files (`website/**/*.md` or `CHANGELOG.md`) AND does not touch `website/src/docs/user-guide.md` |
| `--loop-rounds=1` | tier=low; or plan tags the feature as Two-Way Door |
| `--loop-rounds=3` | tier=high or tier=max (default) |
| `--review=adversarial` | Security-sensitive paths (`oci/`, `auth`, `crypto/`, `signing/`); plan labels `security`; diff touches `package_manager/` |
| `--codex` | Plan header `Reversibility: One-Way Door` (Medium or High); breaking-change signals in plan or prompt; `Overlays: codex=on` |

Defaults per tier (before overlays apply):

| Axis | low | high | max |
|---|---|---|---|
| builder | sonnet | sonnet | opus |
| tester | sonnet | sonnet | opus |
| reviewer | haiku (→ sonnet on structural markers) | sonnet | sonnet (→ opus on adversarial breadth) |
| doc-reviewer | sonnet | sonnet (→ haiku on narrow doc scope) | sonnet (→ haiku on narrow doc scope) |
| loop-rounds | 1 | 3 | 3 |
| review | minimal | full | adversarial |
| codex | off | off (auto-on for One-Way Door) | on (mandatory) |

## Examples

1. `/swarm-execute .claude/artifacts/plan_small_flag.md` whose header reads
   `Tier: low` → tier=**low**, no overlays, confident. Plan-header wins.
2. `/swarm-execute .claude/artifacts/plan_refactor_oci.md` with
   `Tier: high` + `Reversibility: One-Way Door Medium` → tier=**high**
   + `--codex` (from Reversibility signal), confident.
3. `/swarm-execute "add --json flag to ocx ls"` (free text) →
   fall back to `/swarm-plan`'s low-tier signal; tier=**low**, confident.
4. `/swarm-execute "refactor the entire OCI pull pipeline"` (free text,
   no plan) → classifier proposes **max**, but because max requires a
   plan artifact, SKILL.md announces and stops, asking the user to run
   `/swarm-plan max "…"` first.
5. `/swarm-execute .claude/artifacts/plan_x.md` with a partial header
   (no `Tier:` field) and free-text signals split between `low` and
   `high` → low-confidence → meta-plan gate fires.
