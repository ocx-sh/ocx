# Suggested Commands

`task` is the canonical entrypoint. Always `task --list` before inventing ad-hoc commands.

## Fast dev loop

```sh
task                  # fast check: cargo fmt --check + clippy + cargo check (root default)
task check            # cargo check only
task run -- <args>    # cargo run --release --bin ocx -- <args>
task rust:verify      # Rust-only gate: format, clippy, license headers, license deps, build, unit tests
task shell:verify     # shell-only: shellcheck + shfmt
task claude:verify    # AI config gate
```

## Full quality gate (pre-commit / pre-merge)

```sh
task verify           # parallel lint, then build + tests (cached)
task --force verify   # bypass caching, run everything
```

## Tests

```sh
task test                 # build binary + start registry + run all acceptance tests
task test:quick           # skip binary rebuild
task test:parallel        # pytest-xdist (-n auto)
task coverage             # LLVM coverage report
task coverage:open        # open HTML report
```

Single Rust unit test:
```sh
cargo nextest run -p ocx_lib <test_name>
cargo test -p ocx_lib -- <test_name> --nocapture
```

Single acceptance test (run from `test/`):
```sh
cd test && uv run pytest tests/test_install.py::test_install_creates_candidate_symlink -v --no-build
```

## Build

```sh
task rust:build                 # release `ocx` binary
cargo build --release -p ocx    # equivalent direct invocation
cargo fmt                       # always run before commit
```

## Website

```sh
task website:serve   # VitePress dev server
task website:build   # full build (schema + recordings + sbom + catalog + vitepress)
```

## Checkpoint / commit / land

```sh
task checkpoint            # amends in-progress work into single "Checkpoint" commit
/finalize                  # clean branch into Conventional Commits ready to ff-merge
```

Manual finalize fallback:
1. `git commit --amend -m "feat: ..."`
2. `git rebase main`
3. `git checkout main && git merge --ff-only <branch>`
4. `git checkout <branch>`

## OCX self-bootstrap (lint tooling, one-off)

```sh
ocx index update shellcheck shfmt lychee
ocx install --select shellcheck:0.11 --select shfmt:3 --select lychee:0
```

## Worktrees (fixed branch ↔ dir)

| Directory      | Branch    |
|----------------|-----------|
| `ocx`          | `goat`    |
| `ocx-evelynn`  | `evelynn` |
| `ocx-sion`     | `sion`    |
| `ocx-soraka`   | `soraka`  |

## Env vars (frequently used)

`OCX_HOME`, `OCX_DEFAULT_REGISTRY`, `OCX_INSECURE_REGISTRIES`, `OCX_OFFLINE`, `OCX_REMOTE`, `OCX_CONFIG`, `OCX_NO_CONFIG`, `OCX_PROJECT`, `OCX_NO_PROJECT`, `OCX_INDEX`, `OCX_BINARY_PIN`, `OCX_GLOBAL`, `OCX_NO_UPDATE_CHECK`, `OCX_NO_MODIFY_PATH`. Full table in `CLAUDE.md`.

## Linux shell quirks

Standard GNU tools — `grep`, `find`, `sed`, `ls`. No BSD-vs-GNU divergence to worry about. `rtk` proxy (token optimizer) auto-wraps common dev commands via Claude Code hook.
