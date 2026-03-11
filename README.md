<div align="center">

<img src="./assets/logo.svg" width="192" />

# ocx

**🚀 The Simple Package Manager 🦄**

[![DevContainer][devcontainer-badge]][devcontainer]
[![Website][website-badge]][website]
[![License][license-badge]][license]

</div>

[devcontainer]: https://code.visualstudio.com/docs/devcontainers/containers
[devcontainer-badge]: https://img.shields.io/static/v1?label=&message=DevContainer&logo=developmentcontainers&color=565C5E&logoColor=FFFFFF&labelColor=028FC3
[website]: https://ocx.sh
[website-badge]: https://img.shields.io/static/v1?label=&message=Website&logo=vitepress&color=565C5E&logoColor=FFFFFF&labelColor=B70032
[license]: LICENSE
[license-badge]: https://img.shields.io/badge/license-Apache--2.0-blue.svg

## Development

**Prerequisites** — install these tools before running tasks:

| Tool | Purpose | Install |
|------|---------|---------|
| [Rust](https://rustup.rs) | Build toolchain | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| [task](https://taskfile.dev/installation/) | Task runner | `cargo install go-task` or see [docs](https://taskfile.dev/installation/) |
| [Docker](https://docs.docker.com/get-docker/) | Acceptance test registry | Platform installer |
| [uv](https://docs.astral.sh/uv/getting-started/installation/) | Python toolchain for tests | `curl -LsSf https://astral.sh/uv/install.sh \| sh` |
| [Node.js](https://nodejs.org/) + [bun](https://bun.sh) | Website (VitePress) | `nvm install --lts` + `npm install -g bun` or see [bun docs](https://bun.sh/docs/installation) |

The following tools are installed automatically by `task` on first use:

| Tool | Purpose |
|------|---------|
| [cargo-nextest](https://nexte.st) | Fast Rust test runner |
| [hawkeye](https://github.com/korandder/hawkeye) | SPDX license header checks |
| [cargo-deny](https://embarkstudios.github.io/cargo-deny/) | Dependency license auditing |

```sh
task                    # check: fmt + clippy + cargo check
task build              # release binary
task test:unit          # unit tests
task test               # acceptance tests (requires Docker)
task license:check      # verify SPDX headers (auto-installs hawkeye)
task license:deps       # audit dependency licenses (auto-installs cargo-deny)
```

## License

OCX is licensed under the [Apache License, Version 2.0][license].
