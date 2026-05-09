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

/// Reserved CLI keyword that expands to the union of the default group
/// and every named group declared in `ocx.toml`. Used by project-tier
/// commands that accept `-g` (e.g. `ocx run`, `ocx pull`). Unlike
/// `DEFAULT_GROUP`, `ALL_GROUP` is never a literal group name that can
/// appear in `ocx.toml` — it is a CLI expansion alias. Parse-time and
/// mutate-time validators reject `[group.all]` declarations and
/// `--group all` arguments before they reach the composition layer.
pub const ALL_GROUP: &str = "all";

/// Upper bound on `ocx.toml` / `ocx.lock` file size accepted by the
/// parsers. Mirrors the ambient config-loader cap so pathological inputs
/// in CI surface as a structured error rather than an OOM or pathological
/// TOML parse.
pub const FILE_SIZE_LIMIT_BYTES: u64 = 64 * 1024;
