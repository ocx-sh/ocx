# Classification Signals

Signal-to-tier map for `/hex-plan` when `tier=auto`, plus the overlay
triggers that stack on the chosen tier. The classifier reads the free-text
target and any GitHub context (PR/issue title, body, labels), and emits a
tier — **only `low`, `medium`, `high`, or `xhigh`** — with zero or more overlays.

The classifier **never emits `max`** — that tier is explicit only,
`--tier=max` or a plan whose Status block says `Tier: max`
([`protocol.md`](../hex-core/references/protocol.md#tier-grammar)). When signals
split across adjacent tiers, or the overlay mix is unusual, mark
**low-confidence** — that forces the meta-plan gate in
[`SKILL.md`](SKILL.md) step 5. Never fire a mid-flow question; ambiguity is
resolved at the single gate.

## Tier signal table

| Tier | Signals | Examples |
|---|---|---|
| **low** | Trivial two-way door; one file, ≤30 lines estimated; a typo, a constant, a one-line flag default, a doc line; no structural marker, no security-sensitive or hot path; label `trivial`, `typo` | `fix the typo in the README`, `bump the default page size to 50`, `add the missing period to the error message` |
| **medium** | Two-way door; flag/option change; doc edit; fixture or test-data addition; single area; ≤3 files estimated; label `small`, `docs`, `chore` | `add a --format yaml flag`, `update the install docs`, `add a fixture for the archive test` |
| **high** | One-way-door medium; new command/subcommand; new index or storage layout; 1–2 areas; label `enhancement`, `feature` | `new env-composition command`, `add a referrers API`, `introduce a tag-lock cache` |
| **xhigh** | One-way-door high; new module/package; breaking API; cross-area refactor; protocol change; label `breaking-change`, `epic` | `metadata-first pull pipeline`, `new mirror backend`, `refactor the reference layer to event-sourced` |

Pick the **highest** tier with at least one clear signal. A single `low` or `medium`
keyword does not demote a target whose body describes a high-effort change.
`high` is the default working tier — most feature work lands here.

## Confidence rules

- **Confident** — one tier has ≥2 matching signals and no competing signal
  from an adjacent tier. Announce and proceed (the gate is a fast
  announce-and-go).
- **Low-confidence** — signals split across adjacent tiers (e.g. one `medium` +
  one `high`), or the target is terse with no discriminating cue. Flag it;
  [`SKILL.md`](SKILL.md) routes into the blocking meta-plan gate.

Never manufacture a question when confident: *announce and proceed*, or *let
the gate handle it*.

## Overlay triggers

Overlays adjust a single axis on top of the chosen tier and stack — several
may fire. Axis definitions and per-tier defaults are in
[`overlays.md`](overlays.md).

| Overlay | Triggered by signals |
|---|---|
| `architect=on` | "new type/trait hierarchy", "novel algorithm", "cross-area", "protocol change", "storage-layout change" |
| `research=3` | "new dependency category", "SOTA comparison", "compliance requirement", "security-sensitive area", "cryptographic primitive" |
| `adversary=on` | one-way-door medium or high; "public API change"; "breaking change"; "security-sensitive"; "novel algorithm"; label `breaking-change`; label `security`; any one-way door costly to reverse |

Project hints in the Preferences section of `.agents/memory/hex.md`
(always-on perspectives, path-triggered escalations, research axes) fold
into these suggestions before the gate
([spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence)).

## GitHub context as a classification input

When `/hex-plan <N>` resolves to a PR or issue, feed the fetched context into
the signal matcher alongside the free-text prompt:

- **Title + body** — treated as the prompt.
- **Labels** — mapped directly to signals: `breaking-change` → `adversary=on`,
  `small` → hint toward `medium`, `epic` → hint toward `xhigh`, `security` →
  `research=3` + `adversary=on`.
- **PR file list** — feeds the Discover scope, not the tier decision.

## Examples

1. `/hex-plan "add a --json flag to the list command"` → tier **medium**, no
   overlays, confident.
0. `/hex-plan "fix the typo in the README"` → tier **low** (one file, no
   code), confident — the orchestrator edits inline, zero spawns.
2. `/hex-plan "refactor the pull pipeline"` → tier **xhigh** (cross-area) +
   `adversary=on` (public API), `architect=on` (cross-area design).
3. `/hex-plan "extend the index cache"` → **low-confidence** (split between
   `medium` — "extend" — and `high` — "cache-layout change"). The gate fires.
4. `/hex-plan 143` where PR #143 carries `breaking-change` + `enhancement` →
   tier **high** promoted toward **xhigh** by the breaking-change signal, +
   `adversary=on` (from the `breaking-change` label). Resolve at the gate if
   the promotion is unclear.
