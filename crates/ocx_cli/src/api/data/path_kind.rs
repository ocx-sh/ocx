// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

// One definition for `ocx pull` and `ocx package which`: never re-spell it in either.
/// What kind of directory a reported `path` names.
///
/// A tool composed lazily has no package directory until its first invocation
/// materializes one — what exists on disk is its generated shim tree. `ocx pull`
/// and `ocx package which` both report this same two-value vocabulary.
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
