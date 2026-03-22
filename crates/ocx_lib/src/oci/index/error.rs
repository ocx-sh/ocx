// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Errors specific to OCI index operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A remote manifest was expected but not found during index update.
    #[error("Remote manifest not found for '{0}' during index update")]
    RemoteManifestNotFound(String),
}
