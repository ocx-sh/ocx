# Contributing to OCX

## Prerequisites

- **Rust** (edition 2024) — install via [rustup](https://rustup.rs/)
- **[task](https://taskfile.dev)** — primary task runner (`brew install go-task` or see docs)
- **Docker** — required for acceptance tests (starts a local OCI registry)
- **[uv](https://docs.astral.sh/uv/)** — Python toolchain for acceptance tests

## Workspace Layout

Two crates in `crates/`:

| Crate | Purpose |
|-------|---------|
| `ocx_lib` | Core library: OCI client, file structure, package manager |
| `ocx_cli` | Thin CLI shell using clap; produces the `ocx` binary |

`oci-client` is patched to a local git submodule at `external/rust-oci-client`. Run `git submodule update --init` after cloning.

## Building

```sh
cargo check                        # fast syntax/type check
cargo build                        # debug build
cargo build --release -p ocx       # release CLI binary
```

## Running Tests

**Unit tests** (no Docker required):

```sh
cargo nextest run --workspace
cargo nextest run -p ocx_lib <test_name>   # single test
```

**Acceptance tests** (require Docker):

```sh
task test              # build binary, start registry:2, run pytest suite
task test:quick        # skip binary rebuild
task test:parallel     # run tests in parallel with pytest-xdist
```

Acceptance tests live in `test/` and use pytest against a real OCI registry.

The suite binds two `registry:2` containers to `localhost:5000` (primary) and
`localhost:5001` (mirror). If either port is already taken — another checkout of
this repo, an unrelated project's registry — move them:

```sh
export OCX_TEST_REGISTRY_PORT=5010
export OCX_TEST_MIRROR_PORT=5011
task test
```

One variable per service drives all three consumers: the host side of the port
mapping in `test/docker-compose.yml`, the `REGISTRY` / `MIRROR_REGISTRY`
defaults in `test/conftest.py`, and `OCX_INSECURE_REGISTRIES` in
`test/taskfile.yml`. Setting `REGISTRY` / `MIRROR_REGISTRY` directly still wins
over both, which is how CI points the suite at an already-running registry.

Do this rather than leaving the conflict in place. Compose does not fail loudly
on a taken port — the `registry` service comes up with *no* published port, and
`localhost:5000` then reaches the other project's registry instead. The suite
pushes its own fixtures before reading them, so most of it still passes; what
you get is a green run against a registry you did not start, littered with this
suite's test packages.

## Code Style

```sh
cargo fmt              # format (max_width=120, see rustfmt.toml)
cargo clippy --workspace
```

Format before every commit. CI enforces both.

## Commit Conventions

All commits must follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add cascade tag publishing
fix: handle missing candidates dir
refactor: extract ReferenceManager from install task
chore: update oci-client submodule
ci: add verify step to release workflow
```

Scopes are optional. cocogitto validates commit messages in CI.

## Branch Model

- Branch from `main` — never commit directly to `main`.
- Keep commits atomic and complete — no WIP commits on shared branches.

## Before Submitting

Run the full verification suite:

```sh
task verify    # fmt check + clippy + build + unit tests + acceptance tests
```

All checks must pass before opening a pull request.
