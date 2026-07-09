// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt as _;
use tokio::io::AsyncWriteExt as _;

use super::error::ClientError;
use super::progress_reader::ProgressReader;
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
}

impl NativeTransport {
    pub fn new(client: oci::native::Client, auth: auth::Auth) -> Self {
        Self { client, auth }
    }

    async fn auth_for(&self, image: &oci::native::Reference) -> oci::native::Auth {
        self.auth.get_or_fallback(image.registry()).await
    }

    async fn authenticate(&self, image: &oci::native::Reference, operation: oci::RegistryOperation) -> Result<()> {
        let auth = self.auth_for(image).await;
        self.client
            .auth(image, &auth, operation)
            .await
            .map_err(auth_or_availability_error)?;
        Ok(())
    }
}

/// Classifies a failed auth ping. Only a registry that actually answered with a
/// 401/403 (surfaced by `oci_client` as `AuthenticationFailure` /
/// `UnauthorizedError`) is a genuine credentials failure
/// ([`ClientError::Authentication`] → exit 80). Everything else is an
/// availability problem ([`ClientError::Registry`] → exit 69):
/// - connect / timeout (`RequestError`) — never reached the registry;
/// - token-endpoint 5xx / 429 (`ServerError`, tagged in the patched
///   `oci_client` `authenticate`) — the registry is unhealthy or rate-limiting;
/// - an unparseable token body (`RegistryTokenDecodeError`).
fn auth_or_availability_error(e: oci_client::errors::OciDistributionError) -> ClientError {
    use oci_client::errors::OciDistributionError::{RegistryTokenDecodeError, RequestError, ServerError};
    match &e {
        RequestError(request) if request.is_connect() || request.is_timeout() => ClientError::Registry(Box::new(e)),
        ServerError { .. } | RegistryTokenDecodeError(_) => ClientError::Registry(Box::new(e)),
        _ => ClientError::Authentication(Box::new(e)),
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

/// Maps OCI distribution errors to [`ClientError::ReferrersUnsupported`] when
/// the registry returns HTTP 404 for `/v2/<name>/referrers/<digest>`, and
/// falls back to [`ClientError::Registry`] for everything else.
///
/// A 404 here means the endpoint itself is absent (registry lacks the OCI
/// 1.1 Referrers API) — distinct from a 200 with an empty `manifests` array,
/// which means the subject exists but has zero known referrers.
fn referrers_unsupported_or_registry_error(
    e: oci_client::errors::OciDistributionError,
    image: &oci::native::Reference,
) -> ClientError {
    use oci_client::errors::OciDistributionError::*;
    use oci_client::errors::OciErrorCode;
    let registry = image.resolve_registry().to_string();
    match &e {
        RegistryError { envelope, .. } => {
            let is_not_found = envelope.errors.iter().any(|err| {
                matches!(
                    err.code,
                    OciErrorCode::ManifestUnknown | OciErrorCode::NotFound | OciErrorCode::NameUnknown
                )
            });
            if is_not_found {
                ClientError::ReferrersUnsupported { registry }
            } else {
                ClientError::Registry(Box::new(e))
            }
        }
        ServerError { code: 404, .. } => ClientError::ReferrersUnsupported { registry },
        _ => ClientError::Registry(Box::new(e)),
    }
}

/// Filters referrer entries by `artifact_type` (when provided) and converts
/// the survivors to [`oci::Descriptor`].
///
/// The OCI spec permits a server to ignore the `artifactType` query filter
/// (or apply it without setting the advisory `OCI-Filters-Applied` header),
/// so this client-side pass is the only filtering callers can rely on.
fn filter_and_convert_referrers(
    entries: Vec<oci_client::manifest::ImageIndexEntry>,
    artifact_type: Option<&str>,
) -> Vec<oci::Descriptor> {
    entries
        .into_iter()
        .filter(|entry| match artifact_type {
            Some(wanted) => entry.artifact_type.as_deref() == Some(wanted),
            None => true,
        })
        .map(|entry| oci::Descriptor {
            media_type: entry.media_type,
            digest: entry.digest,
            size: entry.size,
            urls: None,
            artifact_type: None,
            annotations: entry.annotations,
        })
        .collect()
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
            .map_err(|e| manifest_not_found_or_registry_error(e, image))
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

    async fn pull_blob_to_file(&self, image: &oci::native::Reference, digest: &oci::Digest, path: &Path) -> Result<()> {
        let digest_str = digest.to_string();
        log::debug!("Pulling blob {} for image {} to {}", digest_str, image, path.display());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
            .map_err(|e| io_error(path, e))?;
        self.client
            .pull_blob(image, digest_str.as_str(), &mut file)
            .await
            .map_err(registry_error)?;
        // Explicitly flush + close the write handle before returning.
        //
        // On Windows, `tokio::fs::File` drop is asynchronous — the underlying
        // OS handle is closed on a background threadpool thread, not during
        // the drop call itself. If the caller immediately reopens the same
        // path (a subsequent reopen for read right after this
        // returns), the still-open write handle can cause ERROR_LOCK_VIOLATION
        // (os error 33). POSIX advisory locks are optional so Linux tolerates
        // the overlap silently. `shutdown()` drives the tokio file through its
        // internal sync + close path synchronously before we return.
        file.shutdown().await.map_err(|e| io_error(path, e))?;
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

    async fn push_referrer_manifest(
        &self,
        image: &oci::native::Reference,
        _subject_digest: &oci::Digest,
        manifest_bytes: &[u8],
        media_type: &str,
    ) -> Result<oci::Descriptor> {
        // The manifest JSON already carries the `subject` field (built by the
        // caller) — pushing it is a plain manifest PUT addressed by the
        // manifest's OWN digest (referrer manifests are not tagged).
        let expected_size = i64::try_from(manifest_bytes.len()).map_err(|_| {
            ClientError::InvalidManifest(format!(
                "referrer manifest size {} exceeds i64::MAX",
                manifest_bytes.len()
            ))
        })?;
        let expected_digest = oci::Algorithm::Sha256.hash(manifest_bytes).to_string();
        let target = image.clone_with_digest(expected_digest.clone());

        // The push is digest-addressed (`PUT /v2/<repo>/manifests/<expected_digest>`)
        // over the exact bytes we hashed, so a spec-compliant registry stores the
        // manifest at precisely `expected_digest` or rejects the request. The
        // transport's `push_manifest_raw` returns the pullable manifest URL (the
        // `Location` header), NOT a bare digest, so it cannot be compared to a
        // digest — integrity is already guaranteed by the content-addressed PUT.
        self.push_manifest_raw(&target, manifest_bytes.to_vec(), media_type)
            .await?;

        Ok(oci::Descriptor {
            media_type: media_type.to_string(),
            digest: expected_digest,
            size: expected_size,
            urls: None,
            artifact_type: None,
            annotations: None,
        })
    }

    async fn list_referrers(
        &self,
        image: &oci::native::Reference,
        subject_digest: &oci::Digest,
        artifact_type: Option<&str>,
    ) -> Result<Vec<oci::Descriptor>> {
        let target = image.clone_with_digest(subject_digest.to_string());
        let index = self
            .client
            .pull_referrers(&target, artifact_type)
            .await
            .map_err(|e| referrers_unsupported_or_registry_error(e, image))?;

        Ok(filter_and_convert_referrers(index.manifests, artifact_type))
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
    /// Checks blob existence, then uploads the blob via a streamed chunked push
    /// with fluent progress.
    ///
    /// Wraps the in-RAM blob in a [`ProgressReader`]-backed byte stream (see
    /// [`progress_body_stream`]) and hands it to the fork's `push_blob_stream` with
    /// the total size. The fork streams each `push_chunk_size`-bounded PATCH body
    /// directly from that stream, pulling it only as the socket accepts more, so
    /// progress advances per [`UPLOAD_FRAME_SIZE`] frame as it is pulled for the
    /// wire (not in `push_chunk_size` upload-session steps) while each request body
    /// stays bounded for proxies/registries that cap single-request body size. On
    /// `SpecViolationError` it falls back to the fork's buffered `push_blob` (its
    /// own chunked-then-monolithic retry, no progress).
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

        // Clone the blob for the fallback path (Bytes clone is cheap — refcounted).
        let fallback_data = data.clone();

        match self
            .client
            .push_blob_stream(
                image,
                progress_body_stream(data, Arc::clone(&on_progress)),
                digest_str.as_str(),
                Some(total as usize),
            )
            .await
        {
            Ok(url) => {
                // The final frame already reported `total`; repeat so callers still
                // see completion for a zero-length blob (which yields no frames).
                on_progress(total);
                Ok(url)
            }
            Err(oci_client::errors::OciDistributionError::SpecViolationError(violation)) => {
                log::warn!("Registry spec violation during streamed chunked push: {}", violation);
                log::warn!("Falling back to buffered push (chunked-then-monolithic retry, no progress)");
                self.client
                    .push_blob(image, fallback_data, digest_str.as_str())
                    .await
                    .map_err(registry_error)
            }
            Err(e) => Err(registry_error(e)),
        }
    }
}

/// Frame size for the streamed push body — the granularity at which upload
/// progress advances. Small enough that progress looks smooth, large enough that
/// per-frame overhead stays negligible against the blob size.
const UPLOAD_FRAME_SIZE: usize = 128 * 1024;

/// Wraps an in-RAM blob as a progress-reporting byte stream for a streamed push.
///
/// The blob is exposed as an [`AsyncRead`](tokio::io::AsyncRead) via
/// [`std::io::Cursor`], teed through [`ProgressReader`] (cumulative byte count on
/// every read), then framed into [`UPLOAD_FRAME_SIZE`] chunks by
/// [`ReaderStream`](tokio_util::io::ReaderStream). The fork's `push_blob_stream`
/// pulls from this stream only as the socket accepts more of each streamed PATCH
/// body (backpressure), so `ProgressReader` fires per [`UPLOAD_FRAME_SIZE`] frame
/// as it is pulled for the wire — progress leads the actual socket hand-off by at
/// most one frame. This mirrors the pull path (`Client::pull_layer`), which wraps
/// the fork's streaming reader in the same [`ProgressReader`].
fn progress_body_stream(
    data: Bytes,
    on_progress: ProgressFn,
) -> impl futures::Stream<Item = std::result::Result<Bytes, oci_client::errors::OciDistributionError>> + Send + 'static
{
    let reader = ProgressReader::new(std::io::Cursor::new(data), on_progress);
    tokio_util::io::ReaderStream::with_capacity(reader, UPLOAD_FRAME_SIZE).map(|frame| {
        // `Cursor` reads never fail; this only reconciles the frame error type with
        // the fork's stream item (`Result<Bytes, OciDistributionError>`).
        frame.map_err(|error| oci_client::errors::OciDistributionError::GenericError(Some(error.to_string())))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use futures::stream;
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

    /// Regression tests for issue #194 — `list_referrers` must distinguish a
    /// registry that lacks the OCI 1.1 Referrers API (404 on the endpoint)
    /// from a subject with zero referrers (200, empty `manifests`), and from
    /// a transient registry failure.
    mod referrers_unsupported_mapping {
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
                url: "https://registry.test/v2/mirror/cmake/referrers/sha256:1111".to_string(),
            }
        }

        #[test]
        fn server_404_maps_to_referrers_unsupported() {
            let error = OciDistributionError::ServerError {
                code: 404,
                url: "https://registry.test/v2/mirror/cmake/referrers/sha256:1111".to_string(),
                message: "not found".to_string(),
            };
            let mapped = referrers_unsupported_or_registry_error(error, &reference());
            assert!(
                matches!(&mapped, ClientError::ReferrersUnsupported { registry } if registry == "registry.test"),
                "expected ReferrersUnsupported, got {mapped:?}"
            );
        }

        #[test]
        fn envelope_not_found_maps_to_referrers_unsupported() {
            let mapped =
                referrers_unsupported_or_registry_error(envelope_error(OciErrorCode::NameUnknown), &reference());
            assert!(
                matches!(mapped, ClientError::ReferrersUnsupported { .. }),
                "got {mapped:?}"
            );
        }

        #[test]
        fn server_5xx_stays_registry_error() {
            let error = OciDistributionError::ServerError {
                code: 503,
                url: "https://registry.test/v2/mirror/cmake/referrers/sha256:1111".to_string(),
                message: "service unavailable".to_string(),
            };
            let mapped = referrers_unsupported_or_registry_error(error, &reference());
            assert!(matches!(mapped, ClientError::Registry(_)), "got {mapped:?}");
        }

        #[test]
        fn rate_limit_envelope_stays_registry_error() {
            let mapped =
                referrers_unsupported_or_registry_error(envelope_error(OciErrorCode::Toomanyrequests), &reference());
            assert!(matches!(mapped, ClientError::Registry(_)), "got {mapped:?}");
        }
    }

    /// Unit tests for [`filter_and_convert_referrers`] — the client-side
    /// `artifactType` filter that must apply regardless of whether the
    /// registry honored the server-side query filter (OCI spec §"Listing
    /// Referrers": servers MAY ignore `?artifactType=`).
    mod referrer_filtering {
        use super::*;

        fn entry(digest: &str, artifact_type: Option<&str>) -> oci_client::manifest::ImageIndexEntry {
            oci_client::manifest::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: digest.to_string(),
                size: 123,
                platform: None,
                artifact_type: artifact_type.map(str::to_string),
                annotations: None,
            }
        }

        #[test]
        fn no_filter_passes_all_entries_through() {
            let entries = vec![
                entry("sha256:aaa", Some("application/vnd.ocx.signature")),
                entry("sha256:bbb", None),
            ];
            let result = filter_and_convert_referrers(entries, None);
            assert_eq!(result.len(), 2);
            assert_eq!(result[0].digest, "sha256:aaa");
            assert_eq!(result[1].digest, "sha256:bbb");
        }

        #[test]
        fn filter_keeps_only_matching_artifact_type() {
            let entries = vec![
                entry("sha256:aaa", Some("application/vnd.ocx.signature")),
                entry("sha256:bbb", Some("application/vnd.ocx.sbom")),
                entry("sha256:ccc", None),
            ];
            let result = filter_and_convert_referrers(entries, Some("application/vnd.ocx.signature"));
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].digest, "sha256:aaa");
        }

        #[test]
        fn filter_with_no_matches_returns_empty() {
            let entries = vec![entry("sha256:aaa", Some("application/vnd.ocx.sbom"))];
            let result = filter_and_convert_referrers(entries, Some("application/vnd.ocx.signature"));
            assert!(result.is_empty());
        }

        #[test]
        fn empty_manifests_returns_empty_vec_not_error() {
            // A 200 response with an empty `manifests` array means "subject
            // exists, zero referrers" — must be `Ok(vec![])`, never an error.
            let result = filter_and_convert_referrers(vec![], None);
            assert!(result.is_empty());
        }
    }

    /// Bug 12: a genuine 401/403 (surfaced by `oci_client` as
    /// `AuthenticationFailure` / `UnauthorizedError`, never `RequestError`) is a
    /// credentials failure — stays `Authentication` (exit 80).
    #[test]
    fn genuine_auth_rejection_stays_authentication() {
        use oci_client::errors::OciDistributionError;
        let failure = OciDistributionError::AuthenticationFailure("bad token".to_string());
        assert!(
            matches!(auth_or_availability_error(failure), ClientError::Authentication(_)),
            "AuthenticationFailure must classify as Authentication"
        );
        let unauthorized = OciDistributionError::UnauthorizedError {
            url: "https://registry.test/v2/".to_string(),
        };
        assert!(
            matches!(auth_or_availability_error(unauthorized), ClientError::Authentication(_)),
            "UnauthorizedError must classify as Authentication"
        );
    }

    /// Bug 15: a token-endpoint 5xx / 429 (tagged `ServerError` in the patched
    /// `authenticate`) or an unparseable token body is an availability failure —
    /// `Registry` (69), not `Authentication` (80).
    #[test]
    fn token_service_unavailable_is_registry_not_authentication() {
        use oci_client::errors::OciDistributionError;
        for code in [503u16, 429] {
            let server = OciDistributionError::ServerError {
                code,
                url: "https://registry.test/token".to_string(),
                message: "down".to_string(),
            };
            assert!(
                matches!(auth_or_availability_error(server), ClientError::Registry(_)),
                "token-service {code} must classify as Registry"
            );
        }
        let decode = OciDistributionError::RegistryTokenDecodeError("bad json".to_string());
        assert!(
            matches!(auth_or_availability_error(decode), ClientError::Registry(_)),
            "an unparseable token body must classify as Registry"
        );
    }

    /// Bug 12 root cause: a connection-refused auth ping (the registry never
    /// answered) must classify `Registry` (→ Unavailable, exit 69), NOT
    /// `Authentication` (80). Port 1 on loopback is closed, so the connect fails
    /// immediately and deterministically.
    #[tokio::test]
    async fn connect_refused_auth_ping_is_registry_not_authentication() {
        let transport = NativeTransport::new(
            oci::native::Client::new(oci::native::ClientConfig::default()),
            crate::auth::Auth::new(),
        );
        let reference = oci::native::Reference::try_from("127.0.0.1:1/ocx/probe:latest").expect("valid reference");
        let result = transport.authenticate(&reference, oci::RegistryOperation::Pull).await;
        assert!(
            matches!(result, Err(ClientError::Registry(_))),
            "a refused connection must be Registry (Unavailable/69), got {result:?}"
        );
    }

    /// Drives `progress_body_stream` to completion, returning the yielded frames
    /// and the cumulative progress values reported along the way.
    async fn collect_push_frames_and_progress(blob: Vec<u8>) -> (Vec<Bytes>, Vec<u64>) {
        let reports = Arc::new(Mutex::new(Vec::<u64>::new()));
        let reports_clone = Arc::clone(&reports);
        let on_progress: ProgressFn = Arc::new(move |n| reports_clone.lock().unwrap().push(n));

        let frames: Vec<Bytes> = progress_body_stream(Bytes::from(blob), on_progress)
            .map(|frame| frame.expect("Cursor-backed frames never error"))
            .collect()
            .await;

        let reports = reports.lock().unwrap().clone();
        (frames, reports)
    }

    /// Concatenates streamed frames back into a single buffer.
    fn reassemble(frames: &[Bytes]) -> Vec<u8> {
        frames.iter().flat_map(|frame| frame.iter().copied()).collect()
    }

    /// Streamed-push progress wiring (the push-side mirror of the `ProgressReader`
    /// unit test): the `Cursor → ProgressReader → ReaderStream` pipeline that
    /// `do_push_blob` hands to `push_blob_stream` must report cumulative bytes on
    /// each frame — strictly increasing across frames, ending exactly at the blob
    /// size — and must forward the blob bytes unchanged.
    #[tokio::test]
    async fn streamed_push_progress_is_cumulative_and_reaches_total() {
        // Larger than UPLOAD_FRAME_SIZE so the stream yields several frames.
        let blob: Vec<u8> = (0..300 * 1024).map(|byte| byte as u8).collect();
        let total = blob.len() as u64;

        let (frames, reports) = collect_push_frames_and_progress(blob.clone()).await;

        assert_eq!(
            reassemble(&frames),
            blob,
            "streamed frames must reassemble to the original blob"
        );
        assert!(
            reports.len() > 1,
            "a >128 KiB blob must produce multiple progress callbacks, got {}",
            reports.len()
        );
        for window in reports.windows(2) {
            assert!(
                window[1] > window[0],
                "progress must be strictly increasing across frames: {reports:?}"
            );
        }
        assert_eq!(
            *reports.last().unwrap(),
            total,
            "final progress callback must equal the blob size"
        );
    }

    /// A blob smaller than one frame (the common case for OCX config / README /
    /// patch layers) must still stream unchanged and report a single cumulative
    /// callback equal to the blob size.
    #[tokio::test]
    async fn streamed_push_sub_frame_blob_reports_total_once() {
        let blob: Vec<u8> = (0..1000u32).map(|byte| byte as u8).collect();
        let total = blob.len() as u64;

        let (frames, reports) = collect_push_frames_and_progress(blob.clone()).await;

        assert_eq!(reassemble(&frames), blob, "sub-frame blob must reassemble unchanged");
        assert_eq!(
            reports,
            vec![total],
            "a blob smaller than one frame must report exactly one callback equal to total"
        );
    }

    /// A zero-length blob yields no frames, so `progress_body_stream` fires no
    /// callbacks — this is why `do_push_blob` re-fires `on_progress(total)` after a
    /// successful push, to still signal completion for an empty blob.
    #[tokio::test]
    async fn streamed_push_empty_blob_yields_no_frames_or_progress() {
        let (frames, reports) = collect_push_frames_and_progress(Vec::new()).await;

        assert!(
            frames.is_empty(),
            "empty blob must yield no frames, got {}",
            frames.len()
        );
        assert!(
            reports.is_empty(),
            "empty blob must fire no progress callbacks, got {reports:?}"
        );
    }

    /// Creates a chunked progress stream that mirrors the pre-streaming
    /// `do_push_blob` upload logic (progress reports lag one chunk behind the
    /// yielded chunk). Retained as a standalone regression witness for the
    /// conservative-reporting invariant, independent of the streamed push path.
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
