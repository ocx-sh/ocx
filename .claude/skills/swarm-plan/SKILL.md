---
name: swarm-plan
description: Use for feature planning, task decomposition, multi-perspective research, or ADR scaffolding. Tier (`low | auto | high | max`) scales research depth, architect model, and review breadth. Canonical research primitive for AI config / ADR work.
user-invocable: true
disable-model-invocation: true
argument-hint: "[tier] <target> [--flags]"
triggers:
  - "plan the"
  - "plan a new"
  - "research and decide"
  - "how should we approach"
  - "decompose the feature"
---

# Planning Orchestrator — Tiered

Thin dispatch layer. Phase plans live in sibling tier files
(`tier-low.md`, `tier-high.md`, `tier-max.md`). This file parse args,
classify target (`classify.md`), resolve overlays (`overlays.md`),
optional gate on meta-plan approval, then hand off to matching tier
file. Shared content (worker assignment, subsystem context,
constraints, handoff format, research-primitive contract) stay here —
never duplicated across tier files.

## Argument syntax

```
/swarm-plan [tier] <target> [--flags]
```

- **tier** (optional): `low | auto | high | max`. Default `auto`.
- **target** (one of): free-text prompt; `<N>` or `#<N>` (auto-probe
  PR then issue); `PR <N>` / `pull/<N>` (explicit PR); `issue <N>` /
  `issues/<N>` (explicit issue); full GitHub URL
  `https://github.com/<owner>/<repo>/(pull|issues)/<N>`.
- **flags** (OCX convention: flags before positional):
  - `--architect=inline|sonnet|opus`
  - `--research=skip|1|3`
  - `--researcher=haiku|sonnet`
  - `--codex` / `--no-codex` — force plan-artifact Codex pass on/off
  - `--dry-run` / `--form` — meta-plan preview (`--form` use `AskUserQuestion`; imply `--dry-run`)

## Workflow

### 1. Parse arguments and detect GitHub target

Detect GitHub refs (ordered, first match win):
1. Full `https://github.com/<owner>/<repo>/(pull|issues)/<N>` URL
2. `PR <N>` / `pull/<N>` / `pulls/<N>` → PR
3. `issue <N>` / `issues/<N>` → issue
4. `#<N>` or bare integer `<N>` → probe PR first, fall back to issue

Fetch via `mcp__github__pull_request_read` /
`mcp__github__issue_read` (preferred); `gh pr view` / `gh issue view`
with `--json title,body,comments,labels,files` as fallback. PRs and
issues equal-class targets — probe order implementation detail.
On fetch fail, treat input as free text (ask via `AskUserQuestion`
only if disambiguation needed).

### 2. Classify (only when tier=`auto`)

Read `classify.md`. Apply tier signals + overlay triggers to prompt
plus any fetched GitHub body/labels. Produce candidate tier +
confidence flag + overlay set. Labels map direct (e.g.,
`breaking-change` → `--codex`). PR file list feed Discover scope (not
classification).

### 3. Resolve overlays

Final config = tier defaults (`overlays.md` per-tier table) +
classifier overlays + user flag overrides. User flags always win.

### 4. Meta-plan gate (single consolidated approval point)

Fire when ANY of: `--dry-run`, `--form`, tier resolve to `max`, or
classification marked low-confidence. **Only** user-prompt point — no
mid-flow `AskUserQuestion` during classification.

Write `.claude/state/plans/meta-plan_[feature].md` with:
Classification (tier + rationale + overlays), GitHub context, Workers
I Would Launch (per phase), Artifacts I Would Produce, Estimated Cost
(parallel worker count, heaviest call, Codex presence), Not Doing
(implementation, PR creation).

**Approval UI** (always single interaction):
- Default: `EnterPlanMode` with meta-plan path; resume on approve.
  *If skill resume after `ExitPlanMode` unreliable in practice,
  fall back to `AskUserQuestion` with Approve / Edit / Cancel options.*
- `--form`: ONE `AskUserQuestion` call with ≤4 batched axis questions
  (Tier / Architect / Research / Codex), first option "Recommended".
  Never sequential prompts. Form IS preview — do not also
  fire markdown gate.

On reject: re-draft meta-plan with rejection rationale (free-text
or explicit axis answers), re-present once.

### 5. Announce final config (always)

Print before loading tier file:

```
Swarm plan
  Tier:     high                                   (auto)
  Overlays: architect=opus                         (signal: new trait hierarchy)
  Workers:  3 explorers, 1 researcher, 1 architect (opus), 1 reviewer
  Artifacts: plan_[feature].md, research_[topic].md, adr_[decision].md
  Codex plan review: off
  Proceed? (Ctrl+C to abort; re-run with explicit tier to override)
```

### 6. Dispatch to tier file

`Read` matching `tier-{low,high,max}.md`, execute its phase plan. No
phase content duplicated here.

## Worker assignment (shared across tiers)

See `workflow-swarm.md` for worker types, models, tools, focus modes.

| Phase | Worker | Count | Role |
|---|---|---|---|
| Discover | `worker-architecture-explorer` | 0–1 | Current-state mapping |
| Discover | `worker-explorer` | 1–4 | Subsystem deep-dive |
| Research | `worker-researcher` | 0–3 | Technology landscape |
| Design (complex) | `worker-architect` | 0–1 | ADR / system design |
| Review | `worker-reviewer` (spec-compliance) | 1 | Plan consistency |
| Review (One-Way Door) | `worker-architect` | 0–1 | Trade-off honesty |
| Review | `worker-researcher` | 0–1 | SOTA gap check |
| Cross-model | `codex-adversary` (plan-artifact) | 0–1 | Cross-family review |

Max concurrent workers: 8 (per `workflow-swarm.md`).

## Subsystem context rules (shared)

Identify involved subsystems, read matching
`.claude/rules/subsystem-*.md` context rules — full subsystem → rule
table live in `CLAUDE.md` "Subsystem context".

## Research as a Reusable Primitive

Discover + Research phases = canonical multi-agent research pattern
for project. Reused by `/architect`, `/meta-maintain-config`
(`create`/`research` modes), `/swarm-plan`. Consumers SHOULD: launch
workers in parallel; split researchers by axis (tech / patterns /
domain) when research non-trivial; persist substantial findings as
`research_[topic].md`; pair at least one explorer with researchers to
ground external findings in local code. `meta-ai-config.md`
"Research Protocol" references this contract.

## Constraints

- NO tasks without testable acceptance criteria; NO vague behaviors
- NO assuming context — Discover run every tier
- NO skipping Review; NO >8 parallel workers
- NO mid-flow `AskUserQuestion` during classification — ambiguity
  always resolve at meta-plan gate
- ALWAYS store artifacts in `.claude/artifacts/`; ALWAYS persist
  substantial research as `research_[topic].md`
- ALWAYS include component contracts (with expected behavior and edge
  cases) and user experience scenarios (with error cases)
- ALWAYS announce final config, even post-approval, hand off to
  `/swarm-execute` with explicit next-step

## Handoff format

```markdown
## Plan Complete: [Feature or "Resolves #N"]

### Classification
- **Scope**: Small | Medium | Large
- **Reversibility**: Two-Way | One-Way Medium | One-Way High
- **Tier**: low | high | max
- **Overlays**: architect=X, research=Y, codex=Z

### Artifacts
- `.claude/state/plans/plan_[feature].md`
- `.claude/artifacts/research_[topic].md`
- `.claude/artifacts/adr_[decision].md` (One-Way Door High)

### Executable Phases (for /swarm-execute)
- **Stub**: components to create with `unimplemented!()`
- **Specify**: tests to write from the design record
- **Implement**: stub bodies to fill
- **Review**: perspectives to run

### Deferred Findings (require human judgment)
- Claude panel: ...
- Codex plan review: ...

### Next Step
    /swarm-execute .claude/state/plans/plan_[feature].md
```

Consumers: `/swarm-execute` (plan artifact); Human (deferred findings).

$ARGUMENTS