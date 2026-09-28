// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors specific to file structure operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An identifier was expected to carry a digest but did not.
    #[error("identifier requires a digest: {0}")]
    MissingDigest(String),

    /// A dispatch object's bytes did not hash to the digest naming them, on write or on read (`adr_index_indirection.md#a3`).
    #[error("dispatch object digest mismatch: claimed '{claimed}', computed '{computed}'")]
    DigestMismatch {
        /// The digest the caller claimed (write) or the on-disk filename encodes (read).
        claimed: ocx_oci::Digest,
        /// The digest actually computed from the bytes.
        computed: ocx_oci::Digest,
    },

    /// A root document (`p/<ns>/<pkg>.json`) failed to parse (`adr_index_indirection.md#a2`); never raised for a root/catalog digest disagreement, which self-heals.
    #[error("malformed root document for source '{index_source}', repository '{repository}': {cause}")]
    MalformedRootDocument {
        index_source: String,
        repository: String,
        #[source]
        cause: serde_json::Error,
    },

    /// A `repository` would join outside the source subtree (CWE-22) (`adr_index_indirection.md#f2`).
    #[error("index repository path '{repository}' escapes the source root")]
    RepositoryEscapesIndexHome {
        repository: String,
        #[source]
        source: ocx_util::fs::path::PathEscapeError,
    },

    /// A name under a source's `p/` tree is not valid UTF-8.
    ///
    /// Raised, never skipped: the walk derives a wholesale `c/index.json`, so a dropped name deletes a package from the catalog.
    #[error("index path is not valid UTF-8: {}", path.display())]
    NonUtf8WireName { path: std::path::PathBuf },
}
