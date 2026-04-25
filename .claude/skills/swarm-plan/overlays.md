# Overlay Axis Definitions

Overlays = single-axis adjustments layered on chosen tier.
Let `auto` mode pick mixed configs (e.g., "high base with
opus architect for medium-scope feature with weighty
architecture") without compound-name tiers.

Classifier (`classify.md`) decide *when* apply overlay from
signals. This file define *what each axis mean* and how affect
pipeline.

## Axis grammar (flag values)

Match `SKILL.md` argument parser.

```
--architect=inline|sonnet|opus
--research=skip|1|3
--researcher=haiku|sonnet
--codex / --no-codex
```

## Axis definitions

### architect axis

Control Design phase.

| Value | Effect |
|---|---|
| `inline` | Design drafted inline in plan artifact by orchestrator. No worker launched. |
| `sonnet` | Launch `worker-architect` with model=sonnet. Produce ADR or system-design artifact. For Medium reversible decisions. |
| `opus` | Launch `worker-architect` with model=opus. Use when decision is one-way door with big trade-offs (new trait hierarchy, novel algorithm, cross-subsystem, protocol change). |

Per-tier defaults:
- low → `inline`
- high → `inline` for Two-Way Door scope, `sonnet` for One-Way Door Medium
- max → `opus` (mandatory, with ADR)

### research axis

Control Research phase worker count.

| Value | Effect |
|---|---|
| `skip` | No `worker-researcher` launched. Orchestrator may still do brief inline check against product-context.md. |
| `1` | One `worker-researcher`. Orchestrator pick single most relevant axis (tech OR patterns OR domain). |
| `3` | Three `worker-researcher` agents parallel, one per axis: tech / patterns / domain. |

Per-tier defaults:
- low → `skip`
- high → `1`
- max → `3` (mandatory)

### researcher axis

Control model for each `worker-researcher` launched during Research phase. Mirror Claude Code own pattern — Explore use Haiku for narrow codebase search; Plan use inherit for synthesis. At OCX, narrow single-axis factual lookups run on Haiku; multi-axis synthesis and any research touching web at scale stay on Sonnet.

| Value | Effect |
|---|---|
| `haiku` | `worker-researcher` with model=haiku. Trigger when `--research=1` AND research target is single narrow factual lookup (no cross-subsystem keywords, no multi-source synthesis signal). |
| `sonnet` | `worker-researcher` with model=sonnet. Default at all tiers where research runs; use whenever haiku trigger no fire. |

Per-tier defaults:
- low → `sonnet` (moot: research=skip at tier=low — no researcher launches)
- high → `sonnet` (→ `haiku` when `--research=1` AND narrow-scope trigger fires)
- max → `sonnet` (research=3 is synthesis — never haiku at max)

**Context-cap guard**: Haiku 4.5's 200k context window is hard constraint. If research prompt project to exceed 150k tokens (e.g., WebFetch on >5 sources, reading >5 large files), escalate to `sonnet` regardless of narrow-scope trigger. See `.claude/artifacts/research_model_capability_matrix.md` for context-cap rationale.

### codex axis (plan-artifact scope)

Control whether `codex-adversary` skill run against plan
artifact as final cross-model gate after Claude review panel
converge. Distinct from `/swarm-execute` Codex pass on
branch diff — same entry point (`codex-adversary`), different scope
target.

| Value | Effect |
|---|---|
| `off` | No Codex plan review. |
| `on` | After Claude panel converges in Phase 6, invoke `codex-adversary` with scope `plan-artifact` on plan file path. One-shot, no looping. Triage findings into actionable / deferred / stated-convention / trivia. Actionable findings re-validated by single `worker-reviewer` (spec-compliance) pass. |

Per-tier defaults:
- low → `off` (Two-Way Door — cost > value)
- high → `off` by default, auto-on when `classify.md` fires
  `--codex` overlay for One-Way Door signals; explicit via `--codex`
- max → `on` (mandatory, final gate before handoff)

Triage mirror existing cross-model pass triage in main
`codex-adversary` skill:

- **Actionable** — orchestrator edit plan artifact, re-run
  spec-compliance reviewer
- **Deferred** — add to Deferred Findings in handoff summary
- **Stated-convention** — critique load-bearing project convention;
  drop, count mentioned
- **Trivia** — wording, formatting; drop, count mentioned

Unavailable path: if `CLAUDE_PLUGIN_ROOT` unset or companion
return non-zero, log `Cross-model plan review skipped: <reason>` and
continue. Gate, not blocker.

## Flag precedence

User-supplied flags always override classifier-inferred overlays. When
`classify.md` pick `--architect=opus` but user pass
`--architect=sonnet`, user win. Announcement in SKILL.md step 7
print final resolved config.