---
name: swarm-review
description: Use for branch, PR, or diff review before landing on main. Tier (`low | auto | high | max`) scales breadth from single reviewer to adversarial panel with architect + SOTA-gap + Codex cross-model gate. Triggers: "review", "/swarm-review", pre-merge checks.
user-invocable: true
argument-hint: "[tier] <branch-or-pr> [--base=<ref>] [--flags]"
triggers:
  - "review this branch"
  - "review this pr"
  - "review this diff"
  - "pre-merge check"
  - "before i merge"
---

# Adversarial Reviewer — Tiered

Thin dispatch layer. Phase plans live in sibling tier files
(`tier-low.md`, `tier-high.md`, `tier-max.md`). This file parse args,
resolve baseline, classify diff (`classify.md`), resolve overlays
(`overlays.md`), optionally gate on meta-plan approval, then hand off
to matching tier file. Shared content (worker table, adversarial
protocol, output format, constraints) stay here — perspective-by-
perspective execution lives in tier files.

## Argument syntax

```
/swarm-review [tier] <target> [--flags]
```

- **tier** (optional): `low | auto | high | max`. Default `auto`.
- **target** (optional): branch name; PR ref (`<N>` / `#<N>` /
  `PR <N>` / `pull/<N>` / full GitHub PR URL); empty → current branch `HEAD`.
- **flags** (OCX convention: flags before positional):
  - `--base=<git-ref>` — diff baseline; default `main`. When target
    resolves to PR, inferred from `gh pr view --json baseRefName`
    unless user pass `--base` explicit.
  - `--breadth=minimal|full|adversarial` — Stage 2 perspective breadth
  - `--reviewer=haiku|sonnet|opus` — model for `worker-reviewer`
  - `--doc-reviewer=haiku|sonnet` — model for `worker-doc-reviewer`
  - `--rca=on|off` — Five Whys systemic-fix analysis
  - `--codex` / `--no-codex` — cross-model adversarial pass (code-diff scope)
  - `--dry-run` — meta-plan preview via `EnterPlanMode`
  - `--form` — structured `AskUserQuestion` form; implies `--dry-run`

**`--base` is pipeline input, not axis.** Feeds classifier as diff
baseline for whole run. Overlays (`breadth`, `rca`, `codex`) are
single-axis pipeline adjustments on top of chosen tier.

## Workflow

### 1. Parse arguments and resolve target

Detect target (ordered, first match wins):
1. Full GitHub PR URL
2. `PR <N>` / `pull/<N>` → PR
3. `#<N>` or bare integer → probe PR via `mcp__github__pull_request_read`
4. Branch name — verify via `git rev-parse --verify <name>`
5. Empty → current branch `HEAD`

PR metadata (labels, base ref, title, body) feeds both baseline
resolution and classifier. On fetch fail, fall back to free-text
treatment (match `/swarm-plan` convention).

### 2. Resolve the baseline

- User pass `--base=<ref>` → use verbatim (user wins).
- Else target is PR → use `gh pr view <N> --json baseRefName -q .baseRefName`.
- Else default `main`.

Fast-fail when `git rev-parse --verify <base>` fails — print remediation
(`did you mean origin/main?`) and stop. Empty diff → report "nothing to
review" and exit clean.

### 3. Classify (only when tier=`auto`)

Read `classify.md`. Compute diff metrics once (`git diff <base>...HEAD
--name-only` for files, `--shortstat` for lines). Apply tier signals
(size + subsystems + structural markers + PR labels). Produce candidate
tier + confidence flag + overlay set. When signals split across adjacent
tiers, mark **low-confidence** — meta-plan gate in step 5 resolve
it. No mid-flow `AskUserQuestion`.

### 4. Resolve overlays

Final config = tier defaults (`overlays.md` per-tier table) +
classifier overlays + user flag overrides. User flags always win.

### 5. Meta-plan gate (single consolidated approval point)

Fire when ANY of: `--dry-run`, `--form`, tier resolved to `max`, or
low-confidence classification. Only user-prompt point.

Write `.claude/state/plans/meta-plan_review_[target].md` with:
Classification (tier + rationale + diff metrics snapshot), Baseline
(source: user-flag / PR-base / default), Overlays (with rationale),
Workers per perspective, Estimated cost, Not Doing (no auto-fixes, no
commits).

**Approval UI** (always single interaction):
- Default: `EnterPlanMode` with meta-plan path; resume on approve.
  *Fall back to `AskUserQuestion` Approve/Edit/Cancel if skill resume
  after `ExitPlanMode` unreliable in practice.*
- `--form`: ONE `AskUserQuestion` call with ≤4 batched axis questions
  (Tier / Breadth / RCA / Codex), first option "Recommended".

On reject: re-draft with rejection rationale and re-present once.

### 6. Announce final config (always)

```
Swarm review
  Tier:       high                             (auto — 8 files, 240 lines, 1 subsystem)
  Baseline:   main                             (default)
  Target:     HEAD (branch: soraka)
  Overlays:   breadth=full, rca=on, codex=off  (tier default)
  Workers:    Stage 1 (2 parallel), Stage 2 (4 parallel)
  Codex diff review: off
  Proceed? (Ctrl+C to abort; re-run with explicit tier to override)
```

### 7. Dispatch to tier file

`Read` matching `tier-{low,high,max}.md` and execute its phase plan.

## Worker assignment (shared across tiers)

See `.claude/rules/workflow-swarm.md` for worker types, models, tools,
focus modes. Tier files select subset of these perspectives.

| Perspective | Worker / focus | Tier use |
|---|---|---|
| Spec-compliance | `worker-reviewer` (spec-compliance, phase: `post-implementation`) | all |
| Test coverage | `worker-reviewer` (quality, lens: test-coverage) | high, max |
| Quality | `worker-reviewer` (quality) | all |
| Security | `worker-reviewer` (security) | high (security paths), max |
| Performance | `worker-reviewer` (performance) | high (hot paths), max |
| Architecture | `worker-architect` | max (+ `adversarial`) |
| Documentation | `worker-doc-reviewer` | high, max |
| CLI UX | `worker-reviewer` (quality, lens: cli-ux) | max (+ `adversarial`) |
| SOTA | `worker-researcher` | max (+ `adversarial`) |
| Cross-model | `codex-adversary` (code-diff) | high (when `--codex` fires), max (mandatory) |

Max concurrent workers: 8 (per `workflow-swarm.md`).

## Adversarial protocol & output format

Adversarial questioning anchors ("What if this assumption is wrong?",
"Under what conditions would this fail?", "What edge cases weren't
considered?") apply at every tier. Root-cause Five-Whys analysis is
tier-gated via `rca` axis (see `overlays.md`).

**Output skeleton** — tier files add or remove Stage 2 sections per
breadth they run. Every tier produces:

```markdown
## Code Review: [target]
### Summary
- Verdict: Approved | Needs Work | Request Changes
- Tier / Baseline / Diff: N files, +L / -L lines, S subsystems
### Stage 1 — Correctness (spec-compliance, test-coverage)
### Stage 2 — [perspectives run at this tier]
### Cross-Model Adversarial (Codex)  # if --codex fired
### Root-Cause Analysis               # if rca=on
### Deferred Findings (human judgment required)
```

**Verdict**: Approve (Block/High resolved or deferred with reasoning),
Needs Work (Warn-tier present), Request Changes (unresolved Block-tier /
security / breaking changes / missing tests / arch violations).

## Constraints

- NO auto-fixing — review read-only; actionable findings reported, not committed
- NO approving with unresolved Block-tier findings
- NO nitpicking style when using rustfmt
- NO mid-flow `AskUserQuestion` during classification
- NO exceeding 8 parallel workers
- ALWAYS reference specific files and lines
- ALWAYS suggest alternatives, not just problems
- ALWAYS classify every finding as actionable or deferred
- ALWAYS stay within diff scope (`<base>...HEAD`)

## Plan Status block (mutate on round entry + verdict)

Mutate the plan's `## Status` block: flip `Step` to `/swarm-review → round N` on each round entry; on verdict, set `awaiting /finalize` (Approve) or `awaiting /swarm-execute (review-fix loop)` (Needs Work / Request Changes); bump `Last update`. Skip silently when no `current_plan.md` / no Status block. Full mutation table → [`meta-ai-config.md`](../../rules/meta-ai-config.md) "Plan Status Protocol".

## Handoff

- **To `/swarm-execute`**: when actionable findings exist and user
  want them fixed. `/swarm-execute` runs Review-Fix Loop.
- **To `/builder`**: with remediation task list for specific finding.
- **To `/doc-writer`**: with gap report from `worker-doc-reviewer`.
- **To `/architect`**: for architectural concerns requiring ADR.

$ARGUMENTS