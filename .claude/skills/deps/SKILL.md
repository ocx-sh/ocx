---
name: deps
description: Use when adding a crate, updating dependency versions, auditing licenses, scanning for vulnerabilities, or editing `Cargo.toml` / `deny.toml` / `.licenserc.toml` in the OCX project.
---

# Dependency Management

Role: add, update, and audit Rust crate dependencies with license and advisory compliance.

## Workflow

1. **Research** — query current crate APIs and feature flags via Context7 MCP before evaluating
2. **Check license** — `cargo deny check licenses` against the OCX allowlist
3. **Check advisories** — `cargo deny check advisories` — no known vulnerabilities
4. **Add** — prefer workspace dependencies for shared crates
5. **Verify** — `task license:deps` + `cargo clippy --workspace`
6. **Update** — `cargo update` + `cargo deny check` + `task verify`

## Relevant Rules (load explicitly for planning)

- `.claude/rules/subsystem-deps.md` — tooling, license allowlist, SPDX header format, workspace deps, bans/sources policy, patched `oci-client` submodule (auto-loads on `Cargo.toml` / `deny.toml` / `.licenserc.toml`)
- `.claude/rules/quality-core.md` — YAGNI applied to dependencies: minimize surface area
- `.claude/rules/quality-rust.md` — Rust quality gates triggered by new dependencies

## Qualitative Evaluation Criteria

Before recommending a crate, assess:

- **Maintenance**: last commit, last release, issue/PR response time, CI health
- **Bus factor**: single maintainer vs community; look at contributor graph
- **Maturity**: version ≥1.0, API churn in the last 6 months, deprecation notices
- **Adoption**: GitHub stars in absolute terms AND relative to alternatives in its niche
- **Ecosystem fit**: plays well with tokio, works with our error model, licensed compatibly
- **Alternatives**: is this the #1, #2, or #4 crate in its space? What does `blessed.rs` or `lib.rs` recommend?

Never decide between two crates from memory alone — query Context7 and check GitHub/crates.io in the same session.

## Tool Preferences

- **Context7 MCP** (`mcp__context7__resolve-library-id` + `get-library-docs`) — current API shape for fast-moving crates (`tokio`, `oci-client`, `clap`, `serde`). Fallback: WebFetch of `docs.rs/<crate>`. Never decide between two crates from memory alone.
- **`task` runner** — `task license:deps`, `task license:check`, `task license:format`, `task verify`
- **`cargo machete`** — detect unused crate dependencies (review before removing)

## Constraints

- NO adding a crate without license + advisory check
- NO inventing a feature — prefer existing crate or stdlib solution
- ALWAYS add shared deps to `[workspace.dependencies]`, not per-crate
- ALWAYS use `chore(deps):` commit prefix for dependency updates
- Submodule changes (`external/rust-oci-client`) need upstream PRs

## Handoff

- To Security Auditor — for new attack surface introduced by dependency
- To Builder — for integration work

$ARGUMENTS
