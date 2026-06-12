// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream;
use tokio::io::AsyncWriteExt as _;

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
            .map_err(|e| ClientError::Authentication(Box::new(e)))?;
        Ok(())
    }
}

fn registry_error(e: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> ClientError {
    ClientError::Registry(e.into())
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
                ClientError::Registry(Box::new(e))
            }
        }
        ServerError { code: 404, .. } => ClientError::ManifestNotFound(image.to_string()),
        _ => ClientError::Registry(Box::new(e)),
    }
}

/// Maps OCI distribution errors to [`ClientError::RepositoryNotFound`] when the
/// registry indicates the repository does not exist (404 / NAME_UNKNOWN),
/// and falls back to [`ClientError::Registry`] for everything else.
///
/// Used by `list_tags` so callers can distinguish an authoritative
/// "repository absent" (legitimately empty, e.g. before the first publish)
/// from a transient failure — treating the two alike is the fail-open
/// hazard behind issue #157.
fn repository_not_found_or_registry_error(
    e: oci_client::errors::OciDistributionError,
    image: &oci::native::Reference,
) -> ClientError {
    use oci_client::errors::OciDistributionError::*;
    use oci_client::errors::OciErrorCode;
    let repository = format!("{}/{}", image.registry(), image.repository());
    match &e {
        RegistryError { envelope, .. } => {
            let is_not_found = envelope
                .errors
                .iter()
                .any(|err| matches!(err.code, OciErrorCode::NotFound | OciErrorCode::NameUnknown));
            if is_not_found {
                ClientError::RepositoryNotFound(repository)
            } else {
                ClientError::Registry(Box::new(e))
            }
        }
        ServerError { code: 404, .. } => ClientError::RepositoryNotFound(repository),
        _ => ClientError::Registry(Box::new(e)),
    }
}

fn io_error(path: &Path, e: impl Into<std::io::Error>) -> ClientError {
    ClientError::Io {
        path: path.to_path_buf(),
        source: e.into(),
    }
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
            .map_err(|e| repository_not_found_or_registry_error(e, image))?;
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

    async fn pull_blob(&self, image: &oci::native::Reference, digest: &oci::Digest) -> Result<Vec<u8>> {
        let digest_str = digest.to_string();
        log::debug!("Pulling blob {} for image {} into memory", digest_str, image);
        let mut buf = Vec::new();
        self.client
            .pull_blob(image, digest_str.as_str(), &mut buf)
            .await
            .map_err(registry_error)?;
        Ok(buf)
    }

    async fn pull_blob_to_file(
        &self,
        image: &oci::native::Reference,
        digest: &oci::Digest,
        path: &Path,
        total_size: u64,
        on_progress: ProgressFn,
    ) -> Result<()> {
        let digest_str = digest.to_string();
        log::debug!("Pulling blob {} for image {} to {}", digest_str, image, path.display());
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
        // Pass `&mut writer` so we retain the writer after pull_blob returns,
        // allowing an explicit shutdown below. ProgressWriter<W>: Unpin so
        // &mut ProgressWriter<W>: AsyncWrite via the tokio blanket impl.
        let mut writer = ProgressWriter::new(file, total_size, on_progress);
        self.client
            .pull_blob(image, digest_str.as_str(), &mut writer)
            .await
            .map_err(registry_error)?;
        // Explicitly flush + close the write handle before returning.
        //
        // On Windows, `tokio::fs::File` drop is asynchronous — the underlying
        // OS handle is closed on a background threadpool thread, not during
        // the drop call itself. If the caller immediately reopens the same
        // path (e.g. `verify_blob_digest` opens for read right after this
        // returns), the still-open write handle can cause ERROR_LOCK_VIOLATION
        // (os error 33). POSIX advisory locks are optional so Linux tolerates
        // the overlap silently. `shutdown()` drives the tokio file through its
        // internal sync + close path synchronously before we return.
        writer.shutdown().await.map_err(|e| io_error(path, e))?;
        Ok(())
    }

    async fn head_blob(&self, image: &oci::native::Reference, digest: &oci::Digest) -> Result<u64> {
        let digest_str = digest.to_string();
        log::debug!("HEAD blob {} for image {}", digest_str, image);
        match self.client.fetch_blob_size(image, digest_str.as_str()).await {
            Ok(Some(size)) => Ok(size),
            Ok(None) => Err(ClientError::blob_not_found(image, digest)),
            Err(e) => Err(registry_error(e)),
        }
    }

    async fn pull_blob_streaming(
        &self,
        image: &oci::native::Reference,
        digest: &oci::Digest,
    ) -> Result<Box<dyn tokio::io::AsyncRead + Send + Unpin + 'static>> {
        let digest_str = digest.to_string();
        log::debug!("Streaming blob {} for image {}", digest_str, image);

        // Call the fork's public `pull_blob_stream`, which wraps the response
        // in a `VerifyingStream` that verifies the digest at stream end.
        // Digest mismatch surfaces as `io::Error::other(DigestError::VerificationError)`
        // at the point where the stream yields `None`.
        let sized_stream = self
            .client
            .pull_blob_stream(image, digest_str.as_str())
            .await
            .map_err(registry_error)?;

        // Adapt `SizedStream` (a `BoxStream<Result<Bytes, io::Error>>`) to
        // `AsyncRead` using `tokio_util::io::StreamReader`. The map_err is a
        // no-op here (both sides are `io::Error`) but makes the type explicit.
        let stream_reader = tokio_util::io::StreamReader::new(sized_stream.stream);

        Ok(Box::new(stream_reader))
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
        digest: &oci::Digest,
        on_progress: ProgressFn,
    ) -> Result<String> {
        self.do_push_blob(image, data, digest, on_progress).await
    }

    fn box_clone(&self) -> Box<dyn OciTransport> {
        Box::new(self.clone())
    }
}

/// Checks whether a borrowed `io::Error` carries a fork `DigestError::VerificationError`
/// and, if so, returns the corresponding `ClientError::DigestMismatch`.
///
/// This is the shared detection core. Both the owned-error path
/// ([`map_fork_io_error_to_client_error`]) and the chain-walk path in
/// `pull_layer` use this function to avoid duplicating the downcast logic.
///
/// Returns `None` if the error is not a fork digest error; returns
/// `Some(ClientError::DigestMismatch {...})` on detection.
/// Checks whether a borrowed `io::Error` carries a fork `DigestError::VerificationError`
/// and, if so, returns the corresponding `ClientError::DigestMismatch`.
///
/// This is the shared detection core. Both the owned-error path
/// ([`map_fork_io_error_to_client_error`]) and the chain-walk path in
/// `pull_layer` use this function to avoid duplicating the downcast logic.
///
/// Returns `None` if the error is not a typed fork digest error; the caller
/// maps `None` to `Io`. **No string-fallback** — any `io::Error` whose inner
/// source is not a typed `DigestError::VerificationError` maps to `Io`, not
/// `DigestMismatch`. A string-fallback would be CWE-20 (spoofable: any
/// io::Error whose message happens to contain "digest" could produce a spurious
/// `DigestMismatch{expected: ""}` that would be logged and reported to users
/// as a security event when none occurred).
pub(super) fn check_fork_io_error(error: &std::io::Error) -> Option<ClientError> {
    // The fork produces io::Error::other(DigestError::VerificationError { expected, actual }).
    // We detect this by downcasting the inner error stored in the io::Error.
    // `io::Error::get_ref()` returns `Option<&(dyn Error + Send + Sync + 'static)>`.
    if let Some(inner) = error.get_ref()
        && let Some(oci_client::errors::DigestError::VerificationError { expected, actual }) =
            inner.downcast_ref::<oci_client::errors::DigestError>()
    {
        return Some(ClientError::DigestMismatch {
            expected: expected.clone(),
            actual: actual.clone(),
        });
    }
    None
}

/// Maps an `io::Error` that originates from the fork's `VerifyingStream`
/// (which surfaces digest mismatch as `io::Error { kind: Other, source: DigestError }`)
/// to the typed [`ClientError::DigestMismatch`].
///
/// Any other `io::Error` is mapped to `Err(ClientError::Io)` with no path context
/// (the caller adds path context when needed). A non-digest io::Error results in
/// `Err(ClientError::Io { path: PathBuf::new(), source: error })`.
///
/// # Design
///
/// The fork's `VerifyingStream` (in `external/rust-oci-client/src/blob.rs`) wraps
/// the response stream and, at stream end, compares the accumulated digest against
/// the expected one. On mismatch it yields:
///   `io::Error::new(io::ErrorKind::Other, DigestError::VerificationError { ... })`
///
/// OCX must convert this to `ClientError::DigestMismatch` (not `ClientError::Io`) so
/// the error taxonomy holds regardless of whether the fork's verifier or
/// OCX's `HashingAsyncReader` fires first. See spec §D2 "two verifiers, one typed error".
///
/// Used only in unit tests that validate the mapping contract. Production code uses
/// [`check_fork_io_error`] (the borrowed-ref extraction core) directly.
#[cfg(test)]
pub(super) fn map_fork_io_error_to_client_error(error: std::io::Error) -> super::transport::Result<()> {
    if let Some(client_err) = check_fork_io_error(&error) {
        return Err(client_err);
    }
    Err(ClientError::Io {
        path: std::path::PathBuf::new(),
        source: error,
    })
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
        digest: &oci::Digest,
        on_progress: ProgressFn,
    ) -> Result<String> {
        let digest_str = digest.to_string();
        log::debug!("Checking if blob {} already exists in registry", digest_str);
        match self.client.blob_exists(image, digest_str.as_str()).await {
            Ok(true) => {
                log::debug!("Blob {} already exists, skipping upload", digest_str);
                on_progress(data.len() as u64);
                return Ok(digest_str);
            }
            Ok(false) => {
                log::debug!("Blob {} does not exist, uploading", digest_str);
            }
            Err(e) => {
                log::warn!(
                    "Failed to check blob {} existence, will attempt upload: {}",
                    digest_str,
                    e
                );
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

        match self
            .client
            .push_blob_stream(image, progress_stream, digest_str.as_str())
            .await
        {
            Ok(url) => {
                on_progress(total);
                Ok(url)
            }
            Err(oci_client::errors::OciDistributionError::SpecViolationError(violation)) => {
                log::warn!("Registry spec violation during chunked push: {}", violation);
                log::warn!("Falling back to monolithic push (no progress)");
                self.client
                    .push_blob(image, fallback_data, digest_str.as_str())
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

    /// Regression tests for issue #157 — `list_tags` errors must distinguish
    /// an authoritative "repository absent" from a transient registry failure
    /// so discover callers can stay fail-safe.
    mod repository_not_found_mapping {
        use super::*;
        use oci_client::errors::{OciDistributionError, OciEnvelope, OciError, OciErrorCode};

        fn reference() -> oci::native::Reference {
            oci::native::Reference::try_from("registry.test/mirror/cmake:4.3.3").expect("valid reference")
        }

        fn envelope_error(code: OciErrorCode) -> OciDistributionError {
            OciDistributionError::RegistryError {
                envelope: OciEnvelope {
                    errors: vec![OciError {
                        code,
                        message: String::new(),
                        detail: serde_json::Value::Null,
                    }],
                },
                url: "https://registry.test/v2/mirror/cmake/tags/list".to_string(),
            }
        }

        #[test]
        fn name_unknown_maps_to_repository_not_found() {
            let mapped =
                repository_not_found_or_registry_error(envelope_error(OciErrorCode::NameUnknown), &reference());
            assert!(
                matches!(&mapped, ClientError::RepositoryNotFound(repo) if repo == "registry.test/mirror/cmake"),
                "expected RepositoryNotFound, got {mapped:?}"
            );
        }

        #[test]
        fn not_found_code_maps_to_repository_not_found() {
            let mapped = repository_not_found_or_registry_error(envelope_error(OciErrorCode::NotFound), &reference());
            assert!(matches!(mapped, ClientError::RepositoryNotFound(_)), "got {mapped:?}");
        }

        #[test]
        fn server_404_maps_to_repository_not_found() {
            let error = OciDistributionError::ServerError {
                code: 404,
                url: "https://registry.test/v2/mirror/cmake/tags/list".to_string(),
                message: "not found".to_string(),
            };
            let mapped = repository_not_found_or_registry_error(error, &reference());
            assert!(matches!(mapped, ClientError::RepositoryNotFound(_)), "got {mapped:?}");
        }

        #[test]
        fn server_5xx_stays_registry_error() {
            let error = OciDistributionError::ServerError {
                code: 503,
                url: "https://registry.test/v2/mirror/cmake/tags/list".to_string(),
                message: "service unavailable".to_string(),
            };
            let mapped = repository_not_found_or_registry_error(error, &reference());
            assert!(matches!(mapped, ClientError::Registry(_)), "got {mapped:?}");
        }

        #[test]
        fn rate_limit_envelope_stays_registry_error() {
            let mapped =
                repository_not_found_or_registry_error(envelope_error(OciErrorCode::Toomanyrequests), &reference());
            assert!(matches!(mapped, ClientError::Registry(_)), "got {mapped:?}");
        }
    }

    /// Creates a chunked progress stream that mirrors `do_push_blob`'s upload logic.
    ///
    /// Returns the progress reports collector and the byte stream.
    fn make_progress_stream(
        data: Bytes,
        chunk_size: usize,
    ) -> (
        Arc<Mutex<Vec<u64>>>,
        impl futures::Stream<Item = std::result::Result<Bytes, std::io::Error>>,
    ) {
        let total = data.len() as u64;
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
        (reports, progress_stream)
    }

    /// Replicates the chunking + progress stream from `do_push_blob` and verifies
    /// that progress reports lag behind yielded chunks (conservative reporting).
    #[tokio::test]
    async fn upload_progress_stream_reports_confirmed_bytes() {
        let (reports, progress_stream) = make_progress_stream(Bytes::from(vec![0u8; 100]), 30);

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
        let (reports, progress_stream) = make_progress_stream(Bytes::from(vec![0u8; 10]), 1024);

        let collected: Vec<Bytes> = progress_stream.map(|r| r.unwrap()).collect().await;

        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].len(), 10);

        let reports = reports.lock().unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0], 0); // nothing confirmed when yielding the only chunk
    }
}
