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
| `sonnet` | `worker-builder` with model=sonnet for Stub + Implement. Default for low and high tiers. |
| `opus` | `worker-builder` with model=opus. Used for architecturally complex or cross-subsystem implementation. Mandatory at tier=max. |

Per-tier defaults:
- low → `sonnet`
- high → `sonnet` (opus via `--builder=opus` when classifier detects novel architecture or cross-subsystem change)
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

Evidence from `.claude/artifacts/research_model_capability_matrix.md`: Opus 4.7 leads Sonnet 4.6 by 8.0pp on SWE-bench Verified; gap largest on multi-step agentic chains (adversarial breadth profile at tier=max). Qodo 400-PR study puts Haiku 4.5 ahead of Sonnet 4.5 on single-pass code review for narrow-scope diffs.

| Value | Effect |
|---|---|
| `haiku` | `worker-reviewer` with model=haiku. Narrow-scope review at tier=low with no security/structural markers. |
| `sonnet` | `worker-reviewer` with model=sonnet. Default at tier=high and tier=max (non-adversarial). |
| `opus` | `worker-reviewer` with model=opus. Used at tier=max when `--breadth=adversarial` fires — CLI-UX, architecture-boundary, SOTA-gap perspectives benefit from deeper reasoning. |

Per-tier defaults:
- low → `haiku` (→ `sonnet` when structural markers from `swarm-review/classify.md:48-61` fire: `oci/**`, `package_manager/**`, auth/crypto/signing, new `crates/*/Cargo.toml`, `deny.toml`, `ocx_schema/**`, public API breakage)
- high → `sonnet`
- max → `sonnet` (→ `opus` when `--breadth=adversarial`)

**Security floor**: Haiku never runs reviewer on structural-marker paths. Floor = Sonnet minimum whenever any marker from `swarm-review/classify.md:48-61` present in diff.

### doc-reviewer axis

Controls model for `worker-doc-reviewer` when launches (Stage 2 at tier=high/max). Single-pass narrow-scope doc audit = workload where Haiku 4.5 beats Sonnet 4.5 per Qodo 400-PR study (see `.claude/artifacts/research_model_capability_matrix.md`). Fenced to narrow doc diffs to avoid Haiku 200k context cap binding on full user-guide audits.

| Value | Effect |
|---|---|
| `haiku` | `worker-doc-reviewer` with model=haiku. Triggered when the diff touches ≤2 doc files AND does not touch `website/src/docs/user-guide.md`. |
| `sonnet` | `worker-doc-reviewer` with model=sonnet. Default at all tiers; used whenever the haiku trigger does not fire. |

Per-tier defaults (all tiers share same default — axis only toggles when scope trigger fires):
- low → `sonnet` (moot: doc-reviewer does not launch at tier=low)
- high → `sonnet` (→ `haiku` when narrow-scope doc trigger fires)
- max → `sonnet` (→ `haiku` when narrow-scope doc trigger fires)

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