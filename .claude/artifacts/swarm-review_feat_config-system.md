# Code Review: `feat/config-system`

## Summary

- **Verdict**: **Request Changes** — 3 Block findings (1 correctness regression, 1 `C-GOOD-ERR` normalization gap, 1 plan cross-reference error), plus unresolved High findings in security, documentation, and test coverage.
- **Tier**: max (auto; 57 files, +5279/-160, 10+ subsystems)
- **Baseline**: `main` (default)
- **Target**: `HEAD` on branch `feat/config-system`
- **Overlays**: `breadth=adversarial`, `reviewer=opus`, `rca=on`, `codex=on` (tier=max defaults)
- **Cross-model gate**: ran (Codex found a genuine regression the Claude panel missed — see Block-2)
- **Architect/SOTA signals surfaced in Summary**:
  - Architect flagged `product-context.md` NOT updated despite introducing OCX's first ambient-config surface (Update Protocol violation).
  - SOTA found no major gaps; flagged one `deny.toml` hygiene item (`MPL-2.0` provenance undocumented) and one deferred design note (`ClassifyExitCode` trait-vs-free-function divergence from repo's own `quality-rust-exit_codes.md`).

---

## Stage 1 — Correctness

### Spec-compliance (post-Implement traceability)

Spec-compliance review reports **Pass** against both plan artifacts:

- `plan_configuration_system.md` — 5/5 items covered (precedence chain, `OCX_NO_CONFIG` pruning, `OCX_CONFIG=""` handling, `deny_unknown_fields` asymmetry, `resolved_default_registry` resolution).
- `plan_config_review_cleanup.md` — 13/13 items covered (`ExitCode` enum shape, `classify_error` free function, `RegistryGlobals → RegistryDefaults` rename complete, re-exports present, `#[must_use]`, `futures::future::join_all`, error-message normalization applied across 11 files, help-text changes, `-c` short-flag decision, acceptance tests, unit tests, malformed-ambient-config fix).
- `MirrorError::kind_exit_code` overlay is in place with documented `ExecutionFailed → Failure(1)` lock-in.

Minor discrepancies:

- **S1 (Suggest, actionable)** — Stale TODO `crates/ocx_mirror/src/main.rs:72` (`// TODO: Phase 4 — dispatch logic lands inside MirrorError::kind_exit_code.`) — dispatch already landed at `error.rs:26-33`.
- **S2 (Suggest, deferred)** — Implementation uses a `ClassifyExitCode` trait ladder instead of the central free-function `match` ladder the plan (`plan_config_review_cleanup.md:482-536`) and `quality-rust-exit_codes.md` both call for. Net behavior is identical. Human judgment needed on whether to accept the deviation (and update `quality-rust-exit_codes.md`) or revert to the central ladder.
- **S3 (Suggest, deferred)** — Scope-creep TODO at `cli/classify.rs:24-27` (proc-macro `derive(ClassifyExitCode)`) not in the plan's "Out of Scope" list.

### Test coverage (Specify-phase adequacy)

Test coverage review reports **Needs Work** — unit coverage is dense for `config::*`, but acceptance coverage for the new exit-code contract is spotty and the mirror overlay has zero unit tests.

| Severity | Finding | File |
|---|---|---|
| **High** | `MirrorError::kind_exit_code` has zero unit tests (4 variants undefended, including the deliberate `ExecutionFailed → Failure(1)` lock-in) | `crates/ocx_mirror/src/error.rs` |
| **High** | Three-layer downcast chain `Error::PackageManager(FindFailed([PackageError{kind: NotFound}])) → ExitCode::NotFound` is untested; the comment at `package_manager/error.rs:36-43` acknowledges a non-obvious chain-walker interaction that depends on classification reading the `Vec` head | `cli/classify.rs` tests |
| **High** | `ClientError::Authentication → AuthError`, `ManifestNotFound/BlobNotFound → NotFound`, `Registry → Unavailable`, 5× `DataError` variants untested | `cli/classify.rs` tests |
| **High** | Exit codes 64 / 65 / 69 have **zero acceptance coverage** — public scripting contract untested end-to-end | `test/tests/` |
| Warn | Exit codes 74 / 75 / 77 / 80 have no acceptance coverage (80 explicitly skipped) | `test/tests/` |
| Warn | `discover_paths` return order (`[system, user, home]`) not asserted by any test | `config/loader.rs` tests |
| Warn | `DependencyError::{Conflict, SetupFailed}` classification untested | `package_manager/error.rs` tests |
| Suggest | Edge cases (`[registry] default = ""`, whitespace-only names) untested | `config.rs` tests |

Stale-error-string scan: **clean**. No test asserts on pre-normalization strings. Deferred: concurrent-config-load test (D1) and `AuthError(80)` acceptance (D2 — fixture limitation, already explicitly skipped).

---

## Stage 2 — Adversarial panel

### Quality + CLI-UX

**Needs Work** — 2 Block, 3 High, 6 Warn, 3 Suggest.

| ID | Sev | File | Finding |
|---|---|---|---|
| **B1** | Block | `oci/client/error.rs:37`, `oci/index/error.rs:27,33`, `profile/error.rs:28-32`, `config/error.rs:42` | `C-GOOD-ERR` violations survived the normalization pass. Capital-case first words and trailing punctuation (`?` in `FileTooLarge`, `.` in `profile::UnsupportedVersion`). The entire point of the cleanup plan directive #17 was this pass. |
| **B2** | Block | `crates/ocx_cli/src/command/index_catalog.rs:80` | `log::error!("Error fetching tags...")` inlines the `"Error"` prefix. `quality-rust-errors.md` explicitly forbids this — `log::error!` / `tracing::error!` already categorize the line. |
| **H1** | High | `crates/ocx_lib/src/config/error.rs:18` | `#[error("invalid TOML at {}: {source}", path.display())]` interpolates `{source}` in `Display` AND declares `#[source]`. Printing via `anyhow` `{err:#}` walks the chain and re-appends the source. Result: `invalid TOML at X: <source>: <source>`. Sibling `Io` variant at line 32 shows the correct pattern. |
| **H2** | High | `crates/ocx_lib/src/oci/index/error.rs:50` | `SourceWalkFailed` calls `arc.as_error().classify()` directly and silently falls through to `ExitCode::Failure` on `None`. Inconsistent with `PackageErrorKind::Internal` which correctly delegates to `classify_error` to re-walk the chain. |
| **H3** | High | `crates/ocx_cli/src/command/ci_export.rs:44`, `shell_env.rs:49` | Dual-sentence anyhow messages (sentence-case with period). Preferred single-clause + semicolon style per `quality-rust-errors.md` normalization examples. |
| W1–W6 | Warn | various | Dead `default_offline_mode` helper (`app/context.rs:159-162`); sub-module paths for re-exported traits (`use crate::cli::classify::ClassifyExitCode` x12 sites, collapses to `crate::cli::ClassifyExitCode`); `Config::merge` `&mut self` not `Self` (deferred); `_inputs: &ConfigInputs<'_>` dead parameter shape (`loader.rs:96`); batch classification drops context (deferred — see RCA R-1); Ousterhout failure in `ArcError` rustdoc. |

### Security

**Needs Work** — 1 High, 2 Warn.

| ID | Sev | CWE | File | Finding |
|---|---|---|---|---|
| **S-1** | High | CWE-209 + CWE-59 | `crates/ocx_lib/src/config/error.rs:18` + `loader.rs:159` | TOML parse error echoes `toml::de::Error` which includes content excerpts. Combined with `fstat` symlink-follow on the opened file (`is_file()` passes for symlink targets), an attacker who controls `~/.config/ocx/config.toml` can symlink it at a sensitive readable file (e.g. `~/.ssh/config`); the parse error leaks snippets to stderr. CI logs capture them. Remediation: reject symlinks via `tokio::fs::symlink_metadata` pre-open, or elide `{source}` in `Parse` display for discovered tiers. |
| **S-2** | Warn | CWE-732 | `crates/ocx_lib/src/config/loader.rs:109-199` | No permission-mode warning for world/group-writable `/etc/ocx/config.toml` or `~/.config/ocx/config.toml`. Peer tools (ssh, sudo, gpg) warn or refuse. Shared build host with `0o777` on `/etc/ocx/` → neighbor can redirect registries. Remediation: log `warn!` on `mode & 0o022 != 0`; for system tier also warn if owner is non-root. |
| **S-3** | Warn (deferred) | CWE-426 + CWE-15 | `crates/ocx_lib/src/config/loader.rs:212-218` | `$OCX_HOME` read without validation. Under `sudo` with `env_keep`, a non-root caller can point `$OCX_HOME` anywhere and control the default registry. Decision needed on whether `$OCX_HOME` is user-trusted like `$HOME` or should be stripped under privilege. |

Clean results:
- `dirs = "6"` supply chain: no RUSTSEC advisories on `dirs`, `dirs-sys`, `redox_users`, `option-ext`, `libc`, `windows-sys` at pinned versions.
- Mass-normalization control-flow regression scan: only test-side `to_string().contains(...)` hits found. No production branch relies on a changed Display string.
- Error-message info-disclosure audit: no new secret-adjacent leaks (auth messages unchanged; only `config::Error::Parse` surfaces content — see S-1).
- `pinned_identifier.rs` validation logic unchanged.

### Performance

**Pass** — 0 Block/High/Warn. The loader hot path is sound.

- `futures::future::join_all` ordering verified: input-ordered `Vec`, zip at `loader.rs:110-112` preserves system→user→home precedence. Expected saving: ~2-20 ms cold vs sequential, n=3 today.
- No blocking I/O in async. All `fs::*` imports resolve to `tokio::fs::*`.
- `FileTooLarge` bound is correct: `metadata.len()` short-circuit + `take(MAX + 1)` on the reader defends against `/proc/self/*`-style synthetic files.
- Static-command bypass at `app.rs:65-69` skips config load — **BUT** see Block-2 below: the bypass is incomplete for bare `ocx`.
- `classify_error` chain walk uses `std::iter::successors` — zero allocation, O(chain depth × ≤17 downcast checks). Cold path, fine.
- Suggestion (actionable): `Vec::with_capacity(3)` at `loader.rs:97` — negligible win, cosmetic.
- Deferred: startup-budget regression test for `Context::try_init` before #33's CWD walk lands.

### Documentation

**Needs Work** — 1 Block, 3 High, 2 Warn, 1 Suggest.

| ID | Sev | Finding |
|---|---|---|
| **D-1** | Block | **Exit-code table entirely absent from public docs.** `ExitCode` enum is a new public scripting contract with 11 variants. Only incidental mention at `command-line.md:96` (`"exit code 79"`). Scripts consuming `$?` cannot discover the taxonomy without reading Rust source. Fix: add "Exit codes" table in `reference/command-line.md` listing all variants, numeric values, sysexits alignment, and a `case $?` example. |
| **D-2** | High | `-c` short form undocumented for `--config`. `context_options.rs:15` uses `#[arg(short, long)]`. `command-line.md:86` heading doesn't mention `-c`. |
| **D-3** | High | `CHANGELOG.md` has no unreleased entry for the new config system, `OCX_CONFIG`, `OCX_NO_CONFIG`, the `--config` wiring (was a no-op), the `ExitCode` public contract, or the error-message normalization pass. The `ocx_mirror` exit-code change is a **breaking change** (plan flagged this explicitly) and needs a changelog entry. |
| D-4 | High | Internal AI config drift: `.claude/rules/subsystem-mirror.md:79` documents stale integer exit codes (`0/2/3/4`); actual values are now `0/65/79/1/69`. Needs a follow-up commit (out of diff scope). |
| W-1 | Warn | `reference/command-line.md:96` inline "exit code 79" stranded without cross-reference to the (not-yet-existing) table. |
| W-2 | Warn | `reference/configuration.md:215-224` Error Reference table doesn't surface `Error::Io` as a distinct type (plan finding #2 not fully resolved). |
| Suggest | S | `--config`/`OCX_CONFIG` interaction note: "wins" is slightly imprecise; "merges on top of" would be clearer. |

Prior plan findings (confirmed resolved): macOS path in 4 pages, `OCX_HOME` mentions config discovery, `--config` flag section expanded, `OCX_NO_CONFIG` rationale documented, help text mentions `OCX_CONFIG` + `OCX_NO_CONFIG`.

### Architecture

**Needs Work** — 1 Block, 4 Warn.

| ID | Sev | Finding |
|---|---|---|
| **A-1** | Block | `.claude/artifacts/plan_configuration_system.md:9` cross-references `adr_infrastructure_patches.md` which does not exist in `.claude/artifacts/`. Verification-honesty violation from `quality-core.md`. Fix: create the ADR, re-point the link, or delete the claim. |
| **A-2** | Warn | `crates/ocx_lib/src/config.rs:4-6` declares `pub mod error; pub mod loader; pub mod registry;`. Plan (`plan_configuration_system.md:185`) locked in "config submodule stays private; only `Config`, `ConfigInputs`, `ConfigLoader` re-exported." Unused public surface widens the API contract beyond intent. |
| **A-3** | Warn | `ocx_mirror::MirrorError` has four architectural smells: no `#[derive(thiserror::Error)]`, no `#[non_exhaustive]`, stringifies inner errors (`Vec<String>`, `String` — Block-tier anti-pattern per `quality-rust-errors.md`), and forces fixed `ExecutionFailed → Failure(1)` because the shape can't delegate to `classify_error`. Plan acknowledges but doesn't resolve. |
| **A-4** | Warn | `.claude/rules/product-context.md` NOT updated despite introducing OCX's first ambient-configuration surface. Update Protocol triggers: "Product principle added/reworded" (backend-first still holds; "zero ambient config" no longer does) and new use case ("Docker/CI uses `/etc/ocx/config.toml`"). |
| A-5 | Warn | `_inputs: &ConfigInputs<'_>` unused-parameter at `loader.rs:96`. Leading underscore obscures that the surface is reserved for #33. Cleaner: drop the parameter now, reintroduce when the extension lands. |

Clean architectural results:
- Dependency direction intact (`ocx_lib` has no `anyhow` dep; `classify.rs` walks `std::error::Error::source()` not `anyhow::chain()`).
- Three-layer error pattern (`Error → PackageError → PackageErrorKind`) preserved end-to-end.
- `C-GOOD-ERR` conformance at spot-checked modules (but see Block-1 in Quality — other modules missed).
- `deny.toml` MPL-2.0 addition has a justified source (transitive via `dirs-sys → option-ext`).
- `[registries.<name>]` namespace design accommodates documented future fields without migration.

### SOTA / Technical Soundness

**Pass** with one actionable hygiene item and one deferred design note.

- Layered config shape matches uv/Cargo SOTA. CWD walk correctly deferred to #33.
- `[registries.<name>]` plural namespace matches Cargo RFC 2141.
- Exit codes 79/80/81 in the safe private range — no collision with popular tools.
- `ClassifyExitCode` trait-vs-free-function divergence from repo's own `quality-rust-exit_codes.md` rule — **Warn, deferred** (mirrors spec-compliance S2).
- `dirs = "6"` is SOTA (MIT/Apache-2.0 — the MPL-2.0 transitive comes from `option-ext`); `etcetera` not widely adopted enough to warrant the swap.
- **SOTA-actionable**: `deny.toml` MPL-2.0 allow has no comment naming the crate that requires it. Run `cargo deny check licenses`, identify the crate, add a provenance comment. Without it, future reviewers cannot tell whether the allow is still justified.

---

## Cross-Model Adversarial (Codex)

Codex found three items. One is a **genuine cross-model win** on correctness the Claude panel missed.

### **Block-2 (Codex, verified): Bare `ocx` regresses the `464ca5f` malformed-config fix**

`crates/ocx_cli/src/app.rs:65-71`. The static-command bypass at lines 65-69 matches only `Some(Command::Version)` and `Some(Command::Shell::Completion)`. For bare `ocx` (no subcommand → `cli.command == None`), control falls through to line 71's `Context::try_init` BEFORE reaching the `None`-branch help print at line 77-79. Result: **bare `ocx` with a malformed ambient config fails with a config error instead of printing help** — regressing the exact scenario `464ca5f` was advertised to fix.

The existing regression test at `test/tests/test_config.py:237-267` tests `ocx --help` (clap intercepts the flag before `get_matches()` returns — entirely different code path). It does NOT test `ocx` alone. This is both a correctness regression and a test gap.

**Remediation**: Add `None => { ... print_help ...; return Ok(ExitCode::SUCCESS); }` as a second early-exit branch before `Context::try_init` OR match on `cli.command.is_none()` in the bypass list. Add an acceptance test that runs `ocx` (no args) with a malformed ambient config and asserts exit 0 with help on stdout.

**Verdict**: genuine cross-model win. Claude spec-compliance reviewer said "Pass, commit 464ca5f covers this" without tracing the `None` path. Added to Block findings.

### Codex-3 (actionable, already partially captured): batch classification by `errors.first()`

`crates/ocx_lib/src/package_manager/error.rs:155`. Test-coverage reviewer flagged this as W5 (design question, deferred). Codex escalates: multi-package failures classify by input order, so the same failing batch can exit with different codes depending on ordering. Exposed public contract for script consumers.

**Remediation**: Either document the "first error wins" rule in a rustdoc on `Error::classify`, OR pick a "most specific" kind (e.g., `AuthError > ConfigError > NotFound > Failure`). The rustdoc route is minimum. Reclassified as High, actionable.

### Codex-4 (actionable, partially captured): `ocx_mirror` breaking-change silence

`crates/ocx_mirror/src/error.rs:26`. Old codes `2/3/4` → new `65/79/1/69`. Plan flagged this explicitly in its Risks. Doc reviewer found no CHANGELOG entry. Codex reinforces: wrapper scripts matching historic codes will silently break. This folds into D-3 above; no new finding.

### Codex-5 (deferred): `ConfigInputs` public export without `#[non_exhaustive]`

`crates/ocx_lib/src/lib.rs:6` re-exports `ConfigInputs` whose own docs promise `cwd` and future caller-context fields will be added. Without `#[non_exhaustive]` the struct locks its shape. Fold into A-2 region; deferred pending the decision on whether `config::*` submodules stay `pub`.

**Codex Triage summary**: actionable=3 (all surfaced above as Block-2, Codex-3, and reinforced D-3); deferred=1 (Codex-5); stated-convention=2 probes came back clean (no new `#[source]`/`#[from]` regressions, no new `unsafe`/panic paths); trivia=1 (acceptance coverage skips naked `ocx`).

---

## Root-Cause Analysis

### R-1. Cluster: "classification dispatch has a single-point-of-failure for multi-error cases"

Findings clustered: Quality-H2 (`SourceWalkFailed` silent fall-through), Quality-W5 (batch `errors.first()`), Codex-3.

- **Why 1**: `PackageManagerError::FindFailed` wraps `Vec<PackageError>` with no `#[source]`, per architect-confirmed design.
- **Why 2**: To classify, the `Error::classify` impl pulls `errors.first()` and walks *its* kind.
- **Why 3**: Because `Vec<PackageError>` has no canonical "summary kind", the code picks the first — reasonable v1, but input-order dependent.
- **Why 4**: This pattern is repeated in `SourceWalkFailed` where `arc.as_error().classify()` is called directly, silently bypassing the chain walker, producing the same "silent first-error wins" behavior.
- **Why 5**: The `ClassifyExitCode` trait (spec-compliance S2, SOTA-deferred) distributes dispatch per-type; the plan's central-ladder design would have forced a single explicit "how to handle `Vec`" decision. The distributed ownership let each type make its own local choice.

**Systemic fix**: Either (a) add a priority function `Vec<PackageError> → ExitCode::max_by_priority()` so batches pick the "most specific" code deterministically, OR (b) document the "first error wins" rule in rustdoc and add an acceptance test that locks the behavior. Option (b) is lower cost; (a) is better UX for script consumers.

**Related findings**: spec-compliance S2 (trait-vs-free-function), SOTA deferred (same divergence), Quality-W5, Codex-3.

### R-2. Cluster: "`C-GOOD-ERR` normalization pass incomplete"

Findings clustered: Quality-B1 (4-5 violations across `oci/client`, `oci/index`, `profile`, `config/error.rs`), Quality-H3 (`ci_export`/`shell_env` dual-sentence anyhow messages).

- **Why 1**: The cleanup plan (directive #17) promised a comprehensive pass across 11 modules.
- **Why 2**: The pass was applied file-by-file but included manual judgments ("is the first word a sentence or an acronym?").
- **Why 3**: Files with compound messages (e.g., `profile::UnsupportedVersion` with a continuation sentence) were partially normalized — first clause fixed, continuation missed.
- **Why 4**: Files with terminal punctuation serving a rhetorical purpose (`FileTooLarge "did you point at the wrong file?"`) were left alone.
- **Why 5**: There's no lint/test for `C-GOOD-ERR` compliance — the check is manual. Claude spec-compliance reviewer spot-checked 6 files and reported "compliant"; the actual pass missed violations in the other 5.

**Systemic fix**: Add a structural check — a `#[cfg(test)]` walker that reflects over every `thiserror::Error` variant in a test build and asserts the Display string matches a regex (`^[a-z0-9"'_{\(]` for first char, no trailing `.?!:` except colons separating clauses, acronyms allowlisted). Or run clippy with a custom lint. This is the kind of invariant that can't survive a manual pass.

**Related findings**: Doc-D1 (no public exit-code table — same "manual discoverability fails at scale" pattern).

### R-3. Cluster: "`app.rs` bypass list is path-based, not completeness-based"

Findings clustered: Codex Block-2 (bare `ocx` regression), Test-coverage gap for naked invocation.

- **Why 1**: The malformed-config fix (`464ca5f`) enumerated `Command::Version` and `Command::Shell::Completion` as context-free.
- **Why 2**: The fix wrote `match &cli.command { Some(Version) => ..., Some(Shell::Completion) => ..., _ => {} }`.
- **Why 3**: The `_` arm caught `None` (no subcommand) AND every other unlisted command equally — all of them fall through to `Context::try_init`.
- **Why 4**: The subsequent help-print logic at line 77-79 runs AFTER `try_init` — so bare `ocx` only reaches help if config loads cleanly.
- **Why 5**: The test `test_help_still_works_with_malformed_config` tests `ocx --help` (clap flag, intercepted pre-`get_matches()`), not `ocx` alone. Pattern match vs branch analysis mismatch was invisible in review because the test name implied coverage.

**Systemic fix**: (1) Add `Some(None)` / `None` handling to the bypass list: `match &cli.command { ... Some(Version | Shell::Completion) => ..., None => { /* help */ ... }, Some(_) => {} }`. (2) Add explicit acceptance tests for every claimed-context-free invocation path, named after the path not the symptom (`test_bare_ocx_survives_malformed_config`, `test_ocx_help_survives_malformed_config` — distinct tests).

**Related findings**: Test-coverage High (three-layer downcast chain also relies on a non-obvious path that no test exercises directly — same "review-by-symptom-not-by-path" pattern).

---

## Deferred Findings

| ID | File | Reason deferred (human judgment) |
|---|---|---|
| S2 | `cli/classify.rs` | Trait-vs-free-function: both work. Decide whether to accept the trait pattern and update `quality-rust-exit_codes.md`, or refactor to match the plan. |
| S3 | `cli/classify.rs:24-27` | `derive(ClassifyExitCode)` TODO scope-creep — decide: capture as follow-up issue or remove comment. |
| S-3 | `config/loader.rs:212-218` | `$OCX_HOME` under sudo — product-level decision on threat model. |
| D1 | `config/loader.rs` | Concurrent config-load test — needs multi-process fixture design. |
| D2 | `test_offline.py:183-190` | `AuthError(80)` acceptance — fixture limitation; registry:2 can't inject 401 responses. |
| A-3 | `ocx_mirror::MirrorError` | Structural refactor to carry `anyhow::Error` for delegation — scope decision. |
| A-4 | `product-context.md` | Positioning update — product-owner call on whether backend-first + ambient-config is a new differentiator or a principle reword. |
| Perf | `Context::try_init` startup benchmark | Locks in startup budget — decide before #33 adds CWD walk. |

---

## Verdict rules applied

- **Request Changes** (chosen) — triggered by: unresolved Block-tier findings (B1 5 `C-GOOD-ERR` violations, B2 bare-`ocx` regression, A-1 stale ADR reference, D-1 missing exit-code docs, Codex Block-2 correctness regression), 1 security High (S-1 TOML-parse content disclosure), missing changelog for the declared breaking-change (D-3).
- Systemic causes affect ≥3 findings each in R-1 and R-2 — **three separate clusters** each with ≥2 related findings.
- Architect flagged: `product-context.md` protocol violation (A-4) and plan cross-reference drift (A-1).
- SOTA flagged: `deny.toml` MPL-2.0 provenance undocumented (actionable).
- Cross-model gate ran; cross-model win credited (Block-2).

## Handoff

Actionable findings exist. Recommend:

```
/swarm-execute high "apply swarm-review findings: Block (B1/B2/A-1/D-1/Codex-B2) and High (B1,H1,H2,H3, S-1 security, D-2/D-3 docs, test-coverage Highs)"
```

Or for targeted routing:

- `/builder` for the 5 `C-GOOD-ERR` normalizations (mechanical)
- `/builder` for the `app.rs:65-81` bare-`ocx` fix + new acceptance test
- `/security-auditor` to confirm the symlink-reject remediation design for S-1
- `/docs` for the exit-code table + CHANGELOG entry
- `/architect` for the `product-context.md` positioning update and the `MirrorError` refactor ADR

No commits were made. No code was modified by this review.
