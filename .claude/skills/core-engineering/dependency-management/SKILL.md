---
name: dependency-management
description: Manage Rust dependencies, license compliance, and security auditing for the OCX project. Use when adding, updating, or auditing crate dependencies.
---

# Dependency Management (OCX)

## Tooling Overview

| Tool | Purpose | Config | Task Command |
|------|---------|--------|-------------|
| `cargo-deny` | License allowlist + ban duplicate versions + source audit | `deny.toml` | `task license:deps` |
| `hawkeye` | SPDX license header enforcement on source files | `.licenserc.toml` | `task license:check` |
| `dependabot` | Automated dependency update PRs | `.github/dependabot.yml` | (GitHub) |

All three run in CI via `.github/workflows/verify-licenses.yml` and are part of `task verify`.

## License Policy

### Allowed Licenses (`deny.toml`)

```
Apache-2.0, MIT, BSD-3-Clause, ISC, Unicode-3.0, CC0-1.0,
CDLA-Permissive-2.0, MIT-0, Zlib, OpenSSL, EPL-2.0
```

Confidence threshold: 0.8. Unknown registries denied, unknown git allowed.

### Source File Headers (`.licenserc.toml`)

Every `.rs` file in `crates/` must have:
```rust
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors
```

`external/` is excluded (submodule, upstream owns headers).

Fix missing headers: `task license:format`

## Adding a New Dependency

1. **Check license**: `cargo deny check licenses` — must be in the allowlist
2. **Check advisories**: `cargo deny check advisories` — no known vulnerabilities
3. **Check necessity**: Is there an existing crate or stdlib solution? Minimize dependencies.
4. **Check maintenance**: Is the crate actively maintained? Check last release date and open issues.
5. **Add to Cargo.toml**: Use workspace dependencies when shared across crates
6. **Run**: `task license:deps` to verify compliance
7. **Run**: `cargo clippy --workspace` to verify no new warnings

### Workspace Dependencies

Shared dependencies go in the root `Cargo.toml` `[workspace.dependencies]` section. Crates reference them with `dep.workspace = true`:

```toml
# Root Cargo.toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }

# Crate Cargo.toml
[dependencies]
tokio = { workspace = true }
```

## Updating Dependencies

```bash
cargo update                    # Update Cargo.lock to latest compatible
cargo update -p crate_name      # Update specific crate
cargo deny check                # Verify licenses + advisories after update
task verify                     # Full verification
```

Dependabot groups: `actions` (GitHub Actions), `rust-deps` (Cargo crates). PRs use `chore(deps):` commit prefix.

## Auditing

```bash
task license:check              # SPDX headers on source files
task license:deps               # cargo-deny: licenses + bans + sources
cargo deny check advisories     # Known vulnerabilities in dependencies
```

### Bans Policy (`deny.toml`)

`multiple-versions = "warn"` — multiple versions of the same crate are warned, not blocked. Review warnings to avoid bloat.

### Sources Policy (`deny.toml`)

- `unknown-registry = "deny"` — all crates must come from crates.io
- `unknown-git = "allow"` — git dependencies allowed (needed for patched `oci-client`)

## Patched Dependencies

`oci-client` is patched to a local git submodule at `external/rust-oci-client`. Changes to the submodule need upstream PRs. See memory note `feedback_submodule_upstream_pr.md`.

## Removing Unused Dependencies

```bash
cargo machete                   # Detect unused crate dependencies
cargo udeps                     # Alternative (requires nightly)
```

Review before removing — some dependencies are used via feature flags or proc macros that static analysis misses.
