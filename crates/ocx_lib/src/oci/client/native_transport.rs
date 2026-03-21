// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream;

use super::error::ClientError;
use super::progress_writer::ProgressWriter;
use super::transport::{OciTransport, ProgressFn, Result};
use crate::{auth, log, oci};

/// Real OCI transport that delegates to the `oci_client` crate.
///
/// Handles authentication internally via [`auth::Auth`] so that callers
/// (and the [`OciTransport`] trait surface) don't need to carry auth state.
///
/// # Auth patterns
///
/// The underlying `oci_client::Client` uses two styles of authentication:
///
/// - **Explicit**: Some methods (`list_tags`, `catalog`, `fetch_manifest_digest`,
///   `pull_manifest_raw`) require an `&Auth` parameter — we pass credentials
///   via [`auth_for`](Self::auth_for).
/// - **Internal**: Other methods (`pull_blob`, `push_blob`, `push_manifest_raw`)
///   manage auth internally via cached tokens. No explicit credentials are needed.
///
/// The [`authenticate`](Self::authenticate) method pre-populates the token
/// cache with a Push scope, used before `push_manifest` where the registry
/// may require explicit push authorization upfront.
#[derive(Clone)]
pub(super) struct NativeTransport {
    client: oci::native::Client,
    auth: auth::Auth,
    push_chunk_size: usize,
}

impl NativeTransport {
    pub fn new(client: oci::native::Client, auth: auth::Auth, push_chunk_size: usize) -> Self {
        Self {
            client,
            auth,
            push_chunk_size,
        }
    }

    async fn auth_for(&self, image: &oci::native::Reference) -> oci::native::Auth {
        self.auth.get_or_fallback(image.registry()).await
    }

    async fn authenticate(&self, image: &oci::native::Reference, operation: oci::RegistryOperation) -> Result<()> {
        let auth = self.auth_for(image).await;
        self.client
            .auth(image, &auth, operation)
            .await
            .map_err(|e| ClientError::Authentication(e.to_string()))?;
        Ok(())
    }
}

fn registry_error(e: impl std::fmt::Display) -> ClientError {
    ClientError::Registry(e.to_string())
}

/// Maps OCI distribution errors to [`ClientError::ManifestNotFound`] when the
/// registry indicates the manifest does not exist (404 / MANIFEST_UNKNOWN),
/// and falls back to [`ClientError::Registry`] for everything else.
fn manifest_not_found_or_registry_error(
    e: oci_client::errors::OciDistributionError,
    image: &oci::native::Reference,
) -> ClientError {
    use oci_client::errors::OciDistributionError::*;
    use oci_client::errors::OciErrorCode;
    match &e {
        ImageManifestNotFoundError(_) => ClientError::ManifestNotFound(image.to_string()),
        RegistryError { envelope, .. } => {
            let is_not_found = envelope.errors.iter().any(|err| {
                matches!(
                    err.code,
                    OciErrorCode::ManifestUnknown | OciErrorCode::NotFound | OciErrorCode::NameUnknown
                )
            });
            if is_not_found {
                ClientError::ManifestNotFound(image.to_string())
            } else {
                ClientError::Registry(e.to_string())
            }
        }
        ServerError { code: 404, .. } => ClientError::ManifestNotFound(image.to_string()),
        _ => ClientError::Registry(e.to_string()),
    }
}

fn io_error(path: &Path, e: impl Into<std::io::Error>) -> ClientError {
    ClientError::Io(path.to_path_buf(), e.into())
}

#[async_trait]
impl OciTransport for NativeTransport {
    async fn ensure_auth(&self, image: &oci::native::Reference, operation: oci::RegistryOperation) -> Result<()> {
        self.authenticate(image, operation).await
    }

    async fn list_tags(
        &self,
        image: &oci::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>> {
        let auth = self.auth_for(image).await;
        let response = self
            .client
            .list_tags(image, &auth, Some(chunk_size), last.as_deref())
            .await
            .map_err(registry_error)?;
        Ok(response.tags)
    }

    async fn catalog(
        &self,
        image: &oci::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>> {
        let auth = self.auth_for(image).await;
        let response = self
            .client
            .catalog(image, &auth, Some(chunk_size), last.as_deref())
            .await
            .map_err(registry_error)?;
        Ok(response.repositories)
    }

    async fn fetch_manifest_digest(&self, image: &oci::native::Reference) -> Result<String> {
        let auth = self.auth_for(image).await;
        self.client
            .fetch_manifest_digest(image, &auth)
            .await
            .map_err(registry_error)
    }

    async fn pull_manifest_raw(
        &self,
        image: &oci::native::Reference,
        accepted_media_types: &[&str],
    ) -> Result<(Vec<u8>, String)> {
        let auth = self.auth_for(image).await;
        let (data, digest) = self
            .client
            .pull_manifest_raw(image, &auth, accepted_media_types)
            .await
            .map_err(|e| manifest_not_found_or_registry_error(e, image))?;
        Ok((data.to_vec(), digest))
    }

    async fn pull_blob_to_file(
        &self,
        image: &oci::native::Reference,
        digest: &str,
        path: &Path,
        total_size: u64,
        on_progress: ProgressFn,
    ) -> Result<()> {
        log::debug!("Pulling blob {} for image {} to {}", digest, image, path.display());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
            .map_err(|e| io_error(path, e))?;
        let writer = ProgressWriter::new(file, total_size, on_progress);
        self.client
            .pull_blob(image, digest, writer)
            .await
            .map_err(registry_error)
    }

    async fn push_manifest(&self, image: &oci::native::Reference, manifest: &oci::Manifest) -> Result<String> {
        self.client.push_manifest(image, manifest).await.map_err(registry_error)
    }

    async fn push_manifest_raw(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        media_type: &str,
    ) -> Result<String> {
        let content_type = media_type
            .parse()
            .map_err(|_| ClientError::InvalidManifest(format!("invalid media type: {}", media_type)))?;
        self.client
            .push_manifest_raw(image, data, content_type)
            .await
            .map_err(registry_error)
    }

    async fn push_blob(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        digest: &str,
        on_progress: ProgressFn,
    ) -> Result<String> {
        self.do_push_blob(image, data, digest, on_progress).await
    }

    fn box_clone(&self) -> Box<dyn OciTransport> {
        Box::new(self.clone())
    }
}

impl NativeTransport {
    /// Checks blob existence, then uploads with chunked streaming and progress.
    ///
    /// Splits data into [`self.push_chunk_size`] chunks and streams them via
    /// `push_blob_stream`. Falls back to `push_blob` (no progress) on
    /// `SpecViolationError` (registry doesn't support chunked uploads).
    async fn do_push_blob(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        digest: &str,
        on_progress: ProgressFn,
    ) -> Result<String> {
        log::debug!("Checking if blob {} already exists in registry", digest);
        match self.client.blob_exists(image, digest).await {
            Ok(true) => {
                log::debug!("Blob {} already exists, skipping upload", digest);
                on_progress(data.len() as u64);
                return Ok(digest.to_string());
            }
            Ok(false) => {
                log::debug!("Blob {} does not exist, uploading", digest);
            }
            Err(e) => {
                log::warn!("Failed to check blob {} existence, will attempt upload: {}", digest, e);
            }
        }

        let total = data.len() as u64;
        let data = Bytes::from(data);

        // Build a stream that yields chunks via zero-copy slicing and reports progress.
        // Clone data for the fallback path (Bytes clone is cheap — reference-counted).
        let fallback_data = data.clone();
        let chunk_size = self.push_chunk_size;
        let chunk_count = (total as usize).div_ceil(chunk_size);
        let progress = Arc::clone(&on_progress);
        let progress_stream = stream::unfold((0usize, 0u64), move |(index, confirmed)| {
            if index >= chunk_count {
                return std::future::ready(None);
            }
            let start = index * chunk_size;
            let end = ((index + 1) * chunk_size).min(total as usize);
            let chunk = data.slice(start..end);
            // Report progress for previously confirmed bytes (prior chunks have been
            // consumed by push_blob_stream, meaning their HTTP PATCHes completed).
            progress(confirmed);
            if confirmed > 0 {
                tracing::debug!(confirmed, total, "Uploaded {} / {} bytes", confirmed, total);
            }
            let confirmed = confirmed + chunk.len() as u64;
            std::future::ready(Some((Ok(chunk), (index + 1, confirmed))))
        });

        match self.client.push_blob_stream(image, progress_stream, digest).await {
            Ok(url) => {
                on_progress(total);
                Ok(url)
            }
            Err(oci_client::errors::OciDistributionError::SpecViolationError(violation)) => {
                log::warn!("Registry spec violation during chunked push: {}", violation);
                log::warn!("Falling back to monolithic push (no progress)");
                self.client
                    .push_blob(image, fallback_data, digest)
                    .await
                    .map_err(registry_error)
            }
            Err(e) => Err(registry_error(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::sync::Mutex;

    /// Replicates the chunking + progress stream from `do_push_blob` and verifies
    /// that progress reports lag behind yielded chunks (conservative reporting).
    #[tokio::test]
    async fn upload_progress_stream_reports_confirmed_bytes() {
        let data = Bytes::from(vec![0u8; 100]);
        let total = data.len() as u64;
        let chunk_size = 30usize;
        let chunk_count = (total as usize).div_ceil(chunk_size);

        let reports = Arc::new(Mutex::new(Vec::new()));
        let reports_clone = Arc::clone(&reports);
        let progress: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
            reports_clone.lock().unwrap().push(n);
        });

        let progress_stream = stream::unfold((0usize, 0u64), move |(index, confirmed)| {
            if index >= chunk_count {
                return std::future::ready(None);
            }
            let start = index * chunk_size;
            let end = ((index + 1) * chunk_size).min(total as usize);
            let chunk = data.slice(start..end);
            progress(confirmed);
            let confirmed = confirmed + chunk.len() as u64;
            std::future::ready(Some((Ok::<_, std::io::Error>(chunk), (index + 1, confirmed))))
        });

        // Consume the stream (simulates push_blob_stream polling).
        let collected: Vec<Bytes> = progress_stream.map(|r| r.unwrap()).collect().await;

        let reports = reports.lock().unwrap();

        // 100 bytes / 30-byte chunks = 4 chunks (30, 30, 30, 10).
        assert_eq!(collected.len(), 4);
        assert_eq!(collected[0].len(), 30);
        assert_eq!(collected[1].len(), 30);
        assert_eq!(collected[2].len(), 30);
        assert_eq!(collected[3].len(), 10);

        // Progress reports are conservative: each report reflects bytes from
        // previously consumed chunks, not the chunk being yielded.
        assert_eq!(reports.len(), 4);
        assert_eq!(reports[0], 0); // yielding chunk[0], nothing confirmed yet
        assert_eq!(reports[1], 30); // yielding chunk[1], chunk[0] confirmed
        assert_eq!(reports[2], 60); // yielding chunk[2], chunks[0-1] confirmed
        assert_eq!(reports[3], 90); // yielding chunk[3], chunks[0-2] confirmed
        // After stream completes, caller adds on_progress(total=100).
    }

    #[tokio::test]
    async fn upload_chunking_single_chunk() {
        let data = Bytes::from(vec![0u8; 10]);
        let total = data.len() as u64;
        let chunk_size = 1024usize; // larger than data
        let chunk_count = (total as usize).div_ceil(chunk_size);

        let reports = Arc::new(Mutex::new(Vec::new()));
        let reports_clone = Arc::clone(&reports);
        let progress: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
            reports_clone.lock().unwrap().push(n);
        });

        let progress_stream = stream::unfold((0usize, 0u64), move |(index, confirmed)| {
            if index >= chunk_count {
                return std::future::ready(None);
            }
            let start = index * chunk_size;
            let end = ((index + 1) * chunk_size).min(total as usize);
            let chunk = data.slice(start..end);
            progress(confirmed);
            let confirmed = confirmed + chunk.len() as u64;
            std::future::ready(Some((Ok::<_, std::io::Error>(chunk), (index + 1, confirmed))))
        });

        let collected: Vec<Bytes> = progress_stream.map(|r| r.unwrap()).collect().await;

        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].len(), 10);

        let reports = reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0], 0); // nothing confirmed when yielding the only chunk
    }
}
