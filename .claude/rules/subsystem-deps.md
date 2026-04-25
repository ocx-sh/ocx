---
paths:
  - Cargo.toml
  - crates/*/Cargo.toml
  - deny.toml
  - .licenserc.toml
---

# Dependencies Subsystem

Rust dep, license, audit config for OCX.

## Tooling

| Tool | Purpose | Config | Task |
|------|---------|--------|------|
| `cargo-deny` | License allowlist + ban dup versions + source audit | `deny.toml` | `task license:deps` |
| `hawkeye` | SPDX header enforce on source files | `.licenserc.toml` | `task license:check` |
| `dependabot` | Auto dep update PRs | `.github/dependabot.yml` | (GitHub) |

All three run in CI via `.github/workflows/verify-licenses.yml`. Part of `task verify`.

## License Policy

### Allowed Licenses (`deny.toml`)

```
Apache-2.0, MIT, BSD-3-Clause, ISC, Unicode-3.0, CC0-1.0,
CDLA-Permissive-2.0, MIT-0, Zlib, OpenSSL, EPL-2.0
```

Confidence threshold: 0.8. Unknown registries denied, unknown git allowed (need for patched `oci-client`).

### Source File Headers (`.licenserc.toml`)

Every `.rs` file in `crates/` must have:

```rust
// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors
```

`external/` excluded (submodule, upstream own headers). Fix missing headers via `task license:format`.

## Adding a Dependency

1. `cargo deny check licenses` — must be in allowlist
2. `cargo deny check advisories` — no known vulns
3. Existing crate or stdlib solution exist? Minimize deps.
4. Check maintenance (last release, open issues). Use Context7 MCP for current API shape.
5. Add to `Cargo.toml`. Use workspace deps when shared across crates.
6. `task license:deps` to verify compliance
7. `cargo clippy --workspace` to verify no new warnings

## Workspace Dependencies

Shared deps go in root `Cargo.toml` `[workspace.dependencies]` section. Crates reference via `dep.workspace = true`:

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
- `unknown-registry = "deny"` — all crates from crates.io
- `unknown-git = "allow"` — git deps allowed (need for patched `oci-client`)

## Patched Dependencies

`oci-client` patched to local git submodule at `external/rust-oci-client`. Submodule changes need upstream PRs. See memory note `feedback_submodule_upstream_pr.md`.

## Detecting Unused Dependencies

```bash
cargo machete                   # Unused crate dependencies
cargo udeps                     # Alternative (requires nightly)
```

Review before remove — some deps used via feature flags or proc macros static analysis miss.