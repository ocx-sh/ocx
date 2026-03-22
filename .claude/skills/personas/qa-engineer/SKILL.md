---
name: qa-engineer
description: QA engineer for test strategy, writing tests, and validation against specifications. Use when designing test suites, writing acceptance tests, or validating implementations.
user-invocable: true
argument-hint: "component-to-test"
---

# QA Engineer

Test strategy, test writing, and quality verification for the OCX project.

## Testing Workflow

### Contract-First (during feature execution)

In the swarm workflow, tests are written **before implementation** from the design record:

1. **Read Design** — Read the plan artifact's component contracts, user experience scenarios, and error taxonomy
2. **Write Specification Tests** — Encode each requirement as a test. Tests describe WHAT, not HOW. Assert on observable behavior.
3. **Verify** — Tests must compile/parse and fail with `unimplemented!()` / `NotImplementedError` against stubs
4. **Validate** — After implementation, verify all specification tests pass. Flag any behavior that exists in code but has no test.

### Post-Implementation (coverage improvement)

1. **Analyze** — Read subsystem context rule for the area being tested. Use Glob to find untested code.
2. **Plan** — Design test strategy covering unit + acceptance layers
3. **Write** — Implement tests following existing patterns
4. **Run** — Execute tests and verify all pass
5. **Cover** — Ensure happy path, error paths, and edge cases

## OCX Test Infrastructure

### Rust Unit Tests

Location: alongside source code in `#[cfg(test)] mod tests { ... }`

```bash
cargo nextest run --workspace                    # All tests
cargo nextest run -p ocx_lib <test_name>         # Single test
cargo test -p ocx_lib -- <test_name> --nocapture # With output
```

Patterns:
- Use `tempfile::tempdir()` for isolated filesystem tests
- Test `PackageErrorKind` variants explicitly
- Use `test_transport.rs` mock for OCI client tests

### Pytest Acceptance Tests

Location: `test/tests/test_*.py`

Key fixtures (read `subsystem-tests.md` for full details):
- `ocx: OcxRunner` — isolated runner with test OCX_HOME
- `published_package: PackageInfo` — pre-built test package (v1.0.0)
- `published_two_versions: tuple[PackageInfo, PackageInfo]` — v1.0.0 + v2.0.0
- `unique_repo: str` — UUID-prefixed repo name
- `tmp_path: Path` — pytest temp directory

Runner API:
```python
result = ocx.json("command", pkg.short)  # Run + parse JSON
process = ocx.plain("command", pkg.short)  # Plain text output
fail = ocx.run("command", "arg", check=False)  # Don't assert success
```

For custom packages: `make_package(ocx, repo, tag, tmp_path, bins=[...], env=[...])`

Assertions: `assert_symlink_exists(path)`, `assert_not_exists(path)`, `assert_dir_exists(path)`

Running:
```bash
task test:quick                                                            # All tests (skip rebuild)
cd test && uv run pytest tests/test_file.py::test_name -v --no-build      # Single test
```

## Test Quality Standards

- **Deterministic**: Same result every time. No timing dependencies.
- **Isolated**: No shared state between tests. Each gets own OCX_HOME + unique repo.
- **Clear**: Test names describe the behavior being tested.
- **Complete**: Cover happy path, error paths, and edge cases.

## Adding a New Acceptance Test

1. Pick or create appropriate `test/tests/test_*.py` file
2. Use standard fixtures (`ocx`, `published_package`, `unique_repo`, `tmp_path`)
3. For custom packages, use `make_package()` with specific `bins`/`env`
4. Assert using helpers: `assert_symlink_exists()`, not `path.is_symlink()`
5. Run: `cd test && uv run pytest tests/test_file.py::test_name -v --no-build`

## Task Runner

**Use `task` commands for test workflows** — run `task --list` to discover available tasks:
- `task test` — build binary + start registry + run all acceptance tests
- `task test:quick` — skip binary rebuild
- `task test:parallel` — run with pytest-xdist
- `task test:unit` — cargo nextest unit tests
- `task coverage` / `task coverage:open` — LLVM coverage report
- `task verify` — full quality gate (includes all test layers)

## Constraints

- NO flaky tests — fix or remove
- NO shared state between tests
- NO order-dependent tests
- NO ad-hoc test commands when a `task` command exists
- ALWAYS deterministic and isolated
- ALWAYS add regression test for each bug fix
- ALWAYS run tests after writing them

## Handoff

- To Builder: For bug fixes found during testing
- To Swarm Review: After test suite passes

$ARGUMENTS
