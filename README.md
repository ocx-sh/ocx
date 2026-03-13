<div align="center">

<img src="./assets/logo.svg" width="192" />

# ocx

**The Simple Package Manager**

[![CI][ci-badge]][ci]
[![License][license-badge]][license]
[![Website][website-badge]][website]
[![Discord][discord-badge]][discord]

</div>

Install pre-built tools with a single command, switch versions instantly, and run with clean environments. Designed as a backend for GitHub Actions, Bazel rules, and CI/CD pipelines.

## Quick Start

```sh
# Install ocx
curl -fsSL https://ocx.sh/install.sh | sh

# Install a package
ocx install cmake:3.28

# Run with a clean environment
ocx exec cmake:3.28 -- cmake --version

# Switch versions
ocx install cmake:3.29 --select
ocx select cmake:3.28    # switch back
```

See the [Getting Started guide][getting-started] for more.

## Installation

```sh
# macOS / Linux
curl -fsSL https://ocx.sh/install.sh | sh

# Windows (PowerShell)
irm https://ocx.sh/install.ps1 | iex
```

See the [installation guide][installation] for all options including manual downloads and updating.

## Documentation

- [User Guide][user-guide] — architecture, versioning, locking, authentication
- [Command Reference][command-line] — all commands, flags, and options
- [FAQ][faq] — platform-specific behavior, design decisions

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full guide. Quick start:

```sh
git clone --recurse-submodules https://github.com/ocx-sh/ocx.git
cd ocx
task              # check: fmt + clippy + cargo check
task test         # acceptance tests (requires Docker)
task verify       # full verification suite
```

**Prerequisites:** [Rust](https://rustup.rs), [task](https://taskfile.dev), [Docker](https://docs.docker.com/get-docker/), [uv](https://docs.astral.sh/uv/)

## Community

- [Discord][discord]
- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Security Policy](SECURITY.md)

## License

OCX is licensed under the [Apache License, Version 2.0][license].

<!-- badges -->
[ci]: https://github.com/ocx-sh/ocx/actions/workflows/verify-basic.yml
[ci-badge]: https://github.com/ocx-sh/ocx/actions/workflows/verify-basic.yml/badge.svg
[license]: LICENSE
[license-badge]: https://img.shields.io/badge/license-Apache--2.0-blue.svg
[website]: https://ocx.sh
[website-badge]: https://img.shields.io/badge/docs-ocx.sh-B70032
[discord]: https://discord.gg/BuRhhAYy9r
[discord-badge]: https://img.shields.io/badge/chat-discord-5865F2?logo=discord&logoColor=white

<!-- docs -->
[getting-started]: https://ocx.sh/docs/getting-started
[installation]: https://ocx.sh/docs/installation
[user-guide]: https://ocx.sh/docs/user-guide
[command-line]: https://ocx.sh/docs/reference/command-line
[faq]: https://ocx.sh/docs/faq
