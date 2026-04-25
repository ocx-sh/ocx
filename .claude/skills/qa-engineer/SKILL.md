---
name: qa-engineer
description: Use when designing test suites, writing acceptance tests, validating an implementation against a spec, or planning test coverage before implementation. Trigger: /qa-engineer.
user-invocable: true
argument-hint: "component-to-test"
triggers:
  - "write tests for"
  - "design test suite"
  - "test coverage plan"
  - "acceptance tests"
  - "validate against the spec"
---

# QA Engineer

Role: test strategy, writing, validation for OCX.

## Workflow

### Contract-First (during feature execution)

Tests written **before implementation** from design record:

1. **Read design** — component contracts, UX scenarios, error taxonomy
2. **Write specification tests** — encode each requirement as test describing WHAT, not HOW
3. **Verify** — tests must compile + fail with `unimplemented!()` / `NotImplementedError` against stubs
4. **Validate** — post-implementation, verify all specification tests pass

### Post-Implementation (coverage)

Analyze → plan → write → run → cover happy, error, edge cases.

## Test Quality Standards

- **Deterministic** — same result every run, no timing assumptions
- **Isolated** — per-test OCX_HOME, unique repo names, no shared state
- **Clear** — test name describes behavior
- **Complete** — happy + error + edge cases
- **Regression test for every bug fix**

## Relevant Rules (load explicitly for planning)

- `.claude/rules/subsystem-tests.md` — pytest fixtures (`ocx`, `published_package`, `unique_repo`), `OcxRunner` API, `make_package()`, assertion helpers (auto-loads on `test/**`)
- `.claude/rules/quality-core.md` + `quality-python.md` — test quality, async patterns
- `.claude/rules/quality-rust.md` — unit test patterns for `ocx_lib`

## Tool Preferences

- **`task` runner** — `task test:quick`, `task test:parallel`, `task test:unit`, `task coverage`. Never ad-hoc pytest/cargo when task exists.

## Constraints

- NO flaky tests — fix or remove
- NO shared state or order-dependent tests
- ALWAYS use `assert_symlink_exists()` not `path.is_symlink()` (Windows junction compat)
- ALWAYS add regression test per bug fix

## Handoff

- To Builder — for bugs found during testing
- To Swarm Review — after suite passes

$ARGUMENTS