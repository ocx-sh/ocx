# Plan: [Task Name]

<!--
Implementation Plan
Filename: .claude/state/plans/plan_[task].md
Owner: Builder (/builder) or Architect (/architect)
Handoff to: Builder (/builder), QA Engineer (/qa-engineer)
Related Skills: builder, qa-engineer
-->

## Overview

**Status:** Draft | Approved | In Progress | Complete
**Author:** [Name]
**Date:** [YYYY-MM-DD]
**Beads Issue:** [bd://issue-id or N/A]
**Related PRD:** [Link to PRD]
**Related ADR:** [Link to ADR]

## Objective

[What plan accomplish, concise]

## Scope

### In Scope

- [Item 1]
- [Item 2]

### Out of Scope

- [Item 1]
- [Item 2]

## Research

**Research artifact:** [`.claude/artifacts/research_[topic].md`](./research_[topic].md) or N/A

[Tech landscape research summary. Trending tools, design patterns, industry signals informing plan? Alternatives considered from current adoption trends?]

## Technical Approach

### Architecture Changes

```
[Diagram or description of architectural changes]
```

### Key Decisions

| Decision | Rationale |
|----------|-----------|
| [Decision 1] | [Why] |
| [Decision 2] | [Why] |

## Implementation Steps

> **Contract-First TDD**: Every feature follows Stub → Verify → Specify → Implement → Review.
> Tests written from design record *before* implementation. Validate contract — not implementation details.

### Phase 1: Stubs

Make type signatures, trait definitions, function shells. Bodies use `unimplemented!()` (Rust) or `raise NotImplementedError` (Python). Goal: set public API surface + architectural shape, no business logic.

- [ ] **Step 1.1:** [Stub description — types, traits, function signatures]
  - Files: `path/to/file.rs`
  - Public API: [Signatures + types introduced]

- [ ] **Step 1.2:** [Stub description]
  - Files: `path/to/file.rs`
  - Public API: [Signatures + types introduced]

### Phase 2: Architecture Review

Review stubs against design record (`worker-reviewer`, focus: `spec-compliance`, phase: `post-stub`). Verify:
- Type signatures match documented API contract
- Module boundaries align with architecture section above
- Error types cover all documented failure modes
- No missing public surface vs design

Gate: Architecture review pass before proceed. *Optional for features touching ≤3 files.*

### Phase 3: Specification Tests

Write tests from design record, NOT stubs. Tests encode expected behavior, edge cases, acceptance criteria above. Tests must fail against stubs (bodies `unimplemented!()`).

- [ ] **Step 3.1:** Unit tests (from design record component contracts)
  - Files: `path/to/file.rs` (inline `#[cfg(test)]` modules)
  - Cases: [Happy path, error cases, edge cases from design]

- [ ] **Step 3.2:** Acceptance tests (from design record user experience)
  - Files: `test/tests/test_*.py`
  - Scenarios: [User-facing behaviors from design]

Gate: Tests compile (or parse) + fail with `unimplemented`/`NotImplementedError`.

### Phase 4: Implementation

Fill stub bodies so all spec tests pass. No new tests needed — if needed, design record incomplete (update it).

- [ ] **Step 4.1:** [Implementation description]
  - Files: `path/to/file.rs`
  - Details: [Additional context]

- [ ] **Step 4.2:** [Implementation description]
  - Files: `path/to/file.rs`
  - Details: [Additional context]

Gate: All unit + acceptance tests pass. `task verify` succeeds.

### Phase 5: Review & Documentation

- [ ] **Step 5.1:** Spec compliance review (design record ↔ tests ↔ implementation)
- [ ] **Step 5.2:** Code quality review
- [ ] **Step 5.3:** Documentation updates
  - Update: [Files/sections]

## Files to Modify

| File | Action | Description |
|------|--------|-------------|
| `path/to/file.ts` | Create | [Purpose] |
| `path/to/existing.ts` | Modify | [Changes] |
| `path/to/old.ts` | Delete | [Reason] |

## Dependencies

### Code Dependencies

| Package | Version | Purpose |
|---------|---------|---------|
| [package] | [version] | [why needed] |

### Service Dependencies

| Service | Status | Notes |
|---------|--------|-------|
| [Service] | [Available/Needed] | [Notes] |

## Testing Strategy

> Tests = executable spec. Written from design record in Phase 3, before implementation in Phase 4. Each test case trace back to requirement here.

### Unit Tests (from component contracts)

| Component | Behavior | Expected | Edge Cases |
|-----------|----------|----------|------------|
| [Component 1] | [What it should do] | [Expected result] | [Boundary conditions] |
| [Component 2] | [What it should do] | [Expected result] | [Boundary conditions] |

### Acceptance Tests (from user experience)

| User Action | Expected Outcome | Error Cases |
|-------------|------------------|-------------|
| [Action 1] | [What user sees] | [Error scenarios] |
| [Action 2] | [What user sees] | [Error scenarios] |

### Manual Testing

- [ ] [Test case 1]
- [ ] [Test case 2]

## Rollback Plan

1. [Step revert if issues]
2. [Step restore previous state]
3. [Verification steps]

## Risks

| Risk | Mitigation |
|------|------------|
| [Risk 1] | [How to handle] |
| [Risk 2] | [How to handle] |

## Checklist

### Before Starting

- [ ] PRD/ADR approved
- [ ] Dependencies available
- [ ] Branch created from main

### Before PR

- [ ] All tests passing
- [ ] No linting errors
- [ ] Documentation updated
- [ ] Self-review complete

### Before Merge

- [ ] Code review approved
- [ ] QA sign-off
- [ ] No merge conflicts

## Notes

[Extra context, considerations, comments]

---

## Progress Log

| Date | Update |
|------|--------|
| [Date] | [What was done] |