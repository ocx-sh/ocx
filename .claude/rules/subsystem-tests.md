---
paths:
  - test/**
---

# Test Subsystem

Pytest acceptance tests with Docker Compose registry at `test/`.

## Design Rationale

Pytest (not Rust integration tests) because acceptance tests exercise real compiled binary against real OCI registry — catch issues mocked unit tests miss. Session-scoped registry (started once in `pytest_sessionstart`) enables fast parallel runs with pytest-xdist. UUID-prefixed repo names provide isolation on shared registry, no per-test cleanup. See `arch-principles.md` for full pattern catalog.

## Structure

| Path | Purpose |
|------|---------|
| `test/tests/conftest.py` | Function-scoped fixtures (ocx, published_package, etc.) |
| `test/conftest.py` | Session-scoped fixtures (registry, ocx_binary) + `pytest_sessionstart` |
| `test/src/runner.py` | `OcxRunner`: subprocess wrapper with test isolation |
| `test/src/assertions.py` | Cross-platform assertion helpers |
| `test/src/helpers.py` | `make_package()`: build + push test packages |
| `test/src/registry.py` | OCI registry helpers (fetch manifest, extract platforms) |
| `test/taskfile.yml` | Task runner (default, quick, parallel) |

## Key Fixtures

| Fixture | Scope | Purpose |
|---------|-------|---------|
| `registry` | session | localhost:5000 registry:2 (auto-started via docker-compose) |
| `ocx_binary` | session | Path to compiled `ocx` binary |
| `ocx_home` | function | Isolated temp dir for `OCX_HOME` |
| `ocx` | function | `OcxRunner` instance with test isolation |
| `unique_repo` | function | UUID-prefixed repo name (e.g., `t_a1b2c3d4_test`) |
| `published_package` | function | Pre-built + pre-pushed test package (v1.0.0) → `PackageInfo` |
| `published_two_versions` | function | Two versions (v1.0.0, v2.0.0) → `tuple[PackageInfo, PackageInfo]` |

## OcxRunner API

```python
runner = OcxRunner(binary, ocx_home, registry)
runner.run(*args, format="json", check=True)  # Run command, assert success
runner.json(*args)                             # Run + parse JSON stdout
runner.plain(*args)                            # Run without --format flag
```

Env: `OCX_HOME`, `OCX_DEFAULT_REGISTRY`, `OCX_INSECURE_REGISTRIES` set per instance.

## PackageInfo

Returned by `published_package` / `published_two_versions`:

| Field | Example |
|-------|---------|
| `repo` | `"cmake"` |
| `tag` | `"1.0.0"` |
| `short` | `"cmake:1.0.0"` |
| `fq` | `"localhost:5000/cmake:1.0.0"` |
| `marker` | UUID-based unique string |

## make_package()

Creates, bundles, pushes, indexes test package:

```python
pkg = make_package(ocx, repo, tag, tmp_path,
    bins=["hello"],          # Binary names (default)
    env=[...],               # Custom metadata env entries
    cascade=True,            # Auto-tag latest/major/minor/patch
    size_mb=0,               # Random padding for progress bar tests
)
```

## Assertion Helpers

- `assert_path_exists(path)` — exists (file, dir, or symlink)
- `assert_dir_exists(path)` — is directory
- `assert_symlink_exists(path)` — is symlink or Windows junction
- `assert_not_exists(path)` — not exist and not symlink

**Always use `assert_symlink_exists()` instead of `path.is_symlink()`** for Windows junction compat.

## Test Isolation

- **Per-test OCX_HOME**: each test gets isolated `tmp_path` as `OCX_HOME`
- **UUID repo names**: `unique_repo` fixture prevents collisions in shared registry
- **Shared registry**: session-scoped; all tests push/pull same instance
- **Minimal env**: OcxRunner strips ambient env; only PATH, HOME, OCX vars

## Running Tests

```bash
task test              # Build + registry + all tests
task test:quick        # Skip rebuild
task test:parallel     # pytest-xdist (-n auto)

# Single test:
cd test && uv run pytest tests/test_install.py::test_name -v --no-build
```

## Adding a New Test

1. Create function in appropriate `test/tests/test_*.py` (or new file)
2. Use `ocx: OcxRunner` and `published_package: PackageInfo` fixtures
3. Call `ocx.json("command", pkg.short)` and assert results
4. Custom packages: use `make_package()` with `unique_repo` and `tmp_path`
5. Run: `cd test && uv run pytest tests/test_file.py::test_name -v --no-build`

## Test Files

19 test files cover: install, find, select, uninstall, purge, clean, offline, env, exec, package lifecycle, cascade, package pull, describe, package info, index, color, mirror, CI export, shell profile.

## Quality Gate

During review-fix loops, run `task test:parallel` — not full `task verify`. Acceptance tests only; no Rust rebuild needed with `--no-build`.