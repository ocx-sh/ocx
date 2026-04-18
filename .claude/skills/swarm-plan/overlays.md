# Overlay Axis Definitions

Overlays are single-axis adjustments layered on top of the chosen tier.
They let `auto` mode pick mixed configurations (e.g., "high base with
opus architect for a medium-scope feature that still has weighty
architecture") without introducing compound-name tiers.

The classifier (`classify.md`) decides *when* to apply an overlay based
on signals. This file defines *what each axis means* and how it affects
the pipeline.

## Axis grammar (flag values)

Matches `SKILL.md`'s argument parser.

```
--architect=inline|sonnet|opus
--research=skip|1|3
--researcher=haiku|sonnet
--codex / --no-codex
```

## Axis definitions

### architect axis

Controls the Design phase.

| Value | Effect |
|---|---|
| `inline` | Design drafted inline in the plan artifact by the orchestrator. No worker launched. |
| `sonnet` | Launch `worker-architect` with model=sonnet. Produces ADR or system-design artifact. For Medium reversible decisions. |
| `opus` | Launch `worker-architect` with model=opus. Used when the decision is a one-way door with significant trade-offs (new trait hierarchy, novel algorithm, cross-subsystem, protocol change). |

Per-tier defaults:
- low → `inline`
- high → `inline` for Two-Way Door scope, `sonnet` for One-Way Door Medium
- max → `opus` (mandatory, with ADR)

### research axis

Controls the Research phase worker count.

| Value | Effect |
|---|---|
| `skip` | No `worker-researcher` launched. Orchestrator may still do a brief inline check against product-context.md. |
| `1` | One `worker-researcher`. Orchestrator picks the single most relevant axis (tech OR patterns OR domain). |
| `3` | Three `worker-researcher` agents in parallel, one per axis: tech / patterns / domain. |

Per-tier defaults:
- low → `skip`
- high → `1`
- max → `3` (mandatory)

### researcher axis

Controls the model for each `worker-researcher` launched during the Research phase. Mirrors Claude Code's own pattern — Explore uses Haiku for narrow codebase search; Plan uses inherit for synthesis. At OCX, narrow single-axis factual lookups can run on Haiku; multi-axis synthesis and any research touching the web at scale stays on Sonnet.

| Value | Effect |
|---|---|
| `haiku` | `worker-researcher` with model=haiku. Triggered when `--research=1` AND the research target is a single narrow factual lookup (no cross-subsystem keywords, no multi-source synthesis signal). |
| `sonnet` | `worker-researcher` with model=sonnet. Default at all tiers where research runs; used whenever the haiku trigger does not fire. |

Per-tier defaults:
- low → `sonnet` (moot: research=skip at tier=low — no researcher launches)
- high → `sonnet` (→ `haiku` when `--research=1` AND narrow-scope trigger fires)
- max → `sonnet` (research=3 is synthesis — never haiku at max)

**Context-cap guard**: Haiku 4.5's 200k context window is a hard constraint. If the research prompt projects to exceed 150k tokens (e.g., WebFetch on >5 sources, reading >5 large files), escalate to `sonnet` regardless of the narrow-scope trigger. See `.claude/artifacts/research_model_capability_matrix.md` for the context-cap rationale.

### codex axis (plan-artifact scope)

Controls whether the `codex-adversary` skill runs against the plan
artifact as a final cross-model gate after the Claude review panel
converges. This is distinct from `/swarm-execute`'s Codex pass on the
branch diff — same entry point (`codex-adversary`), different scope
target.

| Value | Effect |
|---|---|
| `off` | No Codex plan review. |
| `on` | After Claude panel converges in Phase 6, invoke `codex-adversary` with scope `plan-artifact` on the plan file path. One-shot, no looping. Triage findings into actionable / deferred / stated-convention / trivia. Actionable findings re-validated by a single `worker-reviewer` (spec-compliance) pass. |

Per-tier defaults:
- low → `off` (Two-Way Door — cost > value)
- high → `off` by default, auto-on when `classify.md` fires the
  `--codex` overlay for One-Way Door signals; explicit via `--codex`
- max → `on` (mandatory, final gate before handoff)

Triage mirrors the existing cross-model pass triage in the main
`codex-adversary` skill:

- **Actionable** — orchestrator edits the plan artifact, re-runs
  spec-compliance reviewer
- **Deferred** — added to Deferred Findings in the handoff summary
- **Stated-convention** — critiques a load-bearing project convention;
  dropped, count mentioned
- **Trivia** — wording, formatting; dropped, count mentioned

Unavailable path: if `CLAUDE_PLUGIN_ROOT` is unset or the companion
returns non-zero, log `Cross-model plan review skipped: <reason>` and
continue. Gate, not blocker.

## Flag precedence

User-supplied flags always override classifier-inferred overlays. When
`classify.md` picks `--architect=opus` but the user passed
`--architect=sonnet`, the user wins. Announcement in SKILL.md step 7
prints the final resolved config.
