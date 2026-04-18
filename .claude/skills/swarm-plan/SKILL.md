---
name: swarm-plan
description: Tiered planning orchestrator that decomposes features into actionable plans using parallel multi-agent discovery, research, design, and adversarial review. Scales from light (low) to full kitchen sink (max) via a tier argument; overlays mix architect/research/review/codex axes on top. Use for feature planning, task decomposition, or multi-perspective research on architectural decisions. Also reusable as the canonical research primitive for AI config and ADR work.
user-invocable: true
disable-model-invocation: true
argument-hint: "[tier] <target> [--flags]"
---

# Planning Orchestrator — Tiered

Thin dispatch layer. Phase plans live in sibling tier files
(`tier-low.md`, `tier-high.md`, `tier-max.md`); this file parses
arguments, classifies the target (`classify.md`), resolves overlays
(`overlays.md`), optionally gates on a meta-plan approval, then hands
off to the matching tier file. Shared content (worker assignment,
subsystem context, constraints, handoff format, research-primitive
contract) stays here — never duplicated across tier files.

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
  - `--dry-run` / `--form` — meta-plan preview (`--form` uses `AskUserQuestion`; implies `--dry-run`)

## Workflow

### 1. Parse arguments and detect GitHub target

Detect GitHub refs (ordered, first match wins):
1. Full `https://github.com/<owner>/<repo>/(pull|issues)/<N>` URL
2. `PR <N>` / `pull/<N>` / `pulls/<N>` → PR
3. `issue <N>` / `issues/<N>` → issue
4. `#<N>` or bare integer `<N>` → probe PR first, fall back to issue

Fetch via `mcp__github__pull_request_read` /
`mcp__github__issue_read` (preferred); `gh pr view` / `gh issue view`
with `--json title,body,comments,labels,files` as fallback. PRs and
issues are equal-class targets — probe order is implementation detail.
On fetch failure, treat input as free text (ask via `AskUserQuestion`
only if disambiguation is actually required).

### 2. Classify (only when tier=`auto`)

Read `classify.md`. Apply tier signals + overlay triggers to the prompt
plus any fetched GitHub body/labels. Produce candidate tier +
confidence flag + overlay set. Labels map directly (e.g.,
`breaking-change` → `--codex`). PR file list feeds Discover scope (not
classification).

### 3. Resolve overlays

Final config = tier defaults (`overlays.md` per-tier table) +
classifier overlays + user flag overrides. User flags always win.

### 4. Meta-plan gate (single consolidated approval point)

Fire when ANY of: `--dry-run`, `--form`, tier resolved to `max`, or
classification marked low-confidence. This is the **only** user-prompt
point — no mid-flow `AskUserQuestion` during classification.

Write `.claude/artifacts/meta-plan_[feature].md` with:
Classification (tier + rationale + overlays), GitHub context, Workers
I Would Launch (per phase), Artifacts I Would Produce, Estimated Cost
(parallel worker count, heaviest call, Codex presence), Not Doing
(implementation, PR creation).

**Approval UI** (always a single interaction):
- Default: `EnterPlanMode` with the meta-plan path; resume on approve.
  *If skill resume after `ExitPlanMode` is unreliable in practice,
  fall back to `AskUserQuestion` with Approve / Edit / Cancel options.*
- `--form`: ONE `AskUserQuestion` call with ≤4 batched axis questions
  (Tier / Architect / Research / Codex), first option "Recommended".
  Never sequential prompts. The form IS the preview — do not also
  fire the markdown gate.

On reject: re-draft meta-plan with the rejection rationale (free-text
or explicit axis answers) and re-present once.

### 5. Announce final config (always)

Print before loading the tier file:

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

`Read` the matching `tier-{low,high,max}.md` and execute its phase
plan. No phase content duplicated here.

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

Identify involved subsystems and read their context rules:

| Subsystem | Rule |
|---|---|
| OCI registry/index | `.claude/rules/subsystem-oci.md` |
| Storage/symlinks | `.claude/rules/subsystem-file-structure.md` |
| Package metadata | `.claude/rules/subsystem-package.md` |
| Package manager | `.claude/rules/subsystem-package-manager.md` |
| CLI commands | `.claude/rules/subsystem-cli.md` |
| Mirror tool | `.claude/rules/subsystem-mirror.md` |
| Acceptance tests | `.claude/rules/subsystem-tests.md` |
| Website/docs | `.claude/rules/subsystem-website.md` |

## Research as a Reusable Primitive

Discover + Research phases are the canonical multi-agent research
pattern for the project. Reused by `/architect`,
`/meta-maintain-config` (`create`/`research` modes), and `/swarm-plan`.
Consumers SHOULD: launch workers in parallel; split researchers by
axis (tech / patterns / domain) when research is non-trivial; persist
substantial findings as `research_[topic].md`; pair at least one
explorer with researchers to ground external findings in local code.
`meta-ai-config.md` "Research Protocol" references this contract.

## Constraints

- NO tasks without testable acceptance criteria; NO vague behaviors
- NO assuming context — Discover runs on every tier
- NO skipping Review; NO >8 parallel workers
- NO mid-flow `AskUserQuestion` during classification — ambiguity
  always resolves at the meta-plan gate
- ALWAYS store artifacts in `.claude/artifacts/`; ALWAYS persist
  substantial research as `research_[topic].md`
- ALWAYS include component contracts (with expected behavior and edge
  cases) and user experience scenarios (with error cases)
- ALWAYS announce final config, even post-approval, and hand off to
  `/swarm-execute` with an explicit next-step

## Handoff format

```markdown
## Plan Complete: [Feature or "Resolves #N"]

### Classification
- **Scope**: Small | Medium | Large
- **Reversibility**: Two-Way | One-Way Medium | One-Way High
- **Tier**: low | high | max
- **Overlays**: architect=X, research=Y, codex=Z

### Artifacts
- `.claude/artifacts/plan_[feature].md`
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
    /swarm-execute .claude/artifacts/plan_[feature].md
```

Consumers: `/swarm-execute` (plan artifact); Human (deferred findings).

$ARGUMENTS
