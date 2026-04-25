# Bug Fix Plan: [Bug Title]

<!--
Bug Fix Plan
Filename: artifacts/bugfix_plan_[topic].md
Owner: Builder (/builder)
Handoff to: Builder (/builder), QA Engineer (/qa-engineer)
Related Skills: builder, qa-engineer
-->

## Status

<!--
Status block — mandatory for every plan.
Read+mutated by /swarm-plan, /swarm-execute, /swarm-review, /commit, /finalize, /next.
First 20 lines of plan must contain this block — /next reads it via grep.
Schema enforced by .claude/tests/test_ai_config.py::TestPlanStatusBlock.
See .claude/rules/meta-ai-config.md "Plan Status Protocol" for protocol details.
-->

- **Plan:** bugfix_[topic]
- **Active phase:** 1 — Reproduce
- **Step:** /swarm-plan → plan-approved
- **Last update:** [YYYY-MM-DD] (initialized)

---

## Overview

**Status:** Draft | Approved | In Progress | Complete
**Author:** [Name]
**Date:** [YYYY-MM-DD]
**GitHub Issue:** [#N or N/A]
**Severity:** Critical | High | Medium | Low

## Bug Report

### Observed Behavior

[What happen — error message, wrong output, crash, etc.]

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

[Trace path symptom → root cause. Document what checked + ruled out.]

1. **Symptom**: [Visible error or misbehavior]
2. **Proximate cause**: [Line/function producing error]
3. **Root cause**: [Underlying condition triggering proximate cause]
4. **Introduced by**: [Commit, PR, or "original implementation" if always broken]

### Root Cause Statement

> [One sentence: "X happens because Y, introduced when Z"]

### Related Code

| File | Lines | Role |
|------|-------|------|
| `path/to/file.rs` | L42-L58 | [Where root cause lives] |
| `path/to/file.rs` | L100 | [Where symptom manifest] |

### Pattern Check

- [ ] Searched similar code with same defect
- [ ] Checked: regression from recent change? (`git log`, `git bisect`)
- [ ] Checked: other callers affected by same root cause?

## Regression Test Specification

> Tests written BEFORE fix. Must FAIL on current code.

### Unit Tests

| Test | File | Asserts |
|------|------|---------|
| [test_name] | `crates/ocx_lib/src/[module]/mod.rs` | [What test check — target root cause] |

### Acceptance Tests (if applicable)

| Scenario | File | Steps |
|----------|------|-------|
| [scenario_name] | `test/tests/test_[area].py` | [Reproduction steps as test] |

## Fix Approach

### Proposed Change

[Minimal fix targeting root cause]

### Files to Modify

| File | Change |
|------|--------|
| `path/to/file.rs` | [What change + why] |

### Alternatives Considered

| Approach | Rejected Because |
|----------|-----------------|
| [Alternative 1] | [Why worse] |

### Risk Assessment

| Risk | Mitigation |
|------|------------|
| [Risk 1] | [How handle] |

## Verification Checklist

- [ ] Regression test fail on current code (prove bug exist)
- [ ] Fix applied — regression test now pass
- [ ] All existing tests still pass (`task verify`)
- [ ] Manual reproduction steps no longer reproduce bug
- [ ] No scope creep — fix minimal, no drive-by changes

## Notes

[Extra context, workarounds, or follow-up work identified]