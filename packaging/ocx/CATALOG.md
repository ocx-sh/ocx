---
title: OCX
description: Install pre-built tools from OCI registries, switch versions instantly, and run with clean environments
keywords: [package-manager, oci, registry, toolchain, ci, cli]
---

# OCX

Install pre-built tools with a single command, switch versions instantly, and
run them with clean environments. Designed as a backend for GitHub Actions,
Bazel rules, and CI/CD pipelines rather than as an interactive end-user tool.

Any OCI registry — Docker Hub, GHCR, or a private one — is the storage, so
tools are distributed by the same infrastructure that already carries your
container images.

## Highlights

- **Registries as storage** — no bespoke package host to run; publish and pull
  through the OCI registry you already have.
- **Project toolchains** — `ocx.toml` declares the tools a project needs and
  `ocx.lock` pins each one to a digest, so a fresh clone resolves to the exact
  same binaries.
- **Clean environments** — `ocx run` and `ocx package exec` compose the
  environment for a child process only, without mutating the parent shell.
- **Version switching** — every installed version stays addressable; selecting
  another is a symlink flip, not a reinstall.
- **CI-native** — `ocx env --ci` writes the composed environment into the
  runner's own channel so later pipeline steps inherit it.

## Usage

```sh
# Install a package and run it with a clean environment
ocx package install cmake:4
ocx package exec cmake:4 -- cmake --version

# Declare a project toolchain, then run against it
ocx add cmake:4
ocx run -- cmake --version
```

## Links

- Documentation: <https://ocx.sh/docs/getting-started>
- Command reference: <https://ocx.sh/docs/reference/command-line>
- Source: <https://github.com/ocx-sh/ocx>
- License: Apache-2.0
