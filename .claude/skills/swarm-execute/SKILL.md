---
name: swarm-execute
description: Use to implement a plan artifact from `/swarm-plan`, or a free-text implementation task with contract-first TDD + Review-Fix Loop. Tier (`low | auto | high | max`) scales builder model, loop rounds, review breadth, and Codex code-diff gate.
user-invocable: true
argument-hint: "[tier] <plan-artifact-or-task> [--flags]"
disable-model-invocation: true
triggers:
  - "execute this plan"
  - "execute the plan"
  - "implement this plan"
  - "implement the plan"
  - "run the plan"
---

# Execution Orchestrator — Tiered

Thin dispatch layer. Phase plans live in sibling tier files
(`tier-low.md`, `tier-high.md`, `tier-max.md`); this file parses
arguments, classifies the target (`classify.md`), resolves overlays
(`overlays.md`), optionally gates on a meta-plan approval, then hands
off to the matching tier file. Shared content (worker table, loop
design principles, constraints, handoff) stays here — phase-by-phase
execution lives in the tier files.

## Argument syntax

```
/swarm-execute [tier] <plan-artifact-or-task> [--flags]
```

- **tier** (optional): `low | auto | high | max`. Default `auto`.
- **target** (one of): plan artifact path (`.claude/state/plans/plan_*.md`);
  free-text task description.
- **flags** (OCX convention: flags before positional):
  - `--builder=sonnet|opus`
  - `--tester=sonnet|opus`
  - `--reviewer=haiku|sonnet|opus`
  - `--doc-reviewer=haiku|sonnet`
  - `--loop-rounds=1|2|3`
  - `--review=minimal|full|adversarial`
  - `--codex` / `--no-codex`
  - `--dry-run` / `--form` — meta-plan preview (`--form` uses `AskUserQuestion`; implies `--dry-run`)

## Workflow

### 1. Parse arguments and detect plan artifact

Detect target type:
1. Path ending `.md` (typically under `.claude/state/plans/`) → plan-artifact mode
2. Anything else → free-text mode

When a plan is present, read it and parse the handoff block for Tier,
Scope, Reversibility, Overlays; parse phase definitions; extract
"Subsystems Touched" from the plan body.

### 2. Classify (only when tier=`auto`)

Read `classify.md`. Apply plan-header signals first (primary); fall
back to free-text signals (pointer to `/swarm-plan`'s classify.md when
no plan artifact) only for axes the plan header doesn't cover. Produce
candidate tier + confidence flag + overlay set.

### 3. Resolve overlays

Final config = tier defaults (`overlays.md` per-tier table) +
classifier overlays + user flag overrides. User flags always win
(except tier=max's mandatory `--builder=opus`).

### 4. Meta-plan gate (single consolidated approval point)

Fire when ANY of: `--dry-run`, `--form`, tier resolved to `max`, or
classification marked low-confidence. This is the **only** user-prompt
point — no mid-flow `AskUserQuestion` during classification.

Write `.claude/state/plans/meta-plan_execute_[feature].md` with:
Classification (tier + rationale + plan-header source), Overlays
(+ rationale), Workers per phase, `loop-rounds` budget, Estimated cost,
Whether Codex fires, Not Doing (push, PR creation).

**Approval UI** (always a single interaction):
- Default: `EnterPlanMode` with the meta-plan path; resume on approve.
  *If skill resume after `ExitPlanMode` is unreliable in practice,
  fall back to `AskUserQuestion` with Approve / Edit / Cancel options.*
- `--form`: ONE `AskUserQuestion` call with ≤4 batched axis questions
  (Tier / Builder / Loop-rounds / Codex), first option "Recommended".

On reject: re-draft meta-plan with the rejection rationale and
re-present once.

### 5. Announce final config (always)

Print before loading the tier file:

```
Swarm execute
  Tier:        high                                (from plan header)
  Target:      .claude/state/plans/plan_foo.md
  Overlays:    builder=sonnet, loop-rounds=3       (tier default)
               codex=on                            (signal: plan Reversibility=One-Way Door Medium)
  Workers:     stub/impl sonnet, 1 arch reviewer,
               3 review rounds (full breadth)
  Codex diff review: on (after loop converges)
  Proceed? (Ctrl+C to abort; re-run with explicit tier to override)
```

### 6. Dispatch to tier file

`Read` the matching `tier-{low,high,max}.md` and execute its phase
plan. No phase content duplicated here.

## Review-Fix Loop

Protocol: see the canonical Review-Fix Loop in [`workflow-swarm.md`](../../rules/workflow-swarm.md#review-fix-loop). The protocol auto-loads for swarm-skill contexts via `workflow-swarm.md` path-scoping. Per-tier loop configuration (rounds, perspectives) is set in each tier file.

## Cross-Model Adversarial Pass — shared protocol

See `overlays.md` "codex axis" for when this fires per tier. Use the
`codex-adversary` skill with scope `code-diff` against the branch diff
after the Claude loop converges.

- **Preconditions**: loop exited, `task verify` green, working tree
  clean except intended diff.
- **Invocation**: delegate to `codex-adversary` with `--scope code-diff
  --base main`. Codex loads `AGENTS.md` automatically — do not inject
  project context.
- **Triage**: Actionable → one-shot `worker-builder` fix pass, gate
  `task verify`. Deferred → summary. Stated-convention / trivia →
  dropped with count.
- **One-shot only**: never re-enter the Review-Fix Loop — prevents
  two-family thrash. If the one-shot fix fails `task verify`, revert
  and promote findings to deferred.
- **Unavailable path**: `CLAUDE_PLUGIN_ROOT` unset / companion
  non-zero / empty output → log `Cross-model gate skipped: <reason>`
  and continue. Gate, not blocker (at tier=max, surface the skip
  prominently).

## Worker assignment (shared across tiers)

See `.claude/rules/workflow-swarm.md` for worker types, models, tools,
focus modes.

| Phase | Worker | Model |
|---|---|---|
| Stub | `worker-builder` (focus: `stubbing`) | sonnet / opus |
| Verify arch (high/max) | `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) | sonnet |
| Verify arch (max) | `worker-architect` | opus |
| Specify | `worker-tester` (focus: `specification`) | sonnet |
| Implement | `worker-builder` (focus: `implementation`) | sonnet / opus |
| Review Stage 1 | `worker-reviewer` (spec-compliance + test-coverage) | sonnet |
| Review Stage 2 | `worker-reviewer` / `worker-doc-reviewer` / `worker-architect` / `worker-researcher` | per role |
| Cross-model | `codex-adversary` (code-diff) | — |

Max concurrent workers: 8 (per `workflow-swarm.md`).

## Quality Gates & Git Protocol

- `task --list` to discover workflows; `task verify` is the final gate (subsystem verify runs during the review loop)
- Stage and commit with conventional commit message; never push
- Use `task checkpoint` for work-in-progress saves

## Living Design Records

Plan artifacts are living documents. When implementation reveals a
behavior or edge case not captured in the design record: update the
plan artifact first, write the corresponding test, then implement.

## Constraints

- NO completing tasks without passing quality gates
- NO leaving work uncommitted locally
- NO exceeding 8 parallel workers
- NO pushing to remote
- NO running stub and test phases concurrently (sequential only)
- NO mid-flow `AskUserQuestion` during classification — ambiguity
  resolves at the meta-plan gate
- ALWAYS report blockers immediately
- ALWAYS validate `git status` shows clean before commit
- ALWAYS update design record before adding tests for unspecified behaviors

## Handoff

- To `/swarm-review`: after implementation complete, for adversarial review
- To `/qa-engineer`: for acceptance testing

### Next Step — copy-paste to continue:

    /swarm-review

$ARGUMENTS
