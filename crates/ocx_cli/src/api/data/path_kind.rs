// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

/// What kind of directory a reported `path` names.
///
/// A tool composed lazily has no package directory until its first invocation
/// materializes one — what exists on disk is its generated shim tree. Reporting
/// the package directory for it would be a lie in a machine-read field, and
/// reporting the shim directory without saying so would be a silent change of
/// meaning. The discriminator is what lets a consumer tell the two apart.
///
/// **One definition, two consumers.** `ocx pull` (this crate's `command/pull.rs`)
/// and `ocx package which` both answer "where is this tool on disk", and both
/// answer it with the same two-value vocabulary — so it lives here rather than
/// being re-spelled in either report type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    /// The package root: the parent of `content/` and `entrypoints/`.
    Package,
    /// The generated shim directory of a deferred tool. Its `bin/` holds one
    /// launcher per declared name; the package directory does not exist yet.
    Shim,
}

impl std::fmt::Display for PathKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Package => write!(f, "package"),
            Self::Shim => write!(f, "shim"),
        }
    }
}
