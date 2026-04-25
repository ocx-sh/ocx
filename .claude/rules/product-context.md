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

> First general-purpose binary package manager built on OCI distribution spec.

OCX turn any Docker/OCI registry — Docker Hub, GHCR, ECR, ACR, Harbor, or private instance — into cross-platform binary distribution platform. Every org already run OCI registries for container images; OCX add package manager UX on top of existing infra at zero extra cost.

This rule = canonical product identity. Read when reasoning about project direction, trade-offs, ADR motivation, research framing, doc narratives, competitive positioning.

## The Problem

Distributing pre-built binaries across platforms and teams fragmented:

- **System PMs** (apt, dnf, pacman) — OS-locked, need root, separate packaging per distro
- **Language PMs** (cargo, npm, pip) — only own ecosystem
- **Homebrew** — no private binaries, bottles tied to specific OS versions
- **GitHub Releases** — file hosting only; no install, no version switching, no env management
- **Version managers** (mise, asdf) — consume binaries but can't publish; each tool need plugin
- **Nix** — content-addressed storage, but demand learning functional language
- **Enterprise servers** (Artifactory, Nexus) — thousands per year, still lack client-side tooling
- **Raw OCI tools** (ORAS, docker pull) — move blobs but no package management UX

## Why OCI

- **Zero infrastructure cost** — reuse registry already run for containers
- **Auth/RBAC/TLS for free** — inherit registry security model
- **Ecosystem tooling** — vulnerability scanning, geo-replication, GC built-in
- **Multi-platform manifests** — OCI image indexes give native OS/arch resolution
- **Standards-based** — stable, widely adopted, vendor-neutral

## Target Users

- **Primary**: Automation tools — GitHub Actions, Bazel rules, devcontainer features, CI scripts
- **Secondary**: Platform/infra engineers managing internal tool distribution
- **Non-target**: End users typing interactive commands — OCX = backend tool, not user-facing PM

## Product Principles

1. **Backend-first** — JSON output, composable commands, clean exit codes, made for machine consumption
2. **Offline-first** — local index never auto-update; populated index + cached object = zero network
3. **Content-addressed** — Nix-like dedup and immutability without Nix-like complexity
4. **Clean-env execution** — `ocx exec` run with package-declared vars only, no ambient PATH pollution
5. **Zero infrastructure cost** — BYO OCI registry; no Homebrew tap, no Artifactory license
6. **Language-agnostic** — single standalone binary, no runtime deps, distribute any pre-built binary
7. **Private-first** — registry auth first-class, internal tools easy as public ones

## Key Differentiators

| # | Differentiator | Why It Matters |
|---|---|---|
| 1 | Built on OCI — zero infra cost | Reuse existing registry, no new servers |
| 2 | Content-addressed with dedup | Nix-like efficiency without Nix complexity |
| 3 | Clean environment execution | CI/CD reproducibility, no "works on my machine" |
| 4 | Offline-first indexing | Deterministic builds without network fragility |
| 5 | Backend-first design | Machine-consumable output for Actions/Bazel/scripts |
| 6 | Language-agnostic, toolchain-free | One binary install any binary |
| 7 | Private distribution first-class | Internal tools simple as `docker push` |
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

- **"Why not Homebrew?"** — macOS-only bottles, no private binary support, need maintain formulas
- **"Why not just `docker pull`?"** — no version management, no env composition, no install store, no offline mode
- **"Why not Nix?"** — need learn functional language; OCX work with any binary, flat learning curve
- **"Why not mise/asdf?"** — consume binaries but can't publish; each tool need maintained plugin
- **"Why not ORAS?"** — push/pull only; no install, no version switching, no env management

## Use Cases

1. **CI/CD tool setup** — pinned index snapshots guarantee deterministic tool versions across runners
2. **GitHub Actions** — action bundle index snapshot; `ocx install` resolve right binary per runner
3. **Bazel rules** — one rule version = fixed binary; no URL matrix maintenance per platform
4. **Devcontainer features** — tools resolved at build time without per-arch conditionals
5. **Polyglot teams** — install Go, Python, Node, Rust, CMake, etc. with one tool, one command pattern
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

## Storage Layout

```
~/.ocx/
├── blobs/{registry}/                     # Raw OCI blobs (manifests, referrers, image indexes)
├── layers/{registry}/                    # Extracted OCI tar layers (content-addressed, shared across packages)
├── packages/{registry}/                  # Assembled packages (content/ hardlinked from layers/)
├── tags/{registry}/                      # Tag-to-digest mappings (local metadata mirror)
├── symlinks/{registry}/                  # Install symlinks (candidates + current)
└── temp/                                 # Download staging directories
```

## Technical Overview

- **Language**: Rust 2024 (async-native with Tokio)
- **Workspace**: `crates/ocx_lib` (core) + `crates/ocx_cli` (CLI) + `crates/ocx_mirror` (mirror tool)
- **Default registry**: `ocx.sh` (configurable via `OCX_DEFAULT_REGISTRY`)
- **Platform support**: linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64
- **Testing**: pytest acceptance tests against real OCI registry (registry:2 via docker-compose)

## Update Protocol

This file = single source of truth for OCX product identity. Stale positioning degrade every downstream decision (ADRs, research framing, doc narratives, competitive rebuttals). Keep current.

**When to update** — any of these trigger edit in same commit:

1. New differentiator found (add row to Key Differentiators)
2. Competitive landscape shift (matrix row change, "why not" rebuttal stale)
3. Target user shift (primary/secondary/non-target list change)
4. Product principle added, dropped, or reworded
5. Scope decision reframe positioning (ADR or plan imply different product story)
6. CLI-level UX change visible to positioning (new flagship command, removed subcommand)
7. Use case validated or invalidated

**Who must check** — every agent at product level re-read this file when work could shift positioning: `worker-researcher` after evaluating library/tool/competitor; `worker-architect` after ADR or design spec; `worker-doc-writer` after user-guide / getting-started edits; `worker-builder` / `worker-reviewer` if implementation expose capability gap or break stated principle.

**Validation** — `/meta-maintain-config refresh` spot-check this file against current CLI help, source code, recent ADRs. Run quarterly or after any scope-shifting merge.