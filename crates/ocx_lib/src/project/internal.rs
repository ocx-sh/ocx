// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Module-private constants shared across `project::config`, `project::hash`,
//! and `project::lock`. The submodule itself is not re-exported, so every
//! item below is effectively module-private without using `pub(super)`
//! visibility qualifiers (per `quality-rust.md` "control visibility through
//! module nesting, not path qualifiers").

/// Reserved group name for the implicit default group (the top-level
/// `[tools]` table in `ocx.toml`, the `"default"` group key in lock
/// entries, and the JSON key in the declaration-hash canonical form).
/// Single source of truth shared across `config.rs`, `hash.rs`, and
/// `lock.rs` — user-facing message literals (e.g. `[group.default]`
/// text) keep the form verbatim for readability.
pub const DEFAULT_GROUP: &str = "default";

/// Upper bound on `ocx.toml` / `ocx.lock` file size accepted by the
/// parsers. Mirrors the ambient config-loader cap so pathological inputs
/// in CI surface as a structured error rather than an OOM or pathological
/// TOML parse.
pub const FILE_SIZE_LIMIT_BYTES: u64 = 64 * 1024;
