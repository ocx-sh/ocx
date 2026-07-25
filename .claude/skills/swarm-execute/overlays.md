# Overlay Axis Definitions — /swarm-execute

Overlays = single-axis adjustments layered on chosen tier.
Let `auto` mode pick mixed configs (e.g., "high base + opus
builder for medium-scope feature with weighty implementation")
without compound-name tiers.

Classifier (`classify.md`) decides *when* to apply overlay from
plan-header signals + free-text cues. This file defines *what each axis
means* and pipeline effect.

## Axis grammar (flag values)

Matches `SKILL.md` argument parser.

```
--builder=sonnet|opus
--tester=sonnet|opus
--reviewer=haiku|sonnet|opus
--doc-reviewer=haiku|sonnet
--loop-rounds=1|2|3
--review=minimal|full|adversarial
--codex / --no-codex
```

## Axis definitions

### builder axis

Controls model for Stub + Implement phases. Review-Fix Loop builders
(one-shot fix passes) inherit unless tier overrides.

| Value | Effect |
|---|---|
| `sonnet` | `worker-builder` with model=sonnet for Stub + Implement. Default at tier=low, and for mechanical work at any tier (rename, doc fix, fixture, single-file edit against an existing pattern). |
| `opus` | `worker-builder` with model=opus. Default at high and max: non-mechanical implementation — multi-file, async/concurrency, error and exit-code semantics, wire-format/serializer, auth/SSRF/credential paths. |

Per-tier defaults:
- low → `sonnet`
- high → `opus` (`--builder=sonnet` to downgrade when the work is genuinely mechanical)
- max → `opus` (mandatory — overrides any explicit `--builder=sonnet`)

### tester axis

Controls model for `worker-tester` spec phase. Test authoring at tier=max covers protocol-level corners + cross-subsystem interactions — novel-reasoning work where Opus-over-Sonnet gap shows (ARC-AGI-2: +10.5pp). At tier=low/high, test scope narrower, Sonnet enough.

| Value | Effect |
|---|---|
| `sonnet` | `worker-tester` with model=sonnet. Default for low and high tiers. |
| `opus` | `worker-tester` with model=opus. Mandatory at tier=max for exhaustive edge-case coverage. |

Per-tier defaults:
- low → `sonnet`
- high → `sonnet`
- max → `opus` (mandatory — overrides any explicit `--tester=sonnet`)

See `.claude/artifacts/adr_tier_model_correlation.md` for rationale.

### reviewer axis

Controls model for every `worker-reviewer` launch across Verify-Arch (post-stub), Review-Fix Loop Stage 1 (spec-compliance + test-coverage), Stage 2 (quality / security / performance). All reviewer invocations in single `/swarm-execute` run share resolved value.

Evidence from `.claude/artifacts/research_model_capability_matrix.md`: Opus 4.7 leads Sonnet 4.6 by 8.0pp on SWE-bench Verified; gap largest on multi-step agentic chains (adversarial breadth profile at tier=max).

| Value | Effect |
|---|---|
| `haiku` | `worker-reviewer` with model=haiku. Explicit user override only — never an automatic default, and never on security/structural-marker paths. |
| `sonnet` | `worker-reviewer` with model=sonnet. Explicit downgrade for trivial diffs (docs, fixtures, single-file mechanical change). |
| `opus` | `worker-reviewer` with model=opus. **Default at high and max**, and mandatory for any diff touching security, auth/credentials, SSRF/network policy, error/exit-code semantics, or wire formats. |

Per-tier defaults:
- low → `sonnet`
- high → `opus`
- max → `opus`

Opus is the floor for security and correctness review — a cheap review of a
security diff is a false economy. Downgrade to sonnet only for trivial diffs;
haiku only via explicit user override and never on security-relevant paths.

### doc-reviewer axis

Controls model for `worker-doc-reviewer` when launches (Stage 2 at tier=high/max). Single-pass narrow-scope doc audit — see `.claude/artifacts/research_model_capability_matrix.md`.

| Value | Effect |
|---|---|
| `haiku` | `worker-doc-reviewer` with model=haiku. Explicit user override only — never an automatic default. |
| `sonnet` | `worker-doc-reviewer` with model=sonnet. Default at every tier. |

Per-tier defaults:
- low → `sonnet` (moot: doc-reviewer does not launch at tier=low)
- high → `sonnet`
- max → `sonnet`

Sonnet is the floor; haiku only via explicit user override.

### loop-rounds axis

Controls max Review-Fix Loop iterations.

| Value | Effect |
|---|---|
| `1` | Single pass: one review round, one builder fix pass, one verify. No iterative loop. Used for Two-Way Door features where churn cost > value. |
| `2` | Up to two review-fix rounds. Used when classifier wants some iteration but scope is medium. |
| `3` | Up to three review-fix rounds (today's default for tier=high and tier=max). Loop exits early on convergence or oscillation. |

Per-tier defaults:
- low → `1`
- high → `3`
- max → `3`

### review axis

Controls Stage 2 perspective breadth in Review-Fix Loop.

| Value | Effect |
|---|---|
| `minimal` | Stage 2 launches **only** `worker-reviewer` (focus: `quality`). Stage 1 still runs spec-compliance. Used at tier=low. |
| `full` | Stage 2 launches `worker-reviewer` (quality / security / performance) + `worker-doc-reviewer` when doc triggers match. Today's default for tier=high. |
| `adversarial` | Stage 2 adds `worker-architect` (architecture), `worker-researcher` (SOTA gap), and `worker-reviewer` (focus: `quality`) with CLI-UX lens to the `full` set. Default for tier=max. |

Per-tier defaults:
- low → `minimal`
- high → `full`
- max → `adversarial`

### codex axis (code-diff scope)

Controls whether `codex-adversary` runs as cross-model gate against
branch diff after Claude Review-Fix Loop converges. Same entry point
as `/swarm-plan` Codex overlay, different scope (`code-diff`, not
`plan-artifact`).

| Value | Effect |
|---|---|
| `off` | No Codex diff review. |
| `on` | After Review-Fix Loop converges, invoke `codex-adversary` with scope `code-diff` on the branch diff. One-shot, no looping. Triage findings into actionable / deferred / stated-convention / trivia; actionable fold into one final builder pass. |

Per-tier defaults:
- low → `off` (Two-Way Door — cost > value)
- high → `off` by default; auto-on when `classify.md` fires the `--codex` overlay for One-Way Door signals from the plan header
- max → `on` (mandatory, final gate before commit)

**Codex model per tier** (passed to `codex-adversary` as `--model`): high → `terra` (`gpt-5.6-terra`), max → `sol` (`gpt-5.6-sol`); `luna` (`gpt-5.6-luna`) if `--codex` forced at low. Override with `--codex-model=luna|terra|sol`. Policy: `workflow-swarm.md` "Cross-model model tiers".

Triage mirrors existing cross-model pass triage in `codex-adversary`:

- **Actionable** — one-shot `worker-builder` (focus: `implementation`) fix pass; gate: `task verify` passes
- **Deferred** — added to Deferred Findings in the commit summary
- **Stated-convention** — critiques a load-bearing project convention; dropped, count mentioned
- **Trivia** — wording, formatting; dropped, count mentioned

Unavailable path: if `CLAUDE_PLUGIN_ROOT` unset or companion
returns non-zero, log `Cross-model gate skipped: <reason>` and continue.
Gate, not blocker.

## Flag precedence

User-supplied flags always override classifier-inferred overlays. When
`classify.md` picks `--builder=opus` but user passed
`--builder=sonnet`, user wins. Exceptions = tier=max mandatory
`--builder=opus` and `--tester=opus` — max-tier enforces Opus
for these axes because complexity triggering max-tier selection
demands it. Announcement in SKILL.md prints final resolved config.