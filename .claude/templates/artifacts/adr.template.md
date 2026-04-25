# ADR: [Decision Title]

<!--
Architecture Decision Record
Filename: artifacts/adr_NNNN_[topic].md (e.g., adr_0001_database_choice.md)
Owner: Architect (/architect)
Handoff to: Builder (/builder), Security Auditor (/security-auditor)
Related Skills: architect

Format: Based on MADR (Markdown Any Decision Records) - https://adr.github.io/madr/
Best Practices:
- Write ADRs BEFORE implementation commit
- Keep short, specific, comparable across codebase
- One decision per ADR (not groups)
- Quantify when possible (SLOs, latency budgets, cost envelopes)
-->

## Metadata

**Status:** Proposed | Accepted | Deprecated | Superseded
**Date:** [YYYY-MM-DD]
**Deciders:** [People involved]
**Beads Issue:** [bd://issue-id or N/A]
**Related PRD:** [Link to PRD]
**Tech Strategy Alignment:**
- [ ] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md`
- [ ] OR deviation justified in Rationale section
**Domain Tags:** [security | data | integration | infrastructure | api | frontend | devops]
**Supersedes:** [adr_NNNN if applicable]
**Superseded By:** [adr_NNNN if applicable]

## Context

[Issue motivating this decision or change?]

## Decision Drivers

- [Driver 1: e.g., scalability requirements]
- [Driver 2: e.g., team expertise]
- [Driver 3: e.g., time constraints]
- [Driver 4: e.g., cost considerations]

## Industry Context & Research

[Tech landscape research before decision. Include trending alternatives, adoption signals, design patterns. Reference research artifacts if available.]

**Research artifact:** [`.claude/artifacts/research_[topic].md`](./research_[topic].md) or N/A
**Trending approaches:** [Where industry moving]
**Key insight:** [Top finding driving decision]

## Considered Options

### Option 1: [Name]

**Description:** [Brief description]

| Pros | Cons |
|------|------|
| [Pro 1] | [Con 1] |
| [Pro 2] | [Con 2] |

### Option 2: [Name]

**Description:** [Brief description]

| Pros | Cons |
|------|------|
| [Pro 1] | [Con 1] |
| [Pro 2] | [Con 2] |

### Option 3: [Name]

**Description:** [Brief description]

| Pros | Cons |
|------|------|
| [Pro 1] | [Con 1] |
| [Pro 2] | [Con 2] |

## Decision Outcome

**Chosen Option:** [Option N]

**Rationale:** [Why picked over others]

### Quantified Impact (where applicable)

| Metric | Before | After | Notes |
|--------|--------|-------|-------|
| Latency (p99) | [X]ms | [Y]ms | [Context] |
| Cost | $[X]/mo | $[Y]/mo | [Context] |
| Throughput | [X] req/s | [Y] req/s | [Context] |
| SLO Impact | [X]% | [Y]% | [Context] |

### Consequences

**Positive:**
- [Consequence 1]
- [Consequence 2]

**Negative:**
- [Consequence 1]
- [Consequence 2]

**Risks:**
- [Risk 1 and mitigation]

## Technical Details

### Architecture

```
[ASCII diagram or description of architecture]
```

### API Contract

```
[Key interfaces, endpoints, or contracts]
```

### Data Model

```
[Key entities and relationships]
```

## Implementation Plan

1. [ ] [Step 1]
2. [ ] [Step 2]
3. [ ] [Step 3]

## Validation

- [ ] Perf benchmarks meet requirements
- [ ] Security review done
- [ ] Cost analysis approved

## Links

- [Related ADR 1](./adr_related.md)
- [PRD](./prd_feature.md)
- [External documentation]

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| [Date] | [Name] | Initial draft |