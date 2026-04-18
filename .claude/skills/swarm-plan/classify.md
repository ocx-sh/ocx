# Classification Signals

Signal-to-tier mapping used when `/swarm-plan` runs with tier=`auto`. Also
defines overlay triggers that stack on top of the chosen tier.

The classifier reads the free-text target and any GitHub context (PR/issue
title, body, labels) and picks a tier plus zero or more overlays. When
signals are genuinely split across adjacent tiers, or the overlay mix is
unusual, mark the classification **low-confidence** — this forces the
meta-plan gate in SKILL.md step 5. Do **not** fire a mid-flow
`AskUserQuestion`; ambiguity is always resolved at the single approval
gate.

## Tier signal table

| Tier | Signals | Examples |
|---|---|---|
| **low** | Two-Way Door; flag/option change; doc edit; fixture addition; single subsystem; ≤3 files estimated; label `small`, `docs`, `chore` | `add --format yaml flag`, `update installation docs`, `add fixture for archive unit test` |
| **high** | One-Way Door Medium; new subcommand; new index format; new storage layout; 1–2 subsystems; label `enhancement`, `feature` | `new ocx env composition command`, `add referrers API`, `introduce tag lock cache` |
| **max** | One-Way Door High; new crate; breaking API; cross-subsystem refactor; protocol change; label `breaking-change`, `epic` | `metadata-first pull pipeline`, `new mirror backend`, `refactor reference manager to event-sourced model` |

A prompt may match multiple rows — pick the **highest** tier for which at
least one clear signal fires. A single `low` keyword does not demote a
feature whose body clearly describes a high-effort change.

## Confidence rules

- **Confident**: one tier has ≥2 matching signals and no competing
  signals from an adjacent tier. Proceed without the meta-plan gate.
- **Low-confidence**: signals split across adjacent tiers (e.g., one
  `low` and one `high` signal), or the target is terse and provides no
  discriminating cues. Flag the classification; SKILL.md routes it into
  the meta-plan gate.

Never manufacture a question when you are confident. The rule is:
*announce and proceed*, or *let the meta-plan gate handle it*.

## Overlay triggers

Overlays adjust a single axis on top of the chosen tier. They stack —
multiple triggers may fire. Axis definitions and their effects live in
`overlays.md`.

| Overlay | Triggered by signals |
|---|---|
| `--architect=opus` | "new trait hierarchy", "novel algorithm", "cross-subsystem", "protocol change", "content-addressed storage layout change" |
| `--research=3` | "new dependency category", "SOTA comparison", "compliance requirement", "security-sensitive area", "cryptographic primitive" |
| `--researcher=haiku` | `--research=1` (single-axis) AND prompt is a narrow single-concept factual lookup (e.g., "check if crate X has a CVE", "find the current version of tool Y", "does library Z support feature W"); NO cross-subsystem keywords; NO multi-source synthesis signal. Context-cap guard: if projected research prompt + sources exceeds 150k tokens, stay `sonnet`. |
| `--codex` (plan review) | One-Way Door Medium/High; "public API change"; "breaking change"; "security-sensitive"; "novel algorithm"; label `breaking-change`; label `security`; any One-Way Door that would be costly to reverse |

Defaults per tier (before overlays apply):

| Axis | low | high | max |
|---|---|---|---|
| architect | inline | inline (Two-Way) / sonnet (One-Way) | opus |
| research | skip | 1 | 3 |
| researcher | sonnet | sonnet (→ haiku on narrow-scope trigger) | sonnet |
| codex plan review | off | off (auto-on for One-Way Door) | on (mandatory) |

## GitHub context as a classification input

When `/swarm-plan <N>` resolves to a PR or issue, feed the following into
the signal matcher alongside the free-text prompt:

- Title + body (treat as the prompt)
- Labels (map directly to signals — `breaking-change` → `--codex`,
  `small` → hint toward `low`, `epic` → hint toward `max`)
- For PRs: file list (feeds Discover scope, not classification)

## Examples

1. `/swarm-plan "add --json flag to ocx ls"` → tier=**low**, no overlays,
   confident.
2. `/swarm-plan "refactor the OCI pull pipeline"` → tier=**high** +
   overlays `--architect=opus` (cross-subsystem), `--codex` (public API).
   Likely promoted to `max` by the `--architect=opus` + cross-subsystem
   combo — depends on prompt details.
3. `/swarm-plan "extend index caching"` → low-confidence (split between
   `low` — "extend" — and `high` — "caching layer change"). Meta-plan
   gate fires.
4. `/swarm-plan 143` where PR #143 has label `breaking-change` +
   `enhancement` → tier=**high** + `--codex` overlay (from
   `breaking-change` label).
