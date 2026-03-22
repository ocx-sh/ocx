// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors specific to file structure operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// An identifier was expected to carry a digest but did not.
    #[error("Identifier requires a digest: {0}")]
    MissingDigest(String),
}
