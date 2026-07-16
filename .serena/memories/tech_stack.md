# Tech Stack

Single source of truth: `.claude/rules/product-tech-strategy.md`. Latest stable unless pinned.

## Rust (primary)

- Edition 2024, resolver v3.
- Toolchain pinned in `rust-toolchain.toml`: `1.95.0`, components `rustfmt` + `clippy`.
- Cross-targets pinned for Windows shim: `x86_64-pc-windows-gnullvm`, `aarch64-pc-windows-gnullvm` (hermetic build via `cargo-zigbuild`).
- Async: Tokio (`"full"` features, workspace pin `1.52`).
- Linker: mold (dev).
- Format: `cargo fmt`; `rustfmt.toml` sets `max_width = 120`.
- Lint: `cargo clippy --workspace --locked --all-targets -- -D warnings`.
- Tests: `cargo nextest` (release).
- License headers: `hawkeye` (`.licenserc.toml`). Dep license audit: `cargo-deny` (`deny.toml`).
- Patched deps: `oci-client`, `docker_credential` → local `external/` submodules.

## Python (acceptance tests, `test/`)

- Python 3.13+ recommended (`requires-python = ">=3.10"`).
- Manager: `uv`. Linter: `ruff`. Runner: `pytest` (+ `pytest-xdist`, `pexpect`, `oras`, `rich`).
- Integration env: Docker Compose registry (`registry:2` on `localhost:5000`).

## TypeScript / Website (`website/`)

- Runtime + pkg manager: Bun.
- Build: Vite via VitePress (`vitepress ^2.0.0-alpha.16`).
- Deps: `@vueuse/core`, `reka-ui`, `asciinema-player`, `vitepress-plugin-group-icons`.

## Bash (tasks, hooks)

- `set -euo pipefail` mandatory. Validated with `shellcheck` + `shfmt` (managed by ocx itself via `.ocx/index/`, packages `ocx.sh/shellcheck:0.11`, `ocx.sh/shfmt:3`).

## Build / Task runner

- `task` (Taskfile v3) is the entrypoint. Root `taskfile.yml` includes per-subsystem files under `taskfiles/` and tool wrappers via `taskfiles/ocx.taskfile.yml` (shellcheck, shfmt, bats, hawkeye, actionlint, lychee). Per-subsystem includes: `rust`, `shell`, `coverage`, `duplo`, `release`, `claude`, `test`, `recordings`, `schema`, `website`.
- Internal helpers dot-prefixed (`.ensure-cargo-tool` etc.) — GitLab-style hidden jobs convention.

## Infra / CI

- GitHub Actions (`.github/workflows/`), OIDC auth, Trivy security scans.
- Static frontend on Cloudflare Pages (owner nginx proxies ocx.sh/dev.ocx.sh; /v2 stays local), secrets via GitHub Secrets.
- Release tooling: `cargo-dist` (`dist-workspace.toml`), `git-cliff` (`cliff.toml`).
