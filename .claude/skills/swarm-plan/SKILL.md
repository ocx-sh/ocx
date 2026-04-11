---
name: swarm-plan
description: Planning orchestrator that decomposes features into actionable plans using parallel multi-agent discovery, research, design, and adversarial review. Use for feature planning, task decomposition, or multi-perspective research on architectural decisions. Also reusable as the canonical research primitive for AI config and ADR work.
user-invocable: true
disable-model-invocation: true
argument-hint: "feature-or-task-description"
---

# Planning Orchestrator

Decompose features into actionable plans using parallel worker swarms for discovery, research, design, and adversarial review. Mirrors the `swarm-execute` pattern — each phase has a gate, workers run in parallel where independent, and output is a living design record handed off to `/swarm-execute`.

## Planning Workflow — Discover → Research → Classify → Design → Decompose → Review

Each phase has a gate that must pass before proceeding.

### Phase 1: Discover (parallel)

Launch exploration workers in parallel to map the current state:

- `worker-architecture-explorer` — maps current architecture, traces dependencies, finds reusable code and patterns
- 2-4 `worker-explorer` agents — each scoped to a relevant subsystem (first identify scope from `.claude/rules/subsystem-*.md` table below)

**In parallel, read directly:**
- `.claude/rules/product-context.md` — product positioning and vision
- `.claude/artifacts/` — related ADRs, plans, and prior research artifacts
- Relevant `subsystem-*.md` rules for the feature area

**When the feature relates to existing GitHub issues or PRs**, prefer MCP tools `mcp__github__list_issues`, `mcp__github__get_issue`, `mcp__github__list_pull_requests` for structured lookup during discovery. Fallback: `gh issue list --json ...` / `gh pr view`.

**Gate**: Worker reports returned. Current architecture mapped, reusable components identified, prior artifacts checked for overlap.

### Phase 2: Research (parallel)

Launch researchers in parallel to map the technology landscape:

- 1-3 `worker-researcher` agents — each scoped to a distinct research axis:
  - **Technology / tools** — trending libraries, competing tools in the problem space
  - **Design patterns** — emerging approaches, industry best practices, known pitfalls
  - **Domain knowledge** — OCI spec evolution, security considerations, algorithm choices

Each researcher produces an opinionated recommendation with trend analysis, adoption signals, and citations. Findings >1 paragraph MUST be persisted as `.claude/artifacts/research_[topic].md` for reuse by future sessions.

**Gate**: Research findings persisted (or explicitly marked as "no new signals"). At least one researcher has checked adoption trends and recent publications.

### Phase 3: Classify (sequential)

Determine decision reversibility and scope:

| Scope | Reversibility | Artifacts Required |
|-------|---------------|--------------------|
| Small (1-3 days) | Two-Way Door | `plan_[feature].md` |
| Medium (1-2 weeks) | One-Way Door (Medium) | `prd_[feature].md` + `plan_[feature].md` |
| Large (2+ weeks) | One-Way Door (High) | `pr_faq_[feature].md` + `prd_[feature].md` + `adr_[decision].md` + `plan_[feature].md` |

Templates at `.claude/templates/artifacts/`.

**Gate**: Scope and reversibility documented in the plan header.

### Phase 4: Design (delegated for complex decisions, inline otherwise)

For **One-Way Door (High)** decisions or cross-subsystem features, launch `worker-architect` (opus) to produce an ADR or system design. For straightforward features, design inline in the plan artifact.

**Design must include:**
- **Component contracts**: public API (types, traits, function signatures) and expected behavior per component
- **User experience scenarios**: action → expected outcome → error cases for each user-facing behavior
- **Error taxonomy**: all documented failure modes with remediation guidance
- **Edge cases**: boundary conditions and corner cases enumerated
- **Trade-off analysis**: min 2 options, weighted criteria, risks, reversibility, recommendation with rationale

**Gate**: Design artifacts exist in `.claude/artifacts/`, contracts are testable (a tester could write failing tests from them without reading any code).

### Phase 5: Decompose (sequential)

Break the design into right-sized tasks that support contract-first TDD execution:

- Each task maps to a Stub → Specify → Implement → Review cycle
- Dependencies between tasks form a graph
- Critical path identified
- Parallelizable tasks flagged for `/swarm-execute`

**Gate**: `plan_[feature].md` contains executable phases (Stub → Verify → Specify → Implement → Review) that `/swarm-execute` can run without further decomposition.

### Phase 6: Review (parallel, bounded loop)

Launch parallel adversarial reviewers on the draft plan. The loop is **scope-gated** (plan artifact only) and **severity-gated** (only actionable findings drive iterations).

**Round 1 — full review:**
- `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) — Are contracts testable? Do they match the user experience section?
- `worker-architect` — Are trade-offs honest? Alternatives considered? Any subsystem boundary violations? *— required for One-Way Door decisions*
- `worker-researcher` — Does the plan miss trending patterns, known pitfalls, or SOTA approaches in the domain?

Each reviewer classifies findings as:
- **Actionable** — Plan author fixes and re-runs affected reviewers in Round 2
- **Deferred** — Requires human decision; surfaced in the handoff summary

**Round 2 (selective):** Re-run only perspectives that had actionable findings. Stop when no actionable findings remain or after 2 rounds total (reviews of plans should converge fast).

**Gate**: Plan ready for `/swarm-execute`. Deferred findings documented in handoff.

## Worker Assignment

See `.claude/rules/workflow-swarm.md` for worker types, models, tools, and focus modes.

| Phase | Worker | Count | Role |
|-------|--------|-------|------|
| Discover | `worker-architecture-explorer` | 1 | Current-state mapping |
| Discover | `worker-explorer` | 2-4 | Subsystem deep-dive |
| Research | `worker-researcher` | 1-3 | Technology landscape |
| Design (complex) | `worker-architect` | 1 | ADR / system design |
| Review | `worker-reviewer` (spec-compliance) | 1 | Plan consistency |
| Review (One-Way Door) | `worker-architect` | 1 | Trade-off honesty |
| Review | `worker-researcher` | 1 | SOTA gap check |

Max concurrent workers: 8 (per `workflow-swarm.md`).

## Contract-First Planning

Every plan must support the contract-first TDD execution model (Stub → Verify → Specify → Implement → Review):

- **Component contracts**: Each component/module must define its public API surface (types, traits, function signatures) and expected behavior clearly enough that tests can be written *before* implementation.
- **User experience**: Each user-facing behavior must be specified as an acceptance scenario (action → expected outcome → error cases) so acceptance tests can be written from the design record.
- **Error taxonomy**: All failure modes must be documented so error-path tests can be written in the specification phase.
- **Edge cases**: Boundary conditions and corner cases must be enumerated in the design, not discovered during implementation.

The Testing Strategy section of the plan IS the test specification. `/swarm-execute` writes tests from it, not from the code.

## Subsystem Context Rules

Before exploring, identify involved subsystems and read their context rules:

| Subsystem | Rule |
|-----------|------|
| OCI registry/index | `.claude/rules/subsystem-oci.md` |
| Storage/symlinks | `.claude/rules/subsystem-file-structure.md` |
| Package metadata | `.claude/rules/subsystem-package.md` |
| Package manager | `.claude/rules/subsystem-package-manager.md` |
| CLI commands | `.claude/rules/subsystem-cli.md` |
| Mirror tool | `.claude/rules/subsystem-mirror.md` |
| Acceptance tests | `.claude/rules/subsystem-tests.md` |
| Website/docs | `.claude/rules/subsystem-website.md` |

## Available MCP Tools

- **Context7** (`resolve-library-id`, `query-docs`) — library API research during Phase 2
- **Sequential Thinking** (`sequentialthinking`) — structured trade-off analysis during Phase 4

## Research as a Reusable Primitive

Phases 1-2 (Discover + Research) are the **canonical multi-agent research pattern** for the project. They are reused directly by:

- `/architect` — single-decision design work (one ADR, one research axis)
- `/meta-maintain-config` (modes `create` and `research`) — AI config authoring grounded in Claude Code docs + domain research
- `/swarm-plan` — feature planning (this skill, full phases)

Consumers of the pattern SHOULD:
1. Launch workers in parallel (never sequentially)
2. Split researchers by axis (tech / patterns / domain) when research is non-trivial
3. Persist substantial findings as `.claude/artifacts/research_[topic].md` so future sessions can reuse them
4. Pair at least one `worker-explorer` or `worker-architecture-explorer` with researchers to ground external findings in local code

`.claude/rules/meta-ai-config.md` "Research Protocol" references this pattern for all AI config work.

## Output

Every planning session produces:

1. **Artifact(s)** in `.claude/artifacts/` — plan, PRD, ADR, PR-FAQ as classified
2. **Research artifact(s)** in `.claude/artifacts/research_*.md` — persisted for reuse
3. **Dependency graph** — task order for `/swarm-execute`
4. **Handoff summary**:

```markdown
## Plan Complete: [Feature]

### Classification
- **Scope**: Small | Medium | Large
- **Reversibility**: Two-Way | One-Way Medium | One-Way High

### Artifacts
- `.claude/artifacts/plan_[feature].md`
- `.claude/artifacts/research_[topic].md`
- `.claude/artifacts/adr_[decision].md` (if One-Way Door High)

### Executable Phases (for /swarm-execute)
- **Stub**: [components to create with `unimplemented!()`]
- **Specify**: [tests to write from the design record]
- **Implement**: [stub bodies to fill]
- **Review**: [perspectives to run]

### Deferred Findings (require human judgment)
- [Decision 1]: [Why human judgment is needed]

### Next Step — copy-paste to continue:

    /swarm-execute .claude/artifacts/plan_[feature].md
```

## Constraints

- NO creating tasks without testable acceptance criteria
- NO assuming context — Phases 1-2 are mandatory on every run
- NO vague behavior descriptions — each must be testable
- NO skipping Phase 6 review for Medium/Large scope
- NO exceeding 8 parallel workers at once
- ALWAYS run Discover and Research phases with parallel workers
- ALWAYS store artifacts in `.claude/artifacts/`
- ALWAYS include component contracts with expected behavior and edge cases
- ALWAYS include user experience scenarios with error cases
- ALWAYS persist substantial research findings as `research_[topic].md` for reuse
- ALWAYS hand off to `/swarm-execute` with an explicit next-step command

## Handoff

- To `/swarm-execute` — plan artifact with executable phases
- To `/swarm-architect` — complex decisions requiring standalone ADR review
- To Human — deferred findings that need judgment calls

$ARGUMENTS
