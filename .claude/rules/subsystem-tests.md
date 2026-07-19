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

**Default env visibility in tests**: `make_package()` defaults env entries to `"visibility": "public"` (see `test/src/helpers.py` lines 160–165). This matches the convention used by in-tree mirrors and acceptance tests that verify env resolution. Tests asserting on env output in `consumer` mode rely on this default. When writing tests for `private` or `interface` entries, pass explicit `visibility` in the `env` list.

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

# Single test (runs prebuilt test/bin/ocx — rebuild via `task test` after Rust changes):
cd test && uv run pytest tests/test_install.py::test_name -v
```

## Adding a New Test

1. Create function in appropriate `test/tests/test_*.py` (or new file)
2. Use `ocx: OcxRunner` and `published_package: PackageInfo` fixtures
3. Call `ocx.json("command", pkg.short)` and assert results
4. Custom packages: use `make_package()` with `unique_repo` and `tmp_path`
5. Run: `cd test && uv run pytest tests/test_file.py::test_name -v`

For shell-friendly assertions (exec output, file existence, exit-code branches), prefer `test/scenarios/` — see Platform Split below.

## Test Files

19 test files cover: install, find, select, uninstall, purge, clean, offline, env, exec, package lifecycle, cascade, package pull, describe, package info, index, color, mirror, CI export, shell profile.

Acceptance coverage for the embedded Starlark host API (`ocx.*`, `expect.*`, the `ocx.os.*` / `ocx.arch.*` typed enum namespaces, and `RunResult`/`Platform` typed values) lives in `test/tests/test_package_test_script.py`. See [subsystem-script.md](./subsystem-script.md) for the host-API style rule those tests pin.

## Platform Split

Two complementary harnesses with different platform reach:

| Harness | Platforms | Use for |
|---------|-----------|---------|
| Pytest (`test/tests/test_*.py`) | Linux + macOS + Windows (per `.github/workflows/verify-deep.yml`) | JSON-output assertions, structured fixtures, Windows junction / `.exe` resolution, anything where Python expressivity beats shell |
| Shell scenarios (`test/scenarios/*.sh`) | Linux + macOS only (Windows skipped via `pytestmark` in `test_scenarios_smoke.py`) | Exec output, marker grep, file/dir existence, exit-code branches — bash is the natural language |

When extending shell scenarios, reuse the harness in `test/src/scenarios/__init__.py` (`Scenario` base class, `# scenario: <Name>` header, registered subclasses for pre-publish state). Do not duplicate setup logic — extend the existing `Scenario` API.

A behaviour assertion belongs in **one** harness, not both. If a pytest case can be expressed verbatim as a shell scenario, prefer the scenario; if it needs structured output parsing or Windows-specific paths, keep it in pytest.

## Shell-Activation Matrix (Docker)

`test/tests/test_shell_activation.py` is a self-contained (stdlib + pytest only) module that proves `ocx self setup` activation survives an **unset `OCX_HOME`** in every login shell — the durable net for a regression class where the managed block sources `env.*` to locate ocx but `env.*` is what sets `OCX_HOME`. It runs the real activation path per shell in a "shell zoo" container and asserts: exit 0, no missing-`env.*` error, the ocx bin dir lands on `PATH`, and (for POSIX/fish/pwsh) a second source does not duplicate it.

- **Files:** `test/docker/shells.Dockerfile` (Debian/glibc + nu/elvish/pwsh) and `test/docker/shells.alpine.Dockerfile` (Alpine/musl, busybox `ash`); `.github/workflows/shell-activation.yml` (build a static musl ocx once → run both image legs; triggers also include `shell.rs` and `command/self_group/**` so activation-logic changes run the matrix); the local entrypoint is `task test:shells` (Docker required).
- **Cross-platform (macOS + Windows):** `.github/workflows/shell-activation-deep.yml` — `workflow_dispatch` + weekly `schedule` (NOT per-PR; macOS/Windows minutes are costly). macOS reuses this same self-contained matrix, installing the ocx-packaged shells (nushell/elvish/pwsh) via `ocx package env`; Windows runs `test/manual/test-windows-activation.ps1` under built-in Windows PowerShell 5.1, built-in pwsh 7, and ocx-installed pwsh 7. cmd/batch PATH idempotency is covered by the `live_batch_*` unit test on the `verify-deep.yml` windows-latest `nextest` leg.
- **Self-contained:** resolves the binary from `$OCX_ACTIVATION_BINARY` / `$OCX_COMMAND` / `test/bin/ocx`, uses `shutil.which` to **skip-if-absent**, so a host `uv run pytest` stays green while the container runs the full matrix. It needs a clean child env (no `OCX_*` leakage such as a stale `OCX_HOME`) so the unset-`OCX_HOME` path is exercised; activation carries no guard variable and the prepend is idempotent move-to-front, so a re-source never duplicates it.
- **Known gaps (xfail/skip, tracked separately):** nushell `source (expr)` is rejected at parse time — `source` resolves at PARSE time but the activation file is written at RUN time, so file-based sourcing of runtime content cannot work; needs an apply-without-source redesign (`load-env` at runtime), not a syntax fix. The elvish "empty global toolchain" `slurp | eval` arity error is an orthogonal `self activate` template issue.

## Benchmark Harness {#bench-harness}

`test/bench/` is a standalone performance harness, separate from the pytest acceptance
suite. It is not pytest-collected for normal runs.

| File | Role |
|------|------|
| `harness.py` | Entry point; owns session lifecycle (toxiproxy proxy, reachability, teardown) |
| `scenarios.py` | 21-row scenario matrix v3 + `SCALING_GROUP_ANCHORS` + `SUITE_BUDGET_SECONDS` |
| `baseline.py` | curl+tar floor command builder |
| `compare.py` | Pure comparison function + `__main__` exit-code handler |
| `report.py` | Pure `generate_report()` + `__main__` file-IO wrapper |
| `conftest.py` | Smoke-validation fixtures only (no Docker required) |
| `dashboard/template.html` | Vue 3 single-file app template for generated HTML report |
| `dashboard/vendor/vue.global.prod.js` | Vue 3.5.x global prod build (inlined into output) |
| `test/tests/test_bench_smoke.py` | pytest-collected smoke tests for harness internals |

Task targets: `task test:bench:setup`, `task test:bench`, `task test:bench:baseline`,
`task test:bench:teardown`, `task test:bench:quick`, `task test:bench:large`,
`task test:bench:scenario`, `task test:bench:report`. The `bench` Docker Compose
profile is isolated — `task test` never starts toxiproxy. See `test/bench/README.md`
for full usage.

## Test-Only Seams (`__testing` feature + `__OCX_*` env overrides)

When an acceptance test must force internal state that production code derives at runtime (the running self-image, the detected host libc, …), **do not invent a new env var or a plain `cfg(test)` gate.** There is one canonical project convention — reuse it:

1. **Cargo feature `__testing`** — declared `__testing = []` in `crates/ocx_lib/Cargo.toml`, re-exported `__testing = ["ocx_lib/__testing"]` in `crates/ocx_cli/Cargo.toml`. Follows the Rust-ecosystem `__name` convention (axum `__private`, reqwest `__tls`): internal, no stability guarantee, never enabled by downstream code.
2. **Gate every seam** `#[cfg(any(test, feature = "__testing"))]` — `test` covers unit tests, `feature = "__testing"` covers the acceptance binary. **Release artifacts physically lack the code path.**
3. **Env-var name is double-underscore-prefixed `__OCX_*`** (e.g. `__OCX_SELF_IMAGE`, `__OCX_TEST_LIBC`) — the prefix signals "private test seam, not user-facing config." These are NOT documented in `website/src/docs/reference/environment.md` and are NOT forwarded via `Env::apply_ocx_config`.
4. **Defense-in-depth assert inside the gate** where misuse is dangerous — e.g. `__OCX_SELF_IMAGE` asserts the override targets a loopback registry, so even a build with the feature on cannot be coerced against a real registry.

The acceptance harness already builds with the feature: `test/taskfile.yml` and `taskfiles/rust.taskfile.yml` pass `--features ocx/__testing`. Adding a new seam needs **no build change** — just gate it and read the `__OCX_*` var. Reference implementation: `crates/ocx_lib/src/package_manager/tasks/update_check.rs::ocx_cli_identifier` (the `__OCX_SELF_IMAGE` seam). Acceptance usage: `test/tests/test_self_update.py`.

## Quality Gate

During review-fix loops, run `task test:parallel` — not full `task verify`. Direct `uv run pytest` never builds: it runs the existing `test/bin/ocx` (stale after Rust changes — refresh via `task test` / `task test:parallel`, which rebuild with `--features ocx/__testing` and copy the binary there).