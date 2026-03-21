---
name: swarm-plan
description: Planning orchestrator that decomposes features into actionable plans using parallel exploration. Use for feature planning, task decomposition, or research.
user-invocable: true
disable-model-invocation: true
argument-hint: "feature-or-task-description"
---

# Planning Orchestrator

Decompose features into actionable plans using parallel exploration swarms.

## Planning Workflow

1. **Explore** — Launch agents in parallel:
   - `worker-architecture-explorer` — maps current architecture, finds reusable code and patterns
   - 2-4 `worker-explorer` agents — search relevant subsystems (read `subsystem-*.md` rules first)
   - 1 `worker-researcher` — industry context, trending tools, design patterns. Persist findings as `.claude/artifacts/research_[topic].md`
   - Read `.claude/references/project-identity.md` for product positioning context
   - Check `.claude/artifacts/` for related ADRs, plans, and prior research
2. **Classify** — Determine decision reversibility:
   - Two-Way Door (easy to reverse) → PR description only
   - One-Way Door (Medium) → RFC + Design excerpt
   - One-Way Door (High) → Full ADR + review
3. **Document** — Create artifacts based on scope
4. **Decompose** — Break into right-sized tasks
5. **Track** — Document implementation plan with dependency graph

## Subsystem Context Rules

Before exploring, identify which subsystems are involved and read their context rules:

| Subsystem | Rule |
|-----------|------|
| OCI registry/index | `.claude/rules/subsystem-oci.md` |
| Storage/symlinks | `.claude/rules/subsystem-file-structure.md` |
| Package metadata | `.claude/rules/subsystem-package.md` |
| Package manager | `.claude/rules/subsystem-package-manager.md` |
| CLI commands | `.claude/rules/subsystem-cli.md` |
| Mirror tool | `.claude/rules/subsystem-mirror.md` |
| Acceptance tests | `.claude/rules/subsystem-tests.md` |

## Available MCP Tools

- **Context7** (`resolve-library-id`, `query-docs`): Research library APIs and specifications during exploration.

## Artifact Requirements

**Small Feature (1-3 days):**
- `plan_[feature].md`

**Medium Feature (1-2 weeks):**
- `prd_[feature].md` + `plan_[feature].md`

**Large Feature (2+ weeks):**
- `pr_faq_[feature].md` + `prd_[feature].md` + `adr_[key-decision].md` + `plan_[feature].md`

Templates at `.claude/templates/artifacts/`.

## Output

Every planning session produces:
1. Artifact(s) in `.claude/artifacts/`
2. Dependency graph showing task order
3. Handoff summary for `/swarm-execute`

## Constraints

- NO creating tasks without clear acceptance criteria
- NO assuming context — explore codebase first
- ALWAYS use parallel workers for research phase
- ALWAYS store artifacts in `.claude/artifacts/`

## Handoff

- To Builder/Swarm Execute: Plan artifact ready for execution
- To Architect: Complex decisions requiring ADR review

$ARGUMENTS
