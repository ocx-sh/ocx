---
name: swarm-review
description: Adversarial multi-perspective code reviewer. Tiered dispatch that scales from a single diff-scoped reviewer (low) to a full adversarial panel with architect, SOTA research, and Codex cross-model gate (max). Auto-classifies effort from diff metrics against a configurable baseline (default main). Use for branch/PR/diff review before landing on main.
user-invocable: true
argument-hint: "[tier] <branch-or-pr> [--base=<ref>] [--flags]"
---

# Adversarial Reviewer — Tiered

Thin dispatch layer. Phase plans live in sibling tier files
(`tier-low.md`, `tier-high.md`, `tier-max.md`); this file parses
arguments, resolves the baseline, classifies the diff (`classify.md`),
resolves overlays (`overlays.md`), optionally gates on a meta-plan
approval, then hands off to the matching tier file. Shared content
(worker table, adversarial protocol, output format, constraints) stays
here — perspective-by-perspective execution lives in tier files.

## Argument syntax

```
/swarm-review [tier] <target> [--flags]
```

- **tier** (optional): `low | auto | high | max`. Default `auto`.
- **target** (optional): branch name; PR reference (`<N>` / `#<N>` /
  `PR <N>` / `pull/<N>` / full GitHub PR URL); empty → current branch `HEAD`.
- **flags** (OCX convention: flags before positional):
  - `--base=<git-ref>` — diff baseline; default `main`. When target
    resolves to a PR, inferred from `gh pr view --json baseRefName`
    unless the user passes `--base` explicitly.
  - `--breadth=minimal|full|adversarial` — Stage 2 perspective breadth
  - `--reviewer=haiku|sonnet|opus` — model for `worker-reviewer`
  - `--doc-reviewer=haiku|sonnet` — model for `worker-doc-reviewer`
  - `--rca=on|off` — Five Whys systemic-fix analysis
  - `--codex` / `--no-codex` — cross-model adversarial pass (code-diff scope)
  - `--dry-run` — meta-plan preview via `EnterPlanMode`
  - `--form` — structured `AskUserQuestion` form; implies `--dry-run`

**`--base` is a pipeline input, not an axis.** It feeds the classifier
as the diff baseline for the whole run. Overlays (`breadth`, `rca`,
`codex`) are single-axis pipeline adjustments on top of the chosen tier.

## Workflow

### 1. Parse arguments and resolve target

Detect target (ordered, first match wins):
1. Full GitHub PR URL
2. `PR <N>` / `pull/<N>` → PR
3. `#<N>` or bare integer → probe PR via `mcp__github__pull_request_read`
4. Branch name — verified via `git rev-parse --verify <name>`
5. Empty → current branch `HEAD`

PR metadata (labels, base ref, title, body) feeds both the baseline
resolution and the classifier. On fetch failure, fall back to free-text
treatment (match `/swarm-plan` convention).

### 2. Resolve the baseline

- If user passed `--base=<ref>`, use it verbatim (user wins).
- Else if target is a PR, use `gh pr view <N> --json baseRefName -q .baseRefName`.
- Else default to `main`.

Fast-fail when `git rev-parse --verify <base>` fails — print remediation
(`did you mean origin/main?`) and stop. Empty diff → report "nothing to
review" and exit cleanly.

### 3. Classify (only when tier=`auto`)

Read `classify.md`. Compute diff metrics once (`git diff <base>...HEAD
--name-only` for files, `--shortstat` for lines). Apply tier signals
(size + subsystems + structural markers + PR labels). Produce candidate
tier + confidence flag + overlay set. When signals split across adjacent
tiers, mark **low-confidence** — the meta-plan gate in step 5 resolves
it. No mid-flow `AskUserQuestion`.

### 4. Resolve overlays

Final config = tier defaults (`overlays.md` per-tier table) +
classifier overlays + user flag overrides. User flags always win.

### 5. Meta-plan gate (single consolidated approval point)

Fire when ANY of: `--dry-run`, `--form`, tier resolved to `max`, or
low-confidence classification. Only user-prompt point.

Write `.claude/artifacts/meta-plan_review_[target].md` with:
Classification (tier + rationale + diff metrics snapshot), Baseline
(source: user-flag / PR-base / default), Overlays (with rationale),
Workers per perspective, Estimated cost, Not Doing (no auto-fixes, no
commits).

**Approval UI** (always a single interaction):
- Default: `EnterPlanMode` with the meta-plan path; resume on approve.
  *Fall back to `AskUserQuestion` Approve/Edit/Cancel if skill resume
  after `ExitPlanMode` is unreliable in practice.*
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

`Read` the matching `tier-{low,high,max}.md` and execute its phase plan.

## Worker assignment (shared across tiers)

See `.claude/rules/workflow-swarm.md` for worker types, models, tools,
focus modes. Tier files select a subset of these perspectives.

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
tier-gated via the `rca` axis (see `overlays.md`).

**Output skeleton** — tier files add or remove Stage 2 sections per the
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

- NO auto-fixing — review is read-only; actionable findings reported, not committed
- NO approving with unresolved Block-tier findings
- NO nitpicking style when using rustfmt
- NO mid-flow `AskUserQuestion` during classification
- NO exceeding 8 parallel workers
- ALWAYS reference specific files and lines
- ALWAYS suggest alternatives, not just problems
- ALWAYS classify every finding as actionable or deferred
- ALWAYS stay within the diff scope (`<base>...HEAD`)

## Handoff

- **To `/swarm-execute`**: when actionable findings exist and the user
  wants them fixed. `/swarm-execute` runs the Review-Fix Loop.
- **To `/builder`**: with a remediation task list for a specific finding.
- **To `/doc-writer`**: with the gap report from `worker-doc-reviewer`.
- **To `/architect`**: for architectural concerns requiring an ADR.

$ARGUMENTS
