# Contributing to OCX

## Prerequisites

- **Rust** (edition 2024) — install via [rustup](https://rustup.rs/)
- **[task](https://taskfile.dev)** — primary task runner (`brew install go-task` or see docs)
- **Docker** — required for acceptance tests (starts a local OCI registry)
- **[uv](https://docs.astral.sh/uv/)** — Python toolchain for acceptance tests

## Workspace Layout

`crates/` holds every workspace member (`members = ["crates/*"]`, 21 today). The library is mid-split — `ocx_lib` is being dissolved into 17 `ocx_*` crates (`.claude/artifacts/adr_crate_split_workspace.md`; `scripts/crate_map.toml` is the dependency map), so treat the shells as the destination and `ocx_lib` as the source:

| Crate | Purpose |
|-------|---------|
| `ocx_lib` | Core library: OCI client, file structure, package manager (being emptied by extraction) |
| `ocx_cli` | Thin CLI shell using clap; produces the `ocx` binary |
| `ocx_schema` | Build-only JSON schema generator |
| `ocx_shim` | Windows launcher shim |
| `ocx_*` (17) | The responsibility-derived crates the library becomes; each `README.md` states its tier and what it may depend on |

Three crates are patched to local git submodules under `external/`: `oci-client` (`rust-oci-client`), `docker_credential` and `sigstore` (`sigstore-rs`). Run `git submodule update --init` after cloning.

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

Every pytest run starts its own `zot` registries (primary, promotion target and
prod) on free ports and stops them on exit, so two runs never share one and no
port needs moving. `zot` comes from the project toolchain (`ocx.toml`), which the
shell hook or `ocx exec` puts on `PATH`.

Only the `registry:2` mirror uses Docker. It binds `localhost:5001` and starts the
first time a test needs it. If that port is taken — another checkout of this repo,
an unrelated project's registry — move it:

```sh
export OCX_TEST_MIRROR_PORT=5011
task test
```

One variable drives both consumers: the host side of the port mapping in
`test/docker-compose.yml` and the `MIRROR_REGISTRY` default in `test/conftest.py`.
Setting `MIRROR_REGISTRY` directly still wins over both.

Without Docker, `export OCX_TEST_NO_DOCKER=1` skips the mirror and Sigstore tests
(reason `no docker:`). Nothing infers it: with the variable unset, a missing `docker`
CLI fails those tests.

## Code Style

```sh
cargo fmt              # format (max_width=120, see rustfmt.toml)
task rust:clippy:check # Bazel clippy aspect, ratcheted against clippy-warn-baseline.json (Linux only)
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
