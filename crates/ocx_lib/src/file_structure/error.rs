// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::cli::ClassifyExitCode;
use crate::cli::ExitCode;

/// Errors specific to file structure operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An identifier was expected to carry a digest but did not.
    #[error("identifier requires a digest: {0}")]
    MissingDigest(String),

    /// Bytes read from (or about to be written to) a content-addressed
    /// snapshot object did not hash to the digest that named them — a
    /// trust-boundary check (CWE-345) that fires on write (source-served
    /// bytes disagree with their claimed digest) and on read (an on-disk
    /// object was tampered with after being written). See
    /// `adr_index_indirection.md` Decision A3.
    #[error("snapshot object digest mismatch: claimed '{claimed}', computed '{computed}'")]
    DigestMismatch {
        /// The digest the caller claimed (write) or the on-disk filename encodes (read).
        claimed: crate::oci::Digest,
        /// The digest actually computed from the bytes.
        computed: crate::oci::Digest,
    },

    /// A root document (`p/<ns>/<pkg>.json`) could not be parsed as the
    /// frozen wire shape (`adr_index_indirection.md` F1/A2) — genuine
    /// corruption, one of the few hard read-path failures. Never raised for
    /// a bare root/catalog digest disagreement, which self-heals by
    /// re-derivation instead (`IndexStore::read_root`).
    #[error("malformed root document for source '{index_source}', repository '{repository}': {cause}")]
    MalformedRootDocument {
        index_source: String,
        repository: String,
        #[source]
        cause: serde_json::Error,
    },

    /// A `repository` reaching a wire-grammar path builder would join OUTSIDE
    /// the source subtree under the index home (CWE-22 path traversal). The
    /// repository component is split verbatim on `/` into path segments
    /// (`file_structure::repository_path`), so an absolute segment, a `..`
    /// escape, a Windows drive/UNC prefix, or a backslash-separated escape
    /// would land a read or write outside the home. Defense-in-depth behind
    /// the catalog-key boundary validation in `LocalIndex::sync_catalog`
    /// (`adr_index_indirection.md` Decision F2).
    #[error("index repository path '{repository}' escapes the source root")]
    RepositoryEscapesIndexHome {
        repository: String,
        #[source]
        source: crate::utility::fs::path::PathEscapeError,
    },
}

impl ClassifyExitCode for Error {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::MissingDigest(_) => ExitCode::DataError,
            Self::DigestMismatch { .. } => ExitCode::DataError,
            Self::MalformedRootDocument { .. } => ExitCode::DataError,
            Self::RepositoryEscapesIndexHome { .. } => ExitCode::DataError,
        })
    }
}
