# Acceptance Tests

Pytest-based acceptance tests for the `ocx` CLI.
Tests run against a local OCI registry provided by Docker Compose.

## Prerequisites

- Rust toolchain (for building `ocx`)
- Docker (for the registry container)
- Python 3.10+ with [uv](https://docs.astral.sh/uv/)

## Quick Start

```sh
cd test
uv run pytest -v
```

Both the binary build (`cargo build --release`) and the registry container
are started automatically.  To skip the build (e.g. when iterating on tests):

```sh
uv run pytest --no-build -v
```

To run tests in parallel:

```sh
uv run pytest -n auto -v
```

## Directory Structure

```
test/
  docker-compose.yml         Registry service definition
  pyproject.toml             Python project / pytest config
  README.md
  src/                       Test support code
    runner.py                OcxRunner, PackageInfo, platform helpers
    assertions.py            Reusable path/symlink assertions
  tests/                     Test modules
    conftest.py              Fixtures
    test_*.py                Test cases
```

## Key Classes

### `OcxRunner` (`src/runner.py`)

Wraps the `ocx` binary with a controlled environment.
Each instance carries its own `OCX_HOME`, `OCX_DEFAULT_REGISTRY`, and
`OCX_INSECURE_REGISTRIES` so tests never leak host state.

```python
runner.json("install", "-s", "repo:1.0.0")            # parse stdout as JSON
runner.plain("exec", "repo:1.0.0", "--", "hello")     # raw CompletedProcess
runner.run("find", "repo:1.0.0", check=False)         # allow non-zero exit
```

### `PackageInfo` (`src/runner.py`)

Dataclass returned by the `published_package` / `published_two_versions`
fixtures.  Carries the repo name, tag, fully-qualified reference, the
content directory, and a unique marker string for exec verification.

## Fixture Setup

All fixtures are defined in `tests/conftest.py`.

### Session-scoped (shared across all tests)

| Fixture       | Description |
|---------------|-------------|
| `ocx_binary`  | Builds (unless `--no-build`) and resolves the `ocx` binary |
| `registry`    | Registry address (default `localhost:5000`); auto-starts docker-compose |

### Function-scoped (fresh per test)

| Fixture                  | Description |
|--------------------------|-------------|
| `ocx_home`               | Isolated `OCX_HOME` directory via `tmp_path` |
| `ocx`                    | `OcxRunner` instance wired to the isolated home |
| `unique_repo`            | UUID-prefixed repository name for namespace isolation |
| `published_package`      | Pushes a v1.0.0 test package, returns `PackageInfo` |
| `published_two_versions` | Pushes v1.0.0 and v2.0.0, returns `(PackageInfo, PackageInfo)` |

### Test Isolation

Each test is fully self-contained:

1. **Unique `OCX_HOME`** — created via `tmp_path`, destroyed after the test
2. **Unique repository name** — UUID prefix prevents registry collisions
3. **Clean environment** — `OcxRunner` passes only OCX-related variables

This design allows tests to run in parallel (`-n auto`) on a shared registry.

## Configuration

| Environment Variable | Default             | Description |
|---------------------|---------------------|-------------|
| `OCX`               | `target/release/ocx` | Path to the ocx binary |
| `REGISTRY`          | `localhost:5000`     | Registry address |

| CLI Flag      | Description |
|---------------|-------------|
| `--no-build`  | Skip the automatic `cargo build --release` step |
