# Plan: Tiered /swarm-review with Configurable Baseline

## Overview

**Status:** Draft
**Author:** swarm-plan (inline design)
**Date:** 2026-04-18
**Related:** `.claude/skills/swarm-plan/` and `.claude/skills/swarm-execute/` (completed tiered adoptions — the pattern we follow)

## Objective

Port `/swarm-review` from a single-body skill to the progressive-disclosure
tier-dispatch pattern already in use by `/swarm-plan` and `/swarm-execute`. Add
a configurable `--base=<ref>` flag (default `main`) that (a) sets the diff
baseline for the entire pipeline and (b) feeds auto-tier classification so
that a small branch-vs-tag diff lands on `low` while a wide
`main...feature` diff lands on `high` or `max`.

## Classification

- **Scope**: Medium
- **Reversibility**: One-Way Door Medium (changes argument surface of a
  user-invocable skill; downstream references in `workflow-swarm.md` and
  `workflow-feature.md` follow)
- **Tier**: high
- **Overlays**: architect=inline, research=skip, codex=off

## Scope

### In Scope

- Rewrite `.claude/skills/swarm-review/SKILL.md` as a thin dispatch (<200 lines)
- Add `classify.md`, `overlays.md`, `tier-low.md`, `tier-high.md`,
  `tier-max.md`
- Extend `.claude/tests/test_ai_config.py` to parametrize
  `swarm-review` into `_TIERED_SWARM_SKILLS`
- Update `.claude/rules/workflow-swarm.md` — add `/swarm-review` tier
  vocabulary and overlay table alongside the existing plan/execute entries

### Out of Scope

- Changes to reviewer worker agents (they stay as-is — reviewers already
  classify findings; the skill only scales perspective breadth and Codex use)
- Builder-side fixes (swarm-review is read-only; actionable findings are
  reported, not auto-fixed — fixes are the caller's choice via `/swarm-execute`)
- Changes to `/codex-adversary` (we invoke it the same way
  `/swarm-execute` does — scope `code-diff`)

## Technical Approach

### Dispatch architecture

Mirrors `swarm-plan` and `swarm-execute`:

```
/swarm-review [tier] <target> [--flags]
        │
        ▼
    SKILL.md (dispatch)
    ├─ Parse arguments (tier, target, --base, --breadth, --rca, --codex)
    ├─ Resolve baseline → compute diff
    ├─ classify.md (when tier=auto) — uses diff metrics + paths + PR labels
    ├─ overlays.md (axis grammar + per-tier defaults)
    ├─ Meta-plan gate (max / low-confidence / --dry-run / --form)
    ├─ Announce final config
    └─ Read tier-{low,high,max}.md and execute
```

### Key decisions

| Decision | Rationale |
|----------|-----------|
| `--base` is a pipeline-input flag, not an overlay axis | It sets diff scope for the whole run; classifier consumes it. Overlays are per-axis pipeline adjustments. Treating `--base` as an axis would be a category error. |
| Auto tier detection uses diff metrics first, free-text signals second | The diff is the authoritative signal for review effort; prompt text is secondary context. |
| Default baseline = `main` | Matches the existing `git diff main...HEAD` assumption in the current `SKILL.md` and the rest of the swarm pipeline. |
| PR targets infer baseline from `gh pr view --json baseRefName` | PRs may merge to release branches, not `main`. Using the PR's actual base is correct and matches GitHub's mental model. Users can still override with `--base`. |
| Three-dot diff (`<base>...HEAD`) | Compares against the common ancestor — ignores new commits on the base branch. Matches `git log` / `gh pr diff` semantics. |
| swarm-review has no Review-Fix Loop | It's read-only. The "loop" analog is perspective breadth + optional Codex pass. No rounds axis. |
| TDD skeleton anchors satisfied naturally | Spec-compliance reviewer phases are `post-stub`, `post-specification`, `post-implementation`. "Stub" / "Specify" / "Implement" / "Review" appear naturally in tier files. |

### Argument grammar

```
/swarm-review [tier] <target> [--flags]
```

- `tier` (optional): `low | auto | high | max`. Default `auto`.
- `target` (optional): branch name; `<N>` / `#<N>` / PR URL; issue refs
  resolve to their linked PR where possible; empty defaults to `HEAD` on
  the current branch.
- Flags (before positional, per OCX convention):
  - `--base=<git-ref>` — diff baseline (default `main`; PRs override to
    their base ref unless the user passes `--base` explicitly)
  - `--breadth=minimal|full|adversarial` — Stage 2 perspective set
  - `--rca=on|off` — Five Whys systemic-fix analysis
  - `--codex` / `--no-codex` — cross-model adversarial pass
  - `--dry-run` — meta-plan preview via `EnterPlanMode`
  - `--form` — `AskUserQuestion` form; implies `--dry-run`

### Classifier inputs

1. `git diff <base>...HEAD --name-only` → changed file count
2. `git diff <base>...HEAD --shortstat` → lines added/removed
3. Changed paths mapped to subsystems via the subsystem path-scope table
   in `.claude/rules.md`
4. Structural signals in the diff:
   - New `crates/*/Cargo.toml` → new crate (max signal)
   - `.github/workflows/**` changes (security review required)
   - `crates/ocx_lib/src/oci/**`, auth, crypto paths (security-sensitive)
   - `package_manager/` changes (adversarial breadth)
5. PR labels when target resolves to a PR: `breaking-change` → max,
   `security` → adversarial breadth, `small` / `docs` → low hint

### Tier metric table

| Tier | Files | Lines | Subsystems | Signals |
|------|-------|-------|-----------|---------|
| low  | ≤3    | ≤100  | 1         | None from the adversarial list |
| high | ≤15   | ≤500  | 1-2       | No One-Way Door High signals |
| max  | >15 or any breaking/protocol/new-crate/security signal | any | ≥2 or cross-subsystem | Any One-Way Door High signal |

A PR label or prompt keyword can escalate tier even when metrics suggest
lower; the classifier picks the highest tier with at least one firing
signal, same rule as `/swarm-plan`.

### Per-tier axis defaults

| Axis | low | high | max |
|------|-----|------|-----|
| breadth | minimal | full | adversarial |
| rca | off | on (for Block/High findings) | on (for all findings above Suggest) |
| codex | off | off (auto-on for One-Way Door signals) | on (mandatory) |

### Review perspectives per tier

**low** — Stage 1 only (spec-compliance + quality on changed files).

**high** — Stage 1 (spec-compliance + test-coverage, parallel). Stage 2
full: quality / security / performance / docs / CLI UX when applicable.

**max** — Stage 1 (spec-compliance + test-coverage). Stage 2 adversarial:
full Stage 2 + architect (boundary/SOLID) + researcher (SOTA) + CLI UX +
mandatory Codex cross-model pass.

## User Experience Scenarios

| Command | Target | Base | Expected auto tier |
|---------|--------|------|--------------------|
| `/swarm-review` | current branch HEAD | `main` | depends on diff size |
| `/swarm-review 143` | PR #143 branch | PR base (auto) | depends on diff |
| `/swarm-review --base=v0.5.0` | HEAD | tag `v0.5.0` | likely max (large diff) |
| `/swarm-review high feature-x` | branch `feature-x` | `main` | high (user override) |
| `/swarm-review low --base=goat` | HEAD | `goat` worktree branch | low (small sibling diff) |
| `/swarm-review --form` | current branch | `main` | user-picked via form |

## Error Taxonomy

| Failure | Behavior |
|---------|----------|
| `<base>` ref missing | `git rev-parse` fails → fast-fail with remediation (`did you mean origin/main?`) |
| Empty diff | Report "nothing to review" and exit cleanly |
| Target branch missing | Fast-fail with the list of available branches nearby |
| PR fetch fails | Fall back to free-text handling (match `/swarm-plan` convention); don't hard-fail |
| Codex unavailable | Log `Cross-model gate skipped: <reason>` and continue (gate, not blocker — except at max where the skip is surfaced prominently) |

## Edge Cases

- Merge commits in the diff → three-dot range ignores them via common ancestor
- Binary files → counted for tier metrics, skipped during reviewer analysis
- Renamed files → treated as one changed file for metrics
- Diff >1000 files → auto tier = max; announce scope warning so the user
  can re-run with a tighter `--base`
- Reviewer disagreement at max tier (architect vs. quality reviewer) →
  no auto-reconciliation; both perspectives surface in the report

## Files to Modify

| File | Action | Purpose |
|------|--------|---------|
| `.claude/skills/swarm-review/SKILL.md` | Rewrite | Thin dispatch layer |
| `.claude/skills/swarm-review/classify.md` | Create | Diff-metric + signal classifier |
| `.claude/skills/swarm-review/overlays.md` | Create | Axis grammar + per-tier defaults |
| `.claude/skills/swarm-review/tier-low.md` | Create | Minimal single-reviewer pass |
| `.claude/skills/swarm-review/tier-high.md` | Create | Default — Stage 1 + full Stage 2 |
| `.claude/skills/swarm-review/tier-max.md` | Create | Adversarial + architect + SOTA + Codex |
| `.claude/tests/test_ai_config.py` | Modify | Extend `_TIERED_SWARM_SKILLS` |
| `.claude/rules/workflow-swarm.md` | Modify | Add `/swarm-review` tier & overlay vocabulary |

## Testing Strategy

### Structural tests (automated — `.claude/tests/test_ai_config.py`)

Existing parametrized tests run against every entry in
`_TIERED_SWARM_SKILLS`. Adding swarm-review produces these coverage lines
for free:

- `test_skill_md_under_200_lines` — SKILL.md dispatch ceiling
- `test_tier_files_exist` — tier-{low,high,max}.md present & referenced
- `test_support_files_exist` — classify.md, overlays.md present & referenced
- `test_classify_has_example_per_tier` — anchors `**low**` / `**high**` / `**max**`
- `test_overlays_match_skill_flag_grammar` — flag grammar lockstep between SKILL.md and overlays.md
- `test_tier_files_preserve_contract_first_tdd` — TDD anchors present
  (naturally satisfied via spec-compliance reviewer phase terminology:
  `post-stub`, `post-specification`, `post-implementation`, `Review`)

### Manual verification

- [ ] `task claude:tests` passes
- [ ] `task claude:verify` passes (lint + tests)
- [ ] `/swarm-review` (no args) produces the announce block with `base=main`
- [ ] `/swarm-review low --base=HEAD~1` picks tier=low and reads the
  single-prev-commit diff
- [ ] `/swarm-review max` with a cross-crate diff triggers the meta-plan gate

## Rollback Plan

1. `git checkout .claude/skills/swarm-review/SKILL.md` — restore the old single-file skill
2. `rm .claude/skills/swarm-review/{classify,overlays,tier-low,tier-high,tier-max}.md`
3. Revert `.claude/tests/test_ai_config.py` and `.claude/rules/workflow-swarm.md`

## Risks

| Risk | Mitigation |
|------|------------|
| TDD-anchor test fails because review tier files don't naturally contain the anchors | Use reviewer phase names (`post-stub` / `post-specification` / `post-implementation`) — all four anchors appear as strings regardless of usage |
| Users expect `--base` to be an overlay axis | Document clearly in `SKILL.md` and `overlays.md` that `--base` is a pipeline input, not an axis. Examples make the distinction obvious. |
| Baseline autodetection for PRs calls `gh` in the hot path | Best-effort: if `gh pr view --json baseRefName` fails, fall back to the `--base` flag value (default `main`). |
| Very large diffs overwhelm reviewers | Classifier promotes to max + announces scope warning; user can re-run with tighter `--base` |

## Implementation Steps

> **Contract-First TDD**: classifier test anchors are the executable
> contract — tier-file shape + flag grammar + support-file presence are
> all enforced by `test_ai_config.py`. Writing the SKILL.md first then
> running the parametrized test suite IS the TDD cycle for this change.

### Phase 1 — Stubs (scaffolding)

- [ ] **1.1** Create `classify.md`, `overlays.md`, `tier-{low,high,max}.md`
      with the file-name anchors and example signals the tests require
      (empty bodies OK at stub stage)
- [ ] **1.2** Rewrite `SKILL.md` with the dispatch structure, flag
      grammar in the frontmatter / body, and `Read tier-*.md` references

### Phase 2 — Architecture Review (inline)

- Verify the SKILL.md flag grammar matches `overlays.md` exactly
- Verify `classify.md` references all three tiers with concrete examples
- Verify tier files contain the TDD anchors

### Phase 3 — Specification Tests

- [ ] **3.1** Add `swarm-review` entry to `_TIERED_SWARM_SKILLS` in
      `test_ai_config.py` with the full flag grammar
- [ ] **3.2** Run `task claude:tests` — expect failures flagged at
      stubs (empty classify/overlays/tier bodies)

### Phase 4 — Implementation (body content)

- [ ] **4.1** Flesh out `classify.md` with full signal table + overlay
      triggers + examples per tier
- [ ] **4.2** Flesh out `overlays.md` with axis definitions + per-tier
      defaults + flag precedence
- [ ] **4.3** Flesh out tier-low / tier-high / tier-max with full phase
      plans, gates, artifacts, handoff

### Phase 5 — Review & Wiring

- [ ] **5.1** Update `workflow-swarm.md` Tier & Overlay Vocabulary section
- [ ] **5.2** Run `task claude:tests` (must be green)
- [ ] **5.3** Run `task claude:verify`

## Handoff

### Next Step

    /swarm-execute .claude/artifacts/plan_swarm_review_tiered.md

Or, since this is pure AI-config work following an established pattern,
direct inline implementation is appropriate at tier=high — which is what
this session does.
