// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Configuration for which `ocx-mirror` / `ocx` binary versions to use in
//! the generated pipeline workflow.
//!
//! [`OcxMirrorConfig`] is also the sole source for the `ocx` binary download
//! tag — the renderer reuses `release_tag` for both the ocx-mirror cargo-install
//! path and the `ocx` binary `gh release download` step.

use serde::Deserialize;

/// Pins the `ocx-mirror` binary version and, optionally, a git revision used
/// for `cargo install --git --rev` fallback paths.
#[derive(Debug, Clone, Deserialize)]
pub struct OcxMirrorConfig {
    /// Tagged release (e.g. `v0.7.2`). Required when any platform declares
    /// `containers:` (linux container legs need the musl static artifact via
    /// `gh release download`). Must match `^v\d+\.\d+\.\d+(-[a-z0-9.]+)?$`.
    #[serde(default)]
    pub release_tag: Option<String>,
    /// 40-hex git SHA. When set, supersedes `release_tag` for the
    /// `cargo install --git --rev` code path. `release_tag` is still used for
    /// musl-asset download when present. Must match `^[0-9a-f]{40}$`.
    #[serde(default)]
    pub rev: Option<String>,
}
