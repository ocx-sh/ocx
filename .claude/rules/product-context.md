# OCX — The Simple Package Manager

> A fast, cross-platform binary package manager built on OCI registries.

This document gives you (an AI assistant or new contributor) a comprehensive understanding of what OCX is, why it exists, and what makes it compelling. Use it as context when reasoning about the project's direction, trade-offs, and positioning.

---

## The One-Liner

**OCX is the first general-purpose binary package manager built on the OCI distribution spec.** It turns any Docker/OCI registry — Docker Hub, GHCR, ECR, ACR, Harbor, or a private instance — into a cross-platform binary distribution platform.

---

## The Problem OCX Solves

Distributing pre-built binaries across platforms and teams is a surprisingly fragmented space:

- **System package managers** (apt, dnf, pacman) are OS-locked, require root, and demand separate packaging per distro.
- **Language package managers** (cargo, npm, pip) only serve their own ecosystem.
- **Homebrew** doesn't support private binaries and ties bottles to specific OS versions and prefixes.
- **GitHub Releases** are just file hosting — no install, no version switching, no environment management.
- **Version managers** (mise, asdf) consume binaries but cannot publish them; each tool needs a maintained plugin.
- **Nix** offers content-addressed storage and reproducibility but demands learning a functional programming language.
- **Enterprise artifact servers** (Artifactory, Nexus) cost thousands per year and still lack client-side tooling.
- **Raw OCI tools** (ORAS, docker pull) move blobs but offer no package management UX.

Every organization already runs OCI registries for container images. OCX adds a package manager experience on top of that existing infrastructure — at zero additional cost.

---

## Core Architecture

### Three Independent Local Stores

```
~/.ocx/
├── objects/{registry}/{repo}/{digest}/     # Content-addressed binary store
│   ├── content/                            # Extracted package files
│   ├── metadata.json                       # Package metadata
│   └── refs/                               # Back-references for GC
├── index/{registry}/                       # Local metadata mirror
│   ├── tags/{repo}.json                    # Tag → digest mapping
│   └── objects/{digest}.json               # Cached manifests
└── installs/{registry}/{repo}/             # Stable symlinks
    ├── candidates/{tag} → ../../objects/…  # Pinned version
    └── current → candidates/{tag}          # Floating selection
```

1. **Object Store** — Content-addressed by SHA-256 digest. Identical binaries are stored once regardless of how many tags reference them. Immutable paths safe to embed in scripts and configs.

2. **Index Store** — Local JSON snapshots of registry metadata (tags, manifests). Enables offline installs and reproducible CI — same snapshot = same binaries, no matter what happens on the registry.

3. **Install Store** — Two-tier symlinks: `candidates/{tag}` (pinned) and `current` (floating pointer). Symlink paths never change, only targets get re-pointed. Safe to hardcode in IDE configs and shell profiles.

### Reference Tracking and Garbage Collection

Forward symlinks and back-references (`refs/`) enable safe, lockless garbage collection. `ocx clean` only removes objects with an empty `refs/` directory — no graph traversal, no coordination.

### OCI Registry Integration

- Multi-platform manifests: `ocx install cmake:3.28` auto-detects OS/arch and fetches the correct build
- Works with any OCI-compliant registry (Docker Hub, GHCR, ECR, ACR, Harbor, registry:2)
- Inherits registry infrastructure: auth, RBAC, TLS, vulnerability scanning, geo-replication
- `--cascade` tag publishing: push once, automatically update `latest`, major, minor, and patch tags

---

## CLI at a Glance

```bash
# Install and use
ocx install cmake:3.28              # Fetch + install (auto-detect platform)
ocx install cmake:3.28 --select     # Also set as "current"
ocx exec cmake:3.28 -- cmake --version   # Run with clean environment
ocx env cmake:3.28                  # Print resolved environment variables

# Version management
ocx select cmake:3.28               # Set "current" to this version
ocx deselect cmake                  # Remove "current" pointer

# Publishing
ocx package create ./build -m metadata.json -o cmake-3.28.tar.xz
ocx package push myregistry/cmake:3.28 cmake-3.28.tar.xz --cascade

# Housekeeping
ocx uninstall cmake:3.28            # Remove candidate symlink
ocx uninstall cmake:3.28 --purge    # Also remove the object if unreferenced
ocx clean                           # GC all unreferenced objects
ocx clean --dry-run                 # Preview what would be removed

# Index operations
ocx index catalog                   # List known repositories
ocx index list cmake                # List tags for a package
ocx index update cmake              # Sync local index from remote
```

Global flags: `--offline` (no network), `--remote` (skip local index), `--format json` (machine-readable output).

---

## Key Differentiators

### 1. Built on OCI — Zero Infrastructure Cost

Every team already has an OCI registry. OCX turns it into a binary distribution platform. No Homebrew tap to maintain, no Artifactory license to buy, no custom download server to host.

### 2. Content-Addressed Storage with Deduplication

Nix-like storage efficiency without Nix-like complexity. `cmake:3.28` and `cmake:latest` pointing to the same binary share one object directory on disk.

### 3. Clean Environment Execution

`ocx exec` runs commands with a clean environment plus only the package-declared variables. No ambient PATH pollution, no "works on my machine" surprises. Critical for CI/CD reproducibility.

### 4. Offline-First, Snapshot-Based Indexing

The local index is never auto-updated. If the index is populated and the binary is in the object store, everything works with zero network. Same CI run, same binaries, every time.

### 5. Backend-First Design

OCX is designed to be called by GitHub Actions, Bazel rules, devcontainer features, and Python scripts — not just humans in a terminal. JSON output mode, clean exit codes, and composable commands.

### 6. Language-Agnostic, Toolchain-Free

A single standalone binary with no runtime dependencies. Distributes any pre-built binary regardless of source language. No Rust toolchain (unlike cargo-binstall), no Ruby formulas (unlike Homebrew), no functional language (unlike Nix).

### 7. Private Distribution as First-Class

Homebrew explicitly disclaims interest in private software. OCX inherits auth, RBAC, and access control from whatever OCI registry you use — internal tool distribution is as simple as `docker push`.

### 8. Declarative Environment Metadata

Packages declare their environment variables in `metadata.json` with three types:
- **path**: prepended to existing vars (e.g., `PATH=${installPath}/bin`)
- **constant**: set to fixed value (e.g., `JAVA_HOME=${installPath}`)
- **accumulator**: merged across multiple packages

Multi-package environment composition without shell scripts.

---

## Competitive Positioning

| Dimension | Homebrew | apt/dnf | GH Releases | mise/asdf | Nix | ORAS | **OCX** |
|---|---|---|---|---|---|---|---|
| Cross-platform | macOS+Linux | Linux only | Manual | Plugin-based | Linux+macOS | Any | **Any** |
| Private binaries | Workaround | Own repo | Tokens | No | Binary cache | Registry auth | **Any OCI registry** |
| Version switching | Partial | No | No | Yes | Yes | No | **Yes** |
| Clean env execution | No | No | No | PATH only | Shell | No | **Yes** |
| Content-addressed | No | No | No | No | Yes | Registry-side | **Yes (local + remote)** |
| Publish workflow | Formula PR | Package build | Upload | N/A | Derivation | oras push | **ocx package push** |
| Learning curve | Low | Low | Low | Low | Very High | Low | **Low** |
| Infra cost | GitHub | Mirrors | GitHub | None | Cache server | Registry | **None (BYO registry)** |
| Automation-ready | Limited | Limited | API only | Limited | Possible | Yes | **Designed for it** |

---

## Intended Use Cases

1. **CI/CD tool setup** — Pinned index snapshots guarantee deterministic tool versions across all runners without network fragility.
2. **GitHub Actions** — Action bundles an index snapshot; `ocx install` resolves the right binary for each runner's OS/arch.
3. **Bazel rules** — One rule version = fixed binary. No URL matrix maintenance per platform.
4. **Devcontainer features** — Tools resolved at container build time without per-architecture conditionals.
5. **Polyglot teams** — Install Go, Python, Node, Rust, CMake, etc. with one tool and one command pattern.
6. **Internal tool distribution** — Push proprietary CLI tools to your private registry; teams install with `ocx install`.

---

## Technical Details

- **Language**: Rust (async-native with Tokio)
- **Workspace**: `crates/ocx_lib` (core library) + `crates/ocx_cli` (thin CLI shell)
- **Default registry**: `ocx.sh` (configurable via `OCX_DEFAULT_REGISTRY`)
- **Compression**: tar.xz (LZMA), tar.gz, plain tar with configurable compression levels
- **Platform support**: linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64
- **Testing**: Pytest-based acceptance tests against a real OCI registry (registry:2 via docker-compose)

---

## How to Update This Pitch

This file should be refreshed when the project evolves significantly. To update it in a future Claude session:

1. **Read the current state**: Start by reading this file, `CLAUDE.md` (if it exists), and the memory file at `~/.claude/projects/-home-mherwig-dev-ocx/memory/MEMORY.md`.
2. **Check the website**: Fetch `https://ocx.sh` for any messaging or feature changes.
3. **Review recent changes**: Run `git log --oneline -20` and scan for new commands, features, or architectural changes.
4. **Check CLI help**: Run `cargo run --bin ocx -- --help` and subcommand help for any new commands or flags.
5. **Read key source files**:
   - `crates/ocx_cli/src/command/` — CLI commands (new subcommands?)
   - `crates/ocx_lib/src/package_manager/` — core operations
   - `crates/ocx_lib/src/file_structure/` — storage architecture
   - `crates/ocx_lib/src/package/metadata.rs` — metadata format
6. **Check tests**: Look at `test/tests/` for new test scenarios that indicate new features.
7. **Update sections**: Edit this file in place. Keep the structure. Update the comparison table, CLI examples, and differentiators as needed. Remove anything that's no longer accurate.
8. **Keep it concise**: This is a pitch document, not API documentation. Focus on "why" and "what", not implementation details.
