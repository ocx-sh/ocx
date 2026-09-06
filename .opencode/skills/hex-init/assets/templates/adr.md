# ADR: [Decision Title]

<!--
Architecture Decision Record. Filename and location: this project's
documented ADR convention; `.agents/adrs/adr_NNNN_[topic].md`
(e.g. adr_0001_database_choice.md) if undocumented.
Owner: /hex-architect or a human. Handoff to: /hex-execute; the
reviewer:security perspective covers security implications during
/hex-review.

Format: MADR (Markdown Any Decision Records) - https://adr.github.io/madr/
Best practices: write before the implementing commit; keep short,
specific, comparable across the codebase; one decision per ADR, not a
group of decisions; quantify where possible (SLOs, latency budgets, cost
envelopes).

Mark unresolved ambiguity inline as
`[NEEDS CLARIFICATION: <question>] Recommended: <answer> — <reason>` -
hard cap 3 per artifact. Each marker carries a recommended answer; a
plain approval at the meta-plan gate accepts all recommendations.
-->

## Metadata

**Status:** Proposed | Accepted | Deprecated | Superseded
**Date:** [YYYY-MM-DD]
**Deciders:** [People involved]
**Issue/Ticket:** [link or N/A]
**Related PRD:** [Link to PRD]
**Architectural Conventions:**
- [ ] Decision follows this project's stated architectural conventions /
      golden path
- [ ] OR the deviation is justified in the Rationale section below
**Domain Tags:** [security | data | integration | infrastructure | api | frontend | devops]
**Supersedes:** [adr_NNNN if applicable]
**Superseded By:** [adr_NNNN if applicable]

## Context

[What issue motivates this decision or change?]

## Decision Drivers

- [Driver 1: e.g., scalability requirements]
- [Driver 2: e.g., team expertise]
- [Driver 3: e.g., time constraints]
- [Driver 4: e.g., cost considerations]

## Industry Context & Research

[Landscape research informing this decision, if a researcher pass ran.
Trending alternatives, adoption signals, design patterns.]

**Research artifact:** [location per project conventions] or N/A
**Trending approaches:** [Where the industry is moving]
**Key insight:** [Top finding driving the decision]

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

**Rationale:** [Why this was picked over the others]

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
- [Risk 1 and its mitigation]

## Non-Functional Requirements

<!--
Cover each axis or state "not affected". Quantify where possible.
-->

| Axis | Impact of this decision |
|---|---|
| Scalability | [or "not affected"] |
| Availability | |
| Latency | |
| Security | |
| Cost | |
| Operability | |

## Technical Details

### Architecture

```
[Diagram or description of the architecture]
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

- Related ADR: [adr_NNNN_topic.md]
- Related PRD: [prd_feature.md]
- External documentation: [link]

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| [Date] | [Name] | Initial draft |
