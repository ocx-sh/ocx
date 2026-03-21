---
paths:
  - website/**
  - .claude/artifacts/**
  - .claude/skills/personas/**
---

# OCX Product Context

> First general-purpose binary package manager built on the OCI distribution spec.

**Full product identity**: Read `.claude/references/project-identity.md` for the complete product vision, competitive positioning, use cases, and differentiators.

## Quick Context

OCX turns any OCI registry into a cross-platform binary distribution platform at zero infrastructure cost. Backend-first (JSON, composable commands, clean exit codes), offline-first (local index snapshots), content-addressed (Nix-like dedup without Nix complexity).

**Target users**: Automation tools (GitHub Actions, Bazel, devcontainers), platform engineers. Not end users — OCX is a backend tool.

**Key differentiators**: OCI-native (zero infra cost), content-addressed storage, clean-env execution, offline-first indexing, backend-first design, language-agnostic, private-first, declarative env metadata.

**Competitors**: Homebrew (macOS-only, no private), apt (OS-locked), Nix (steep learning curve), mise/asdf (can't publish), ORAS (no PM UX), GH Releases (just file hosting).

## Three-Store Architecture

```
~/.ocx/
├── objects/{registry}/{repo}/{digest}/   # Content-addressed binaries (immutable)
├── index/{registry}/                     # Local metadata mirror (offline/reproducibility)
└── installs/{registry}/{repo}/           # Stable symlinks (candidates + current)
```

## How to Update

Read `.claude/references/project-identity.md`, verify against current CLI help and source code, edit the reference doc in place.
