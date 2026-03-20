// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;

use async_trait::async_trait;

use super::error::ClientError;
use crate::oci;

pub type Result<T> = std::result::Result<T, ClientError>;

/// Low-level OCI registry transport operations.
///
/// Abstracts the wire-level OCI distribution API calls, enabling the
/// higher-level [`super::Client`] business logic to be tested without
/// hitting a real registry.
///
/// Implementations are expected to handle authentication internally.
/// Every method calls [`ensure_auth`](Self::ensure_auth) with the
/// appropriate operation scope before performing any network I/O, so
/// callers never need to worry about auth ordering.
#[async_trait]
pub trait OciTransport: Send + Sync {
    // ── Authentication ───────────────────────────────────────────────

    /// Pre-authenticate for the given operation scope.
    ///
    /// Ensures credentials are resolved and a token is cached for
    /// `image`'s registry with the requested operation scope (Pull or Push).
    /// Repeated calls for the same scope are no-ops (token cache hit).
    async fn ensure_auth(&self, image: &oci::native::Reference, operation: oci::RegistryOperation) -> Result<()>;

    // ── Read operations ──────────────────────────────────────────────

    /// Lists tags for the given image, returning one page of results.
    async fn list_tags(
        &self,
        image: &oci::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>>;

    /// Lists repositories (catalog) for the registry of the given image reference.
    async fn catalog(
        &self,
        image: &oci::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>>;

    /// Fetches only the digest of a manifest without pulling the full content.
    async fn fetch_manifest_digest(&self, image: &oci::native::Reference) -> Result<String>;

    /// Pulls raw manifest bytes and returns them with the digest string.
    async fn pull_manifest_raw(
        &self,
        image: &oci::native::Reference,
        accepted_media_types: &[&str],
    ) -> Result<(Vec<u8>, String)>;

    /// Pulls a blob and writes it to the specified file path.
    async fn pull_blob_to_file(&self, image: &oci::native::Reference, digest: &str, path: &Path) -> Result<()>;

    // ── Write operations ─────────────────────────────────────────────

    /// Pushes a typed OCI manifest and returns the resulting digest string.
    async fn push_manifest(&self, image: &oci::native::Reference, manifest: &oci::Manifest) -> Result<String>;

    /// Pushes raw manifest bytes with the given media type string.
    /// Returns the resulting digest string.
    async fn push_manifest_raw(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        media_type: &str,
    ) -> Result<String>;

    /// Pushes in-memory blob data. Returns the resulting digest string.
    async fn push_blob(&self, image: &oci::native::Reference, data: Vec<u8>, digest: &str) -> Result<String>;

    // ── Clone support ────────────────────────────────────────────────

    /// Clones the transport into a boxed trait object.
    fn box_clone(&self) -> Box<dyn OciTransport>;
}
