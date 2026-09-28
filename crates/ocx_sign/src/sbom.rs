// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! SBOM reading and summarization (CycloneDX only, [`cyclonedx`]).

use thiserror::Error;

pub mod cyclonedx;

/// What `ocx package sbom --summary` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SbomSummary {
    /// The document's own `specVersion`, verbatim (e.g. `"1.6"`).
    pub spec_version: String,
    /// `serialNumber`, when the document carries one.
    pub serial_number: Option<String>,
    /// The length of the document's top-level `components` array.
    pub component_count: usize,
    /// `metadata.component.name` — the document's own root component.
    pub top_level_component: Option<String>,
}

/// An SBOM summarization failure; the input is signature-verified but still attacker-shaped JSON.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SbomError {
    /// The document is not valid JSON.
    #[error("SBOM document is not valid JSON")]
    NotJson(#[source] serde_json::Error),

    /// The document's top level is not a JSON object.
    #[error("SBOM document root is not a JSON object")]
    NotAnObject,

    /// `specVersion` is missing, or present but not a JSON string.
    #[error("SBOM document has no string specVersion field")]
    MissingSpecVersion,

    /// `specVersion` names a version outside the accepted CycloneDX range.
    #[error(
        "SBOM specVersion {found:?} is not a supported CycloneDX version (accepted: {})",
        cyclonedx::ACCEPTED_SPEC_VERSIONS.join(", ")
    )]
    UnsupportedSpecVersion {
        /// The document's declared version.
        found: String,
    },

    /// `specVersion` is in the accepted range but the document otherwise
    /// failed to parse as CycloneDX.
    #[error("SBOM document declares CycloneDX {spec_version} but failed to parse")]
    MalformedDocument {
        /// The document's declared (accepted) version.
        spec_version: String,
        #[source]
        source: serde_json::Error,
    },
}
