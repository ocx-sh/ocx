// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Per-platform runner configuration for the test pipeline.
//!
//! [`PlatformConfig`] maps an OCI platform key (e.g. `linux/amd64`) to a
//! GitHub Actions runner label and optional container matrix. Absence of
//! `containers` means native mode; presence means container mode.

use serde::Deserialize;

use super::tests_config::TestEntry;

/// Configuration for a single container image to test against.
///
/// In container mode the OCX binary is injected via a per-leg ephemeral
/// Dockerfile `ADD` before each test leg runs.
#[derive(Debug, Clone, Deserialize)]
pub struct ContainerConfig {
    /// OCI image reference (e.g. `ubuntu:24.04`, `alpine:3.20`).
    pub image: String,
    /// Shell to invoke inside the container. Defaults by image prefix per A9:
    /// alpine → `sh`; ubuntu/debian/fedora/rocky/opensuse → `bash`; otherwise required.
    pub shell: Option<String>,
    /// Optional stable ID used to construct JUNIT filenames and GHA matrix
    /// check names. Defaults to slugified `image` (`:` and `/` → `_`).
    pub id: Option<String>,
}

/// Configuration for one platform target in the test pipeline.
///
/// A platform without `containers` runs tests natively on the declared GHA
/// runner. A platform with `containers` runs each test in each listed
/// container image (container mode, linux only).
#[derive(Debug, Clone, Deserialize)]
pub struct PlatformConfig {
    /// GitHub Actions runner label (e.g. `ubuntu-latest`, `macos-latest`).
    pub runner: String,
    /// Container images to test against. Absence = native mode.
    #[serde(default)]
    pub containers: Option<Vec<ContainerConfig>>,
    /// Command prefix inserted before every test invocation (e.g.
    /// `["arch", "-x86_64"]` for `darwin/amd64` cross-execution).
    /// Defaults per A8: `darwin/amd64` on `macos-*` → `["arch", "-x86_64"]`; else empty.
    ///
    /// Declared in the spec schema; not yet wired into CI generation.
    #[allow(dead_code)]
    #[serde(default)]
    pub prefix: Option<Vec<String>>,
    /// Default shell for native legs (e.g. `bash`, `pwsh`).
    #[serde(default)]
    pub shell: Option<String>,
    /// Per-platform test override. When set, replaces the top-level `tests:`
    /// list entirely for this platform — no partial merge.
    #[serde(default)]
    pub tests: Option<Vec<TestEntry>>,
}
