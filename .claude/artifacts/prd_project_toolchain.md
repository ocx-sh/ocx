# PRD: Project Toolchain Config (`ocx.toml` + `ocx.lock`)

## Overview

**Status:** Draft
**Author:** Architect worker (sion worktree)
**Date:** 2026-04-19
**Version:** 1.0
**GitHub Issue:** #33 (project-tier config walk)
**PR-FAQ:** [`pr_faq_project_toolchain.md`](./pr_faq_project_toolchain.md)
**ADR:** [`adr_project_toolchain_config.md`](./adr_project_toolchain_config.md)
**Stakeholders:** OCX maintainers, CI workflow authors, mirror catalog owners, early-access adopters

## Problem Statement

Today, using OCX to manage a project's binary toolchain requires either hand-coded `ocx install` invocations in CI scripts, or `ocx shell profile` manifests that only apply at the home tier (not per-project). There is no single, committable, reproducibility-guaranteed declaration of "these are the tools this repository needs." Every consumer re-states its toolchain in a different place, producing drift between CI, developer shells, and production build environments. Without a lock file, the best OCX can offer today is tag-based resolution, which means two developers invoking the same `ocx install cmake:3.28` on different days may receive different underlying digests as the registry tag moves.

The project-level toolchain config feature introduces `ocx.toml` (human-authored declaration) and `ocx.lock` (machine-written digest pin) to close this gap. The goal is strict reproducibility with strict opt-in state changes: the lock is always frozen, no command auto-updates it, and no shell hook auto-installs or auto-pulls.

### Evidence

**Quantitative Evidence:**

- Every repository that currently uses OCX in CI has 5–20 lines of `ocx install` or `apt-get install` glue — the aggregate drift surface is high.
- The OCX mirror catalog itself (internal dogfood) has three independent toolchain-install paths (`.github/workflows/ci.yml`, `.devcontainer/`, root `taskfile.yml`) that have drifted at least twice in the past year.
- mise.toml adoption has grown ~3× year-over-year in Rust-adjacent tooling repos (GitHub search trend, 2024–2026), indicating industry-wide demand for the exact pattern being proposed.

**Qualitative Evidence:**

- Early-access feedback (see PR-FAQ): "works on my machine" failures are the #1 reported friction.
- OCX's own CLAUDE.md pins tool versions in prose (`lychee`, `shellcheck`, `shfmt`) because there was nowhere else to put them — a dogfood tell that the feature is missing from the product.
- The config loader already documents the hook for this feature at `crates/ocx_lib/src/config/loader.rs:7`: "future tiers (e.g., the project-level `ocx.toml` walk in #33)". The design was intentional; we are executing on a two-year-old architectural commitment.

## Goals & Success Metrics

| Goal | Metric | Target |
|---|---|---|
| G1. Deterministic, reproducible tool resolution | Byte-identical `ocx.lock` output across Linux / macOS / Windows runners | 100% across all five supported platforms |
| G2. One-file onboarding | New-hire time to "first successful `make`" in an OCX-using repo | < 10 minutes (down from measured hours) |
| G3. CI script shrink | Average CI toolchain-install line count in adopter repos | < 5 lines (currently 5–20) |
| G4. Zero-surprise activation | Reported "tool mysteriously changed" incidents per month among adopters | 0 (measured via issue tracker) |
| G5. Shell hook latency | P99 time for `ocx shell hook` in steady-state (no config change) | < 5ms |
| G6. Adoption within OCX itself | OCX dogfood initiative: OCX repo's own CI uses `ocx.toml` + `ocx.lock` | Day 1 |

## User Stories

### Persona 1: CI workflow author

- **As a CI workflow author**, I want to commit one file that lists every tool my build needs, so that my GitHub Actions workflow fetches only what the repo declares — no more, no less.
  - Acceptance: `ocx.toml` in repo root + `ocx.lock` committed. CI step `ocx pull` followed by `ocx exec -- <build>` succeeds. Total CI minutes for toolchain setup is ≤ 30 seconds after object-store cache is warm.

- **As a CI workflow author**, I want my build to fail loudly when `ocx.toml` is changed without `ocx.lock` being regenerated, so that I never ship a build with a drifting toolchain.
  - Acceptance: `ocx exec` with a stale lock exits with code 65 (data error) and prints the exact `[tools]` or `[group.*]` section whose hash diverged. The failure is visible in the CI log's first line.

### Persona 2: Interactive developer

- **As a developer**, I want my shell to automatically have the right `cmake`, `shellcheck`, `shfmt` on PATH when I `cd` into my project, so that I don't have to type `ocx exec -- ...` for every command.
  - Acceptance: After running `ocx shell init bash` once and adding the emitted line to `.bashrc`, `cd` into a project with `ocx.toml` + `ocx.lock` exports the right env vars within 50ms. `cd` out of the project removes them.

- **As a developer**, I never want my shell hook to download, install, or modify anything on its own — I only ever want to export variables for tools that are already present.
  - Acceptance: An offline sandbox test of `ocx shell hook` with an empty object store emits `# ocx: <tool> not installed; run 'ocx pull'` to stderr, emits zero export lines for that tool, and makes zero network calls.

### Persona 3: Security-conscious operator

- **As an operator building a locked-down dev environment**, I want the activation mechanism to work with stock `direnv` and never phone home, so that I can audit the behavior with standard Linux tooling.
  - Acceptance: `ocx generate direnv` writes an `.envrc` of the form `eval "$(ocx --offline shell direnv)"` + `watch_file ocx.toml ocx.lock`. Running it under `strace -e trace=network -f` logs zero connect(2) calls.

### Persona 4: Mirror catalog maintainer

- **As a mirror maintainer**, I want to use `ocx.toml` to express "these are the build-time tools my mirror recipe needs" without forcing those tools into the user's home profile.
  - Acceptance: `mirrors/<name>/ocx.toml` is picked up by the nearest-only CWD walk when commands run from that directory. The user's home-tier `ocx.toml` does not contaminate the mirror recipe's toolchain.

### Persona 5: Deliberate updater

- **As a developer bumping a tool**, I want one command to re-resolve one tool's tag and rewrite only that tool's section of the lock, so that my diff is tight and reviewable.
  - Acceptance: `ocx update cmake` modifies only the `[[tool]]` entry for `cmake` in `ocx.lock` and updates `declaration_hash` + `generated_at` in `[metadata]`. All other `[[tool]]` entries are byte-identical.

### Persona 6: Legacy profile user migrating

- **As an existing OCX user with a `$OCX_HOME/profile.json`**, I want to migrate to the home-tier `ocx.toml` without losing my current set of globally-installed tools, during a visible deprecation window.
  - Acceptance: v1 release keeps `profile.json` working with a one-line deprecation note on `ocx shell profile load`. `ocx shell profile generate` (new v1 command) and a separate migration command produce an equivalent `~/.ocx/ocx.toml`. v2 release removes the `profile.json` path entirely (with at least one release of advance notice).

## Requirements

### Functional Requirements

| ID | Requirement | Priority | Notes |
|---|---|---|---|
| FR-1 | `ConfigLoader::discover_paths` walks from `ConfigInputs.cwd` upward looking for `ocx.toml`, stopping at `OCX_CEILING_PATH` or the filesystem root. Nearest-only (first match wins); does NOT chain-load parents. | Must Have | Extends existing hook at `loader.rs:103` |
| FR-2 | `ProjectConfig` schema: `[tools]` (flat name → tag string), `[group.<name>]` (same shape, additive), optional `platforms = [...]`. `#[serde(deny_unknown_fields)]` at every level. | Must Have | ADR Decision 1 |
| FR-3 | `ProjectConfig` is a separate struct from the ambient `Config`. The ambient `config.toml` files never contain `[tools]` sections; `#[serde(deny_unknown_fields)]` on `Config` rejects them. | Must Have | ADR D7/D8 |
| FR-4 | `ocx.toml` can live at home tier (`$OCX_HOME/ocx.toml`) using the identical `ProjectConfig` schema. Project `ocx.toml` shadows home-tier wholesale per group; `[tools]` merges with project winning on conflict. | Must Have | ADR Decision 6 |
| FR-5 | `ProjectLock` schema: `[metadata]` block with `lock_version` (serde_repr u8), `declaration_hash`, `generated_by`, `generated_at`. `[[tool]]` array of tables, alphabetically sorted by (name, group). Per-platform `[tool.platforms.<platform>]` with `manifest_digest = "sha256:..."` and optional `unavailable = true`. | Must Have | ADR Decision 2 |
| FR-6 | `declaration_hash` covers only canonicalized `[tools]` + `[group.*]` sections. Cosmetic changes to `ocx.toml` (whitespace, comments, key order) do not invalidate the lock. Canonicalization is locked to a JSON wire form and verified by a frozen-input test. | Must Have | ADR Decision 3 |
| FR-7 | `ocx lock [--group NAME]...` resolves every tag in scope to a manifest digest per configured platform, writes `ocx.lock` atomically (tempfile + rename + exclusive lock, mirroring `profile.rs:142-188`). Idempotent: second run on unchanged input produces byte-identical output. | Must Have | ADR Decision 4 |
| FR-8 | Platform enumeration defaults to `[linux/amd64, linux/arm64, darwin/amd64, darwin/arm64, windows/amd64]`. `ocx.toml` can override via `platforms = [...]`. Tools with no manifest for a platform are recorded with `unavailable = true`. | Must Have | ADR Decision 4 |
| FR-9 | `ocx exec` inside a directory with `ocx.toml`: if no packages are given as CLI args, resolve from `ocx.lock`. Validate `declaration_hash` against the current `ocx.toml`; on mismatch, exit with code `DataError` (65) and a diagnostic pointing at the stale section. Never call the registry at exec time. | Must Have | ADR Decision 5 |
| FR-10 | `ocx exec` with `ocx.toml` present but `ocx.lock` missing exits with `ConfigError` (78) and instructs the user to run `ocx lock`. Never silently re-resolves tags. | Must Have | Reproducibility invariant |
| FR-11 | `ocx exec --group <name>` selects the named additive group (in addition to `[tools]`). Multiple `--group` flags union the groups. Tool conflicts across active groups (same tool at different tags) are hard errors with `UsageError` (64). | Must Have | ADR Decision 1 |
| FR-12 | `ocx pull` with no args in a project directory pulls every tool from the lock for the current platform. `ocx pull --group <name>` pulls the named group additively. Uses existing `pull_all` unmodified. | Must Have | Reuses `package_manager/tasks/pull.rs:111` |
| FR-13 | `ocx update [PKG]... [--group NAME]` re-resolves specified tools (or all if none named) to current registry digests and rewrites `ocx.lock`. Only this command ever mutates `ocx.lock` (besides `ocx lock` itself). | Must Have | Opt-in state change |
| FR-14 | `ocx shell hook [--shell SHELL]` reads nearest `ocx.toml` + `ocx.lock`, diffs against the `_OCX_APPLIED` fingerprint from the environment, and emits shell-specific `export` / `unset` lines. Never pulls, never installs, never calls the registry. Emits a one-line stderr note for tools missing from the object store. | Must Have | ADR Decision 5 |
| FR-15 | `ocx shell direnv [--shell SHELL]` is the stateless variant of `shell hook` — emits exports for the nearest `ocx.toml` + `ocx.lock` without reading or writing `_OCX_APPLIED`. Same security boundary as `shell hook`. | Must Have | ADR Decision 5 |
| FR-16 | `ocx shell init <bash\|zsh\|fish\|...>` prints a shell init snippet (to stdout, one line) that the user adds to their shell config. The snippet installs the prompt hook. Reuses `ProfileBuilder` for per-shell syntax; supports the same 9 shells as `shell_profile_load`. | Must Have | ADR Decision 5 |
| FR-17 | `ocx generate direnv` writes `.envrc` in the current directory. Content: `eval "$(ocx --offline shell direnv)"\nwatch_file ocx.toml\nwatch_file ocx.lock`. Refuses to overwrite an existing `.envrc` without `--force`. | Must Have | ADR Decision 5 |
| FR-18 | `ocx shell profile load` and `ocx shell profile list` keep working but emit a one-line deprecation note pointing at `ocx.toml` + `ocx shell init`. `ocx shell profile generate [--shell SHELL]` is added in v1 as a file-writing alternative to `load`. | Must Have | ADR Decision 6 |
| FR-19 | Exit codes follow `ExitCode` enum in the CLI (per `quality-rust-exit_codes.md`): `DataError` (65) for stale lock, `ConfigError` (78) for missing lock, `NotFound` (79) for unresolvable tag or missing platform manifest, `UsageError` (64) for bad `--group` name or conflicting tools across groups, `Unavailable` (69) for registry unreachable. | Must Have | Aligns with existing taxonomy |
| FR-20 | JSON schema for `ocx.toml` + `ocx.lock` is generated by `ocx_schema` crate and published to the website for taplo auto-completion, matching the existing `config.toml` pattern. | Should Have | Reuses `ocx_schema` pipeline |
| FR-21 | `ocx.toml` accepts an explicit-digest form `cmake = "3.28@sha256:abc..."` to express the equivalent of today's `ProfileMode::Content` (pinned to digest). | Should Have | Enables profile migration |
| FR-22 | `ocx init` command scaffolds a minimal `ocx.toml` in the current directory. Prompts before overwriting. | Nice to Have | Developer onboarding |
| FR-23 | First-time `ocx lock` in a repo whose `ocx.lock` is not tracked by git emits a visible note: "Remember to commit ocx.lock — it is what makes your toolchain reproducible." | Nice to Have | Adoption guardrail |

### Non-Functional Requirements

| ID | Requirement | Target |
|---|---|---|
| NFR-1 | `ocx exec` cold-start overhead when `ocx.toml` is present | ≤ 50ms beyond today's baseline (lock parse + digest lookup, no network) |
| NFR-2 | `ocx shell hook` steady-state latency (no changes since last run) | P99 ≤ 5ms, P50 ≤ 2ms |
| NFR-3 | `ocx lock` wall time for 10 tools × 5 platforms | ≤ 10 seconds at warm HTTP cache, ≤ 30 seconds cold |
| NFR-4 | `ocx.lock` is byte-identical across platforms for the same inputs | Enforced by acceptance test on Linux / macOS / Windows CI runners |
| NFR-5 | Canonicalization algorithm is stable forever | Locked by a "known input → known hash" test with a hardcoded expected digest; any change fails loudly |
| NFR-6 | Shell hook makes zero network calls | Verified by `strace -e trace=network -f` test under `--offline` |
| NFR-7 | Shell hook modifies no filesystem state | Verified by immutable-FS sandbox test (read-only bind mount of `$OCX_HOME`) |
| NFR-8 | CWD walk bounded by `OCX_CEILING_PATH` | Walk never reads a file above the ceiling; test asserts this |
| NFR-9 | Lock file atomic write | Tempfile + rename + exclusive lock via existing pattern; crash mid-write leaves prior lock intact |
| NFR-10 | All new error types follow three-layer pattern (`Error` → contextual struct → `*ErrorKind`) per `quality-rust-errors.md` | Code-review gate |

## Scope

### In Scope

- `ocx.toml` + `ocx.lock` file formats and schemas.
- Config loader extension to walk CWD for `ocx.toml` (nearest-only, `OCX_CEILING_PATH` bound).
- Home-tier `$OCX_HOME/ocx.toml` as a second location using the same schema; project wins on conflict.
- Six new CLI commands: `ocx lock`, `ocx update`, `ocx shell hook`, `ocx shell direnv`, `ocx shell init`, `ocx shell profile generate` (+ optional `ocx init`, `ocx generate direnv`).
- `ocx exec` and `ocx pull` gain a project-config branch when invoked in a directory with `ocx.toml`.
- Shell hook security boundary: read-only, offline, no state mutation.
- JSON schema generation for `ocx.toml` + `ocx.lock`.
- Deprecation notice for `ocx shell profile {add, remove, list, load}`.
- User-guide restructure around the `ocx.toml` workflow.

### Out of Scope

- **Solver conditionals** (platform-gated tools, optional features). v1 schema is string-only; inline tables are deferred.
- **Automatic tag-update detection.** OCX never tells the user "a newer version is available." `ocx update` is explicit only.
- **Removal of `ocx install` / `ocx shell profile`** in v1. Those remain functional; deprecation is one-release minimum.
- **Shim-based activation.** Explicitly ruled out in ADR Decision 5.
- **Plugin ecosystem** (asdf-style plugins). OCX uses OCI registries directly.
- **Non-OCI registry types** in `ocx.toml` (e.g., pulling from a GitHub Releases URL). OCX's storage model is OCI-exclusive.
- **Migration of ambient `config.toml` to use the new schema.** `config.toml` and `ocx.toml` remain separate files with separate schemas.
- **Cross-project dependency resolution** (monorepo-wide lock files). Nearest-only walk; each subdirectory can have its own `ocx.toml`.

## Dependencies

| Dependency | Owner | Status | Risk |
|---|---|---|---|
| `feature/dependencies` branch landing on `main` | Core team | **Hard blocker** — must land first | High if not prioritized |
| `ConfigInputs.cwd` hook at `loader.rs:33` | Done (already wired) | Available | None |
| `pull_all` + `resolve_env` | Done (reused unmodified) | Available | None |
| `ProfileBuilder` for shell export syntax | Done (reused unmodified) | Available | None |
| Atomic write pattern (`profile.rs:142-188`) | Done (reused) | Available | None |
| `toml`, `serde`, `serde_repr`, `sha2` crates | Already in workspace | Available | None |
| JSON schema generation (`ocx_schema`) | Existing | Available | Low — may need a second schema target |
| Website / taplo integration | Existing (done for `config.toml`) | Available | Low |

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Lock file format debt (one-way door) | High | Very High | ADR commits to one-way-door discipline; `lock_version` uses `serde_repr` to reject unknown versions; canonicalization frozen by test |
| Users `.gitignore` `ocx.lock` by reflex | Medium | High | Visible first-run note; user-guide prominent "commit your ocx.lock" section |
| Prompt-hook latency degrades shell UX | Medium | Medium | `_OCX_APPLIED` fingerprint short-circuit; NFR-2 test keeps latency in check |
| Shell hook boundary weakens over time | Low | Very High | Boundary is tested (offline sandbox, read-only FS); documented in subsystem rule; any contributor weakening it must explain why in a PR-reviewed ADR amendment |
| CWD walk escapes into unrelated checkouts | Medium | Medium | `OCX_CEILING_PATH` env var gives users a hard stop; default is `$HOME` when unset; walk-never-exceeds-ceiling test enforced |
| Two different `ocx.toml` files in nested projects confuse users | Medium | Low | Nearest-only walk means "the one you cd'd into wins"; behavior is predictable even if occasionally surprising |
| Canonicalization bug causes false-positive staleness | Low | High | Frozen test + JSON wire form (not TOML) to isolate from TOML-library drift |
| Dogfooding reveals schema limitations late | Medium | Medium | OCX's own repo is the first adopter (G6); schema issues surface in v1 review, not at external launch |
| `feature/dependencies` delayed | Medium | High | Blocker tracked; if branch slips, this feature slips in lockstep — we do not ship without transitive deps |

## Open Questions

- [ ] **Migration helper shape.** Should `ocx profile export --format ocx-toml > ~/.ocx/ocx.toml` be a new command, or should `ocx shell profile generate --as-ocx-toml` serve? Preference: dedicated `ocx profile export` since the scope is migration-only.
- [ ] **`ocx init` scope.** Should it create just `ocx.toml`, or also run `ocx lock` immediately? Preference: just `ocx.toml`; separate `ocx lock` run makes the two-step commit model obvious.
- [ ] **`.envrc` watch_file vs direnv's native `use ocx`** (a potential future direnv plugin). In v1 we ship plain `watch_file` to avoid a direnv-plugin dependency; a `use ocx` stdlib function can be added later.
- [ ] **Home-tier group precedence.** If home `[group.dev]` contains `shfmt` and project `[group.dev]` contains `shellcheck`, does the project group shadow wholesale or merge keys? ADR chose wholesale shadow; user test may reveal merge is more intuitive.
- [ ] **Offline `ocx lock`.** Should `ocx lock` work with an offline index? Today `pull_all` requires a registry; the index-based resolve may let us produce a lock from a pre-synced index. Preference: support it, since it matches the OCX backend-tool mindset.

## Appendix

### Mockups/Wireframes

Example `ocx.toml`:

```toml
# ocx.toml at repo root — committed to Git
[tools]
cmake = "3.28"
shellcheck = "0.11"
shfmt = "3"

[group.ci]
lychee = "0"

[group.release]
sbom = "1"
```

Example `ocx.lock` (abbreviated):

```toml
# ocx.lock — machine-written; commit to Git
[metadata]
lock_version = 1
declaration_hash = "sha256:a1b2c3..."
generated_by = "ocx 0.34.0"
generated_at = "2026-04-19T10:30:00Z"

[[tool]]
name = "cmake"
tag = "3.28"
group = "default"
index = "ocx.sh"

[tool.platforms]
linux-amd64   = { manifest_digest = "sha256:111..." }
linux-arm64   = { manifest_digest = "sha256:222..." }
darwin-amd64  = { manifest_digest = "sha256:333..." }
darwin-arm64  = { manifest_digest = "sha256:444..." }
windows-amd64 = { manifest_digest = "sha256:555..." }

[[tool]]
name = "lychee"
tag = "0"
group = "ci"
index = "ocx.sh"
# ... platforms ...
```

### Research

- See synthesized findings in ADR "Industry Context & Research" section.
- Cargo.lock shape (array of tables): https://doc.rust-lang.org/cargo/guide/cargo-toml-vs-cargo-lock.html
- mise.lock per-platform sub-tables: https://mise.jdx.dev/lockfile.html
- PDM content_hash: https://pdm-project.org/latest/usage/lockfile/
- direnv hook contract: https://direnv.net/docs/hook.html

---

## Approval

| Role | Name | Date | Status |
|---|---|---|---|
| Product | | | Pending |
| Engineering | | | Pending |
| Design | N/A — no UI | | — |

---

## Next Steps & Handoffs

After PRD approval:

1. [x] **Architect Review**: Technical feasibility assessed in ADR
   - Output: [`adr_project_toolchain_config.md`](./adr_project_toolchain_config.md)

2. [ ] **Engineering Estimate**: Decompose into phase-level GitHub issues
   - Trigger: `/swarm-plan` (large scope, 6+ weeks)
   - Output: Implementation Plan (`.claude/state/plans/plan_project_toolchain.md`)

3. [ ] **Create GitHub Issues**: Decompose into trackable work items
   - Parent: #33 (existing)
   - Per-phase child issues, starting with "Phase 1: Loader extension"

4. [ ] **Hard prerequisite**: Land `feature/dependencies` on main before starting Phase 1.

**Related Artifacts**:

- ADR: [`adr_project_toolchain_config.md`](./adr_project_toolchain_config.md)
- PR-FAQ: [`pr_faq_project_toolchain.md`](./pr_faq_project_toolchain.md)
- Implementation Plan: to be authored after `feature/dependencies` lands

---

## Version History

| Version | Date | Author | Changes |
|---|---|---|---|
| 1.0 | 2026-04-19 | Architect worker (sion) | Initial draft — one-way-door feature design |
