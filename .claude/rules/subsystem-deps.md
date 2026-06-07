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
Apache-2.0, MIT, BSD-2-Clause, BSD-3-Clause, ISC, Unicode-3.0, CC0-1.0,
CDLA-Permissive-2.0, MIT-0, Zlib, OpenSSL, EPL-2.0, MPL-2.0, BSL-1.0
```

`MPL-2.0` (dirs-sys → option-ext), `BSL-1.0` + `BSD-2-Clause` (starlark
family transitive) carry scoped REMOVE-condition comments in `deny.toml`.

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

## Manually-tracked exact pins

Most deps use caret (`"1.2"`) ranges. A small set is **exact-pinned** (`=X.Y.Z`)
because upstream does not promise API stability between releases. Treat any
bump as a **breaking change**: it requires a manual review pass AND re-running
the engine-isolation test (`crates/ocx_lib/src/script` firewall) before merge.

| Crates | Pin | Reason / upgrade tripwire |
|--------|-----|---------------------------|
| `starlark`, `starlark_syntax`, `starlark_map`, `starlark_derive` | `=0.13.0` | starlark-rust has no API-stability promise between releases. The whole family is co-versioned and must move together. Sourced from crates.io. Any bump = breaking review + re-run the `script/` engine-isolation test + re-confirm the starlark-transitive advisory skips in `deny.toml` — `derivative` (RUSTSEC-2024-0388), `paste` (RUSTSEC-2024-0436), `fxhash` (RUSTSEC-2025-0057) — removing each whose `cargo tree -i <crate>` is then empty. |
| `allocative` | `=0.3.4` | Part of starlark's public API surface, not a free choice: `StarlarkValue` is a sealed trait whose supertrait bound is `allocative::Allocative`, so every host-allocated typed value (`script/*_value.rs`) implements it against the exact allocative version the starlark family links. crates.io starlark 0.13.0 links allocative 0.3.4; this pin must track whatever the starlark family links — bump only together with the starlark family above; mismatch = the sealed-trait bound stops resolving. |

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

Two deps are `[patch.crates-io]`'d to local git submodules under `external/`:

| Crate(s) | Submodule | Why |
|---|---|---|
| `oci-client` | `external/rust-oci-client` | OCX-specific fixes; changes need upstream PRs (see `feedback_submodule_upstream_pr.md`) |
| `docker_credential` | `external/docker_credential` | Credential-helper fixes |

Each submodule is also in the root `[workspace] exclude` (so cargo can run each fork's own test suite without the outer workspace claiming ownership) and registered in `.gitmodules`.

The starlark family (`starlark`, `starlark_syntax`, `starlark_map`, `starlark_derive`, `allocative`) is sourced from crates.io — no fork. The embedded evaluator (`ocx package test --script`) uses stock starlark; the former `ocx-sh/starlark-rust` fork existed only for an LSP server (`ocx lsp`) that has since been removed.

**Path-patch lint gotcha.** Cargo applies `--cap-lints allow` only to registry/git deps, **not path deps**. A blanket `RUSTFLAGS=-D warnings` (e.g. `actions-rust-lang/setup-rust-toolchain`'s default) therefore promotes a vendored fork's upstream warnings to hard errors. OCX scopes warning-denial to `[workspace.lints.rust] warnings = "deny"` (members only) and sets `rustflags: ""` on the `setup-rust-toolchain` steps that compile the path-vendored forks (`verify-basic`, `build-windows-shims` gate, `deploy-website`). Adding another path-patched fork with warnings = same fix.

## Detecting Unused Dependencies

```bash
cargo machete                   # Unused crate dependencies
cargo udeps                     # Alternative (requires nightly)
```

Review before remove — some deps used via feature flags or proc macros static analysis miss.