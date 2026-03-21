# OCX Project Identity

> A fast, cross-platform binary package manager built on OCI registries.

This document gives AI agents and contributors a comprehensive understanding of what OCX is, why it exists, and what makes it compelling. Read it when reasoning about the project's direction, trade-offs, and positioning.

## Vision

**OCX is the first general-purpose binary package manager built on the OCI distribution spec.** It turns any Docker/OCI registry — Docker Hub, GHCR, ECR, ACR, Harbor, or a private instance — into a cross-platform binary distribution platform.

### The Problem

Distributing pre-built binaries across platforms and teams is a surprisingly fragmented space:

- **System PMs** (apt, dnf, pacman) are OS-locked, require root, and demand separate packaging per distro.
- **Language PMs** (cargo, npm, pip) only serve their own ecosystem.
- **Homebrew** doesn't support private binaries and ties bottles to specific OS versions.
- **GitHub Releases** are just file hosting — no install, no version switching, no env management.
- **Version managers** (mise, asdf) consume binaries but can't publish them; each tool needs a plugin.
- **Nix** offers content-addressed storage but demands learning a functional programming language.
- **Enterprise servers** (Artifactory, Nexus) cost thousands per year and still lack client-side tooling.
- **Raw OCI tools** (ORAS, docker pull) move blobs but offer no package management UX.

Every organization already runs OCI registries for container images. OCX adds a package manager experience on top of that existing infrastructure — at zero additional cost.

### Why OCI

- **Zero infrastructure cost** — reuse the registry you already run for containers
- **Auth/RBAC/TLS for free** — inherit registry's security model, no separate auth system
- **Ecosystem tooling** — vulnerability scanning, geo-replication, garbage collection come built-in
- **Multi-platform manifests** — OCI image indexes provide native OS/arch resolution
- **Standards-based** — OCI distribution spec is stable, widely adopted, and vendor-neutral

## Target Users

- **Primary**: Automation tools — GitHub Actions, Bazel rules, devcontainer features, CI scripts
- **Secondary**: Platform/infrastructure engineers managing internal tool distribution
- **Non-target**: End users typing interactive commands (OCX is a backend tool, not a user-facing PM)

## Product Principles

1. **Backend-first** — JSON output, composable commands, clean exit codes, designed for machine consumption
2. **Offline-first** — Local index never auto-updates; populated index + cached object = zero network needed
3. **Content-addressed** — Nix-like dedup and immutability without Nix-like complexity
4. **Clean-env execution** — `ocx exec` runs with package-declared vars only, no ambient PATH pollution
5. **Zero infrastructure cost** — BYO OCI registry; no Homebrew tap, no Artifactory license
6. **Language-agnostic** — Single standalone binary, no runtime deps, distributes any pre-built binary
7. **Private-first** — Registry auth as first-class, not an afterthought; internal tools as easy as public ones

## Key Differentiators

| # | Differentiator | Why It Matters |
|---|---------------|----------------|
| 1 | Built on OCI — zero infra cost | Reuse existing registry, no new servers |
| 2 | Content-addressed with dedup | Nix-like efficiency without Nix complexity |
| 3 | Clean environment execution | CI/CD reproducibility, no "works on my machine" |
| 4 | Offline-first indexing | Deterministic builds without network fragility |
| 5 | Backend-first design | Machine-consumable output for Actions/Bazel/scripts |
| 6 | Language-agnostic, toolchain-free | One binary installs any binary |
| 7 | Private distribution first-class | Internal tools as simple as `docker push` |
| 8 | Declarative env metadata | Multi-package env composition without shell scripts |

## Competitive Positioning

| Dimension | Homebrew | apt/dnf | GH Releases | mise/asdf | Nix | ORAS | **OCX** |
|---|---|---|---|---|---|---|---|
| Cross-platform | macOS+Linux | Linux only | Manual | Plugin-based | Linux+macOS | Any | **Any** |
| Private binaries | Workaround | Own repo | Tokens | No | Binary cache | Registry auth | **Any OCI registry** |
| Version switching | Partial | No | No | Yes | Yes | No | **Yes** |
| Clean env exec | No | No | No | PATH only | Shell | No | **Yes** |
| Content-addressed | No | No | No | No | Yes | Registry-side | **Yes (local + remote)** |
| Publish workflow | Formula PR | Package build | Upload | N/A | Derivation | oras push | **ocx package push** |
| Learning curve | Low | Low | Low | Low | Very High | Low | **Low** |
| Infra cost | GitHub | Mirrors | GitHub | None | Cache server | Registry | **None (BYO registry)** |
| Automation-ready | Limited | Limited | API only | Limited | Possible | Yes | **Designed for it** |

### "Why Not Just..." Rebuttals

- **"Why not Homebrew?"** — macOS-only bottles, no private binary support, requires maintaining formulas.
- **"Why not just `docker pull`?"** — No version management, no env composition, no install store, no offline mode.
- **"Why not Nix?"** — Requires learning a functional language; OCX works with any binary and has a flat learning curve.
- **"Why not mise/asdf?"** — They consume binaries but can't publish them; each tool needs a maintained plugin.
- **"Why not ORAS?"** — Push/pull only; no install, no version switching, no environment management.

## Use Cases

1. **CI/CD tool setup** — Pinned index snapshots guarantee deterministic tool versions across runners
2. **GitHub Actions** — Action bundles an index snapshot; `ocx install` resolves the right binary per runner
3. **Bazel rules** — One rule version = fixed binary; no URL matrix maintenance per platform
4. **Devcontainer features** — Tools resolved at build time without per-architecture conditionals
5. **Polyglot teams** — Install Go, Python, Node, Rust, CMake, etc. with one tool and one command pattern
6. **Internal tool distribution** — Push proprietary CLI tools to private registry; teams install with `ocx install`

## CLI at a Glance

```bash
ocx install cmake:3.28              # Fetch + install (auto-detect platform)
ocx install cmake:3.28 --select     # Also set as "current"
ocx exec cmake:3.28 -- cmake --version   # Run with clean environment
ocx env cmake:3.28                  # Print resolved environment variables
ocx select cmake:3.28               # Set "current" to this version
ocx package push myregistry/cmake:3.28 cmake.tar.xz --cascade  # Publish
ocx clean                           # GC all unreferenced objects
ocx index catalog                   # List known repositories
```

Global flags: `--offline`, `--remote`, `--format json`.

## Technical Overview

- **Language**: Rust (async-native with Tokio)
- **Workspace**: `crates/ocx_lib` (core) + `crates/ocx_cli` (CLI) + `crates/ocx_mirror` (mirror tool)
- **Default registry**: `ocx.sh` (configurable via `OCX_DEFAULT_REGISTRY`)
- **Platform support**: linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64
- **Testing**: Pytest acceptance tests against real OCI registry (registry:2 via docker-compose)
