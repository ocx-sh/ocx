# Plan: [Task Name]

<!--
Implementation Plan
Filename: artifacts/plan_[task].md
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

[Clear, concise statement of what this plan will accomplish]

## Scope

### In Scope

- [Item 1]
- [Item 2]

### Out of Scope

- [Item 1]
- [Item 2]

## Research

**Research artifact:** [`.claude/artifacts/research_[topic].md`](./research_[topic].md) or N/A

[Summary of technology landscape research. What trending tools, design patterns, or industry signals informed this plan? What alternatives were considered based on current adoption trends?]

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
> Tests are written from the design record *before* implementation, ensuring they validate the
> contract — not the implementation details.

### Phase 1: Stubs

Create type signatures, trait definitions, and function shells. All bodies use `unimplemented!()` (Rust) or `raise NotImplementedError` (Python). The goal is to establish the public API surface and architectural shape without any business logic.

- [ ] **Step 1.1:** [Stub description — types, traits, function signatures]
  - Files: `path/to/file.rs`
  - Public API: [Signatures and types this introduces]

- [ ] **Step 1.2:** [Stub description]
  - Files: `path/to/file.rs`
  - Public API: [Signatures and types this introduces]

### Phase 2: Architecture Review

Review stubs against this design record (`worker-reviewer`, focus: `spec-compliance`, phase: `post-stub`). Verify:
- Type signatures match the documented API contract
- Module boundaries align with the architecture section above
- Error types cover all documented failure modes
- No missing public surface area compared to the design

Gate: Architecture review passes before proceeding. *Optional for features touching ≤3 files.*

### Phase 3: Specification Tests

Write tests from the design record, NOT from the stubs. Tests encode the expected behavior, edge cases, and acceptance criteria documented above. Tests should fail against the stubs (since bodies are `unimplemented!()`).

- [ ] **Step 3.1:** Unit tests (from design record's component contracts)
  - Files: `path/to/file.rs` (inline `#[cfg(test)]` modules)
  - Cases: [Happy path, error cases, edge cases from design]

- [ ] **Step 3.2:** Acceptance tests (from design record's user experience)
  - Files: `test/tests/test_*.py`
  - Scenarios: [User-facing behaviors from design]

Gate: Tests compile (or parse) and fail with `unimplemented`/`NotImplementedError`.

### Phase 4: Implementation

Fill in the stub bodies so all specification tests pass. No new tests should be needed — if they are, the design record was incomplete (update it).

- [ ] **Step 4.1:** [Implementation description]
  - Files: `path/to/file.rs`
  - Details: [Additional context]

- [ ] **Step 4.2:** [Implementation description]
  - Files: `path/to/file.rs`
  - Details: [Additional context]

Gate: All unit tests and acceptance tests pass. `task verify` succeeds.

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

> Tests are the executable specification. They are written from this design record in Phase 3,
> before implementation begins in Phase 4. Each test case must trace back to a requirement here.

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

1. [Step to revert if issues arise]
2. [Step to restore previous state]
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

[Any additional context, considerations, or comments]

---

## Progress Log

| Date | Update |
|------|--------|
| [Date] | [What was done] |
