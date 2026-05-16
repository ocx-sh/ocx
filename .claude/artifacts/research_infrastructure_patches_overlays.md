# Research: Infrastructure Patches & Overlay Systems

**Date:** 2026-03-22
**Domain:** packaging, oci, configuration
**Triggered by:** Design discussion for injecting infrastructure-specific customizations into OCX packages

## Direct Answer

Seven mechanisms surveyed. Best prior art for OCX: Nix overlays (declarative env injection without rebuild), OCI referrers (content attached to existing artifacts via digest-link), Kustomize (non-destructive layering without forking). Anti-patterns: Conda activate.d (shell scripts), Homebrew tap forks (maintenance burden).

## Key Findings

1. **OCI Referrers API** (v1.1, Feb 2024): Natural fit for version-specific overlays. Same-registry constraint is binding — overlays must live in the same registry as the target. ECR, ACR, Quay, Harbor, Zot all support it. GHCR does not yet.

2. **Nix overlays**: Prove declarative env var injection works without rebuild. `overrideAttrs` modifying runtime env (e.g., `SSL_CERT_FILE`) doesn't require rebuilding the binary. Directly translates to OCX: overlay JSON carries env additions merged at exec time.

3. **Kustomize**: Provides the clearest articulation of the non-destructive invariant: publisher controls base, infra operator controls overlay, neither modifies the other, client applies merge at resolve time.

4. **Well-known tags** (`_desc`, `_overlay`): Work on all registries. Repository-scoped. No Referrers API needed. Already established in OCX via `__ocx.desc`.

5. **Flux OCIRepository**: Proves OCI artifacts as runtime config delivery at production scale. Client pulls artifact, parses, applies — registry is purely transport.

6. **Anti-patterns**: Conda `activate.d` (requires shell integration, breaks clean-env), Homebrew formula forks (maintenance burden), any shell-script overlay mechanism.

## Design Patterns Worth Considering

- **Non-destructive layering** (Kustomize): Base + overlay produces derived config; neither modified
- **Content-addressed binding** (OCI referrers): Overlay bound to digest, not tag
- **Scope-matched discovery**: Repository-scoped via tag, version-scoped via referrers
- **Declarative env injection** (Nix): Overlay vars win on conflict, no shell scripts
- **Cascading config hierarchy** (mise): Global → workspace → project

## Sources

- OCI Distribution Spec v1.1 — Referrers API mechanics
- ORAS Attached Artifacts — Same-registry constraint
- NixOS Wiki Overlays — Composition model
- Kubernetes Kustomize — Non-destructive layering
- Flux OCIRepository — Production OCI config delivery
- mise Dev Tools — Cascading config hierarchy
