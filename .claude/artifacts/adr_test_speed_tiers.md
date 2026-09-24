# ADR: Tiered, cache-driven verification for the AI implementation loop

## Metadata

**Status:** Accepted (2026-09-23 — owner decision recorded at execution start: accept on Stage 5 landing; Amendment 2026-09-22 AM-1 … AM-8 binding; Amendment 2026-09-23 AM-9 … AM-12 proposed)
**Date:** 2026-09-22
**Deciders:** Michael Herwig (owner), architect (hex-architect Phase 4, revised Phase 5 round 1)
**Beads Issue:** N/A
**Related PRD:** N/A — input is the discussion dossier `.agents/discussions/test-suite-speed-tiers.md`
**Tech Strategy Alignment:**
- [x] Decision follows Golden Path in `.claude/rules/product-tech-strategy.md` (Rust/Tokio, Python/pytest/uv, Bazel as adopted by `adr_bazel_build_adoption.md`, Taskfile as entry point)
- [ ] Deviation — none. The strategy file's `Linker | Mold (dev)` row is stale (see § Follow-ups); this ADR does not act on it.
**Domain Tags:** devops | infrastructure | security
**Supersedes:** nothing. **Amends:** `adr_bazel_build_adoption.md` § Stage 4 (acceptance verdicts stay local-cache only; under-declared modules run uncached; sweep `extra_data` removed; `exclusive` dropped by AM-9), `adr_crate_split_workspace.md` § "Verification tiers" (the phase-1 `ocx` escalation in `scripts/scoped_gate.py` `TABLE_ESCALATES`; merge commits need a full mark).
**Superseded By:** —

**Blast radius:** cross-area — `crates/ocx_cli/build.rs`, `Cargo.toml` (one named profile), `scripts/scoped_gate.py`, `scripts/commit_gate.py`, `scripts/bazel_tag_guard.py`, `scripts/test_diff_guard.py`, `crates/ocx_schema/tests/schema_outputs.rs`, `test/taskfile.yml`, `taskfile.yml`, `test/BUILD.bazel`, `test/bazel.bzl`, `dist-workspace.toml` (regenerated `release.yml`), `.claude/` agent, rule and skill text, telemetry.

**Reversibility, per part:**

| Part | Door | Undo |
|---|---|---|
| D1 provenance placeholders under `__testing` + release scan | two-way | revert the `build.rs` branch and the scan job |
| D2 lint tier | two-way | `git mv` the modules back, restore `extra_data`, raise `SUITE_FLOOR` |
| D3 inner-loop composition, verb markers, security escalation | two-way | restore `TABLE_ESCALATES = {ocx_test_support, ocx}` |
| D4 full acceptance at WP merge (mechanical) and finalize | two-way | drop the merge-commit clause from `commit_gate.py` |
| D5 agent-gate wording | two-way | text |
| D6 acceptance verdicts: local disk cache only; under-declared modules uncached | two-way | drop a module's `external` tag and its `UNCACHED_MODULES` entry (each is a declared-input fix, not a revert) |
| D7 port-down: deleting acceptance originals | **one-way (medium)** | a deleted case is recoverable from git, but the knowledge of *why* it was e2e is not; gated behind the pilot go/no-go |

**Required artifacts:** this ADR; `.claude/artifacts/system_design_test_tiers.md`; `.claude/artifacts/research_test_tier_tooling.md`, `.claude/artifacts/research_test_tier_patterns.md`, `.claude/artifacts/research_test_tier_operability.md`.

## Context

An AI work package (WP) today waits tens of minutes for gate feedback. Three measured causes, and one found while writing this ADR:

1. **The acceptance binary churns every commit.** `crates/ocx_cli/build.rs` bakes `describe` (SHA + `-dirty`) and the commit timestamp unconditionally, a build timestamp when `CI` is set, and `GITHUB_RUN_ID`/`GITHUB_SHA`/`GITHUB_REF`/`GITHUB_WORKFLOW`/`GITHUB_SERVER_URL`/`GITHUB_REPOSITORY` whenever present. Every acceptance `sh_test` takes `test/bin/ocx*` as a content-hashed input (`test/BUILD.bazel` `:suite_anchor`), so a new commit — even docs-only, even a clean/dirty flip — re-executes all 181 targets on the local disk cache: 1421 s cold, serial, against 160 s for `task test:parallel` (DX-97, `research_test_tier_operability.md` § 1b).
2. **The CLI crate always escalates.** `scripts/scoped_gate.py:159` puts `ocx` in `TABLE_ESCALATES` "for phase 1 only", so any edit under `crates/ocx_cli/` runs the full `task verify`. Every verb lives there.
3. **Agent config demands the full gate per task.** `.claude/skills/builder/SKILL.md:25,55`, `.claude/agents/worker-builder.md:44`, `.claude/agents/worker-tester.md:83`, `.claude/skills/deps/SKILL.md:17,41`, `.claude/rules/workflow-swarm.md:214` require unscoped `task verify`; `.claude/rules/subsystem-{cli.md:414,oci.md:667,package-manager.md:252,package.md:165}` and `subsystem-taskfiles.md:70` say "Full `task verify` = final gate before commit". Both contradict `CLAUDE.md` (per-WP `verify:scoped`).
4. **(New, verified here) the scoped arm skips reverse dependents.** `taskfile.yml:249-250` runs `bazel test //crates/<changed>:all` per changed crate only, bypassing `bazel:test:unit`'s per-target reader floor. A change to `ocx_setup` never runs `ocx_cli`'s four `rust_test` targets in the scoped arm.

**Two corrections to the dossier.** (a) Provenance explains the **acceptance lane's** misses, not the instance-wide ~1.5 % AC hit rate: the Bazel build of `ocx_cli` runs no build script (`crates/ocx_cli/BUILD.bazel` is a plain `rust_library`; no `cargo_build_script` anywhere in `*.bazel`/`*.bzl`). (b) **No remote writer for acceptance verdicts exists.** The acceptance job in `verify-deep.yml:425-459` holds the read credential only; the only upload is `verify-basic.yml:446`, on the `main` push, for `//crates/...`; `test/bazel.bzl:146-151` states "None of these results reaches another machine". Every CI acceptance run starts on a fresh runner disk and is therefore uncached. Acceptance caching pays **only on a persistent local disk** — agent and developer machines. Exit criteria below measure exactly that.

## Decision Drivers

- **Latency of the inner loop** — target under 2 min warm for a one-crate Rust edit, measured on the first verification after the edit (dossier Verification).
- **The pre-merge gate stays full and mechanically enforced.** Every mature tiered system backstops a narrow tier with a full tier someone gates on (`research_test_tier_patterns.md` § 1, Chromium).
- **Cache correctness over cache hit rate.** An undeclared input turns a cache into a stale-green generator (`bazel#3856`; `test/bazel.bzl` § residual under-declaration).
- **Agent cost** — porting is non-mechanical Opus work; a full port is 45-90+ agent-hours (ESTIMATED, `research_test_tier_operability.md` § 3).
- **No second source of truth** — rejects cucumber-rs and any dual-suite end state.

## Industry Context & Research

**Research artifacts:** `.claude/artifacts/research_test_tier_tooling.md`, `.claude/artifacts/research_test_tier_patterns.md`, `.claude/artifacts/research_test_tier_operability.md`; prior: `.claude/artifacts/research_bazel_cache_trust_boundary.md` (findings #4, #10, #24), `.claude/artifacts/research_test_impact_adjacent.md`, `.claude/artifacts/research_crate_split_verification_tiers.md`.

**Trending approaches:** inner/outer loop split by dependence on external resources (Augment); presubmit/postsubmit with a monitored slow tier (Google TAP; Chromium CQ as counter-example); merge-queue gating on the prospective merge (bors `try`/`auto`, cargo#14718); provenance kept out of dev/test cache keys and stamped only for release (vergen `VERGEN_IDEMPOTENT` + `SOURCE_DATE_EPOCH`; Bazel stable/volatile status).

**Key insights driving this decision:**
1. The provenance split is the one pattern every source agrees on (`research_test_tier_tooling.md` § Direct Answer 3).
2. Cargo spawns itself per test because in-process re-entry of env, cwd and registry state is unsafe (`cargo-test-support`). A `rust_test` that spawns the binary gains no crate-level cache precision.
3. A `rust_test` placed in `ocx_cli` gains speed but not cache precision: `ocx_cli` links all 16 tier crates, so any Rust change re-runs it. Precision comes only from tests in the lowest owning crate.
4. Command-to-test mapping is cheap and trusted nowhere alone; the backstop is a full run on the protected path (`research_test_impact_adjacent.md` finding 8).
5. No source measures escape rates for agent-specific narrow gates; instrument it (`research_test_tier_patterns.md` § 2).

## Considered Options

### Option A: Tiered gates + provenance placeholders + lint tier + mechanical T2; port-down gated by a pilot

D1-D7 as specified below.

| Pros | Cons |
|---|---|
| Fixes the measured churn at its source, one build-script branch | Two build systems still compile the changed crate in T1 (cargo test binary + Bazel `rust_test`); Stage 0 levers target it, and Option C is the named exit if they fail |
| T2 at WP merge enforced by `commit_gate.py`, not prose | More moving parts than D+: lint tier, escape records, security list |
| Under-declared readers run uncached — no stale green from them | Verb markers are one more hand-maintained mapping; mitigated by the coverage guard and review-with-the-test |
| Port-down effort spent only where the pilot proves it pays | Leaves most e2e cases e2e by design |

### Option B: Option A without the pilot gate

Same contracts as A — ported cases also land in the lowest owning crate — but the classification is followed directly by Sonnet/Opus porting waves over every port-capable case, as the dossier ratified.

| Pros | Cons |
|---|---|
| Maximises fast, crate-cached Rust coverage | 45-90+ agent-hours committed before any number says the ports pay |
| One decision, no GO/NO-GO step | Two test languages and fixture stacks live for the whole window; a stalled partial migration is worse than either endpoint |
| | Seam work (C-SEAM) lands before its value is known: 87 ambient env reads across 46 files, plus third-party env reads |

### Option C: Bazel-native `rust_binary` for `ocx`, provenance via constant `rustc_env`, acceptance depending on it by label

| Pros | Cons |
|---|---|
| **Removes the two-build-system cost**: T1 compiles the changed crate once, and the acceptance binary shares the Bazel action cache with `rust_test` | `__testing` must reach 9 crates via `crate_features` → a second configured copy of those libraries (transition or duplicate targets) |
| Acceptance gains a graph edge from Rust, so `rdeps` sees it | `ocx-shim` and the mirror binary need the same treatment; rules_rust vs cargo byte drift becomes a new failure mode |
| No build script, no stamping: finding #24's volatile-status trap does not apply | Larger, unscoped change; its latency advantage is unmeasured until Stage 0 publishes the breakdown |

### Option D+: D1 + D3 + D5 only (provenance, routing, agent wording)

| Pros | Cons |
|---|---|
| Captures most of A's latency gain for less work | Stale green stays possible: seven undeclared cross-package readers and five `extra_data` sweepers remain cached |
| All three parts two-way | T2 at WP merge stays prose; no escape measurement, so a narrow tier's leak is invisible |
| | No security escalation list: sign/verify verbs and `.github/**` could route narrow once `ocx` leaves `TABLE_ESCALATES` |

### Option D: Status quo + agent-config fix only

| Pros | Cons |
|---|---|
| Cheapest; D5 alone removes the contradiction | Every Rust commit still re-executes 181 acceptance targets locally; CLI edits still escalate — fails the Intent |

### Rejected without scoring

- **cucumber-rs / Gherkin** — changes how tests are written, not what they execute; adds a second source of truth.
- **In-memory fake OCI registry** — owns OCI wire semantics; Block-tier under `quality-core.md` "Don't Own Non-Domain Code".
- **bazel-diff / target-determinator** — needed only once in-repo caching falls short; `bazel test //crates/...` with result caching already selects reverse dependents.
- **A remote writer for acceptance verdicts** — out of scope; see D6.
- **mold** — not pursued: rust-lld is already the default linker for `x86_64-unknown-linux-gnu` since Rust 1.90, and this repo pins 1.95.0 (`rust-toolchain.toml`); no `.cargo/config.toml` overrides it.

### Trade-off matrix

Scores 1 (worst) – 5 (best). Re-scored in round 1.

| Criterion | Weight | A | B (A − pilot) | C | D+ | D |
|---|---|---|---|---|---|---|
| Inner-loop latency | 0.30 | 4 | 4 | 5 | 4 | 1 |
| Gate strength / escape risk | 0.25 | 5 | 3 | 4 | 3 | 4 |
| Implementation + agent cost | 0.20 | 3 | 1 | 2 | 4 | 5 |
| Operability / maintenance | 0.15 | 4 | 2 | 3 | 4 | 4 |
| Reversibility | 0.10 | 4 | 2 | 3 | 5 | 5 |
| **Weighted** | | **4.05** | **2.65** | **3.65** | **3.85** | **3.40** |

Sensitivity: A leads D+ by 0.20 = +0.50 on gate strength (uncached under-declared readers, mechanical T2, security escalation) offset by −0.20 on cost and −0.10 on reversibility. A leads C by 0.40 = +0.70 on gate strength, cost, operability and reversibility, minus 0.30 on latency; C's latency score is provisional — if Stage 0 shows the cargo test-binary build dominates T1 and the levers fail, C is the amendment candidate (§ Implementation Plan, Stage 0).

## Decision Outcome

**Chosen Option:** A.

**Rationale:** it removes the measured churn (D1) and the unconditional CLI escalation (D3) at two-way cost, keeps the full gate blocking *and mechanically enforced* (D4), closes the stale-green path instead of monitoring it (D6), and converts the port-down from a commitment into an experiment with a number attached. The pilot gate is what separates A from B: in-process hazards are already visible — `ocx_cli_test` runs `--test-threads=1` because 25 testcases mutate the process environment, and skips 8 that need a process of their own (`crates/ocx_cli/BUILD.bazel:54-87`). **This disagrees with the dossier decision "Port-down: a dedicated migration"; see Open Question 1.**

### Decisions

- **D1 — Placeholder provenance under `__testing`**, plus a release byte scan of every staged binary before upload. Contract C-PROV.
- **D2 — Lint tier.** Structural sweeps leave the acceptance suite for `test/lint/`, uncached, in verify phase 1 and every `verify:scoped`. Contract C-LINT.
- **D3 — Inner loop** = `task bazel:test:unit` (cached, reverse dependents by construction, reader floor kept) + per-crate clippy + lint tier + `test:smoke` + `test:scoped` over the selected globs. `ocx` leaves `TABLE_ESCALATES`; shared CLI code, hubs, ecosystem crates and the security list still escalate. Contract C-ROWS.
- **D4 — Full acceptance at WP merge and finalize, enforced by `commit_gate.py`**: a merge commit needs a `full` mark whose tree digest equals the tree being committed. Contract C-TIER.
- **D5 — Agent gate wording** swept across `.claude/rules/**`, `.claude/agents/**`, `.claude/skills/**`. Contract C-AGENT.
- **D6 — Acceptance verdicts are cached on the local disk only.** No remote writer for acceptance in this ADR. Modules with undeclared inputs carry the `external` tag — the only tag measured to stop a test result being reused (WP-35, `scripts/bazel_tag_guard.py:46-53`) — until declared. `release:` runs acceptance with `NOCACHE=1`. A remote writer is **out of scope** and needs its own ruled decision with these preconditions: `--test_env=K=V` pinning of every `env_inherit` name that changes a verdict (`CI`, `__OCX_TESTING_REQUIRE_ENVIRONMENT_D`), an empty under-declared list, credential-scope tests in `.claude/tests/test_workflows.py` for the new writer, and a `/security-auditor` review.
- **D7 — Port-down** = classification of all modules (read-only) + a pilot; wide waves only on GO. Contracts C-SEAM, C-RUBRIC.

### Quantified Impact

"Before" cells are measured; "After" cells are targets measured at stage exit.

| Metric | Before (measured) | After (target) | Source |
|---|---|---|---|
| Acceptance targets executed locally, warm disk cache, docs-only commit | 181 (describe/commit timestamp change) | 0 | DX-97, Stage 1 |
| Acceptance targets executed locally, clean → dirty tree with no code change | 181 (`-dirty`) | 0 | Stage 1 |
| Acceptance targets executed locally, unchanged tree re-run | 0 | 0 | CLAUDE.md § Bazel |
| Acceptance targets executed in CI (any trigger) | 181 (fresh runner, no remote writer) | 181 — **unchanged, by D6** | `verify-deep.yml:425-459` |
| Acceptance targets executed, `test/tests/test_x.py` edit | 5 (itself + 4 sweepers) | 1, plus every module on the uncached list | `test/BUILD.bazel:237-249` |
| `test/bin/ocx` sha256 across two commits differing only in docs | differs | equal | dossier Verification |
| T1 wall clock, reference edit, first verification after the edit | escalates to full `task verify` for `ocx`; unmeasured for leaf crates | ≤ 120 s | Stage 0 breakdown |
| CI minutes per PR push | ≈ 18 (basic) | ≈ 18 (basic tier not in scope) | `subsystem-ci.md` |

### Consequences

**Positive:**
- A Rust edit to a non-hub, non-security crate no longer runs 181 serial acceptance targets inside the loop.
- A new commit that does not change code — docs, clean/dirty flip — reuses every local acceptance verdict.
- Sweep modules stop re-keying siblings; under-declared readers stop producing cached verdicts.
- The scoped arm runs reverse-dependent `rust_test` targets with the reader floor.
- T2 at WP merge becomes a refusal a script issues, not a sentence an agent may skip.

**Negative:**
- The acceptance binary reports placeholder provenance; bugs that only real values trigger (long `describe`, unusual characters in refs) are caught by the release scan and check, not the suite. Accepted by the owner.
- CI acceptance stays uncached (D6). Re-dispatch and flaky retries in CI pay the full run.
- Modules on the uncached list re-run in every T2 until their inputs are declared.
- `bazel:test:accept` stays serial at T2: a WP merge that changed the binary pays the cold 1421 s path. See Open Question 2. *(Superseded by AM-9: concurrent, 283 s cold.)*

**Risks:**
- *Stale local green via an under-declared input.* Mitigation: D6 uncached list; tag-guard clause; `release:` NOCACHE; `verify-deep.yml` on `main` is cold by construction.
- *Escape through the narrow tier.* Mitigation: C-ESC records and the mechanical T2.
- *T1 misses its budget because two build systems compile the edit.* Mitigation: Stage 0 levers; failure is an ADR amendment (Option C), not a quiet re-baseline.

## Technical Details

### Architecture

Full C4 and data flow: `.claude/artifacts/system_design_test_tiers.md`.

```
 agent edit ─▶ task verify:scoped --force ─▶ scripts/scoped_gate.py --plan
                     │                          │ decision: routed | scoped | escalate
                     │                          ▼
   T0 lint ◀─────────┤   T1 inner: task bazel:test:unit (cached, floored) + clippy
                     │             + test:smoke + test:scoped <selected globs>
                     │
 WP merge commit ──▶ commit_gate.py: needs full mark, tree == merged tree
                     └─ T2: task verify (full; acceptance on local disk cache,
                            uncached list always executes)
 release:         ──▶ T2 with bazel:test:accept NOCACHE=1
 push to main     ──▶ T3: verify-deep.yml (acceptance cold on a fresh runner)
```

### API Contract

Component contracts below replace the template's single API contract: this ADR has no network API, and each contract names a check that can go red.

#### C-TIER — tier definitions and enforcement

| Tier | Runs | Trigger | Budget | Cache key inputs |
|---|---|---|---|---|
| **T0 lint** | `task test:lint:structure` (pytest over `test/lint/`, `OCX_TESTS_NO_REGISTRY=1`) + `scripts/scoped_gate.py --check-coverage` | verify phase 1; every `verify:scoped` regardless of decision | ≤ 30 s wall, gated (C-LINT) | none — uncached; reads the working tree |
| **T1 inner** | `task bazel:test:unit`; `cargo clippy -p <crate>` per changed crate; T0; `test:smoke`; `test:scoped` over `plan.acceptance_globs` | `task verify:scoped --force`, per WP iteration | ≤ 120 s, first verification after a code-changing edit (Stage 0) | `rust_test`: sources + transitive rlibs + toolchain + declared data; pytest legs uncached |
| **T2 full** | `task verify` (both phases, incl. `bazel:test:accept`) | WP merge commit (enforced), `/hex-finalize`, any escalation, commits on `main` | unbounded; measured | acceptance: module + `:suite_inputs` (incl. `bin/ocx*` digest); modules on the uncached list: none (`external`) |
| **T2 release** | `task verify` with `bazel:test:accept NOCACHE=1` | `release:` commits, `task release:prepare` | unbounded | none |
| **T3 deep** | `verify-deep.yml` | push to `main`, merge queue, dispatch | CI | cold: fresh runner disk, remote holds no acceptance results |

**Enforcement.** `scripts/commit_gate.py` gains one clause: a commit made while `MERGE_HEAD` exists (a WP merge: `git merge --no-ff --no-commit`, then `task verify`, then `git commit`, which runs `commit-msg`) needs a `full` mark whose `tree` equals `git write-tree` of the index being committed. The mark gains `tree` (§ Data model), written by `task verify` with the **same definition** — `git write-tree` of the repository's real index at mark time — so an unstaged or untracked file changes neither side and cannot falsely refuse a merge commit. After `git merge --no-commit` the index is the merged tree. `--self-test` shows: merge commit + scoped mark → red; + full mark for another tree → red; + full mark for this tree → green; ordinary commit + scoped mark → green (unchanged). Rebase stays exempt as today. Residual escape, named: a fast-forward WP merge creates no commit and bypasses the clause; hex-execute's merge step uses `--no-ff`, and the full mark on `main` and at finalize remains the backstop.

Invariant: a scoped mark never satisfies `commit_gate.py` for a `release:` commit, a commit on `main`, or a merge commit.

Reference edit (for every latency number): a one-line change to emitted code in `crates/ocx_setup/src/` (the documented leaf with 10 `rust_test` targets in its reverse closure, `adr_bazel_build_adoption.md` DX-18; the change must alter emitted code, DX-37).

#### C-PROV — `build.rs` provenance under `__testing`

Predicate: `std::env::var_os("CARGO_FEATURE___TESTING").is_some()` — set when package `ocx` is built with `--features ocx/__testing`, the only way `test/taskfile.yml:163` builds the acceptance binary.

When the predicate holds, `build.rs`:
1. Does not invoke `GixBuilder` — no `rerun-if-changed` on `.git/`, no git state read.
2. Does not read `CI` or any `GITHUB_*` variable, and emits no `rerun-if-env-changed` for them.
3. Keeps `CargoBuilder` (`target_triple`, `debug`) and `RustcBuilder` (`semver`) — toolchain-derived, already in every cache key.
4. Emits exactly these `cargo:rustc-env` lines from one constant table:

| Key | Release / non-`__testing` (unchanged) | `__testing` placeholder |
|---|---|---|
| `VERGEN_GIT_SHA` | gix | `0000000000000000000000000000000000000000` |
| `VERGEN_GIT_DESCRIBE` | gix describe | `placeholder-g00000000` |
| `VERGEN_GIT_DIRTY` | gix | `true` |
| `VERGEN_GIT_COMMIT_TIMESTAMP` | gix | `1970-01-01T00:00:00.000000000Z` |
| `VERGEN_BUILD_TIMESTAMP` | now, only under `CI` | `1970-01-01T00:00:00.000000000Z` |
| `GITHUB_SERVER_URL` | pass-through | `https://ci.invalid` |
| `GITHUB_REPOSITORY` | pass-through | `placeholder/placeholder` |
| `GITHUB_RUN_ID` | pass-through | `0` |
| `GITHUB_WORKFLOW` | pass-through | `placeholder` |
| `GITHUB_REF` | pass-through | `refs/heads/placeholder` |
| `GITHUB_SHA` | pass-through | `0000000000000000000000000000000000000000` |
| `__OCX_BUILD_CHANNEL` | pass-through | `test` |
| `__OCX_BUILD_VERSION` | pass-through | **pass-through (unchanged)** — feeds `app::version::version()`, lock `generated_by` and update-check semver parsing; no test build sets it |

5. Emits `cargo:rerun-if-changed=build.rs`.

`Provenance::current()` then returns `commit`, `build`, `ci` and `channel` all `Some` — the release code path with fixed literals. No placeholder is claimed impossible; each is **detectable**: string fields carry a marker (`placeholder-g00000000`, `ci.invalid`, `placeholder/placeholder`), the SHA is all-zero (caught by `--exec`, not the scan), and `dirty = true` fails the release check's `dirty == false`.

**Release guard.** `scripts/release_provenance_check.py`:
- `--scan <file>...` — byte scan of every staged binary for the **three string markers only**; any hit reds. Runs on every target's artifact, including those the runner cannot execute. Fail-closed on a false positive. The all-zero SHA is deliberately not scanned: dependency code already puts a 40-zero run (gix's null object id) and longer zero runs into `test/bin/ocx`, so it would red every real release.
- `--exec <binary>` — only for a binary native to the runner: `<binary> --format json version` must show `channel != "test"`, a 40-hex non-zero `commit.sha`, `commit.dirty == false`, `ci.run_url` under `https://github.com/ocx-sh/ocx/actions/runs/<digits>`, a non-epoch `build.timestamp`. No other target is executed, and nothing here claims to.
- `--self-test` — red on a placeholder fixture for each marker, on a fixture missing `ci`; green on a **real release-built binary** (`cargo build --release -p ocx`, no `__testing`), never a synthetic blob — a synthetic green cannot show that dependency bytes stay clear of the markers.

Wiring: a release-workflow job that the `host` job (`release.yml:240`, which uploads the GitHub Release at `:273`) `needs`, so the scan runs before **both** publish paths (GitHub Release, then `custom-post-release-oci-publish` at `:323`). `release.yml` is cargo-dist-generated, so the job is added through `dist-workspace.toml` and the file regenerated — never hand-edited. **Stage 1 exit requires showing, in the regenerated `release.yml`, that `host` needs the scan job**; if cargo-dist offers no pre-`host` hook, Stage 1 fails and the ADR is amended. `.github/workflows/oci-publish.yml:165`'s bare `ocx version` becomes `--exec` on that native binary; `deploy-dev.yml` gets `--scan` over its staged artifacts. Existing guard `release_feature_set_excludes_testing_seams` stays.

Acceptance-side proof: one new case asserts the exact placeholder JSON for `commit`, `ci`, `channel` and `build.timestamp`, and the **exact key set** of the version report, so a new provenance key reds until it gets a placeholder. `test_config.py`, `test_config_test.py`, `test_install_libc.py`, `test_self_update.py`, `test_update_check_throttle.py` (`research_test_tier_operability.md` § 2) are re-read at Stage 1; each either tolerates placeholders or its real-value assertion moves to the release check. There is no second test binary.

#### C-LINT — lint-tier target contract

- **Home:** `test/lint/test_*.py`, outside `test/BUILD.bazel`'s `tests/test_*.py` glob — no acceptance target, no `extra_data`, no Bazel cache.
- **Admission, enforced by `test/lint/conftest.py`:** fails any test requesting `ocx`, `ocx_binary`, `registry`, `mirror_registry`, `legacy_registry`, `published_package` or `sigstore_stack`; and wraps `subprocess` so an argv whose executable resolves under `test/bin/` or is `task` raises. `--self-test` style proof: a fixture test for each forbidden route reds.
- **Membership (explicit; Stage 2 re-verifies each against the conftest):**

| Module | Moves |
|---|---|
| `test_smoke_coverage.py` | whole |
| `test_no_crate_path_assertions.py` | whole |
| `test_deprecated_spellings.py` | whole |
| `test_patch_global_slot.py` | whole (source-only sweep); if Stage 2 finds a binary case, that case is split out — no module keeps `extra_data` |
| `test_doc_scripts_publish.py` | sweep and pure-render cases (`pt6_no_test_path_literal…`, `pt6_publish_task_consumes…`, `pt7_…`, `pt8_…`, `pt2_nested_slug_*`, `rn*`, `test_substitute_renderable_parity`, `test_display_is_substring…`); the cases that invoke `task` (`pt1`, `pt3`, `pt4`, `pt5*`, `pt6_list_task…`, `pt2_nested_publish…`) stay in acceptance and must not sweep |
| `test_logging.py` | only `test_every_crate_that_logs_has_a_row` (`:335`); `:356` and `:372` use `ocx`/`published_package` and stay |
| `test_shell_reconcile_edge_cases.py` | its 3 sweep cases; the module's `_SWEEP_SHELL` read goes with them |
| `test_doc_binding.py`, `test_doc_scripts_parser.py`, `test_doc_command_reference.py`, `test_doc_project_toolchain.py` | candidates (read `website/**`/`crates/**`, no binary fixture found by grep); Stage 2 confirms or leaves them on the uncached list |

  The dossier's "11 pure sweep modules" count is replaced by this table; Stage 2 publishes the final list in the move commit.
- **Inputs:** the working tree, via `git ls-files --cached --others --exclude-standard` or explicit repo paths. Uncached, so no stale verdict.
- **Reader floor:** `test/LINT_FLOOR` (collected count), as `cmds:`, never `preconditions:`. The `--tiered-shapes` diff guard refuses any decrease of it: no shape moves a test out of `test/lint/`.
- **Skip/xfail ceilings:** `test/LINT_SKIP_CEILING` and `test/LINT_XFAIL_CEILING`, read by `.suite-ceilings` off the lint run's summary line — the acceptance tier's pattern, so a skip added to dodge a red is caught where the collected floor cannot see it.
- **Budget:** `task test:lint:structure` fails above 30 s wall; `--self-test` reds a fixture run with a sleeping test. Measured at Stage 2 exit.
- **Where it runs:** `.verify:lint` phase 1, every `verify:scoped` (all three decisions), `verify-basic.yml`'s `smoke` job via the same task.
- **Graph invariant after the move:** `test/BUILD.bazel` `extra_data` is empty and `scripts/bazel_tag_guard.py`'s sweeper clause requires the sweeping set to be empty.
- **Each moved case keeps a proven red:** one mutation per module, shown red and green in the move commit's evidence.
- `test/SUITE_FLOOR` lowered by exactly the collected count moved, same commit; `test/LINT_FLOOR` created with the same count.

#### C-UNCACHED — under-declared acceptance modules (D6)

- `test/bazel.bzl` carries `UNCACHED_MODULES`: every acceptance module whose source names an undeclared root (`target/`, `website/`, `crates/`, `.github/`, `test/doc_scripts/`) — today at least `test_schema_generation.py` (`SCHEMA_BINARY = PROJECT_ROOT / "target" / "release" / "ocx_schema"`, `:52`) and whatever D2 does not move. Those targets carry **`external`** — the one tag WP-35 measured to stop a test result being reused (`scripts/bazel_tag_guard.py:46-53`, `TAG_INSUFFICIENT_TEST_MSG` `:403`); `no-cache` still reports `(cached)` on a second run and is not used.
- `scripts/bazel_tag_guard.py` is amended: on acceptance targets `external` is admitted **exactly** on `UNCACHED_MODULES`; a module naming an undeclared root and absent from the list reds; a listed module that no longer names one reds (so the list cannot rot). BZL-CORE-01 note: `external` on a *test* action strengthens the gate (the verdict is never reused), so it is not the weakening BZL-CORE-01 forbids; the guard keeps redding every other cache-suppressing tag, and `no-cache`/`local` stay refused as insufficient.
- The list shrinks by declaring inputs (an exported filegroup from the owning package) or moving the module. **It must be empty as the exit of Stage 5.** Named resolution for `test_schema_generation.py`, the one module whose undeclared input is a *build product* (`target/release/ocx_schema`, built by cargo): `crates/ocx_schema/BUILD.bazel` has no binary target today (only `rust_library` over `src/main.rs` + `src/lib.rs`, and `rust_test`s), and its docstring says its assertions mirror `crates/ocx_schema/tests/schema_outputs.rs`. Stage 5 ports any assertion `schema_outputs` lacks into that `rust_test` and deletes the module under the C-RUBRIC deletion guard. Fallback if a case cannot port: add a `rust_binary` for `ocx_schema` and give the `sh_test` that label as `data`. No permanent residue under `external`.

#### C-ROWS — scoped rows, verb markers, security escalation, coverage guard

- **Single table:** `test/taskfile.yml` `SCOPED_ROWS` moves to `test/scoped_rows.toml`, read only by `scripts/scoped_gate.py` (stdlib `tomllib`).

```toml
[crates]                      # crate name → test globs (relative to test/), or "escalate"
ocx_setup = ["tests/test_self_*.py", "tests/test_session_path.py"]
ocx_test_support = "escalate"

[verbs]                       # path under crates/ocx_cli/src/command/ → "escalate" only
"*_common.rs"      = "escalate"   # index_common, patch_common, package_sign_common
"script_runner.rs" = "escalate"
"deprecated.rs"    = "escalate"

[security]                    # repo-relative globs and command files that always escalate
escalate = [".github/workflows/**", ".github/actions/**", "crates/ocx_oci/**",
            "crates/ocx_trust/**", "crates/ocx_config/**", "crates/ocx_store/**",
            "crates/ocx_sign/**",
            "crates/ocx_cli/src/command/package_sign*.rs",
            "crates/ocx_cli/src/command/package_verify.rs",
            "crates/ocx_cli/src/command/package_attest.rs",
            "crates/ocx_cli/src/command/login.rs", "crates/ocx_cli/src/command/logout.rs"]
```

- **Verb → test mapping lives in the test module**, as a registered pytest marker: `pytestmark = pytest.mark.command("package_push", "package_push_*")` naming command-file stems. `scoped_gate.py` reads markers with stdlib `ast` (no import). Selection is thus reviewed with the test. `test/pyproject.toml` registers the marker under `--strict-markers`; `scripts/test_diff_guard.py` gains adding a `command` marker line as a sanctioned shape, like `smoke`.
- **Routing, in `classify`:** `[security]` first (escalate by name); then a path under `crates/ocx_cli/src/command/**` routes to the modules whose `command` marker matches its stem, unless `[verbs]` says `escalate`; any other `crates/ocx_cli/src/**` path escalates (permit-list default). `ocx` leaves `TABLE_ESCALATES`; `ocx_test_support` stays.
- **Plan output:** `plan.acceptance_globs` = sorted union of selected modules and `[crates]` globs; the scoped-run record carries it (§ Data model).
- **Coverage guard** (`scoped_gate.py --check-coverage`, T0), each invariant red in `--self-test`:
  1. every `test/tests/test_*.py` is matched by a `[crates]` glob or carries a `command` marker;
  2. every `crates/ocx_cli/src/command/**/*.rs` (including `launcher/`, `self_group/`) is named by a marker, a `[verbs]` escalate entry, or `[security]`;
  3. `[security].escalate` ⊇ the `reviewer:security` `when:` globs in `.agents/memory/hex.md`.
- `--self-test` gains: a marked verb file routes and decides `scoped`; a `*_common.rs` file escalates; an unmarked command file escalates by name; **one red per `[security]` glob**; the three coverage reds.
- **Stage 3 measures** the share of the last 50 `crates/ocx_cli/` diffs on `main` that still escalate. Above 50 % means D3 fixes little; the number is recorded, and a follow-up decides whether `api/**` or `app/**` get rows.

#### C-SEAM — in-process harness seam (pilot scope)

```rust
// crates/ocx_cli/src/app.rs — gated cfg(any(test, feature = "__testing"))
pub struct Environment {
    pub vars: BTreeMap<OsString, OsString>, // the complete environment; nothing falls through
    pub cwd: PathBuf,
}
pub async fn run(
    argv: &[OsString],
    env: &Environment,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
) -> ExitCode;
```

Invariants (each gets a test that can go red):
1. **No ambient environment reads on an admitted path.** Env is installed through `ocx_util::env::overrides` in a *hermetic* mode (unlisted key reads as absent instead of falling through, `crates/ocx_util/src/env.rs:156-173`); the hermetic mode itself is gated `cfg(any(test, feature = "__testing"))`. Third-party reads bypass that layer, so the pilot's `rust_test` target sets a poisoned `env` in `BUILD.bazel` covering `OCX_HOME`, `HOME`, `DOCKER_CONFIG`, `SSL_CERT_FILE`, `SSL_CERT_DIR`, `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY` (both cases), and the `OCX_AUTH_*` family; an ambient read on an admitted path fails the case.
2. **No network.** `run` refuses to construct a registry transport for admitted verbs (returns `ExitCode::UsageError`); a case that needs a registry is e2e by rubric item 2.
3. **No `process::exit`, no `execvp`, no spawn-and-wait as the result.** `exec`, `package exec`, `launcher *`, `self update` are not admitted.
4. **No global installs.** No `set_global_default`, no signal handler; tracing via `with_default` scoped to the call.
5. **Output only through `out`/`err`.** **Runtime:** the caller's Tokio runtime. **Serialised** under `EnvLock`. `// ponytail: serialised via EnvLock; per-invocation env threading when concurrency matters.`

The seam is **not** the default port target: port to the lowest crate whose public API exercises the behaviour; use `run` only for cases whose contract *is* the CLI surface, knowing those re-run on every Rust edit.

#### C-RUBRIC — keep-e2e rubric (starting contract; the pilot refines it)

A case **stays e2e** if any holds:
1. It drives a real shell interpreter, a pty, or a login-shell activation.
2. It talks to a registry, the Sigstore stack, a forge or any network endpoint.
3. Its contract is observed across a process boundary: exit code as a parent sees it, byte-exact stdout vs stderr separation, `env_clear()`, signal or child exit propagation, `execvp`.
4. It needs a global the harness refuses (C-SEAM 3-4), or a process of its own.
5. It asserts on-disk layout from the full command pipeline under a real `OCX_HOME` (symlinks, junctions, hardlinks, permissions).
6. It is platform-gated (Windows/macOS) or a doc-script / recording fidelity check.
7. It is the only case in the suite reaching its verb + flag combination (the classification records the path).
8. It tests Python test tooling, not ocx (e.g. `test_state_providers.py` SP0/SP1 import `src.state_providers`) — it stays Python; a Rust port would exercise nothing.

Otherwise it is **port-capable**; the classification records target crate and API. Every port carries a `// ported-from: test/tests/<module>.py::<case>` marker, a mutation proof (mutate the exercised code, Rust test red, restore, green, prove the restore landed) **committed as evidence** under `.claude/artifacts/port_evidence/<wave>.md`, and deletion of the original plus a `test/SUITE_FLOOR` decrease by exactly the deleted count in the same commit.

**Deletion guard.** `scripts/test_diff_guard.py` gains exactly one sanctioned deletion shape: a removed test function whose `module::case` appears in an added `ported-from` marker **and** that marker's enclosing Rust test resolves to a test name that *executed* (pass) in the JUnit output of `task bazel:test:unit` for the same range — so an `#[ignore]`d, cfg-disabled or untargeted port does not qualify. `--self-test` reds: a marker with no executed test; an `#[ignore]` port; a floor decrease that does not match.

**Pilot GO criteria** (all must hold): seam invariants 1-5 proven red/green (if the seam is built); 100 % of pilot ports mutation-proven with committed evidence; median agent cost per ported case recorded for both models; classification shows ≥ 300 port-capable cases (≈ 8 % of 3,884) whose target is a crate *below* `ocx_cli`. Otherwise NO-GO: keep the pilot's ports, stop.

**Pilot selection:** 3-5 modules from the classification's port-capable list whose cases exercise an existing Rust API. `test_state_providers.py` SP0-SP4 are **not** pilot material (rubric item 8).

**Model for porting workers:** the pilot ports the same cases with Sonnet 5 and Opus 5.5 and records cost, wall clock and first-pass mutation-proof rate per case. CLAUDE.md's model policy binds the result: if Sonnet falls short twice on the same port, porting is Opus work.

#### C-AGENT — agent-gate wording

Every file under `.claude/rules/**`, `.claude/agents/**`, `.claude/skills/**` that prescribes a gate states:

> Per task / review-fix iteration: `task verify:scoped --force`. Full `task verify` runs at WP merge (enforced by the commit gate), at finalize, and whenever `verify:scoped` escalates (it then runs `task verify` itself).

Known sites: the six in Context item 3 plus `subsystem-cli.md:414`, `subsystem-oci.md:667`, `subsystem-package-manager.md:252`, `subsystem-package.md:165`, `subsystem-taskfiles.md:70`, `workflow-feature.md` § Quality Gates. Regression test in `.claude/tests/test_ai_config.py`: a line-scoped regex over those three trees flags any line naming `` `task verify` `` (not followed by `:`) together with `per task|each task|every iteration|final gate before commit|review-fix`, unless the line also names `merge`, `finalize` or `escalat`, or the file:line is on an explicit allow-list in the test. A self-test case feeds a synthetic offending line and a synthetic allowed line.

#### C-ESC — escape record

- `verify:scoped` appends one line per run to `$(git rev-parse --git-common-dir)/ocx-gate/scoped_runs.jsonl` — `{ts, worktree, tree, head, acceptance_globs}`, `tree` defined as the mark's — never overwriting. The common git dir is shared by every worktree, so a WP's records are visible from the worktree that merges it (the single mark file stays as `commit_gate.py` reads it).
- When T2 fails an acceptance module **while `MERGE_HEAD` exists**, it looks up records whose `tree` equals `MERGE_HEAD^{tree}` — the WP tip — from **any** worktree. If at least one exists and none selected the module, it appends `{tree, head, module, scoped_globs}` to `escapes.jsonl` beside the run log and sets `ocx.gate.escape=true`. No matching record → `unattributed`, never an escape.
- **Still unattributed, named:** T2 runs with no `MERGE_HEAD` (finalize, escalations, commits on `main`); fast-forward merges; a WP tip committed with no scoped run on its exact tree (an edit after the last `verify:scoped`). **Over-count, named:** a failure caused by `main`'s side of the merge rather than the WP is still recorded as an escape; the record carries both trees so it can be discounted by hand.
- `--self-test`: a record on `MERGE_HEAD^{tree}` from another worktree that missed the module → one escape; a record for another tree → unattributed; a record that selected the module → none; no `MERGE_HEAD` → unattributed.

### NFR coverage

| NFR | Decision | How measured |
|---|---|---|
| **Latency** | T1 ≤ 120 s | Stage 0: warm baseline, apply the reference edit, time the **first** `task verify:scoped --force`, per-step wall clock; unchanged re-runs reported separately. Every later stage re-measures the same way. |
| **Cost — CI** | basic tier unchanged (≈ 18 runner-min/push); CI acceptance stays cold | GitHub run durations |
| **Cost — cache disk** | acceptance results are logs + JUnit, local only | `du ~/.cache/ocx/bazel-disk` |
| **Cost — agent spend** | classification ≈ 1 h Sonnet; pilot 3-5 modules × 2 models; waves only on GO | pilot records per-case cost |
| **Operability** | every new check has a `--self-test` or proven red; floors on every reader (LINT_FLOOR, coverage counts, port-guard JUnit read) | `task scripts:self-test` |
| **Security — cache trust** | no acceptance verdict leaves the machine that produced it; under-declared modules uncached; `release:` NOCACHE | § Security |
| **Security — release provenance** | `__testing` never in release + byte scan of every artifact before upload + native exec check | `release_provenance_check.py --self-test` |
| **Security — routing** | security globs and sign/verify/login verbs always escalate | C-ROWS self-test, one red per glob |
| **Availability of the gate** | a `bazel-cache.ocx.sh` outage slows, never fails, a cache-reading build (BZL-CACHE-26); telemetry never blocks | existing |

Unaffected NFRs (product runtime, registry behaviour, on-disk formats, CLI surface): no change.

### Security — the cache trust boundary

- **No writer, by construction.** Acceptance verdicts are not uploaded: `.bazelrc:73` `--remote_upload_local_results=false`, the acceptance job holds the read credential only (`verify-deep.yml:425-434`), and `.claude/tests/test_workflows.py` already refuses `--remote_upload_local_results=true` on any job other than the one credentialed `main`-push job, which runs `//crates/...` only. Findings #4/#6/#12 (poisoned trusted writer) therefore do not reach acceptance. Finding #10's conclusion ("acceptance out of remote-cache scope") holds again, for a different reason than it gave (`local` tag, since removed); a Stage 5 addendum records that.
- **Stale local verdicts** are closed by C-UNCACHED, not monitored: the only cached acceptance verdicts are modules whose inputs are fully declared; the rest carry `external`, the one tag measured to force re-execution (`no-cache` does not). `env_inherit` values (`CI`, `__OCX_TESTING_REQUIRE_ENVIRONMENT_D`) are not in the key; a local machine keeps one value of each across runs, and the CI lane is cold, so no cross-value serving occurs without a remote writer.
- **`release:`** runs `bazel:test:accept NOCACHE=1`; `verify-deep.yml` on `main` is cold on a fresh runner.
- **Provenance integrity:** C-PROV release guard. Placeholders carry no secret; `--ci-annotations` reads `GITHUB_*` at runtime (`package_push.rs:125`), so build-time placeholders cannot reach OCI annotations.
- **Remote writer (out of scope):** D6 preconditions.

### Data model

Mark (`.claude/hooks/.state/commit-verified`) gains `tree`; readers ignore unknown fields today (`commit_gate.py` keys on `scope`/`head`/`toplevel`), and the merge-commit clause reads `tree`:

```json
{"timestamp": 0, "head": "…", "scope": "full", "crates": [],
 "toplevel": "…", "tree": "<git write-tree of the real index>"}
```

`tree` is the same digest as span attribute `ocx.git.tree`, and `commit_gate.py` compares it to `git write-tree` of the same index — one definition on both sides. Scoped-run log line (C-ESC, under the common git dir): `{"ts": 0, "worktree": "…", "tree": "…", "head": "…", "acceptance_globs": ["…"]}`.

## Implementation Plan

Each stage is a separate commit series, independently revertable. Every exit criterion names an outcome that can be red. Stages 1 and 4 are file-disjoint from 2-3 and run in parallel worktrees; 2 precedes 3 (both edit `test/taskfile.yml` and `taskfile.yml`).

| # | Stage | Exit criterion (red if not met) | Revert |
|---|---|---|---|
| 0 | **Baseline, telemetry, latency levers.** Root-cause the `bazel-build` Tempo anomaly (`errorCount` ≈ `spanCount`). Publish the T1 per-step breakdown for the reference edit (first verification after the edit). Measure, each alone and combined: a named `[profile.test-bin]` (`inherits = "release"`, `incremental = true`, `debug = 0`, higher `codegen-units`, each setting commented with its measured effect per `rust-cargo.md` REL-01) for `test/bin/ocx`; local-disk `sccache` as `RUSTC_WRAPPER` (`SCCACHE_DIR` on local disk, never the S3 store). Record local acceptance executed-count for a docs-only commit and a one-crate commit. | A green build's trace carries 0 error spans and a planted failure ≥ 1; breakdown published with the dominant step named; each lever kept only if it measurably lowers T1; **T1 > 120 s with the kept levers is a stage failure whose only exit is an ADR amendment** (candidate: Option C) | drop the profile / wrapper |
| 1 | **D1 provenance + release guard.** | `test/bin/ocx` sha256 equal across two docs-only commits and clean/dirty; `Executed 0 out of 181` locally on the docs-only commit (warm disk); `release_provenance_check.py --self-test` red on each marker fixture, green on a real release-built binary; regenerated `release.yml` shows `host` needs the scan job; the five modules green. Cross-machine sha is recorded only: with no remote writer it has no consumer, so a mismatch does **not** fail the stage | revert `build.rs` branch + scan job |
| 2 | **D2 lint tier.** | `extra_data` empty; tag guard's sweeper clause requires zero; a `test/tests/test_install.py` edit executes 1 target plus the uncached list; each moved case shown red once; `SUITE_FLOOR` lowered by exactly the moved count; `test:lint:structure` ≤ 30 s measured and its budget self-test red; conftest's forbidden-route fixtures red | `git mv` back |
| 3 | **D3 routing.** `task bazel:test:unit` in the scoped arm; `test/scoped_rows.toml`; `command` markers; `[security]`; coverage guard; `ocx` out of `TABLE_ESCALATES`. | `scoped_gate.py --self-test` red/green for every new case incl. one per security glob; `--check-coverage` red on each of its three fixtures; T1 for the reference edit and a verb-file edit ≤ 120 s (first verification); share of CLI diffs still escalating recorded | restore `TABLE_ESCALATES` |
| 4 | **D4 + D5.** `commit_gate.py` merge-commit clause and `tree` in the mark; agent wording sweep. | `commit_gate.py --self-test` shows the four merge cases; `test_ai_config.py` sweep case red on its synthetic line and with one site reverted, green after | drop clause / text revert |
| 5 | **D6 + escape telemetry.** `UNCACHED_MODULES` + `external` + tag-guard clause; `release:` NOCACHE; scoped-run log; C-ESC; `ocx.gate` / `ocx.git.tree`; `test_schema_generation.py` ported to `schema_outputs` (C-UNCACHED); trust-research addendum. | **Planted stale verdict** in a throwaway worktree: a fixture module reading an undeclared `website/` file, cached green, file changed → `bazel:tag:guard` reds until it is listed with `external`, then T2 re-executes it and reds; the same fixture tagged `no-cache` instead → guard reds; a planted failure outside the scoped selection, merged from another worktree, produces exactly one escape line and a non-matching tree produces none; **`UNCACHED_MODULES` is empty** | drop fields / list |
| 6 | **Port-down pilot + classification.** Sonnet classifies all 181 modules (read-only, ≈ 1 h). Opus pilot on 3-5 modules chosen per C-RUBRIC; builds C-SEAM only if a pilot case needs it; two-model comparison. | GO/NO-GO recorded against C-RUBRIC's criteria with the numbers | ported cases stay; nothing else lands |
| 7 | **Porting waves (only on GO).** File-disjoint packages grouped by target crate, ≤ 3-4 concurrent (cargo jobs = 12, RAM, acceptance-suite flock); Opus sample review per wave. | each wave: deletion guard green only on executed ports; evidence file committed; `SUITE_FLOOR` decrease equals deleted count; T2 green | per-wave revert restores originals |

### Follow-ups (not in this ADR)

- `.claude/rules/product-tech-strategy.md` `Linker | Mold (dev)` is stale (mold is wired nowhere; rust-lld is the 1.95 default) — correct the row.
- `test:scoped` could pass `--lf`/`--nf` for agent re-iteration (gap G4) — measure after Stage 3.

## Validation

- [ ] Stage 0 breakdown published before any other stage lands
- [ ] Every new check shown red and green (`quality-core.md` Unchecked Green)
- [ ] `/security-auditor` review of C-PROV's release guard, C-ROWS `[security]`, C-UNCACHED
- [ ] T1 ≤ 120 s on the first verification after the reference edit, or an ADR amendment

## Open Questions

- [NEEDS CLARIFICATION: Port-down scope — the dossier ratified "a dedicated migration of every module" (Option B here). This ADR recommends pilot-gated waves.]
  Recommended: pilot-gated (Stage 6 GO criteria) — same contracts, rubric and deletion discipline; the 45-90+ agent-hours are spent only after a number says they pay.
- [NEEDS CLARIFICATION: T2 acceptance engine when the binary changed — `bazel:test:accept` is serial and costs 1421 s cold, against 160 s for `task test:parallel`; after D1 the cold path still applies to every WP merge that touched Rust, and D4 now makes that merge wait for it.]
  Recommended: measure at Stage 1 what share of WP merges keep the binary unchanged; if under half, WP-merge T2 runs `test:parallel` (the full mark still requires the full suite) and `bazel:test:accept` stays at finalize and in CI.
  *Closed 2026-09-23 by AM-11.*
- [NEEDS CLARIFICATION: Porting-worker model — Sonnet 5 ($2/$10 per MTok) or Opus 5.5 ($4/$20).]
  Recommended: decided by the Stage 6 two-model comparison on cost per mutation-proven case, bounded by CLAUDE.md's rule that non-mechanical work is Opus.

## Amendment 2026-09-22 (plan review)

Accepted with the ADR on Stage 5 landing (2026-09-23). Raised by the plan-review panel on `plan_test_speed_tiers.md` (round 1); each fact below was re-checked against the tree. These rows override the sections they name; the owner may veto any of them.

| # | Section amended | Change | Evidence |
|---|---|---|---|
| AM-1 | Implementation Plan, Stage 0 exit | The Tempo root-cause and the clause "a green build's trace carries 0 error spans and a planted failure ≥ 1" move to Follow-ups, with the `bazel.tests.{executed,cached}` span attributes. T1 is judged on task-step wall clock. | T1 is a wall-clock budget, and the spans do not feed it |
| AM-2 | C-PROV release guard, `--self-test`; Stage 1 exit | `--self-test` keeps only the synthetic reds. The green on a real release-built binary, and the red on a real `__testing` binary, move to `task release:provenance:proof`. `release:prepare` runs it. `verify-deep.yml`'s `cross-compile` job (a `--release` build with no `__testing`, run on every push to `main`) also runs `--scan` over its binary. | `scripts:self-test` runs in verify phase 1, before any build. No verify step builds a binary without `__testing`: `rust:build` passes it (`taskfiles/rust.taskfile.yml:234`) |
| AM-3 | C-ROWS `[security]` | The list gains `command/package_sbom.rs`, `command/shell_allow.rs`, `command/shell_revoke.rs`, `command/self_group/update.rs`, `command/config_push.rs`, `command/config_update.rs` and `command/config_setup.rs`. Widening is the safe direction. Stage 3 records the escalation share with and without these rows. | They cover SBOM referrers, consent grants, replacing the running binary, and managed-tier registries and CA roots |
| AM-4 | Validation line 1; Stage 0 / Stage 3 | Stage 0 runs **alone and first**: no other stage's work starts until its breakdown has merged. G-0 at Stage 0 judges the **pre-D3** scoped arm, which runs no `rdeps` `rust_test`, so a GREEN there does not predict D3. Stage 3's exit re-judges T1 on the new arm. The Option C amendment question is raised by whichever of the two reds first. | `taskfile.yml:249-250`, where today's arm runs `bazel test //crates/<c>:all` per changed crate |
| AM-5 | Stage 0 `[profile.test-bin]` | `incremental = true` is not allowed while local sccache is kept. sccache does not cache incremental crates (mozilla/sccache#236). "Profile" and "sccache" are therefore measured as exclusive rows, and the kept profile sets `incremental = false` if sccache is also kept. | research panel R1 |
| AM-6 | C-UNCACHED named resolution | `test_schema_generation.py` is **not** the only module whose undeclared input is a build product. `test_project_env.py:92` and `test_execution_records.py:255` also read `target/release/ocx_schema`. Those two assert on `ocx` output, not on schema generation, so they take the fallback: a `rust_binary` for `ocx_schema`, passed as `data`. `test_schema_generation.py` is still ported. | grep of `test/tests/`, this review |
| AM-7 | C-RUBRIC deletion guard | "The JUnit output of `task bazel:test:unit`" means the per-case report at `target/bazel/junit.xml`. It must also be **fresh for a tree containing the range tip**: the guard requires the tip to be an ancestor of HEAD (after a `--no-ff` merge HEAD is the merge commit, not the tip), runs `task bazel:test:unit` itself at HEAD, and refuses a report older than that run. `bazel-testlogs/**/test.xml` cannot be used, because it holds one `<testcase>` per *target* and names no `#[test]`. | `taskfiles/bazel.taskfile.yml:413,462`; `subsystem-ci.md` § Test telemetry |
| AM-8 | C-TIER Enforcement | While `MERGE_HEAD` exists, `--mark` **refuses** (exit 1, no mark written) when the working tree differs from the index: an unstaged change, or an untracked file that is not ignored. The marked `tree` then equals the tree that was built. Without this, an unstaged repair could earn a full mark for a different, staged tree. Outside a merge, marking is unchanged. The ordinary "verify, then stage, then commit" flow keeps working, and no other clause reads `tree`. | Codex adversary X3; `scripts/scoped_gate.py --mark` writes from the index today |

**Pending owner decision (not amended here):** the new `scripts/test_diff_guard.py` shapes (move, lint files, marker line, config, ported-from deletion) ship **opt-in**, behind one flag. The plan passes that flag only for its own ranges, and crate-split DEC-10's default behaviour is unchanged. C-ROWS's "a `command` marker line is a sanctioned shape, like `smoke`" therefore holds only under the flag until the owner decides whether the marker and config shapes become the default.

## Amendment 2026-09-23 (speed-up workstream)

Status: proposed; the owner may veto any row. Raised by the speed-up workstream after Stage 5 landed; evidence is commits `7ee5207a` (per-module inputs) and `b31996a9` (concurrent acceptance), each fact re-checked against the tree. The runs were taken on those commits' pre-rebase originals, before the branch was rebased onto the pilot port-down (`b0ec8e81`). These rows override the sections they name.

| # | Section amended | Change | Evidence |
|---|---|---|---|
| AM-9 | Consequences › Negative, "`bazel:test:accept` stays serial at T2"; the Stage 4 statement carried from `adr_bazel_build_adoption.md` that `exclusive` stays | **Acceptance targets run concurrently, at xdist parity.** `exclusive` leaves `ACCEPTANCE_TAGS`. `--local_test_jobs` is set only on the `task bazel:test:accept` command line (`ACCEPT_JOBS`, default `min(8, nproc)`), never in an rc file. Both refusals are enforced by `bazel:tag:guard` in `task verify` and `verify-basic.yml` — `tag-acceptance-serialised` for an acceptance target carrying `exclusive`, and `rc_serialisation_findings` for a `--local_test_jobs` line in `.bazelrc` / `.bazelrc.user` — not only by the hand-run `bazel_accept_proofs.py --check-s015` (`accept-serial-tag-present`, `global_serialisation_findings`). The per-target blocking flock is replaced by runner-side host locks (`test/bazel.bzl` § Concurrency): (1) stack bring-up barrier — `pytest_sessionstart` runs once per target under its own exclusive `acceptance-stack-<port>.lock`, and inside it `test/src/helpers.py`'s `_COMPOSE_LOCK`, before pytest. The barrier holds the real lock file and, for the hook call, points `helpers._COMPOSE_LOCK` at a second file (`ocx-compose.lock.barrier`), so the hook's own recycle takes that file instead of deadlocking on the one its parent holds, while every other process still contends on the real one; (2) per-`xdist_group` slot locks for **every** group a module names — a group one module names is serial in one process, but two runs of that module from sibling checkouts are two — declared in `module_slots` (`OCX_ACCEPTANCE_SLOTS`, sorted acquisition), derived from source and checked both ways by `bazel_tag_guard.py` (`tag-slot-*`, which also reds a group spelled outside a `tests/test_*.py` module); (3) host suite lock `acceptance-suite-<port>.lock`, SHARED by Bazel targets, EXCLUSIVE in `test/taskfile.yml`'s pytest step, so targets overlap each other (a sibling checkout's run too, at xdist parity) and never that step — the stale-run eviction and binary rebuild before it, and `task test:smoke`, take no lock; (4) turnstile `acceptance-turnstile-<port>.lock`, HELD by that pytest step for its whole run (`flock -o <turnstile> flock -o <suite>`) and passed through (taken, released) by every target before it takes the suite lock shared — flock(2) is unfair, so without it a queued exclusive waiter sat behind any unbroken chain of overlapping shared holders; with it no new target joins once the step is queued; (5) an innermost exclusive `<basetemp>.lock` around pytest, because two `--runs_per_test` copies of one target share an arena pytest wipes at start. Every lock, the taskfile's included, is taken as `flock -o`, so a process a run leaves behind cannot keep holding it; every blocking take is preceded by a `flock -n` probe that prints `acceptance sh_test: <module> is waiting for <lock>` on stderr (the taskfile step prints its own `waiting for` line). Lock order: turnstile (passed) → suite → stack → `_COMPOSE_LOCK` (barrier), turnstile (passed) → suite → slots → `<basetemp>.lock` (session), turnstile → suite (taskfile); `_COMPOSE_LOCK` innermost, no cycle. Plus a basetemp per target keyed on a checksum of the suite path (a folder-name key collides across repositories), `-p no:cacheprovider`, and a refusal to run on a host without `flock(1)` unless `--test_env=OCX_ACCEPTANCE_UNSERIALISED=1`. `bazel:tag:guard` runs the generated runner against logging fakes on every run and reds a lost lock (`runner-*`). **Residuals, named:** each session's `pytest_sessionstart` re-probes the registry with no lock held, so a full store — or a false negative under load — runs `compose_up`/`_recycle_registry` and can wipe the registry under running targets (red either way, never a stale green; xdist's per-worker `registry` fixture probes the same way; the fix needs a sanctioned edit to frozen `test/src/helpers.py`, deferred); a second full green run is pending a registry recycle. | Full suite cold, `NOCACHE=1 ACCEPT_JOBS=8`, host 32 cores, load1 2.6 at start: **283 s**, 172/172 green, 3445 cases (= `SUITE_FLOOR` then; 3400 since the pilot port-down, `b0ec8e81`), 156 skipped, 5 xfailed — vs **1734 s** serial (WP-12 A3). Slot-lock proof on the real suite: `test_patches`, `test_frozen`, `test_managed_config` ran in disjoint windows with the lock, overlapping without it. Tag guard on the live graph: red on exactly the 4 `patch_global_slot` members before `module_slots`, green with it. Fix pass (review W1/W3–W5/S7/S8/S10): every-group slots red `test_doc_scripts` on its two single-module groups until declared; `bazel_accept_proofs.py --self-test` reds each runner mutation (slot loop, suite lock exclusive/absent, barrier dropped/unlocked/without `_COMPOSE_LOCK`, folder-name arena, no-flock refusal); `task --dry bazel:test:accept` resolves 8 on 32 cores, 4 / 2 under a 4- / 2-CPU affinity mask |
| AM-10 | C-TIER cache key inputs ("module + `:suite_inputs`"); `test/bazel.bzl` "honest unit is module + shared group" | **Per-module inputs.** `:suite_inputs` holds only what every module reads: `conftest.py`, `pyproject.toml`, `uv.lock`, `src/**`, `tests/**` helpers and fixtures (incl. `tests/conftest.py`), `docker-compose.yml`, `zot-config.json`, `test/ocx.toml` + `test/ocx.lock` (every `ocx` run walks up to them — invisible to the AST, so kept shared), `sigstore/**` (imported by `tests/conftest.py`), and `bin/ocx*` via `:suite_anchor`. Everything else is per-module `module_data` on exactly its readers; files no module reads are declared nowhere. `bazel_tag_guard.py` counts a transitively imported helper's source (conftests included) as a read and reds a missing declaration it can derive; reads no AST sees are hand-kept in `HAND_DECLARED_READS` (`:suite_scripts` on `test_doc_scripts_publish`, reached through a `task` recipe; the schema binary) and `IMPLIED_READS` (root `ocx.lock` wherever root `ocx.toml` is declared). A read the AST cannot resolve is no longer a silent stale-green risk: a module whose own source or any helper it imports launches `task` (a `"task"` / `"task …"` literal), or names by string a path under a `test/` directory holding no `:suite_inputs` member, reds `tag-hand-read-unreviewed` until it has a `HAND_DECLARED_READS` entry (an empty entry records a reviewed no-read). | `kind(sh_test, rdeps(//test:all, //test:<f>))` before → after: `taskfile.yml` 172→1; `bench/`, `scenarios/`, `scripts/`, `specs/` 172→1; `recordings/*.py` 172→6; `SUITE_FLOOR`/`SKIP_CEILING`/`XFAIL_CEILING`, `docker/` 172→0. Warm run after a `test/taskfile.yml` whitespace edit: `Executed 2 out of 172` (its one reader `test_doc_scripts_publish` + the `test_windows_shim` `external` residual). Derivation proof: dropping `test_state_providers`' recordings is red under the new derivation, green under the old. Dropping `:suite_scripts` from `test_doc_scripts_publish` or `//:ocx.lock` from the ten project-toolchain modules was green until the hand-kept maps; red with them |
| AM-11 | Open Questions, item 2 (T2 acceptance engine) | **Closed.** WP-merge T2 keeps `bazel:test:accept`. OQ2's purpose — keep WP merges off the serial 1421/1734 s cold path — is met by AM-9, not by routing T2 to `test:parallel`. | Cold path ≈ 283 s, ≈ 2× `test:parallel` (≈ 135-160 s); warm path executes only changed modules (AM-10) |
| AM-12 | D6 remote writer; Security › Remote writer | **Stays deferred.** None of D6's preconditions is met: `__OCX_TESTING_REQUIRE_ENVIRONMENT_D` is still in `_INHERITED_ENV` (not pinned via `--test_env`); `UNCACHED_MODULES` = {`test_windows_shim`}; no credential-scope tests for a second writer; no `/security-auditor` review. **Precondition added:** one acceptance-binary profile across CI and local. CI builds the acceptance binary with `.github/actions/build-rust/action.yml` (`cargo build --release --target=…`); local T2 builds `--profile test-bin` (`test/taskfile.yml`). `bin/ocx*` is a declared input of every target, so the digests differ by construction and a CI writer would produce entries no local reader can hit. CI does not rebuild the binary: `test/taskfile.yml`'s `SKIP_BUILD` defaults to `CI`, so `.build-binaries` is skipped and `verify-deep.yml` tests the downloaded `--release` artifact (`CI=true task --dry bazel:test:accept` lists no `--profile test-bin` build; without `CI` it does). CI-to-CI reuse is unmeasured. Which binary CI should test stays open: [ocx-sh/ocx#518](https://github.com/ocx-sh/ocx/issues/518). | `test/bazel.bzl` `_INHERITED_ENV`, `UNCACHED_MODULES`; `.github/actions/build-rust/action.yml:69`; `test/taskfile.yml` `--profile test-bin` |

**Options weighed for AM-9 / AM-11.** (a) Concurrent targets behind host locks — chosen: removes the serial cost at its source and keeps one T2 engine. (b) Keep `exclusive` and route WP-merge T2 to `test:parallel` (OQ2's recommendation) — rejected: two T2 engines, and the full mark would rest on a run that caches nothing. (c) `--local_test_jobs` in an rc file — rejected: per-command, so it would also throttle the Rust test targets.

## Links

- `.claude/artifacts/system_design_test_tiers.md`
- `.claude/artifacts/adr_bazel_build_adoption.md` (§ Stage 4 amendment, A1 DX-18/DX-37, A4, DX-97)
- `.claude/artifacts/adr_crate_split_workspace.md` (§ Verification tiers)
- `.claude/artifacts/adr_build_provenance.md`
- `.claude/artifacts/research_bazel_cache_trust_boundary.md` (findings #4, #6, #10, #12, #24)
- `.agents/discussions/test-suite-speed-tiers.md`

Template deviations, deliberate: `### API Contract` holds component contracts; added `### Trade-off matrix`, `### Decisions`, `### NFR coverage`, `### Security`, `### Follow-ups`, `## Open Questions`, `## Review history`.

## Review history

**Round 1 (2026-09-22).** Panel: spec, quality, security (opus), gap researcher (sonnet); cross-model: Codex adversary. All findings verified against the cited code before acting.

Applied: remote-writer premise removed, D6 = local only (security B1, adversary A2, quality W5, spec 3); under-declared modules uncached, list must reach empty, `release:` NOCACHE (quality W6, security W1, adversary A3); T2 at WP merge enforced by `commit_gate.py` with tree digest (quality B2, spec 2); first-verification latency, Stage 0 levers, T1 > 120 s = amendment, matrix re-scored with D+ and B restated (quality B1/W1/W2/W3, gap G1-G3, adversary A6, spec 8); port-down guard on executed JUnit names, evidence, rubric items 7-8, pilot off SP0/SP1, C-SEAM third-party env + no transport + gated override (quality B3, adversary A5, security W3); `[security]` escalation (security W4); placeholders, byte scan before both publish paths, no non-native exec (spec 1, security W2/S1, adversary A1, quality W4); `[plumbing]` dropped, `*_common.rs` escalate, `command/**/*.rs`, markers in modules, escalation share measured (spec 4, quality W8/S1); lint tier listed, `test_logging` split, `test_patch_global_slot` no exception, conftest forbids `test/bin` and `task`, 30 s gate (spec 5, quality W7, security S2, adversary A4/A8); escape records keyed by worktree + tree (adversary A7); agent sweep with line-scoped regex (spec 6, S10); `task bazel:test:unit` (spec 7); planted stale verdict (spec 9); template headings (spec S11).

Refined while applying: `release.yml` is cargo-dist-generated, so the scan job goes through `dist-workspace.toml` and its pre-`host` placement is a Stage 1 exit. The weekly uncached T3 job is dropped: with no remote writer, `verify-deep.yml` on `main` is already cold. `test_doc_scripts_publish.py` invokes `task`, so only its non-`task` cases can move.

Not applied: gap G4 (`--lf`/`--nf`) — deferred to Follow-ups, no evidence it matters before Stage 3. No finding was rejected as factually wrong.

**Round-1 re-validation (spec): 6 findings applied.** Each cited fact re-checked against the tree first. (1, BLOCK) uncached mechanism is `external`, not `no-cache` — WP-35, `scripts/bazel_tag_guard.py:46-53`, `TAG_INSUFFICIENT_TEST_MSG` `:403`; the tag-guard clause admits exactly `external` on `UNCACHED_MODULES`; BZL-CORE-01 note corrected. (2, BLOCK) `--scan` drops the all-zero SHA (a 40-zero run is present in `test/bin/ocx`, confirmed by grep); three string markers kept, SHA left to `--exec`; the self-test's green is a real release-built binary. (3, WARN) C-ESC records live under the common git dir and match `MERGE_HEAD^{tree}` from any worktree; unattributed and over-count cases named. (4, WARN) `test_schema_generation.py`: `crates/ocx_schema/BUILD.bazel` has no binary target, so the module ports into the `schema_outputs` `rust_test` (fallback: a `rust_binary` label as `data`). (5, SUGGEST) mark and gate `tree` both = `git write-tree` of the real index. (6, SUGGEST) sensitivity prose decomposed.

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-09-22 | architect | Initial draft (Proposed) |
| 2026-09-22 | architect | Round 1 panel fixes (see Review history) |
| 2026-09-22 | architect | Round-1 re-validation (spec): 6 findings applied |
| 2026-09-22 | architect | Amendment 2026-09-22 (plan review): AM-1 … AM-8; status unchanged (Proposed) |
| 2026-09-23 | owner | Accepted on Stage 5 landing, Amendment AM-1 … AM-8 binding (decision recorded at execution start) |
| 2026-09-23 | architect | Amendment 2026-09-23 (speed-up workstream): AM-9 … AM-12 proposed; OQ2 closed by AM-11; Consequences "stays serial" superseded by AM-9 |
