// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The applied-tool set: one [`AppliedEntry`] per tool composed into a
//! shell environment.
//!
//! Produced by [`crate::package_manager::collect_applied`] and consumed by
//! `ocx direnv export` to describe the toolchain that was put on `PATH`.

/// A single tool in the applied set — `(name, manifest_digest, group)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedEntry {
    /// Binding name of the tool as seen by the user (e.g. `cmake`, `node`).
    pub name: String,
    /// Manifest digest pinned by `ocx.lock` for this tool.
    pub manifest_digest: String,
    /// Group the tool was selected from (`default` or a named group).
    pub group: String,
}
