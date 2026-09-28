// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

pub mod authoring;
pub mod binary;
pub mod bundle;
pub mod dependency;
pub mod entrypoint;
pub mod env;
pub mod integrations;
pub mod slug;
pub mod template;
pub mod validation;
pub mod visibility;

pub use binary::{Binaries, BinaryError, BinaryName};
pub use integrations::{IntegrationEntry, Integrations};

pub use entrypoint::{Entrypoint, EntrypointError, EntrypointName, Entrypoints};
pub use validation::{ValidMetadata, validate_for_publish};

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

    pub fn version(&self) -> bundle::Version {
        match self {
            Metadata::Bundle(bundle) => bundle.version,
        }
    }

    /// Number of leading path components stripped on extraction, if declared.
    pub fn strip_components(&self) -> Option<u8> {
        match self {
            Metadata::Bundle(bundle) => bundle.strip_components,
        }
    }

    /// The declared entrypoints; `None` for a variant without them.
    pub fn entrypoints(&self) -> Option<&entrypoint::Entrypoints> {
        match self {
            Metadata::Bundle(bundle) => Some(&bundle.entrypoints),
        }
    }

    /// The declared `binaries` claims; `None` (field absent) differs from an
    /// empty set, which claims none.
    pub fn binaries(&self) -> Option<&binary::Binaries> {
        match self {
            Metadata::Bundle(bundle) => bundle.binaries.as_ref(),
        }
    }

    /// The declared `integrations` map; absent and empty are the same wire state.
    pub fn integrations(&self) -> &integrations::Integrations {
        match self {
            Metadata::Bundle(bundle) => &bundle.integrations,
        }
    }
}
