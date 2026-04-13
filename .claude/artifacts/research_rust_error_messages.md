# Research: Rust Error Message Style (2026)

<!--
Research Artifact
Filename: artifacts/research_rust_error_messages.md
Scope: Canonical style rule for `thiserror` and `anyhow` error messages in OCX
Related Plan: plan_config_review_cleanup.md
-->

## Status

- **Date:** 2026-04-13
- **Axis:** Domain knowledge (Rust ecosystem conventions)
- **Consumers:** `plan_config_review_cleanup.md`, future error-handling work

## Direct Rule

> All `#[error("...")]` strings in `ocx_lib` MUST begin with a **lowercase letter** and have **no trailing punctuation**. Messages are concise factual noun phrases (`"invalid manifest"`) or task-level clauses (`"failed to read config file {path}"`). Never prefix with `"Error:"` / `"error:"`. Use `#[source]` for chains, never `.to_string()` in `map_err`.
>
> `ocx_cli` context strings added via `anyhow::Context` may use sentence-case, because they are the terminal-boundary layer the user reads directly.

## Canonical Sources

Two official Rust-project sources agree verbatim:

- [Rust API Guidelines `C-GOOD-ERR`](https://rust-lang.github.io/api-guidelines/interoperability.html#error-types-are-meaningful-and-well-behaved-c-good-err):
  > "The error message given by the `Display` representation of an error type should be lowercase without trailing punctuation, and typically concise."

- [`std::error::Error` trait docs](https://doc.rust-lang.org/std/error/trait.Error.html):
  > "Error messages are typically concise lowercase sentences without trailing punctuation."

Canonical example from both: `"invalid digit found in string"`. Not `"Invalid digit found in string."`.

## Real-World Alignment

| Project | Style at lib layer | Example |
|---|---|---|
| [cargo](https://github.com/rust-lang/cargo/blob/master/src/cargo/util/context/mod.rs) | lowercase, no period | `"failed to get successful HTTP response from \`{url}\`"`, `"expected table for configuration key"` |
| [`thiserror` docs](https://docs.rs/thiserror/latest/thiserror/) | lowercase, no period | `"data store disconnected"`, `"invalid header (expected {expected:?}, found {found:?})"` |
| [`anyhow` docs](https://docs.rs/anyhow/latest/anyhow/) | context strings sentence-case OK | `"Failed to read instrs from {}"` — shown only at terminal boundary |
| [jj (jujutsu)](https://github.com/martinvonz/jj) | lib: lowercase / CLI UI: sentence-case | Split is intentional: `ui.rs::format_error_with_sources()` is the sentence-case renderer; lib errors are lowercase |

**No clippy lint exists.** Enforcement is code review only — hence the plan must do this in one pass.

## Library vs CLI Split

| Layer | Style | Rationale |
|---|---|---|
| `ocx_lib` (`thiserror` variants) | lowercase, no period | Composes into `source()` chains via `{err:#}`. A mixed chain `"failed to install: Registry authentication failed."` reads wrong |
| `ocx_cli` (`anyhow::Context`) | Sentence-case acceptable | Terminal boundary; user reads it directly |

OCX uses `log::error!("Error: {err:#}")` in `main.rs`, which walks the full `source()` chain and prints every lib-layer message verbatim. This makes the lowercase rule load-bearing for chain readability.

## OCX Violations Inventory (audit by worker-architecture-explorer)

Five files in `crates/ocx_lib/src/` contain sentence-case violations. All are pure string changes — no test asserts on `#[error(...)]` verbatim output.

### `crates/ocx_lib/src/error.rs`

| Before | After |
|---|---|
| `"A network operation was attempted while in offline mode."` | `"network operation attempted in offline mode"` |
| `"Internal file error for '{path}': {source}"` | `"internal file error for '{path}': {source}"` |
| `"Path '{}' has an unexpected structure"` | `"path '{}' has an unexpected structure"` |
| `"JSON serialization error: {0}"` | `"JSON serialization error: {0}"` (kept — "JSON" is an acronym; no change) |
| `"Unsupported media type '{media_type}'..."` | `"unsupported media type '{media_type}'..."` |

Note: initialisms and acronyms (JSON, TOML, I/O, SHA-256, HTTP, URL, SSL, TLS) retain their canonical case — the lowercase rule applies to the **first letter** and English words, not proper nouns or acronyms.

### `crates/ocx_lib/src/auth/error.rs`

| Before | After |
|---|---|
| `"Invalid authentication type '...', valid types are: ..."` | `"invalid authentication type '...', valid types are: ..."` |
| `"Authentication type '...' requires environment variable '...' to be set"` | `"authentication type '...' requires environment variable '...' to be set"` |
| `"Failed to retrieve Docker credentials: {0}"` | `"failed to retrieve Docker credentials: {0}"` |

### `crates/ocx_lib/src/oci/client/error.rs`

| Before | After |
|---|---|
| `"Registry authentication failed: {0}"` | `"registry authentication failed: {0}"` |
| `"Manifest digest mismatch: expected '...', got '...'"` | `"manifest digest mismatch: expected '...', got '...'"` |
| `"Expected an image manifest, got an image index"` | `"expected an image manifest, got an image index"` |
| `"Invalid manifest: {0}"` | `"invalid manifest: {0}"` |
| `"Manifest not found: {0}"` | `"manifest not found: {0}"` |
| `"Registry operation failed: {0}"` | `"registry operation failed: {0}"` |
| `"I/O error for '{}': {source}"` | `"I/O error for '{}': {source}"` (kept — I/O is an acronym) |
| `"Serialization error: {0}"` | `"serialization error: {0}"` |
| `"Invalid UTF-8 encoding: {0}"` | `"invalid UTF-8 encoding: {0}"` |

### `crates/ocx_lib/src/package_manager/error.rs`

| Before | After |
|---|---|
| `"Conflicting digests for {repository}: ..."` | `"conflicting digests for {repository}: ..."` |
| `"Dependency setup failed: {0}"` | `"dependency setup failed: {0}"` |

The batch-level `Display` impl at the bottom of `error.rs` that renders `"Failed to {verb} package:"` is a CLI-visible top message, not a `thiserror` variant — normalize to `"failed to {verb} package:"` for consistency with the rule.

### `crates/ocx_lib/src/oci/index/error.rs`

| Before | After |
|---|---|
| `"Remote manifest not found for '...' during index update"` | `"remote manifest not found for '...' during index update"` |
| `"Tag lock repository mismatch in '...': expected ..., found ..."` | `"tag lock repository mismatch in '...': expected ..., found ..."` |

### `crates/ocx_lib/src/oci/digest/error.rs`

| Before | After |
|---|---|
| `"Invalid package digest: {0}"` | `"invalid package digest: {0}"` |

### `crates/ocx_lib/src/file_structure/error.rs`

| Before | After |
|---|---|
| `"Identifier requires a digest: {0}"` | `"identifier requires a digest: {0}"` |

### `crates/ocx_lib/src/archive/error.rs`

| Before | After |
|---|---|
| `"Archive I/O error for '{}': {source}"` | `"archive I/O error for '{}': {source}"` |
| `"Tar error: {0}"` | `"tar error: {0}"` |
| `"Zip error: {0}"` | `"zip error: {0}"` |
| `"Archive entry '...' escapes the extraction root"` | `"archive entry '...' escapes the extraction root"` |
| `"Symlink '...' with target '...' escapes the root directory"` | `"symlink '...' with target '...' escapes the root directory"` |
| `"Unsupported archive format: {0}"` | `"unsupported archive format: {0}"` |
| `"Internal archive error: {0}"` | `"internal archive error: {0}"` |

### `crates/ocx_lib/src/compression/error.rs`

| Before | After |
|---|---|
| `"Cannot determine compression algorithm for '...'"` | `"cannot determine compression algorithm for '...'"` |
| `"Failed to open '...' for decompression"` | `"failed to open '...' for decompression"` |
| `"Failed to create compressed output '...'"` | `"failed to create compressed output '...'"` |
| `"Compression engine initialization failed"` | `"compression engine initialization failed"` |
| `"Compression I/O error"` | `"compression I/O error"` |

### `crates/ocx_lib/src/ci/error.rs`

| Before | After |
|---|---|
| `"CI environment variable '$...' is not set. Is this running inside a CI system?"` | `"CI environment variable '$...' is not set; is this running inside a CI system?"` (kept `"CI"` acronym; trailing `?` removed; split to single sentence with semicolon) |
| `"Failed to write CI file '...': {source}"` | `"failed to write CI file '...': {source}"` |

### `crates/ocx_lib/src/package/error.rs`

| Before | After |
|---|---|
| `"Invalid package version: {0}"` | `"invalid package version: {0}"` |
| `"Unsupported logo format: {0}"` | `"unsupported logo format: {0}"` |
| `"Required path does not exist: {0}"` | `"required path does not exist: {0}"` |

## Sources

- [Rust API Guidelines — `C-GOOD-ERR`](https://rust-lang.github.io/api-guidelines/interoperability.html#error-types-are-meaningful-and-well-behaved-c-good-err) — canonical rule with examples
- [`std::error::Error` trait docs](https://doc.rust-lang.org/std/error/trait.Error.html) — `std::Error` trait confirming lowercase/no-period
- [`thiserror` docs](https://docs.rs/thiserror/latest/thiserror/) — examples (all lowercase, no period)
- [`anyhow` docs](https://docs.rs/anyhow/latest/anyhow/) — `Context` pattern, CLI-boundary usage
- [cargo source — `util/context/mod.rs`](https://github.com/rust-lang/cargo/blob/master/src/cargo/util/context/mod.rs) — reference CLI implementation
- [jj source — `cli/src/ui.rs`](https://github.com/martinvonz/jj/blob/main/cli/src/ui.rs) — confirms library/CLI split in a modern Rust tool
