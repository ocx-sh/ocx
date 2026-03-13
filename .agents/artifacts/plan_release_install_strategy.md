# Plan: Release and Install Strategy Implementation

**ADR**: `adr_release_install_strategy.md`
**Created**: 2026-03-12
**Status**: Not Started

---

## Implementation Rules

These rules apply to **every increment** in this plan. Violating any of them means the increment is not done.

### 1. Complete Increments Only

Every increment must be **functionally complete and self-contained**. No half-done features, no TODOs left behind. Each increment must compile, pass all tests, and be committable as a coherent unit.

### 2. Test Everything

- **Unit tests**: New Rust code gets unit tests in the same crate.
- **Acceptance tests**: New CLI behavior gets pytest acceptance tests in `test/tests/`.
- **CI validation**: New workflow changes get validated by running the workflow (or by `task verify`).
- Run `task verify` before marking any increment as done.

### 3. Document Everything

- **Code**: Follow existing patterns (doc comments on public types, inline comments only where non-obvious).
- **Website**: If the increment changes user-facing behavior, update the relevant website page(s).
- **CLAUDE.md**: If the increment adds env vars, commands, or architectural changes, update the env var table or relevant sections.
- **CLI commands reference**: Update `.claude/rules/cli-commands.md` if command behavior changes.
- Follow `.claude/rules/documentation.md` for all website content.

### 4. Follow the ADR

The ADR (`adr_release_install_strategy.md`) is the design authority. Do not deviate from architectural decisions without updating the ADR first. Key constraints are also captured in `.claude/rules/release-implementation.md`.

### 5. Branch Discipline

- Work on a feature branch (not `main`).
- Commit after each completed increment.
- **Never push to remote** — the human decides when to push.
- Run `cargo fmt` before every commit.

### 6. Status Tracking

After completing each increment:
1. Update the increment file's status to `Done` with the completion date.
2. Update this meta-plan's status table below.
3. Commit the status updates together with the implementation.

---

## Stages and Increments

### Stage 1: Version Foundation

| # | Increment | File | Status |
|---|-----------|------|--------|
| 01 | Workspace version + version guard test + shared version utility | [inc_01_workspace_version.md](increments/release_install/inc_01_workspace_version.md) | Done |

### Stage 2: Changelog Infrastructure

| # | Increment | File | Status |
|---|-----------|------|--------|
| 02 | git-cliff config + initial CHANGELOG.md | [inc_02_git_cliff_config.md](increments/release_install/inc_02_git_cliff_config.md) | Done |
| 03 | Taskfile release commands | [inc_03_taskfile_release.md](increments/release_install/inc_03_taskfile_release.md) | Done |

### Stage 3: Commit Standards

| # | Increment | File | Status |
|---|-----------|------|--------|
| 04 | Conventional commit CI enforcement (cocogitto) | [inc_04_cocogitto_ci.md](increments/release_install/inc_04_cocogitto_ci.md) | Done |
| 05 | Dependabot conventional commit prefix + npm ecosystem | [inc_05_dependabot_config.md](increments/release_install/inc_05_dependabot_config.md) | Done |

### Stage 4: Website Changelog

| # | Increment | File | Status |
|---|-----------|------|--------|
| 06 | Website changelog page + sidebar entry | [inc_06_website_changelog.md](increments/release_install/inc_06_website_changelog.md) | Done |

### Stage 5: Install Scripts

| # | Increment | File | Status |
|---|-----------|------|--------|
| 07 | install.sh — Unix/macOS bootstrap | [inc_07_install_sh.md](increments/release_install/inc_07_install_sh.md) | Done |
| 08 | install.ps1 — Windows PowerShell bootstrap | [inc_08_install_ps1.md](increments/release_install/inc_08_install_ps1.md) | Done |
| 09 | Install script CI tests | [inc_09_install_script_ci.md](increments/release_install/inc_09_install_script_ci.md) | Done |

### Stage 6: Release Pipeline

| # | Increment | File | Status |
|---|-----------|------|--------|
| 10 | cargo-dist initialization + base release workflow | [inc_10_cargo_dist_init.md](increments/release_install/inc_10_cargo_dist_init.md) | Done |
| 11 | Release notes + version verification in release workflow | [inc_11_release_notes.md](increments/release_install/inc_11_release_notes.md) | Done |
| 12 | Post-release version bump PR automation | [inc_12_version_bump_pr.md](increments/release_install/inc_12_version_bump_pr.md) | Done |

### Stage 7: Canary & Verification Pipeline

| # | Increment | File | Status |
|---|-----------|------|--------|
| 13 | Enrich canary release body with git-cliff | [inc_13_canary_enrichment.md](increments/release_install/inc_13_canary_enrichment.md) | Done |
| 18 | Canary release workflow (dedicated, custom build matrix) | [inc_18_canary_release.md](increments/release_install/inc_18_canary_release.md) | Done |
| 19 | Verify-deep workflow (cross-platform acceptance tests) | [inc_19_verify_deep.md](increments/release_install/inc_19_verify_deep.md) | Done |

### Stage 8: Documentation

| # | Increment | File | Status |
|---|-----------|------|--------|
| 14 | Installation page (website) + README update | [inc_14_installation_docs.md](increments/release_install/inc_14_installation_docs.md) | Done |
| 15 | Website auto-deploy on release trigger | [inc_15_website_auto_deploy.md](increments/release_install/inc_15_website_auto_deploy.md) | Done |

### Stage 9: Self-Management

| # | Increment | File | Status |
|---|-----------|------|--------|
| 16 | Update check notification + OCX_NO_UPDATE_CHECK | [inc_16_update_check.md](increments/release_install/inc_16_update_check.md) | Done |
| 17 | Publish OCX to own registry (CI job + metadata) | [inc_17_publish_to_registry.md](increments/release_install/inc_17_publish_to_registry.md) | Not Started |

### Stage 10: CI Hardening

| # | Increment | File | Status |
|---|-----------|------|--------|
| 20 | Version-ahead-of-release CI check | [inc_20_version_ahead_check.md](increments/release_install/inc_20_version_ahead_check.md) | Not Started |

---

## Dependency Graph

```
Stage 1 ──→ Stage 2 ──→ Stage 3
                │
                └──→ Stage 4
                        │
Stage 5 (independent) ──┤
                        │
Stage 6 ←── Stage 1 + Stage 2
        │
        ├──→ Stage 7 (13 → 18 → 19)
        ├──→ Stage 8 ←── Stage 4 + Stage 5
        ├──→ Stage 9 ←── Stage 6
        └──→ Stage 10
```

**Critical path**: Stage 1 → Stage 2 → Stage 6 → Stage 9

**Parallelizable**:
- Stage 3 (commit standards) can run alongside Stage 4 (website changelog)
- Stage 5 (install scripts) can start after Stage 1 but before Stage 6
- Stage 7 (canary/verify), Stage 8 (docs) can run in parallel after Stage 6
- Within Stage 7: inc 18 (canary release) must complete before inc 19 (verify-deep)

---

## Key References

| Document | Purpose |
|----------|---------|
| `adr_release_install_strategy.md` | Design authority — all architectural decisions |
| `.claude/rules/release-implementation.md` | Implementation constraints and gotchas |
| `.claude/rules/documentation.md` | Website content standards |
| `.claude/rules/cli-api-patterns.md` | CLI output formatting patterns |
| `.claude/rules/cli-commands.md` | CLI command reference (update when behavior changes) |
