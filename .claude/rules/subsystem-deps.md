---
paths:
  - Cargo.toml
  - crates/*/Cargo.toml
  - deny.toml
  - .licenserc.toml
---

# Dependencies Subsystem

Rust dependency, license, and audit configuration for OCX.

## Tooling

| Tool | Purpose | Config | Task |
|------|---------|--------|------|
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

Confidence threshold: 0.8. Unknown registries denied, unknown git allowed (needed for patched `oci-client`).

### Source File Headers (`.licenserc.toml`)

Every `.rs` file in `crates/` must have:

```rust
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors
```

`external/` is excluded (submodule, upstream owns headers). Fix missing headers via `task license:format`.

## Adding a Dependency

1. `cargo deny check licenses` — must be in the allowlist
2. `cargo deny check advisories` — no known vulnerabilities
3. Is there an existing crate or stdlib solution? Minimize dependencies.
4. Check maintenance (last release, open issues). Use Context7 MCP for current API shape.
5. Add to `Cargo.toml`. Use workspace dependencies when shared across crates.
6. `task license:deps` to verify compliance
7. `cargo clippy --workspace` to verify no new warnings

## Workspace Dependencies

Shared deps go in the root `Cargo.toml` `[workspace.dependencies]` section. Crates reference them with `dep.workspace = true`:

```toml
# Root Cargo.toml
[workspace.dependencies]
tokio = { version = "1", features = ["full"] }

# Crate Cargo.toml
[dependencies]
tokio = { workspace = true }
```

## Updating

```bash
cargo update                    # Update Cargo.lock to latest compatible
cargo update -p crate_name      # Update specific crate
cargo deny check                # Verify licenses + advisories after update
task verify                     # Full verification
```

Dependabot groups: `actions` (GitHub Actions), `rust-deps` (Cargo crates). PRs use `chore(deps):` commit prefix.

## Bans + Sources Policy

- `multiple-versions = "warn"` — warn, not block; review to avoid bloat
- `unknown-registry = "deny"` — all crates must come from crates.io
- `unknown-git = "allow"` — git dependencies allowed (needed for patched `oci-client`)

## Patched Dependencies

`oci-client` is patched to a local git submodule at `external/rust-oci-client`. Changes to the submodule need upstream PRs. See memory note `feedback_submodule_upstream_pr.md`.

## Detecting Unused Dependencies

```bash
cargo machete                   # Unused crate dependencies
cargo udeps                     # Alternative (requires nightly)
```

Review before removing — some deps are used via feature flags or proc macros that static analysis misses.
