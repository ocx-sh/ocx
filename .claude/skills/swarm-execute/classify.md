# Classification Signals — /swarm-execute

Signal-to-tier map for `/swarm-execute` with tier=`auto`.
Also defines overlay triggers stack on top of chosen tier.

**Primary signal: plan-artifact header.** Plans from tiered
`/swarm-plan` carry classification in handoff block. #1 classifier
input — read first, apply verbatim, fall back to text signals only
when target is free-text task description.

When signals split across adjacent tiers, or overlay mix unusual,
mark classification **low-confidence** — forces meta-plan gate in
SKILL.md. Do **not** fire mid-flow `AskUserQuestion`; ambiguity
resolved at single approval gate.

## Primary: plan-artifact header

When target is file path ending `.md` (typically under
`.claude/artifacts/`), grep plan for handoff block from `/swarm-plan`:

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

If plan header missing any field, fall back to free-text
classification for that axis only — no guess.

## Fallback: free-text targets

When no plan file passed (argument = free-text task description),
apply same signal table as `/swarm-plan`'s classify.md — same target
language, same scope cues.

> **Read `.claude/skills/swarm-plan/classify.md` "Tier signal table" section
> directly** when classifying free-text execute target. This file no
> duplicate that table — sibling skills cross-read to stay lockstep.

Execute-specific deltas from plan-side table:

- Execute's `low` tier still needs ≥1 concrete file/test to
  change. Pure doc edit alone not execute target — point
  user to `/commit`.
- Execute's `max` tier needs existing plan artifact at target.
  Free-text `max` targets re-route through `/swarm-plan max`
  first to produce plan — announce, stop.

## Confidence rules

- **Confident**: plan-header `Tier:` set, or free-text target has ≥2
  matching signals, no competing signal from adjacent tier.
  Proceed, skip meta-plan gate.
- **Low-confidence**: plan header partial/absent AND free-text signals
  split across adjacent tiers. Flag; SKILL.md routes to
  meta-plan gate.

Never manufacture question when confident. *Announce and proceed*, or
*let meta-plan gate handle*.

## Overlay triggers

Overlays adjust single axis on top of chosen tier. Stack —
multiple triggers may fire. Axis defs in `overlays.md`.

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

Defaults per tier (before overlays):

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

1. `/swarm-execute .claude/state/plans/plan_small_flag.md` header reads
   `Tier: low` → tier=**low**, no overlays, confident. Plan-header wins.
2. `/swarm-execute .claude/state/plans/plan_refactor_oci.md` with
   `Tier: high` + `Reversibility: One-Way Door Medium` → tier=**high**
   + `--codex` (from Reversibility signal), confident.
3. `/swarm-execute "add --json flag to ocx ls"` (free text) →
   fall back to `/swarm-plan`'s low-tier signal; tier=**low**, confident.
4. `/swarm-execute "refactor the entire OCI pull pipeline"` (free text,
   no plan) → classifier proposes **max**, but max needs
   plan artifact, SKILL.md announces and stops, ask user to run
   `/swarm-plan max "…"` first.
5. `/swarm-execute .claude/state/plans/plan_x.md` partial header
   (no `Tier:` field) + free-text signals split between `low` and
   `high` → low-confidence → meta-plan gate fires.