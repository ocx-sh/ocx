# Bug Fix Plan: [Bug Title]

<!--
Bug Fix Plan
Filename: artifacts/bugfix_plan_[topic].md
Owner: Builder (/builder)
Handoff to: Builder (/builder), QA Engineer (/qa-engineer)
Related Skills: builder, qa-engineer
-->

## Overview

**Status:** Draft | Approved | In Progress | Complete
**Author:** [Name]
**Date:** [YYYY-MM-DD]
**GitHub Issue:** [#N or N/A]
**Severity:** Critical | High | Medium | Low

## Bug Report

### Observed Behavior

[What happens — error message, incorrect output, crash, etc.]

### Expected Behavior

[What should happen instead]

### Reproduction Steps

1. [Exact step 1]
2. [Exact step 2]
3. [Exact step 3]

### Environment

| Factor | Value |
|--------|-------|
| Platform | [OS, arch] |
| OCX version | [version or commit] |
| Registry | [which registry, if relevant] |
| Configuration | [relevant env vars, config] |

### Frequency

[Always | Intermittent (conditions) | One-time]

## Root Cause Analysis

### Investigation Log

[Trace the path from symptom to root cause. Document what you checked and ruled out.]

1. **Symptom**: [The visible error or misbehavior]
2. **Proximate cause**: [The line/function that produces the error]
3. **Root cause**: [The underlying condition that makes the proximate cause trigger]
4. **Introduced by**: [Commit, PR, or "original implementation" if always broken]

### Root Cause Statement

> [One clear sentence: "X happens because Y, which was introduced when Z"]

### Related Code

| File | Lines | Role |
|------|-------|------|
| `path/to/file.rs` | L42-L58 | [Where the root cause lives] |
| `path/to/file.rs` | L100 | [Where the symptom manifests] |

### Pattern Check

- [ ] Searched for similar code that might have the same defect
- [ ] Checked: is this a regression from a recent change? (`git log`, `git bisect`)
- [ ] Checked: are there other callers affected by the same root cause?

## Regression Test Specification

> Tests are written BEFORE the fix, and must FAIL on current code.

### Unit Tests

| Test | File | Asserts |
|------|------|---------|
| [test_name] | `crates/ocx_lib/src/[module]/mod.rs` | [What the test checks — targets root cause] |

### Acceptance Tests (if applicable)

| Scenario | File | Steps |
|----------|------|-------|
| [scenario_name] | `test/tests/test_[area].py` | [Reproduction steps as a test] |

## Fix Approach

### Proposed Change

[Description of the minimal fix targeting the root cause]

### Files to Modify

| File | Change |
|------|--------|
| `path/to/file.rs` | [What changes and why] |

### Alternatives Considered

| Approach | Rejected Because |
|----------|-----------------|
| [Alternative 1] | [Why this is worse] |

### Risk Assessment

| Risk | Mitigation |
|------|------------|
| [Risk 1] | [How to handle] |

## Verification Checklist

- [ ] Regression test fails on current code (proves bug exists)
- [ ] Fix applied — regression test now passes
- [ ] All existing tests still pass (`task verify`)
- [ ] Manual reproduction steps no longer reproduce the bug
- [ ] No scope creep — fix is minimal, no drive-by changes

## Notes

[Any additional context, workarounds, or follow-up work identified]
