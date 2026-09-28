// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Constants shared by `config`, `hash` and `lock`.

/// Reserved name of the implicit default group (top-level `[tools]`).
pub const DEFAULT_GROUP: &str = "default";

/// Reserved `-g` keyword expanding to every group; never a declarable group.
pub const ALL_GROUP: &str = "all";

/// Size cap on `ocx.toml` / `ocx.lock`, so a pathological input is a
/// structured error rather than an OOM.
pub const FILE_SIZE_LIMIT_BYTES: u64 = 64 * 1024;
