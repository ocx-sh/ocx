# Spec: [Component/Feature Name]

<!--
Component or feature specification, written before planning to pin down
scope and behavior while ambiguity is still cheap to resolve. Filename
and location: this project's documented spec convention;
`.agents/specs/spec_[name].md` if undocumented.
A human-authored pre-plan artifact; amended by hex-review's fold-back
phase, never created by it.
Use: write the spec, then pass it to planning as the target
(`/hex-plan <path-to-spec>`); the plan's component contracts and
user-experience scenarios derive from it.

Mark unresolved ambiguity inline as
`[NEEDS CLARIFICATION: <question>] Recommended: <answer> — <reason>` -
hard cap 3 per artifact. Each marker carries a recommended answer; a
plain approval at the meta-plan gate accepts all recommendations.

Number component specifications C-001, C-002, ... - these IDs carry into
the plan's Component Contracts unchanged and are the coverage join keys
(hex-core references/protocol.md § Traceability IDs). Scenario IDs
(S-001, ...) are assigned by the plan's UX-scenario table.

Sections marked "(UI only)" apply to visual/interactive components; skip
them for APIs, CLIs, or backend services.
-->

## Overview

**Status:** Draft | In Review | Approved
**Author:** [Name]
**Date:** [YYYY-MM-DD]
**Issue/Ticket:** [link or N/A]
**Related PRD:** [Link to PRD]

## Goals

- [Goal 1: e.g., improve task completion rate]
- [Goal 2: e.g., reduce cognitive/integration load]
- [Goal 3: e.g., maintain consistency with existing components]

## User / Interaction Flow

[The sequence a user or caller goes through — a UI flow, an API call
sequence, a CLI session. ASCII or prose, whichever is clearer.]

```
[Start] → [Step 1] → [Step 2] → [Decision Point]
                                    ↓         ↓
                              [Path A]    [Path B]
                                    ↓         ↓
                                [End]     [End]
```

## Component / Module Specifications

### C-001 — [Component 1]

**Purpose:** [What it does]

**States / Modes:**

| State | Description | Notes |
|-------|--------------|-------|
| Default | [Description] | |
| Error | [Description] | |
| [Other relevant state] | [Description] | |

**Properties / Parameters:**

| Name | Type/Value | Notes |
|------|------------|-------|
| [name] | [type] | |

### C-002 — [Component 2]

[Repeat the structure above]

## Interface Contract

[For a UI: layout per breakpoint. For an API: request/response schema.
For a CLI: flags and output format — whatever the caller needs to know.]

### Layout (UI only)

#### Desktop / Wide

```
┌─────────────────────────────────────┐
│  Header                              │
├─────────┬─────────────────────────────┤
│  Side   │     Main Content            │
│  Nav    │                             │
├─────────┴─────────────────────────────┤
│  Footer                               │
└─────────────────────────────────────┘
```

#### Tablet / Medium

[Layout description]

#### Mobile / Narrow

[Layout description]

### API / Data Contract (if applicable)

```
[Request/response shapes, key entities and relationships]
```

## Non-Functional Requirements

| Concern | Requirement | Notes |
|---------|-------------|-------|
| Performance | | |
| Security | | |
| Accessibility | | |
| Internationalization | | |

## Visual Design (UI only)

### Typography

| Element | Font | Size | Weight | Line Height |
|---------|------|------|--------|-------------|
| H1 | | | | |
| Body | | | | |

### Colors

| Usage | Token | Light Mode | Dark Mode |
|-------|-------|-------------|-----------|
| Primary | `--color-primary` | | |
| Background | `--color-bg` | | |

### Spacing

[Grid/scale — e.g. an 8px grid with xs/sm/md/lg/xl steps]

### Animations

<!-- Keep subtle and purposeful; respect prefers-reduced-motion. -->

| Element | Trigger | Duration | Easing | Properties |
|---------|---------|----------|--------|------------|
| [Element] | [Trigger] | [Duration] | [Easing] | [Properties] |

## Accessibility (UI only)

### Requirements

- [ ] Color contrast ratio ≥ 4.5:1 (AA)
- [ ] Focus indicators visible
- [ ] Keyboard navigation supported
- [ ] Screen reader compatible
- [ ] Touch targets ≥ 44px

### ARIA Labels

| Element | aria-label | aria-describedby |
|---------|------------|--------------------|
| [Element] | [Label] | [Description ID] |

## Assets

| Asset | Format | Notes | Location |
|-------|--------|-------|----------|
| [Asset] | [Format] | | |

## Design Links (UI only)

- Main Design File: [link]
- Component Library: [link]
- Prototype: [link]

---

## Approval

| Role | Name | Date | Status |
|------|------|------|--------|
| Design Lead | | | Pending |
| Product | | | Pending |
| Engineering | | | Pending |
