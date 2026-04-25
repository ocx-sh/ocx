# PRD: [Feature Name]

<!--
Product Requirements Document
Filename: artifacts/prd_[feature].md
Owner: Architect (/architect)
Handoff to: Architect (/architect)
Related Skills: writing-prds, decomposing-tasks, requirements-analysis
-->

## Overview

**Status:** Draft | In Review | Approved
**Author:** [Name]
**Date:** [YYYY-MM-DD]
**Version:** [1.0]
**Beads Issue:** [bd://issue-id or N/A]
**PR-FAQ:** [Link to approved PR-FAQ - required for new initiatives]
**Stakeholders:** [List]

## Problem Statement

[What problem solve? Who got problem? Why matter?]

### Evidence

Good PRD prove problem with evidence. Include both:

**Quantitative Evidence** (numbers show impact):
- [Financial metric: e.g., "$X lost per month due to..."]
- [Behavioral data: e.g., "Y% of users abandon at step Z"]
- [Volume indicator: e.g., "N support tickets/week about..."]

**Qualitative Evidence** (human side):
- [User quote: "Direct feedback from user research..."]
- [Support insight: "Common complaint pattern..."]
- [Observation: "During usability testing, we saw..."]

## Goals & Success Metrics

| Goal | Metric | Target |
|------|--------|--------|
| [Goal 1] | [Metric] | [Target value] |
| [Goal 2] | [Metric] | [Target value] |

## User Stories

<!--
Use INVEST criteria for all stories:
- Independent: Self-contained
- Negotiable: Not a contract
- Valuable: Delivers value
- Estimable: Can be sized
- Small: Fits in a sprint
- Testable: Clear acceptance criteria
-->

### [Persona 1]

- As a [persona], I want [capability] so that [benefit]
  - Acceptance: [Testable criteria]
- As a [persona], I want [capability] so that [benefit]
  - Acceptance: [Testable criteria]

### [Persona 2]

- As a [persona], I want [capability] so that [benefit]
  - Acceptance: [Testable criteria]

## Requirements

### Functional Requirements

| ID | Requirement | Priority | Notes |
|----|-------------|----------|-------|
| FR-1 | [Requirement] | Must Have | |
| FR-2 | [Requirement] | Should Have | |
| FR-3 | [Requirement] | Nice to Have | |

### Non-Functional Requirements

| ID | Requirement | Target |
|----|-------------|--------|
| NFR-1 | Performance | [e.g., < 200ms response time] |
| NFR-2 | Availability | [e.g., 99.9% uptime] |
| NFR-3 | Security | [e.g., SOC 2 compliant] |

## Scope

### In Scope

- [Feature/capability 1]
- [Feature/capability 2]

### Out of Scope

- [Explicitly excluded item 1]
- [Explicitly excluded item 2]

## Dependencies

| Dependency | Owner | Status | Risk |
|------------|-------|--------|------|
| [Dependency 1] | [Team/Person] | [Status] | [Low/Med/High] |

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| [Risk 1] | [L/M/H] | [L/M/H] | [Strategy] |

## Open Questions

- [ ] [Question 1]
- [ ] [Question 2]

## Appendix

### Mockups/Wireframes

[Link or embed]

### Research

[Link to user research, competitive analysis, etc.]

---

## Approval

| Role | Name | Date | Status |
|------|------|------|--------|
| Product | | | Pending |
| Engineering | | | Pending |
| Design | | | Pending |

---

## Next Steps & Handoffs

After PRD approved:

1. [ ] **Architect Review**: Technical feasibility check
   - Trigger: `/architect`
   - Output: ADR (`artifacts/adr_[topic].md`)

2. [ ] **Design Spec**: Visual design + wireframes
   - Trigger: `/architect`
   - Output: Design Spec (`artifacts/design_spec_[component].md`)

3. [ ] **Engineering Estimate**: Effort sizing + decomposition
   - Trigger: `/builder` or `/architect`
   - Output: Implementation Plan (`.claude/state/plans/plan_[task].md`)

4. [ ] **Create Beads Issues**: Decompose to trackable work items
   - Command: `bd create "Title" -t feature`

**Related Artifacts**:
- ADR: [Link after architect review]
- Design Spec: [Link after designer review]
- Implementation Plan: [Link after decomposition]

---

## Version History

| Version | Date | Author | Changes |
|---------|------|--------|---------|
| 1.0 | [Date] | [Name] | Initial draft |
| | | | |

<!--
Track major requirement changes here. Helps stakeholders see
requirement evolution + give context for future reference.
-->