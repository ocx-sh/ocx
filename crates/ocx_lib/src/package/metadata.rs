// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

pub mod bundle;
pub mod dependency;
pub mod entrypoint;
pub mod env;
pub mod slug;
pub mod template;
pub mod validation;
pub mod visibility;

// Re-export entrypoint public API so callers can use `metadata::Entrypoint` etc.
pub use entrypoint::{Entrypoint, EntrypointError, EntrypointName, Entrypoints};
// Re-export ValidMetadata so callers continue to use `metadata::ValidMetadata`.
pub use validation::ValidMetadata;

/// OCX package metadata.
///
/// Declares a package's type, extraction options, environment variables, and
/// dependencies. Currently only the `bundle` type is supported.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Metadata {
    Bundle(bundle::Bundle),
}

impl Metadata {
    pub fn env(&self) -> Option<&env::Env> {
        match self {
            Metadata::Bundle(bundle) => Some(&bundle.env),
        }
    }

    pub fn dependencies(&self) -> &dependency::Dependencies {
        match self {
            Metadata::Bundle(bundle) => &bundle.dependencies,
        }
    }

    /// Returns the `entrypoints` field for bundle metadata, or `None` for non-bundle variants.
    ///
    /// Mirrors the pattern of `bundle_dependencies()` — returns `None` for
    /// future non-bundle metadata types so callers can opt into entrypoint
    /// behavior without checking the variant themselves.
    pub fn entrypoints(&self) -> Option<&entrypoint::Entrypoints> {
        match self {
            Metadata::Bundle(bundle) => Some(&bundle.entrypoints),
        }
    }
}
