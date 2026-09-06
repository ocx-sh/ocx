# Overlay Axis Definitions

Overlays are single-axis adjustments layered on the tier
[`classify.md`](classify.md) chose. They let `auto` mode assemble a mixed
config (e.g. a `high` base that still wants the full breadth panel for a
dependency-manifest change) without compound tier names.
[`classify.md`](classify.md) decides *when* an overlay fires from signals;
this file defines *what each axis means* and how it changes the pipeline.
Grammar only lives here — the shared overlay rules are in
[`protocol.md`](../hex-core/references/protocol.md#overlay-grammar).

## `--base` is not an axis

`--base=<git-ref>` is a **pipeline input**, not an overlay axis. It sets
the diff baseline for the whole run — metrics, tier selection, review
scope, and the adversary scope all read from it. An axis is a
per-dimension adjustment that stacks with the others; `--base` is a single
pre-pipeline resolution step (see [`SKILL.md`](SKILL.md) step 2).

## Axis grammar (flag values)

Matches the [`SKILL.md`](SKILL.md) parser:

```
--breadth=minimal|full|adversarial
--rca=on|off
--adversary / --no-adversary
```

hex-review never exposes a literal model flag — model choice resolves
through [`models.md`](../hex-core/references/models.md) only, per
[spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence).

## breadth axis

Controls the Stage 2 panel. Stage 1 (`reviewer` focus `spec` +
test-coverage emphasis) runs at every tier regardless of breadth.

| Value | Effect |
|---|---|
| `minimal` | Stage 2 launches **only** `reviewer` (focus `quality`). No security, performance, docs, architect, or researcher perspective. |
| `full` | Stage 2 launches `reviewer` (quality / security / performance, each only when its trigger fires) plus `doc-reviewer` when a doc trigger matches. |
| `adversarial` | Stage 2 adds `architect` (boundary, dependency-direction review) and `researcher` (SOTA / known-pitfall gap check) to the `full` set; the `quality` reviewer adds an interface/UX-consistency emphasis when the diff touches a user-facing surface. |

Per-tier defaults:

| Tier | breadth default |
|---|---|
| low | `minimal` — the orchestrator reads the diff itself against `spec` + `quality`; zero spawns |
| medium | `minimal` |
| high | `full` |
| xhigh | `adversarial` |
| max | `adversarial` |

## rca axis

Controls the depth of root-cause (Five Whys) analysis applied to findings.

| Value | Effect |
|---|---|
| `off` | No Five Whys chains. A finding is reported with proximate cause and remediation only. A systemic-smelling finding is still flagged deferred with a reason. |
| `on` | Findings above a tier-scoped [severity](../hex-core/references/severity.md#finding-severity) get a Five Whys chain and a systemic-fix recommendation. Scope: `high` applies it to Block/High findings; `xhigh` and `max` apply it to everything above Suggest. |

Per-tier defaults: low → `off`, medium → `off`, high → `on` (Block/High),
xhigh → `on` (above Suggest), max → `on` (above Suggest).

## adversary axis

Controls whether the configured cross-model adversary skill runs against
the diff (or the artifact, when the target is a markdown file), launched
**inside the Stage 2 batch, last** — never after the panel
([`adversary.md`](../hex-core/references/adversary.md#adversary-contract),
`adr_0016` C-987) — and triaged in the Cross-model phase. The skill name is read from the
Preferences section of `.agents/memory/hex.md` (`codex-adversary`
is only an example value); the full contract — scopes, one-shot rule, 4-way
triage, graceful skip, stall bound and backstop — is in
[`adversary.md`](../hex-core/references/adversary.md#adversary-contract).
This is `code-diff` scope for a branch/PR/working-tree target,
`plan-artifact` scope for a markdown-artifact target
([`classify.md`](classify.md#plan-and-artifact-targets)).

| Value | Effect |
|---|---|
| `off` | No cross-model pass. |
| `on` | Invoke the adversary skill once against the diff or artifact, launched last in the Stage 2 batch. One-shot, no loop. Triage its findings 4-way (actionable / deferred / stated-convention / trivia). Review is read-only: an actionable finding is reported, never auto-fixed — hand off to `/hex-execute` if the caller wants it applied. |

Per-tier defaults:

| Tier | adversary default |
|---|---|
| low | `off` (inline tier); explicit `--adversary` runs it alone, read-only |
| medium | `off` (two-way door — cost outweighs value) |
| high | `off`, auto-on when [`classify.md`](classify.md) fires `adversary=on` for a one-way-door or security signal; explicit via `--adversary` |
| xhigh | `on` (a default part of the flow; a skip is surfaced prominently) |
| max | `on` — **every** entry of a list-valued `adversary` key, in the Stage 2 batch; below `max` the first entry only (`adr_0017` C-995) |

When the adversary produces no review — the named skill is unavailable, or it
ran and did not complete one — log
`Cross-model review skipped: <reason>` and continue — a gate, not a
blocker ([`adversary.md`](../hex-core/references/adversary.md#adversary-contract)).

## Precedence

User-supplied flags always override classifier-inferred overlays, which in
turn fold in `hex.md › Preferences` hints on top of the tier baseline —
later wins
([spawn-selection precedence](../hex-core/references/protocol.md#spawn-selection-precedence)).
When [`classify.md`](classify.md) infers `breadth=adversarial` from a
cross-area diff but the user passes `--breadth=full`, the user wins.
`tiers.hex-review.<tier>.overlays` sets a per-project default for an
overlay axis — it rewrites this axis's tier baseline (a layer-1 rewrite, not a project hint — see [`config.md` § tiers](../hex-core/references/config.md#tiers)), below any user flag.
[`SKILL.md`](SKILL.md) step 6 prints the final resolved config with each
axis's source.

## Per-tier defaults (cheat-sheet)

| Axis | low | medium | high | xhigh | max |
|---|---|---|---|---|---|
| breadth | minimal (inline) | minimal | full | adversarial | adversarial |
| rca | off | off | on (Block/High) | on (above Suggest) | on (above Suggest) |
| adversary | off | off | off (auto-on on signal) | on (mandatory) | on (every configured) |
