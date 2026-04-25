---
name: deps
description: Use when adding a crate, updating dependency versions, auditing licenses, scanning for vulnerabilities, or editing `Cargo.toml` / `deny.toml` / `.licenserc.toml` in the OCX project.
---

# Dependency Management

Role: add, update, audit Rust crate deps with license + advisory compliance.

## Workflow

1. **Research** — query current crate APIs + feature flags via Context7 MCP before eval
2. **Check license** — `cargo deny check licenses` against OCX allowlist
3. **Check advisories** — `cargo deny check advisories` — no known vulns
4. **Add** — prefer workspace deps for shared crates
5. **Verify** — `task license:deps` + `cargo clippy --workspace`
6. **Update** — `cargo update` + `cargo deny check` + `task verify`

## Relevant Rules (load explicitly for planning)

- `.claude/rules/subsystem-deps.md` — tooling, license allowlist, SPDX header format, workspace deps, bans/sources policy, patched `oci-client` submodule (auto-loads on `Cargo.toml` / `deny.toml` / `.licenserc.toml`)
- `.claude/rules/quality-core.md` — YAGNI on deps: minimize surface area
- `.claude/rules/quality-rust.md` — Rust quality gates triggered by new deps

## Qualitative Evaluation Criteria

Before recommend crate, assess:

- **Maintenance**: last commit, last release, issue/PR response time, CI health
- **Bus factor**: single maintainer vs community; check contributor graph
- **Maturity**: version ≥1.0, API churn last 6 months, deprecation notices
- **Adoption**: GitHub stars absolute AND relative to alternatives in niche
- **Ecosystem fit**: works with tokio, our error model, license compatible
- **Alternatives**: #1, #2, or #4 crate in space? What `blessed.rs` or `lib.rs` recommend?

Never pick between two crates from memory alone — query Context7 + check GitHub/crates.io same session.

## Tool Preferences

- **Context7 MCP** (`mcp__context7__resolve-library-id` + `get-library-docs`) — current API shape for fast-moving crates (`tokio`, `oci-client`, `clap`, `serde`). Fallback: WebFetch of `docs.rs/<crate>`. Never pick between two crates from memory alone.
- **`task` runner** — `task license:deps`, `task license:check`, `task license:format`, `task verify`
- **`cargo machete`** — detect unused crate deps (review before remove)

## Constraints

- NO add crate without license + advisory check
- NO invent feature — prefer existing crate or stdlib
- ALWAYS add shared deps to `[workspace.dependencies]`, not per-crate
- ALWAYS use `chore(deps):` commit prefix for dep updates
- Submodule changes (`external/rust-oci-client`) need upstream PRs

## Handoff

- To Security Auditor — new attack surface from dep
- To Builder — integration work

$ARGUMENTS