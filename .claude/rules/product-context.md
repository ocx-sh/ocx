---
paths:
  - website/**
  - .claude/artifacts/**
  - .claude/agents/worker-researcher.md
  - .claude/agents/worker-architect.md
  - .claude/agents/worker-doc-writer.md
  - .claude/skills/architect/**
  - .claude/skills/builder/**
  - .claude/skills/code-check/**
  - .claude/skills/qa-engineer/**
  - .claude/skills/security-auditor/**
  - .claude/skills/swarm-execute/**
  - .claude/skills/swarm-plan/**
  - .claude/skills/swarm-review/**
  - .claude/skills/docs/**
  - .claude/skills/deps/**
---

# OCX Product Context

> First general-purpose binary package manager built on the OCI distribution spec.

OCX turns any Docker/OCI registry — Docker Hub, GHCR, ECR, ACR, Harbor, or a private instance — into a cross-platform binary distribution platform. Every organization already runs OCI registries for container images; OCX adds a package manager experience on top of that existing infrastructure at zero additional cost.

This rule is the canonical product identity. Read it when reasoning about project direction, trade-offs, ADR motivation, research framing, doc narratives, or competitive positioning.

## The Problem

Distributing pre-built binaries across platforms and teams is fragmented:

- **System PMs** (apt, dnf, pacman) — OS-locked, require root, separate packaging per distro
- **Language PMs** (cargo, npm, pip) — only serve their own ecosystem
- **Homebrew** — no private binaries, bottles tied to specific OS versions
- **GitHub Releases** — file hosting only; no install, no version switching, no env management
- **Version managers** (mise, asdf) — consume binaries but can't publish them; each tool needs a plugin
- **Nix** — content-addressed storage, but demands learning a functional language
- **Enterprise servers** (Artifactory, Nexus) — thousands per year and still lack client-side tooling
- **Raw OCI tools** (ORAS, docker pull) — move blobs but offer no package management UX

## Why OCI

- **Zero infrastructure cost** — reuse the registry you already run for containers
- **Auth/RBAC/TLS for free** — inherit registry's security model
- **Ecosystem tooling** — vulnerability scanning, geo-replication, GC come built-in
- **Multi-platform manifests** — OCI image indexes provide native OS/arch resolution
- **Standards-based** — stable, widely adopted, vendor-neutral

## Target Users

- **Primary**: Automation tools — GitHub Actions, Bazel rules, devcontainer features, CI scripts
- **Secondary**: Platform/infrastructure engineers managing internal tool distribution
- **Non-target**: End users typing interactive commands — OCX is a backend tool, not a user-facing PM

## Product Principles

1. **Backend-first** — JSON output, composable commands, clean exit codes, designed for machine consumption
2. **Offline-first** — local index never auto-updates; populated index + cached object = zero network needed
3. **Content-addressed** — Nix-like dedup and immutability without Nix-like complexity
4. **Clean-env execution** — `ocx exec` runs with package-declared vars only, no ambient PATH pollution
5. **Zero infrastructure cost** — BYO OCI registry; no Homebrew tap, no Artifactory license
6. **Language-agnostic** — single standalone binary, no runtime deps, distributes any pre-built binary
7. **Private-first** — registry auth as first-class, internal tools as easy as public ones

## Key Differentiators

| # | Differentiator | Why It Matters |
|---|---|---|
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

- **"Why not Homebrew?"** — macOS-only bottles, no private binary support, requires maintaining formulas
- **"Why not just `docker pull`?"** — no version management, no env composition, no install store, no offline mode
- **"Why not Nix?"** — requires learning a functional language; OCX works with any binary and has a flat learning curve
- **"Why not mise/asdf?"** — they consume binaries but can't publish them; each tool needs a maintained plugin
- **"Why not ORAS?"** — push/pull only; no install, no version switching, no environment management

## Use Cases

1. **CI/CD tool setup** — pinned index snapshots guarantee deterministic tool versions across runners
2. **GitHub Actions** — action bundles an index snapshot; `ocx install` resolves the right binary per runner
3. **Bazel rules** — one rule version = fixed binary; no URL matrix maintenance per platform
4. **Devcontainer features** — tools resolved at build time without per-architecture conditionals
5. **Polyglot teams** — install Go, Python, Node, Rust, CMake, etc. with one tool and one command pattern
6. **Internal tool distribution** — push proprietary CLI tools to private registry; `ocx install` from any team

## CLI at a Glance

```bash
ocx install cmake:3.28              # Fetch + install (auto-detect platform)
ocx install cmake:3.28 --select     # Also set as "current"
ocx exec cmake:3.28 -- cmake --version   # Run with clean environment
ocx env cmake:3.28                  # Print resolved environment variables
ocx select cmake:3.28               # Set "current" to this version
ocx package push my/cmake:3.28 cmake.tar.xz --cascade  # Publish
ocx clean                           # GC all unreferenced objects
ocx index catalog                   # List known repositories
```

Global flags: `--offline`, `--remote`, `--format json`.

## Three-Store Architecture

```
~/.ocx/
├── objects/{registry}/{repo}/{digest}/   # Content-addressed binaries (immutable)
├── index/{registry}/                     # Local metadata mirror (offline/reproducibility)
└── installs/{registry}/{repo}/           # Stable symlinks (candidates + current)
```

## Technical Overview

- **Language**: Rust 2024 (async-native with Tokio)
- **Workspace**: `crates/ocx_lib` (core) + `crates/ocx_cli` (CLI) + `crates/ocx_mirror` (mirror tool)
- **Default registry**: `ocx.sh` (configurable via `OCX_DEFAULT_REGISTRY`)
- **Platform support**: linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64
- **Testing**: pytest acceptance tests against real OCI registry (registry:2 via docker-compose)

## Update Protocol

This file is the single source of truth for OCX product identity. Stale positioning degrades every downstream decision (ADRs, research framing, doc narratives, competitive rebuttals). Keep it current.

**When to update** — any of the following triggers an edit in the same commit:

1. New differentiator discovered (add row to Key Differentiators)
2. Competitive landscape shifts (matrix row changes, "why not" rebuttal goes stale)
3. Target user shift (primary/secondary/non-target list changes)
4. Product principle added, dropped, or reworded
5. Scope decision reframes positioning (ADR or plan implies a different product story)
6. CLI-level UX change visible to positioning (new flagship command, removed subcommand)
7. Use case validated or invalidated

**Who must check** — every agent working at product level re-reads this file when its work could shift positioning: `worker-researcher` after evaluating a library/tool/competitor; `worker-architect` after an ADR or design spec; `worker-doc-writer` after user-guide / getting-started edits; `worker-builder` / `worker-reviewer` if an implementation exposes a capability gap or breaks a stated principle.

**Validation** — `/meta-maintain-config refresh` spot-checks this file against current CLI help, source code, and recent ADRs. Run it quarterly or after any scope-shifting merge.
