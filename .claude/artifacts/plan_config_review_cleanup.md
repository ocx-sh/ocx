# Plan: Config System Review Cleanup

<!--
Implementation Plan
Filename: artifacts/plan_config_review_cleanup.md
Owner: Builder (/builder)
Handoff to: /swarm-execute
Related Skills: builder, qa-engineer, docs
Related Research: research_exit_codes.md, research_rust_error_messages.md
Parent artifact: plan_configuration_system.md (the feature this cleans up after)
-->

## Overview

**Status:** Draft (post Phase 6 Round 2 revisions)
**Author:** /swarm-plan (Claude Opus 4.6)
**Date:** 2026-04-13
**Branch:** `feat/config-system` (sits on top of commit `b407981`)
**Scope classification:** Medium (1–2 weeks, multi-subsystem)
**Reversibility:** Two-Way Door (Medium) — no durable external contracts except the exit-code numeric values, which are stable POSIX conventions

## Phase 6 Review Revisions

After the first draft of this plan was reviewed by spec-compliance + architect + researcher perspectives, the following changes were applied in Round 2:

- **Step 1.7 moved to Phase 4 (Step 4.0)**: the `tokio::join!` discovery refactor is an implementation change, not a stub — running it in Phase 1 would bypass the Phase 2 architecture review gate.
- **`classify_error` moved from `ocx_cli/src/main.rs` to `ocx_lib::cli::classify_error`** as a free function taking `&anyhow::Error`. Both binaries call it; `ocx_mirror` overlays `MirrorError::kind_exit_code` on top. Avoids DIP violation and duplicate dispatch ladders.
- **`-c` short flag decision inverted**: keep `-c` on `--config` (universal convention: docker, git, ssh, kubectl, make, tmux), drop `-c` from `package push --cascade`. The original draft was wrong about which flag was higher-frequency at the global scope.
- **Error normalization step order reversed**: audit for test assertions on old message strings BEFORE normalizing, not after. Two Hats Rule — never mix refactoring and breaking changes.
- **`ocx_mirror::MirrorError` display normalization promoted from deferred to in-scope (Step 4.7)**: ~6 string edits in `crates/ocx_mirror/src/error.rs`; avoids mixed-case chained error output.
- **`auth::error` variants added to exit-code taxonomy** — spec reviewer flagged silent fall-through to `Failure` for auth misconfigurations.
- **`OCX_NO_CONFIG` explicit kill-switch acceptance test added** (Test 3.2.10).
- **Phase 5 docs gated on Phase 4.4 completion** — docs cite normalized error message strings verbatim; they cannot run in parallel.
- **Reverted to `futures::future::join_all`** (NOT `tokio::join!`). Round 2 architect + spec-compliance reviewers flagged that `user_path()` and `home_path()` return `Option<PathBuf>`, so arity is **variable at runtime** (1–3 tiers). `tokio::join!` is fixed-arity at compile time and can't handle the optional tiers without building dummy futures. `futures::future::join_all` iterates a variable-length collection naturally. Confirmed by Round 1 researcher: `futures` is already pinned at 0.3.31 in `ocx_lib/Cargo.toml` — no new dependency.
- **`MirrorError::ExecutionFailed` delegation dropped** (Round 3 Block fix). The variant is `ExecutionFailed(Vec<String>)` — stringified messages without a structured inner error, so `classify_error(inner)` delegation would fail to compile. Maps to a fixed `ExitCode::Failure (1)` for now. A follow-up issue should refactor `MirrorError::ExecutionFailed` to carry a structured `anyhow::Error` so delegation becomes possible — but that's out of scope for this cleanup.
- **Step 1.3 + 1.4 merged** into a single step (was a latent ordering trap — re-exporting a type before it exists breaks `cargo check`).
- **Three-layer error downcast pseudocode** added to Step 4.2 so the builder doesn't have to rediscover the `Error → PackageError → PackageErrorKind` traversal.
- **Stale "Out of Scope" line removed** — `MirrorError` display normalization was promoted to in-scope in Round 2 (Step 4.6) but the deprecated line contradicted it.
- **Rename alternatives documented**: `RegistryDefaults` (user suggestion) remains the default; `RegistryDefaults` (architect alternative) is presented as a superior semantic alternative for human decision.
- **`log::error!` clarification**: the `log::` in `main.rs` is `ocx_lib::log` which re-exports `tracing_log` — no `tracing` migration question exists.
- **`ocx_mirror` exit-code breaking change** flagged explicitly in Risks + commit body.
- **`join_all` → `tokio::join!` comment requirement** added so reviewers can see order-preservation invariant.

## Objective

Address every actionable finding from `/swarm-review` on the `feat/config-system` branch, plus four user-directed additions:

1. Formalize exit codes as an `ExitCode` enum in `ocx_lib::cli` that both `ocx` and `ocx_mirror` consume
2. Rename `RegistryGlobals` → `RegistryDefaults` (keep `RegistryConfig` as-is)
3. Normalize all `ocx_lib` error messages to Rust API Guidelines style
4. Resolve the `--no-config` question by clarifying docs, not by adding a CLI flag

## Review Findings Addressed

Every item below maps to a reviewer finding or an explicit user directive. Deferred items are listed separately at the end.

| # | Finding | Source | Severity | Phase |
|---|---|---|---|---|
| 1 | macOS user-tier path is wrong on 3 doc pages | doc-reviewer | **Block** | Phase 5 |
| 2 | `Error::Io` variant missing from Error Reference table | doc-reviewer | Warn | Phase 5 |
| 3 | `OCX_HOME` entry doesn't mention config discovery | doc-reviewer | Warn | Phase 5 |
| 4 | `command-line.md:75–81` `--config` section under-documented | doc-reviewer | Warn | Phase 5 |
| 5 | Help text omits `OCX_CONFIG_FILE` and `OCX_NO_CONFIG` | cli-ux | Warn | Phase 4 |
| 6 | ~~`-c` short flag collides with `package push --cascade`~~ — **false positive**, Round 4: `ContextOptions` is `#[command(flatten)]` at the root (no `global = true`), so `-c` on `--config` is consumed before the subcommand and does not collide with `-c` on `--cascade` inside `package push`. No change needed | cli-ux | ~~Warn~~ | dropped |
| 7 | `RegistryDefaults` not re-exported from `lib.rs` (after rename) | rust-quality + user | Warn | Phase 4 |
| 8 | `#[must_use]` missing on `Config::resolved_default_registry` | rust-quality | Suggest→Actionable | Phase 4 |
| 9 | `join_all` for `try_exists` in `discover_paths` | perf | Warn | Phase 4 |
| 10 | Acceptance test gap: `FileTooLarge` error UX | test-coverage | Warn | Phase 3 |
| 11 | Acceptance test gap: `--config` overrides `OCX_CONFIG_FILE` (both set, no `OCX_NO_CONFIG`) | test-coverage | Warn | Phase 3 |
| 12 | Acceptance test gap: layered merge precedence ($OCX_HOME + `--config`) | test-coverage | Warn | Phase 3 |
| 13 | Unit test gap: `RegistryConfig::merge` None-preserves | test-coverage | Warn | Phase 3 |
| 14 | Tighten `load_and_merge_rejects_non_regular_file` assertion | test-coverage | Warn | Phase 3 |
| 15 | Rename `RegistryGlobals` → `RegistryDefaults` | **user directive** | — | Phase 4 |
| 16 | Exit-code enum in `ocx_lib::cli::ExitCode` | **user directive** | — | Phases 1–4 |
| 17 | Error message normalization across `ocx_lib` | **user directive** | — | Phase 4 |
| 18 | Docs for `--no-config`: clarify env-only design | **user directive** | — | Phase 5 |
| 19 | Consolidate `ocx_cli` error prefix: drop `"Error: "` duplication | rust-quality | Warn | Phase 4 |

## Scope

### In Scope

- **`ocx_lib::cli::ExitCode` enum** with sysexits-aligned values, `Into<std::process::ExitCode>`, `#[non_exhaustive]`
- **Exit-code dispatch** in both `ocx_cli/src/main.rs` and `ocx_mirror/src/main.rs`, downcasting the anyhow chain
- **`RegistryDefaults` rename** with callsite updates in `ocx_lib::config::*` and tests
- **Re-exports** of `RegistryDefaults` and `RegistryConfig` from `crates/ocx_lib/src/lib.rs`
- **`#[must_use]`** on `Config::resolved_default_registry`
- **`discover_paths`** perf fix: parallel `try_exists` via `futures::future::join_all` (preserving vector order)
- **Error message normalization pass** across all `ocx_lib` error modules identified in `research_rust_error_messages.md` (error.rs, auth/error.rs, oci/client/error.rs, oci/index/error.rs, oci/digest/error.rs, package_manager/error.rs, file_structure/error.rs, archive/error.rs, compression/error.rs, ci/error.rs, package/error.rs). Acronyms (JSON, TOML, CI, HTTP, I/O) retain canonical case
- **Help text expansion** for `--config` to mention `OCX_CONFIG_FILE` and `OCX_NO_CONFIG`
- **`-c` short flag collision resolution**: drop the short from `--config` (keep long-only). Cascade keeps `-c` because it is the higher-frequency interactive flag within `package push`
- **Unit test additions**: `RegistryConfig::merge` None-preserves, tightened non-regular-file assertion
- **Acceptance test additions**: FileTooLarge, layered merge, `--config` wins over env
- **Doc fixes** across `configuration.md`, `environment.md`, `user-guide.md`, `command-line.md`, `getting-started.md`:
  - macOS platform note on user tier paths (all occurrences)
  - `Error::Io` row in Error Reference table
  - `OCX_HOME` entry mentions `$OCX_HOME/config.toml` discovery
  - `--config` reference fleshed out
  - `OCX_NO_CONFIG` rationale: "env var is the idiomatic hermetic-CI mechanism, no CLI flag needed"

### Out of Scope (Deferred)

- **`etcetera` crate swap** — v2 concern; would require migrating all `dirs::*` callers across OCX
- **`miette` + `toml_edit` for span-highlighted parse errors** — v2 quality upgrade
- **Clippy `unwrap_used`/`expect_used` workspace lint** — separate hygiene pass; requires workspace-wide audit
- **Registry URL/alias hostname validation** — belongs at the OCI client layer where the value is consumed for TLS SNI, not in the config loader
- **`/proc`-bypass bounded-read second check test** — author documented the synthetic case is infeasible to simulate portably; track in a follow-up issue if needed
- **Refactoring `MirrorError::ExecutionFailed(Vec<String>)` to carry a structured error** — the current `Vec<String>` shape prevents `classify_error` delegation; refactoring to `ExecutionFailed(anyhow::Error)` would unlock per-inner-error exit-code dispatch. Follow-up issue. For now, `ExecutionFailed` maps to a fixed `Failure (1)`.
- **Context::clone overhead** — already confirmed net positive by perf reviewer
- **New `--no-config` CLI flag** — explicitly rejected per user directive; docs clarification substitutes
- **`String::with_capacity` preallocation at loader.rs:164** — immeasurable; not worth the diff noise

## Research

- [`research_exit_codes.md`](./research_exit_codes.md) — sysexits.h alignment rationale, real-world tool survey, proposed enum shape, OCX error→code mapping
- [`research_rust_error_messages.md`](./research_rust_error_messages.md) — canonical style rule from Rust API Guidelines, cargo/jj/thiserror evidence, full OCX violation inventory
- [`plan_configuration_system.md`](./plan_configuration_system.md) — the parent feature plan that this cleans up
- [`research_configuration_patterns.md`](./research_configuration_patterns.md) — baseline research that informed the feature

## Technical Approach

### Key Design Decisions

| Decision | Rationale |
|---|---|
| Exit code values align with `sysexits.h` (64–78) | POSIX reference since 1988; Rust CLI Book endorses it; 60-slot reserved range; scripts can `case $?` |
| Own the enum in `ocx_lib::cli` instead of depending on `sysexits` crate | Zero external coupling; enum is trivial; both binaries are internal to the workspace |
| `#[repr(u8)]` + `Into<std::process::ExitCode>` | Concrete numeric repr is load-bearing; Into lets `main.rs` return via `?` |
| `#[non_exhaustive]` on the enum | Adding a variant in v2 is not a semver break |
| Rename `RegistryGlobals` → `RegistryDefaults` | Round 4 user decision: `RegistryDefaults` surfaces the *role* (fallback values for bare identifiers) and is semantically clearer than `RegistryDefaults`. Test bodies like `RegistryDefaults { default: Some("ghcr.io") }` read as "the defaults are set to ghcr.io", which is unambiguous. `RegistryConfig` stays as-is (per-entry `[registries.<name>]` shape) |
| Keep `RegistryConfig` as-is | User directive. It names the per-entry config shape and already matches `LocalConfig`/`RemoteConfig` pattern elsewhere |
| Error messages: lowercase first letter, no trailing punctuation | Rust API Guidelines `C-GOOD-ERR`; canonical across cargo, thiserror, jj lib layers |
| Acronyms retain canonical case (JSON, TOML, CI, HTTP, I/O) | The rule applies to the first *word*, not proper nouns or initialisms — matches cargo and `std::error::Error` examples |
| Keep `-c` on `--config`, leave `--cascade -c` alone | Round 4 code verification: `ContextOptions` is `#[command(flatten)]` at the `Cli` root with no `global = true`. Clap consumes `-c` for `--config` before the subcommand is parsed; inside `package push` the subcommand has its own `-c` scope bound to `--cascade`. There is no runtime collision. The original reviewer claim of simultaneous `-c` definitions was incorrect — different parse scopes, no conflict. No code change to `--cascade`'s short flag |
| `ocx_cli`'s `main.rs` drops the hardcoded `"Error: "` prefix | Already-lowercase lib messages compose cleanly; `ocx_mirror` already uses `{err:#}` without a prefix. Consistency across both binaries. Note: the `log::error!` macro in `main.rs` is `ocx_lib::log` which re-exports `tracing_log` — no `tracing` migration is needed, this is purely a formatter change |
| No `--no-config` CLI flag | User directive. `OCX_NO_CONFIG=1` env var is the idiomatic hermetic-CI mechanism; a CLI flag would duplicate surface without solving a new problem |
| macOS platform note in docs, not `etcetera` crate swap | Scope-bounded fix; `dirs::config_dir()` on macOS returns `~/Library/Application Support` which is correct per Apple HIG; the escape hatch `$OCX_HOME/config.toml` is always available regardless of platform |
| Parallel `try_exists` via `futures::future::join_all` | Filesystem syscalls are order-independent; the push order in `candidates` already fixes precedence, and `join_all` preserves input order. `tokio::join!` was proposed in Round 2 but rejected in Round 3 — `user_path()` and `home_path()` return `Option<PathBuf>`, giving variable runtime arity (1–3), which the compile-time macro cannot handle. `futures` crate is already a dep (0.3.31). Add an order-preservation comment above the call so reviewers can see the invariant |

### Architecture Changes

#### New modules: `crates/ocx_lib/src/cli/exit_code.rs` + `crates/ocx_lib/src/cli/classify.rs`

```rust
// Full enum per research_exit_codes.md
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[non_exhaustive]
pub enum ExitCode {
    Success = 0,
    Failure = 1,
    UsageError = 64,
    DataError = 65,
    Unavailable = 69,
    IoError = 74,
    TempFail = 75,
    PermissionDenied = 77,
    ConfigError = 78,
    NotFound = 79,
    AuthError = 80,
    OfflineBlocked = 81,
}

impl From<ExitCode> for std::process::ExitCode { ... }
```

Re-exported from `ocx_lib::cli` alongside existing `LogLevel`, `LogSettings`, `ColorModeConfig`, etc.

#### Exit-code dispatch — shared in `ocx_lib::cli::classify_error`

Free function `ocx_lib::cli::classify_error(err: &anyhow::Error) -> ExitCode` walks the anyhow chain via `err.chain()`, downcasts to each `ocx_lib` error subtree, and returns the first match. Default fallback is `ExitCode::Failure`. Both binaries call it.

**Rationale for placement in `ocx_lib`, not each binary's `main.rs`**: the dispatch logic is entirely about `ocx_lib` error types. Duplicating it into `ocx_cli/main.rs` and (separately) `ocx_mirror/main.rs` would drift — the next lib error variant added would require editing both binaries' dispatch ladders. Single source of truth prevents drift. Keeps `main.rs` thin (3 lines: `let code = ocx_lib::cli::classify_error(&err); log::error!("{err:#}"); code.into()`).

#### `ocx_mirror`'s `MirrorError` overlay

`ocx_mirror`'s outer error type `MirrorError` wraps `ocx_lib` errors (via `ExecutionFailed(anyhow::Error)`) but also has its own variants (`SpecInvalid`, `SpecNotFound`, `SourceError`). The shared classifier handles the inner lib errors automatically; `MirrorError::kind_exit_code(&self)` handles the mirror-only variants:
- `SpecInvalid` → `DataError (65)`
- `SpecNotFound` → `NotFound (79)`
- `ExecutionFailed(inner)` → delegate to `classify_error(inner)`
- `SourceError` → `Unavailable (69)` or `TempFail (75)` based on inner cause

`ocx_mirror/src/main.rs` calls `err.kind_exit_code()` which in turn may call `ocx_lib::cli::classify_error` for the delegation case.

#### `ocx_cli/src/main.rs` (sketch)

```rust
use ocx_lib::cli::{classify_error, ExitCode};

// ... inside main ...
match app::run().await {
    Ok(exit) => exit,
    Err(err) => {
        ocx_lib::log::error!("{err:#}");      // no "Error: " prefix; matches ocx_mirror style
        classify_error(&err).into()            // ExitCode → std::process::ExitCode
    }
}
```

#### Rename + re-export

`crates/ocx_lib/src/config.rs`:
- `RegistryGlobals` → `RegistryDefaults` (struct name, field doc comments, all call sites)
- `Config::registry: Option<RegistryDefaults>` (renamed field type)

`crates/ocx_lib/src/lib.rs`:
```rust
pub use config::{Config, RegistryDefaults, RegistryConfig};
pub use config::loader::{ConfigInputs, ConfigLoader};
```

(`RegistryDefaults` becomes the name users see; `RegistryGlobals` ceases to exist in any public API.)

### User Experience Scenarios

| User Action | Expected Outcome | Error Cases |
|---|---|---|
| `ocx install cmake:3.28` (happy path) | Exit 0 | — |
| `ocx --config /bad/path.toml install cmake:3.28` | Exit **79** `NotFound`; stderr: `config file not found: /bad/path.toml (check --config or OCX_CONFIG_FILE)` | — |
| `ocx --config huge.toml install cmake:3.28` where huge.toml > 64 KiB | Exit **78** `ConfigError`; stderr: `config file huge.toml exceeds maximum allowed size (N bytes > 65536 bytes); OCX config files are typically under 1 KiB — did you point at the wrong file?` | — |
| `ocx --config bad.toml install cmake:3.28` with TOML parse error | Exit **78** `ConfigError`; stderr: `invalid TOML at bad.toml: <parse error>` | — |
| `OCX_CONFIG_FILE=a.toml ocx --config b.toml install cmake:3.28` with conflicting `[registry] default` in each | Exit 0; `b.toml`'s value wins (acceptance test Added) | — |
| `OCX_NO_CONFIG=1 ocx install cmake:3.28` | Exit 0; discovered chain fully suppressed | — |
| `ocx --help` | `--config <FILE>` appears in global options with doc comment: "Path to the ocx configuration file. Can also be set via `OCX_CONFIG_FILE`. To disable discovery entirely, set `OCX_NO_CONFIG=1`." | — |
| `ocx package push --cascade foo bar` | `-c` is the short for `--cascade`; `--config` has no short | — |
| `ocx install nonexistent_pkg:0` | Exit **79** `NotFound`; stderr: `package not found: nonexistent_pkg` (normalized message) | — |
| `ocx install cmake:3.28 --offline` when package not cached | Exit **81** `OfflineBlocked`; stderr: `network operation attempted in offline mode` (normalized message) | — |
| `ocx install cmake:3.28` with expired auth token | Exit **80** `AuthError`; stderr: `registry authentication failed: ...` (normalized message) | — |

### Error Taxonomy (Exit Code Mapping)

Per `research_exit_codes.md`. Expanded after Phase 6 review to cover all `ocx_lib` error subtrees. Any subtree marked **`Failure (1)` — fall-through** is a deliberate v1 gap; a test locks in the fall-through behavior so it cannot silently become "I meant to classify this":

```
# config subsystem
config::Error::FileNotFound              → ExitCode::NotFound (79)
config::Error::FileTooLarge              → ExitCode::ConfigError (78)
config::Error::Parse                     → ExitCode::ConfigError (78)
config::Error::Io                        → ExitCode::IoError (74)
config::Error::InvalidBooleanString      → ExitCode::DataError (65)

# oci client
oci::client::Authentication              → ExitCode::AuthError (80)
oci::client::ManifestNotFound            → ExitCode::NotFound (79)
oci::client::Io                          → ExitCode::IoError (74)
oci::client::Registry (connect/timeout)  → ExitCode::Unavailable (69)
oci::client::Registry (HTTP 429/503)     → ExitCode::TempFail (75)
oci::client::Registry (HTTP 401/403)     → ExitCode::AuthError (80)
oci::client::InvalidManifest             → ExitCode::DataError (65)
oci::client::DigestMismatch              → ExitCode::DataError (65)
oci::client::UnexpectedManifestType      → ExitCode::DataError (65)
oci::client::Serialization               → ExitCode::DataError (65)
oci::client::InvalidEncoding             → ExitCode::DataError (65)

# oci index / digest / identifier / platform
oci::index::RemoteManifestNotFound       → ExitCode::NotFound (79)
oci::index::TagLockRepositoryMismatch    → ExitCode::DataError (65)
oci::digest::Invalid                     → ExitCode::DataError (65)
oci::identifier::*                       → ExitCode::DataError (65) — all parse/structure errors
oci::platform::*                         → ExitCode::DataError (65)

# package manager
package_manager::PackageErrorKind::NotFound       → ExitCode::NotFound (79)
package_manager::PackageErrorKind::Ambiguous      → ExitCode::DataError (65)
package_manager::PackageErrorKind::NoCandidate    → ExitCode::NotFound (79)
package_manager::PackageErrorKind::NoSelected     → ExitCode::NotFound (79)
package_manager::PackageErrorKind::TaskPanicked   → ExitCode::Failure (1)
package_manager::DependencyError::Conflict       → ExitCode::DataError (65)
package_manager::DependencyError::SetupFailed    → ExitCode::Failure (1) — fall-through (delegates to inner)

# auth
auth::Error::InvalidType                 → ExitCode::ConfigError (78)
auth::Error::MissingEnv                  → ExitCode::ConfigError (78)
auth::Error::DockerCredentialRetrieval   → ExitCode::AuthError (80)

# file structure
file_structure::Error::MissingDigest     → ExitCode::DataError (65)

# archive
archive::Error::Io                       → ExitCode::IoError (74)
archive::Error::Tar                      → ExitCode::DataError (65)
archive::Error::Zip                      → ExitCode::DataError (65)
archive::Error::EntryEscape              → ExitCode::DataError (65) — malformed input
archive::Error::SymlinkEscape            → ExitCode::DataError (65) — malformed input
archive::Error::UnsupportedFormat        → ExitCode::DataError (65)
archive::Error::Internal                 → ExitCode::Failure (1) — fall-through

# compression
compression::Error::UnknownFormat        → ExitCode::DataError (65)
compression::Error::Open                 → ExitCode::IoError (74)
compression::Error::Create               → ExitCode::IoError (74)
compression::Error::EngineInit           → ExitCode::Failure (1) — fall-through
compression::Error::Io                   → ExitCode::IoError (74)

# ci
ci::Error::MissingEnv                    → ExitCode::ConfigError (78)
ci::Error::File                          → ExitCode::IoError (74)

# package
package::Error::VersionInvalid           → ExitCode::DataError (65)
package::Error::UnsupportedLogoFormat    → ExitCode::DataError (65)
package::Error::RequiredPathMissing      → ExitCode::NotFound (79)

# profile
profile::Error::Io                       → ExitCode::IoError (74)
profile::Error::Json                     → ExitCode::DataError (65)
profile::Error::UnsupportedVersion       → ExitCode::DataError (65)
profile::Error::Locked                   → ExitCode::TempFail (75)
profile::Error::ContentModeRequiresDigest → ExitCode::DataError (65)

# top-level ocx_lib::Error
ocx_lib::Error::OfflineMode              → ExitCode::OfflineBlocked (81)
ocx_lib::Error::InternalFile             → ExitCode::IoError (74)
ocx_lib::Error::InternalPathInvalid      → ExitCode::Failure (1) — internal invariant
ocx_lib::Error::SerializationFailure     → ExitCode::DataError (65)
ocx_lib::Error::UnsupportedMediaType     → ExitCode::DataError (65)

# std::io
std::io::ErrorKind::PermissionDenied     → ExitCode::PermissionDenied (77)

# mirror-specific (via MirrorError::kind_exit_code)
MirrorError::SpecInvalid                 → ExitCode::DataError (65)
MirrorError::SpecNotFound                → ExitCode::NotFound (79)
MirrorError::ExecutionFailed(Vec<String>) → ExitCode::Failure (1) — see footnote
MirrorError::SourceError                 → ExitCode::Unavailable (69)

# Footnote: MirrorError::ExecutionFailed currently wraps Vec<String> (stringified
# error messages) rather than a structured inner error. This prevents
# classify_error delegation from compiling. V1 maps to Failure (1); follow-up
# issue tracks refactoring the variant to carry anyhow::Error so per-cause exit
# codes can be surfaced through the mirror pipeline.

# synthetic / terminal
clap parse failure                       → ExitCode::UsageError (64)
Panic / JoinError / unknown subtree      → ExitCode::Failure (1)
```

Deferred to human judgment (two findings from the spec-compliance reviewer):
- `ci::Error::MissingEnv` — `ConfigError (78)` vs `UsageError (64)`. Plan defaults to `ConfigError` since a missing CI env var means the environment is misconfigured (the user didn't invoke something wrong).
- Archive escape errors — `DataError (65)` vs `PermissionDenied (77)`. Plan defaults to `DataError` since escapes are malformed-input artifacts, not access violations.

Both defaults are documented above; a human may override during Phase 4 implementation without re-planning.

## Implementation Steps

> Contract-First TDD. Phases: Stub → Architecture Review → Specify → Implement → Review & Docs.

### Phase 1: Stubs

Pure API-surface scaffolding. Bodies are `unimplemented!()` where dispatch logic is required. Non-behavior changes (renames, re-exports, attribute additions, flag edits, doc edits) go here because they are safe to land even without implementation.

- [ ] **Step 1.1** Create `crates/ocx_lib/src/cli/exit_code.rs` with the full `ExitCode` enum, `#[repr(u8)]`, `#[non_exhaustive]`, all 12 variants, and `impl From<ExitCode> for std::process::ExitCode`. **No dispatch logic yet** — just the enum.
  - Public API: `pub enum ExitCode { ... }`, `impl From<ExitCode> for std::process::ExitCode`
  - Module wire-up: add `pub mod exit_code;` and `pub use exit_code::ExitCode;` to `crates/ocx_lib/src/cli.rs`

- [ ] **Step 1.2** Create `crates/ocx_lib/src/cli/classify.rs` with `pub fn classify_error(_err: &anyhow::Error) -> ExitCode { unimplemented!() }`. Re-export from `cli.rs`: `pub use classify::classify_error;`. Public signature only — dispatch logic lands in Phase 4.

- [ ] **Step 1.3** Rename `RegistryGlobals` → `RegistryDefaults` **and** add the re-exports **in the same atomic step** (merging Round 2's 1.3 and 1.4 — splitting them created a latent ordering trap where `cargo check` would fail between steps). Default to `RegistryDefaults` per user directive; may be substituted with `RegistryDefaults` if the human prefers the role-based name from the Architect review.
  - `crates/ocx_lib/src/config.rs`: struct definition, doc comments, `Config::registry: Option<_>` field type, `Config::merge` body, `RegistryGlobals::merge` impl, all inline unit test bodies. Add/update `pub use self::registry::{RegistryDefaults, RegistryConfig};` (if the struct lives in a submodule) so the type is reachable via `config::RegistryDefaults`.
  - `crates/ocx_lib/src/config/registry.rs`: the `/// [\`super::RegistryGlobals::default\`]` doc comment at line 21.
  - `crates/ocx_lib/src/lib.rs`: add `pub use config::{RegistryDefaults, RegistryConfig};` alongside the existing `Config`, `ConfigInputs`, `ConfigLoader` re-exports.
  - Any other file containing the string `RegistryGlobals` (use `rg "RegistryGlobals"` from workspace root as a final verification).
  - **Prefer LSP rename-references if available** (per MEMORY feedback); fall back to `rg` + `Edit`.

- [ ] **Step 1.4** Add `#[must_use]` to `Config::resolved_default_registry` at `crates/ocx_lib/src/config.rs:84`.

- [ ] **Step 1.5** Expand the doc comment on `ContextOptions::config` at `context_options.rs:11` to mention env equivalents. No flag-attribute changes — `-c` stays bound to `--config` at the root scope.
    ```rust
    /// Path to the ocx configuration file.
    ///
    /// Can also be set via the `OCX_CONFIG_FILE` environment variable.
    /// To disable config discovery entirely, set `OCX_NO_CONFIG=1`.
    #[arg(short, long, value_name = "FILE")]
    pub config: Option<std::path::PathBuf>,
    ```

- [ ] **Step 1.6** Stub `MirrorError::kind_exit_code(&self) -> ocx_lib::cli::ExitCode` in `crates/ocx_mirror/src/error.rs`. Replace the existing `exit_code()` call in `crates/ocx_mirror/src/main.rs:71` with the new method. Leave the body `unimplemented!()`.

**Files created / modified in Phase 1 (Steps 1.1–1.6):**

| File | Action |
|---|---|
| `crates/ocx_lib/src/cli/exit_code.rs` | Create |
| `crates/ocx_lib/src/cli/classify.rs` | Create (stub body) |
| `crates/ocx_lib/src/cli.rs` (module root) | Modify (new re-exports) |
| `crates/ocx_lib/src/lib.rs` | Modify (re-exports, atomic with Step 1.3) |
| `crates/ocx_lib/src/config.rs` | Modify (rename + `#[must_use]` + re-export `RegistryDefaults`) |
| `crates/ocx_lib/src/config/registry.rs` | Modify (doc comment reference) |
| `crates/ocx_cli/src/app/context_options.rs` | Modify (expand `--config` doc comment only — no flag change) |
| `crates/ocx_cli/src/main.rs` | Modify (call `classify_error` + drop `"Error: "` prefix; body wired but classify returns fallback) |
| `crates/ocx_mirror/src/main.rs` | Modify (call new method) |
| `crates/ocx_mirror/src/error.rs` | Modify (add `kind_exit_code` stub) |

**Gate:** `cargo check --workspace` compiles. `classify_error` returns `unimplemented!()`; tests for it in Phase 3 are expected to fail.

### Phase 2: Architecture Review

Launch `worker-reviewer` (focus: `spec-compliance`, phase: `post-stub`) to verify:
- `ExitCode` enum surface matches `research_exit_codes.md`
- `RegistryDefaults` rename is complete (no dangling `RegistryGlobals` references anywhere)
- Re-export set in `lib.rs` matches the design
- `classify_error` stub exists and is wired
- `MirrorError::kind_exit_code` stub exists and is wired

**Gate:** Review passes. Any actionable findings fixed before Phase 3.

### Phase 3: Specification Tests

#### 3.1 Unit tests (inline in Rust files)

- [ ] **Test 3.1.1** — `ExitCode` → `std::process::ExitCode` conversion for every variant. Verifies each numeric value matches the design record (spec-driven, not implementation-driven).
  - File: `crates/ocx_lib/src/cli/exit_code.rs` (inline `#[cfg(test)] mod tests`)
- [ ] **Test 3.1.2** — `RegistryConfig::merge` preserves `Some(url)` in lower when higher is `None`.
  - File: `crates/ocx_lib/src/config.rs` (inline tests module — adjacent to the existing `merge_none_in_higher_does_not_clobber_lower` test for the scalar)
- [ ] **Test 3.1.3** — Tighten `load_and_merge_rejects_non_regular_file` at `crates/ocx_lib/src/config/loader.rs:326` to assert on the specific substring `"config path is not a regular file"` (the string injected at `loader.rs:148`).
  - File: `crates/ocx_lib/src/config/loader.rs` (modify existing test)
- [ ] **Test 3.1.4** — `classify_error` returns the correct `ExitCode` variant for every error in the taxonomy table above. One test case per row. Use constructed `anyhow::Error`s wrapping synthetic lib errors.
  - File: `crates/ocx_cli/src/main.rs` (inline `#[cfg(test)] mod tests`) or a new helper module

#### 3.2 Acceptance tests (Python pytest)

- [ ] **Test 3.2.1** — `test_file_too_large_errors_with_helpful_message`: write 65 KiB+1 of `#` comments to `$OCX_HOME/config.toml`, run `ocx install nonexistent:0`, assert exit code **78** and stderr contains `"exceeds maximum allowed size"` and `"did you point at the wrong file"`.
  - File: `test/tests/test_config.py`
- [ ] **Test 3.2.2** — `test_explicit_config_overrides_env_var_config_file`: set `OCX_CONFIG_FILE=env.toml` with `default = "env.example"` AND pass `--config cli.toml` with `default = "cli.example"`, assert `cli.example` wins. **Do not set `OCX_NO_CONFIG`.**
  - File: `test/tests/test_config.py`
- [ ] **Test 3.2.3** — `test_layered_merge_home_tier_and_explicit_config`: write `$OCX_HOME/config.toml` with one `[registries.shared]` entry and pass `--config extra.toml` with a different `[registries.other]` entry, assert **both** are resolvable in the merged config. This proves additive layering (not suppression).
  - File: `test/tests/test_config.py`
- [ ] **Test 3.2.4** — `test_exit_code_on_config_not_found`: run `ocx --config /tmp/nonexistent.toml index catalog`, assert exit code **79** (`NotFound`).
  - File: `test/tests/test_config.py`
- [ ] **Test 3.2.5** — `test_exit_code_on_config_parse_error`: write malformed TOML to `$OCX_HOME/config.toml`, assert exit code **78** (`ConfigError`).
  - File: `test/tests/test_config.py`
- [ ] **Test 3.2.6** — `test_exit_code_on_offline_blocks_fetch`: run `ocx --offline install nonexistent:0` with no cached package, assert exit code **81** (`OfflineBlocked`).
  - File: `test/tests/test_offline.py` (or new file `test_exit_codes.py`)
- [ ] **Test 3.2.7** — `test_exit_code_on_auth_failure`: simulated via a registry serving 401 for a bogus-auth attempt; assert exit code **80** (`AuthError`). *(If the existing registry fixture cannot easily simulate auth failures, defer to a follow-up issue with a link.)*
- [ ] **Test 3.2.8** — `test_cli_help_mentions_config_env_vars`: run `ocx --help`, assert the `--config` block contains both `OCX_CONFIG_FILE` and `OCX_NO_CONFIG` strings.
  - File: `test/tests/test_config.py`
- [ ] **Test 3.2.9** — `test_no_config_env_var_suppresses_discovery`: write a home config with `default = "should-be-ignored.example"`, set `OCX_NO_CONFIG=1`, run `ocx install nonexistent:0`, assert `should-be-ignored.example` does NOT appear in output. Locks in the kill-switch scenario from the UX table.
  - File: `test/tests/test_config.py` (may already partially exist — extend if so)

**Gate:** Tests compile (Rust) or parse (Python) and fail with `unimplemented!()` / `NotImplementedError` / wrong exit code — proving they test the contract.

### Phase 4: Implementation

Order matters: perf refactor → audit → normalize → dispatch → mirror → verify. The audit must happen BEFORE normalization so any existing test assertions on old error strings can be fixed atomically alongside the change (Two Hats Rule: refactoring doesn't mix with test modifications in separate commits).

- [ ] **Step 4.0** Convert the sequential `try_exists` loop at `crates/ocx_lib/src/config/loader.rs:103–107` to `futures::future::join_all`. The existing `candidates: Vec<PathBuf>` shape naturally handles the variable arity from the `Option<PathBuf>` tiers. `futures` is already a dep at 0.3.31 — no Cargo.toml change needed.

    ```rust
    use futures::future::join_all;

    // ... candidates: Vec<PathBuf> is already built with the optional tiers
    // filtered at push time (existing behavior preserved).

    // join_all preserves input order — precedence semantics unchanged.
    // Running the three try_exists calls concurrently shaves two sequential
    // filesystem round-trips on startup at zero semantic risk.
    let checks = join_all(candidates.iter().map(|p| tokio::fs::try_exists(p))).await;

    let existing: Vec<PathBuf> = candidates
        .into_iter()
        .zip(checks)
        .filter_map(|(p, r)| matches!(r, Ok(true)).then_some(p))
        .collect();
    ```

    **Rationale for `join_all` over `tokio::join!`**: Round 2 proposed `tokio::join!` for fixed arity, but `Self::user_path()` and `Self::home_path()` return `Option<PathBuf>` — the candidate count is 1, 2, or 3 at runtime. `tokio::join!` is fixed-arity at compile time. `join_all` iterates a variable-length collection naturally. Both preserve input order.

- [ ] **Step 4.1** **Pre-normalization audit**: grep the test suite for any assertion that depends on a non-compliant error message string. Record the findings in the commit body so the fix in Step 4.4 is atomic with the message change.

    ```sh
    # Prefix/framing strings
    grep -rn 'Error:' test/tests/ crates/*/test/ crates/ocx_lib/src/**/tests.rs
    # Capitalized leading words that will change
    grep -rniE '(Invalid |Failed |Unsupported |Required |Conflicting |Cannot |Expected |Manifest |Registry |Authentication |Archive |Compression |Tar |Zip |Symlink |Remote |Tag lock |Dependency |Identifier )' test/tests/ crates/*/test/
    # Specific full-sentence matches
    grep -rn 'A network operation was attempted' test/ crates/
    grep -rn 'Is this running inside a CI system' test/ crates/
    ```

    For each hit, classify: "must fix alongside Step 4.4" vs "unrelated, leave alone". Attach the classified list to the Step 4.4 commit so a reviewer can trace the change.

- [ ] **Step 4.2** Implement `classify_error` in `crates/ocx_lib/src/cli/classify.rs` per the error taxonomy table. Walk `err.chain()`, downcast to each lib-layer error subtree, return the first match. **The downcast target is `ocx_lib::Error` (the top-level enum), not the inner `PackageErrorKind` directly** — the three-layer error pattern (`Error → PackageError → PackageErrorKind`) means you must match through the outer variant before inspecting the inner kind. Default fallback is `ExitCode::Failure`.

    Pseudocode sketch:

    ```rust
    pub fn classify_error(err: &anyhow::Error) -> ExitCode {
        for cause in err.chain() {
            // Top-level ocx_lib::Error variants — three-layer pattern
            if let Some(e) = cause.downcast_ref::<ocx_lib::Error>() {
                return match e {
                    ocx_lib::Error::OfflineMode => ExitCode::OfflineBlocked,
                    ocx_lib::Error::InternalFile { .. } => ExitCode::IoError,
                    ocx_lib::Error::SerializationFailure(_) => ExitCode::DataError,
                    ocx_lib::Error::UnsupportedMediaType { .. } => ExitCode::DataError,
                    ocx_lib::Error::InternalPathInvalid(_) => ExitCode::Failure,
                    ocx_lib::Error::PackageManager(pe) => match pe.kind() {
                        PackageErrorKind::NotFound | PackageErrorKind::NoCandidate | PackageErrorKind::NoSelected
                            => ExitCode::NotFound,
                        PackageErrorKind::Ambiguous { .. } => ExitCode::DataError,
                        PackageErrorKind::SymlinkRequiresTag => ExitCode::DataError,
                        PackageErrorKind::IdentifierHasNoDigest => ExitCode::DataError,
                        PackageErrorKind::TaskPanicked => ExitCode::Failure,
                        _ => ExitCode::Failure,
                    },
                    ocx_lib::Error::Config(ce) => match ce {
                        config::Error::FileNotFound { .. } => ExitCode::NotFound,
                        config::Error::FileTooLarge { .. } | config::Error::Parse { .. } => ExitCode::ConfigError,
                        config::Error::Io { .. } => ExitCode::IoError,
                        config::Error::InvalidBooleanString { .. } => ExitCode::DataError,
                        _ => ExitCode::ConfigError,
                    },
                    // Forward to subsystem downcasts for variants that wrap subsystem errors
                    _ => continue,
                };
            }
            // Subsystem types that may surface standalone via anyhow context
            if let Some(e) = cause.downcast_ref::<oci::client::ClientError>() {
                return match e {
                    ClientError::Authentication(_) => ExitCode::AuthError,
                    ClientError::ManifestNotFound(_) => ExitCode::NotFound,
                    ClientError::Io { .. } => ExitCode::IoError,
                    ClientError::Registry(_) => ExitCode::Unavailable,
                    // ...
                    _ => ExitCode::Failure,
                };
            }
            // ... other subsystem downcasts (auth, archive, compression, ci, profile, etc.)
            if let Some(io) = cause.downcast_ref::<std::io::Error>()
                && io.kind() == std::io::ErrorKind::PermissionDenied
            {
                return ExitCode::PermissionDenied;
            }
        }
        ExitCode::Failure
    }
    ```

    The implementation's exact exhaustiveness should mirror the taxonomy table. Any subtree left out falls through to `ExitCode::Failure` — acceptable v1 behavior, locked in by the Phase 3 fall-through tests.
- [ ] **Step 4.3** Change `crates/ocx_cli/src/main.rs` error arm from `log::error!("Error: {err:#}"); ExitCode::FAILURE` to `log::error!("{err:#}"); ocx_lib::cli::classify_error(&err).into()`. Drop the hardcoded `"Error: "` prefix to match `ocx_mirror`'s style. (Note: `log::error!` here is the `tracing_log` bridge re-exported as `ocx_lib::log` — no tracing migration is needed.)
- [ ] **Step 4.4** Error message normalization pass per `research_rust_error_messages.md` across all identified files. Each file is a self-contained `Edit`. Apply the fixes from Step 4.1's audit list in the same commit. Run `cargo check -p ocx_lib` after each file to catch typo regressions.
  - `crates/ocx_lib/src/error.rs`
  - `crates/ocx_lib/src/auth/error.rs`
  - `crates/ocx_lib/src/oci/client/error.rs`
  - `crates/ocx_lib/src/oci/index/error.rs`
  - `crates/ocx_lib/src/oci/digest/error.rs`
  - `crates/ocx_lib/src/package_manager/error.rs`
  - `crates/ocx_lib/src/file_structure/error.rs`
  - `crates/ocx_lib/src/archive/error.rs`
  - `crates/ocx_lib/src/compression/error.rs`
  - `crates/ocx_lib/src/ci/error.rs`
  - `crates/ocx_lib/src/package/error.rs`
- [ ] **Step 4.5** Implement `MirrorError::kind_exit_code` mapping. **Important** (Round 3 correction): `MirrorError::ExecutionFailed(Vec<String>)` wraps stringified error messages, not a structured `anyhow::Error` — there is no inner error to delegate to. Map `ExecutionFailed` to a fixed `ExitCode::Failure (1)` for v1, and add a follow-up issue to refactor `MirrorError::ExecutionFailed` to carry a structured error so `classify_error` delegation becomes possible in v2.

    Final mapping:
    - `SpecInvalid` → `DataError (65)`
    - `SpecNotFound` → `NotFound (79)`
    - `ExecutionFailed(Vec<String>)` → `Failure (1)` (fixed; follow-up issue for delegation)
    - `SourceError` → `Unavailable (69)`

    Update the taxonomy table at the top of this plan to match — `MirrorError::ExecutionFailed(inner) → delegate` is wrong; it should be `Failure (1)` with a footnote.
- [ ] **Step 4.6** **`ocx_mirror::MirrorError` display normalization** (promoted from deferred to in-scope): `crates/ocx_mirror/src/error.rs` has a manual `Display` impl with sentence-case strings. Normalize to the lowercase rule so chained error output from `ExecutionFailed(ocx_lib::Error)` doesn't mix cases:
  - `"Invalid mirror spec:"` → `"invalid mirror spec:"`
  - `"Mirror spec not found:"` → `"mirror spec not found:"`
  - `"Mirror execution failed:"` → `"mirror execution failed:"`
  - `"Source error:"` → `"source error:"`

  Confirmed by the architect review: `MirrorError` has no `#[error]` attribute and no tests asserting on verbatim output; safe to rewrite.
- [ ] **Step 4.7** Post-normalization verification: re-run the grep patterns from Step 4.1 against the test suite. Any remaining hits on old strings indicate either a test that was missed in Step 4.1 or a string OCX should not have changed. Fix atomically with this step.

**Gate:** `task rust:verify` passes. `task test:parallel` passes. All 11 new acceptance tests pass. All 4+ new unit tests pass.

### Phase 5: Documentation

**Dependency:** Phase 5 must start AFTER Phase 4.4 completes — Step 5.1 adds an Error Reference table row quoting the normalized `Error::Io` message verbatim, and the existing rows need their verbatim strings updated to match the normalized forms. Running Phase 5 in parallel with Phase 4 risks copying the pre-normalization strings.

- [ ] **Step 5.1** `website/src/docs/reference/configuration.md`:
  - **Line 22 (file locations table)**: add a platform note. Replace the user-tier row with two rows (Linux, macOS) or add a `:::info Platform note` callout below the table explaining that `dirs::config_dir()` resolves to `~/.config/ocx/config.toml` on Linux and `~/Library/Application Support/ocx/config.toml` on macOS (with `$XDG_CONFIG_HOME` overriding on Linux only).
  - **Line 52 (precedence table)**: same fix — show both paths.
  - **Error Reference table (line 213+)**: add `Error::Io` row: `"error: failed to read config file /path/to/file.toml"` | File exists but cannot be read (permission denied, path is a directory, other I/O failure) | Check permissions; `--config` must point to a regular, readable file.
- [ ] **Step 5.2** `website/src/docs/reference/environment.md`:
  - **Line 82–89 (`OCX_HOME`)**: append one sentence: "OCX also discovers a configuration file at `$OCX_HOME/config.toml` — see the [OCX home tier][env-config-file] in the Configuration reference."
  - **Line 212 (`XDG_CONFIG_HOME`)**: add the macOS note matching `configuration.md`.
  - **`OCX_NO_CONFIG` section**: add one paragraph clarifying the "why no CLI flag" design decision: "`OCX_NO_CONFIG` is available only as an environment variable. A `--no-config` CLI flag would duplicate surface without solving a new problem: the hermetic-CI use case is best expressed via env vars, which are how CI systems already inject policy; a flag would require callers to both set the env var and pass the flag in per-command invocations."
- [ ] **Step 5.3** `website/src/docs/user-guide.md`:
  - **Line 644 (Configuration section table)**: add the macOS platform note.
- [ ] **Step 5.4** `website/src/docs/reference/command-line.md`:
  - **Line 75–81 (`--config {#arg-config}`)**: expand from one sentence to: (a) what it does (path to config file), (b) layering behavior (layers on top of discovered tiers, does NOT replace them), (c) existence requirement (missing file is an error), (d) env equivalent (`OCX_CONFIG_FILE`), (e) link to Configuration reference for full precedence rules.
- [ ] **Step 5.5** `website/src/docs/getting-started.md`:
  - **Line 209+ (Config file subsection)**: no changes needed — the existing `$OCX_HOME/config.toml` example is platform-neutral.
- [ ] **Step 5.6** No new doc pages. All changes are edits to existing pages.

**Gate:** `task website:build` succeeds. Visual inspection of the rendered pages confirms the platform notes read cleanly and links resolve.

### Phase 6: Final Review & Commit

- [ ] **Step 6.1** Run the auto review-fix loop (max 3 rounds):
  - `worker-reviewer` focus `quality` + `rust-quality` + `cli-ux`
  - `worker-doc-reviewer` for docs
- [ ] **Step 6.2** Run `task verify` as the final gate.
- [ ] **Step 6.3** `/finalize` the branch history into conventional commits. **Order matters** — later commits depend on earlier ones:
  - `refactor(config): rename RegistryGlobals to RegistryDefaults` — Step 1.3 (atomic with re-exports)
  - `feat(config): mark resolved_default_registry as must_use` — Step 1.4 (can fold into previous)
  - `feat(cli): add ExitCode enum in ocx_lib::cli with sysexits-aligned values` — Steps 1.1, 1.2, 4.2
  - `feat(cli): classify ocx_lib errors to specific exit codes via classify_error` — Step 4.3
  - `feat(mirror): adopt ExitCode enum; replace 2/3/4 with sysexits values` — Steps 1.6, 4.5
  - `perf(config): parallelize tier discovery with futures::future::join_all` — Step 4.0
  - `refactor(lib): normalize error messages to Rust API Guidelines style` — Steps 4.1, 4.4, 4.7
  - `refactor(mirror): normalize MirrorError display messages to lowercase rule` — Step 4.6
  - `docs(cli): expand --config help text to mention OCX_CONFIG_FILE and OCX_NO_CONFIG` — Step 1.5
  - `docs(config): correct macOS user-tier path; expand --config and environment references` — Step 5.1–5.5
  - `test(config): add FileTooLarge, merge-precedence, exit-code, help-text, kill-switch acceptance tests` — Phase 3
  - `docs(rules): add quality-rust-errors and quality-rust-exit_codes rule files` — separate commit capturing the extracted quality rules

  No CHANGELOG entry required — OCX is pre-v1 and breaks APIs throughout without formal ceremony.

**Gate:** Human review and commit per MEMORY policy (no auto-commits).

## Testing Strategy

### Unit Tests (from component contracts)

| Component | Behavior | Expected | Edge Cases |
|---|---|---|---|
| `ExitCode::from` → `std::process::ExitCode` | Every variant converts to its declared numeric value | Numeric values match research doc | All 12 variants |
| `RegistryConfig::merge` | None in higher preserves Some in lower | `url` preserved | Symmetric to existing scalar test |
| `load_and_merge_rejects_non_regular_file` | Tighter assertion | Specific substring match | Same edge case, tighter assertion |
| `classify_error(anyhow::Error)` | Correct `ExitCode` variant for each error subtree | Per error taxonomy table | Nested `anyhow::Context` wrappers; plain `thiserror` errors; unknown errors fall through to `Failure` |

### Acceptance Tests (from user experience)

| User Action | Expected Outcome | Error Cases |
|---|---|---|
| Huge config file | Exit 78, hint message | 64 KiB + 1 byte boundary |
| `--config` + `OCX_CONFIG_FILE` both set | `--config` wins on conflict | Both load (additive) |
| `$OCX_HOME/config.toml` + `--config` | Both merge; disjoint keys survive | Additive layering |
| Missing `--config` path | Exit 79 | Path in error message |
| Malformed config TOML | Exit 78 | Path in error |
| Offline blocked fetch | Exit 81 | Uncached package |
| Auth failure (if testable) | Exit 80 | 401 registry response |
| `ocx --help` | Mentions `OCX_CONFIG_FILE`, `OCX_NO_CONFIG` | Regex match in stdout |
| `ocx -c ...` | Exit 64, clap usage error | `-c` is no longer bound globally |

### Manual Testing

- [ ] Run `ocx --help` and confirm `--config` block reads cleanly (no mention of `-c`).
- [ ] Run `ocx package push --help` and confirm `-c, --cascade` is still present.
- [ ] Build and serve the website, visit `/docs/reference/configuration` and confirm the macOS platform note renders in the file-locations table.
- [ ] On a macOS machine (if available), verify `dirs::config_dir()` returns `~/Library/Application Support` by running `ocx --config ~/Library/Application\ Support/ocx/config.toml install cmake:0` (expect config to load, install to fail on the bogus tag).

## Files Modified Summary

| File | Action | Primary Reason |
|---|---|---|
| `crates/ocx_lib/src/cli/exit_code.rs` | **Create** | New `ExitCode` enum |
| `crates/ocx_lib/src/cli/classify.rs` | **Create** | `classify_error` free function |
| `crates/ocx_lib/src/cli.rs` (module root) | Modify | Module + re-exports for both new files |
| `crates/ocx_lib/src/lib.rs` | Modify | Re-export `RegistryDefaults`, `RegistryConfig` |
| `crates/ocx_lib/src/config.rs` | Modify | Rename + `#[must_use]` + `RegistryDefaults` re-export |
| `crates/ocx_lib/src/config/registry.rs` | Modify | Doc comment reference update |
| `crates/ocx_lib/src/config/loader.rs` | Modify | `tokio::join!` + tighter non-regular-file assertion |
| `crates/ocx_lib/src/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/auth/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/oci/client/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/oci/index/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/oci/digest/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/package_manager/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/file_structure/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/archive/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/compression/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/ci/error.rs` | Modify | Message normalization |
| `crates/ocx_lib/src/package/error.rs` | Modify | Message normalization |
| `crates/ocx_cli/src/main.rs` | Modify | Call `ocx_lib::cli::classify_error`, drop `"Error: "` prefix |
| `crates/ocx_cli/src/app/context_options.rs` | Modify | Expand `--config` doc comment (keep `-c`) |
| `crates/ocx_cli/src/command/package_push.rs` | Modify | Drop `short = 'c'` from `--cascade` |
| `crates/ocx_mirror/src/main.rs` | Modify | Use new `ExitCode` via `kind_exit_code` |
| `crates/ocx_mirror/src/error.rs` | Modify | `kind_exit_code` method + `MirrorError::Display` normalization |
| `test/tests/test_config.py` | Modify | 10+ new acceptance tests |
| `test/tests/test_package_push.py` | Modify | Add cascade `-c` removal negative test |
| `test/tests/test_offline.py` or new `test_exit_codes.py` | Modify/Create | Offline blocked exit-code test |
| `website/src/docs/reference/configuration.md` | Modify | Platform note + `Error::Io` row |
| `website/src/docs/reference/environment.md` | Modify | `OCX_HOME` + `XDG_CONFIG_HOME` + `OCX_NO_CONFIG` rationale |
| `website/src/docs/user-guide.md` | Modify | Platform note |
| `website/src/docs/reference/command-line.md` | Modify | Expand `--config` section |
| `CHANGELOG.md` | Modify | Breaking changes: `-c` cascade removal, mirror exit codes |

## Dependencies

No new external crates. `tokio::join!` (already available via `tokio` macros feature) replaces the sequential `try_exists` loop — originally the plan proposed `futures::future::join_all` but `tokio::join!` is cleaner for fixed arity and avoids a new dependency. `sysexits` crate rejected per research — enum is owned internally.

## Rollback Plan

Each commit is independently revertible per the `/finalize` phase structure. High-risk commits are `refactor(lib): normalize error messages` and `feat(cli): classify errors to specific exit codes` — both behind `task verify` gates before commit.

1. If error message normalization breaks a hidden test assertion → revert the single commit (`git revert`).
2. If exit code dispatch misclassifies errors → revert `feat(cli): classify errors`; binary falls back to `ExitCode::FAILURE` for all errors (pre-PR behavior).
3. If the `RegistryDefaults` rename breaks downstream consumers → revert `refactor(config): rename`.

## Risks

| Risk | Mitigation |
|---|---|
| Error message assertions in hidden test spots break | Step 4.1 grep audit runs BEFORE Step 4.4 normalization; fixes are atomic in the same commit |
| `ocx_mirror` exit codes change from 2/3/4 to 65/69/79 | Pre-v1; the project is breaking APIs throughout without formal BREAKING CHANGE ceremony. No CHANGELOG entry required. The new sysexits-aligned codes are the durable contract going forward |
| `classify_error` downcast fails to match a real error path | Phase 3 acceptance tests cover the major subtrees. Fall-through is `ExitCode::Failure` (pre-PR behavior) — misclassification degrades to current behavior, not worse |
| Normalized error messages break users' stderr grep patterns | Low — OCX has no stability contract on error messages today. Document the normalization in the commit body |
| macOS doc note disagrees with an existing reader's setup | The note is additive (Linux path still documented); no one's setup breaks |
| Unmapped error subtree (e.g. `archive::Error::Internal`) silently gets exit 1 | Phase 3 fall-through tests lock in the behavior so it can't silently change. Known-gap v1 list documented in the taxonomy table |

## Checklist

### Before Starting

- [x] Research artifacts persisted: `research_exit_codes.md`, `research_rust_error_messages.md`
- [x] Parent plan referenced: `plan_configuration_system.md`
- [x] Review findings enumerated with severity
- [ ] User approval on the design decisions table above

### Before PR

- [ ] `task rust:verify` passes
- [ ] `task test:parallel` passes (all new acceptance tests green)
- [ ] `task website:build` passes (docs render)
- [ ] `task verify` passes end-to-end
- [ ] No `RegistryGlobals` references remain (grep confirms)
- [ ] No `Error:` double-prefix in error output (manual run of a few failure paths)
- [ ] macOS platform note visible in built docs

### Before Merge

- [ ] Phase 6 review-fix loop converged (no actionable findings)
- [ ] Commits squashed/rebased via `/finalize` into the 9-commit sequence above
- [ ] Manual stderr inspection of at least 3 error paths (config not found, bad TOML, offline blocked)

## Handoff for `/swarm-execute`

| Phase | Worker | Notes |
|---|---|---|
| Stubs | `worker-builder` | Steps 1.1–1.6 — pure scaffolding |
| Architecture review | `worker-reviewer` focus spec-compliance phase post-stub | Verifies enum shape + rename completeness |
| Specification | `worker-tester` | Steps 3.1.1–3.1.4 (Rust) + 3.2.1–3.2.9 (Python) — ALL tests written before any Phase 4 work |
| Implementation | `worker-builder` | Step 4.1–4.6 — fill stubs + error normalization pass |
| Docs | `worker-doc-writer` | Step 5.1–5.5 |
| Final review | `worker-reviewer` + `worker-doc-reviewer` | Phase 6 loop |

Parallelizable:
- Step 4.4 (error normalization across 11 files) can proceed file-by-file in parallel `worker-builder` spawns if time-critical — but BOTH Steps 4.1 (audit) and 4.4 (apply) must be atomic.
- **Phase 5 docs work is NOT parallelizable with Phase 4**. Step 5.1 quotes normalized error messages verbatim. Phase 5 starts only after Step 4.4 is committed. (This is a Round 2 correction — the first draft mistakenly allowed parallelism.)

## Notes

- The `--no-config` CLI flag question is resolved by documentation clarity, per user directive. No new flag.
- The `RegistryConfig` struct retains its name. Only `RegistryGlobals` → `RegistryDefaults` is renamed.
- Error message normalization is a one-shot pass. It will not be repeated; new error variants added after this PR must follow the rule from `research_rust_error_messages.md`. Consider adding a code-style reminder to `quality-rust.md` if the rule isn't already captured there.
- Exit code values are NOT covered by a stability contract in v0.x but should be treated as semi-stable — breaking them is a minor-version bump in a future v1.

## Progress Log

| Date | Update |
|---|---|
| 2026-04-13 | Plan drafted via `/swarm-plan` after `/swarm-review` of `feat/config-system` branch |
| 2026-04-13 | Phase 1 correction: `classify_error` signature changed from `&anyhow::Error` to `&(dyn std::error::Error + 'static)` to keep `anyhow` out of `ocx_lib` per `quality-rust-errors.md`. Binary calls `classify_error(err.as_ref())` — `anyhow::Error: AsRef<dyn Error>`. Step 4.2 pseudocode will use `std::iter::successors(Some(err), \|e\| e.source())` instead of `err.chain()` — equivalent behavior for anyhow-wrapped chains. |
