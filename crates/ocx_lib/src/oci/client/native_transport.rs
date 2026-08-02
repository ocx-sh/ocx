// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt as _;
use tokio::io::AsyncWriteExt as _;

use super::builder::MAX_UPLOAD_REQUEST_BYTES;
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
            .map_err(registry_error)?;
        Ok(())
    }
}

/// Classifies any failed registry operation onto the [`ClientError`] taxonomy.
///
/// Three buckets, and the split that matters is the last two:
/// [`ClientError::RegistryTransient`] (exit 75) promises the same command may
/// succeed if it is run again, [`ClientError::Registry`] (exit 69) promises it
/// will not, and [`ClientError::Authentication`] (exit 80) says the credentials
/// are the problem. A connect that never completed and a request that timed out
/// belong in the first: nothing about the request was ever answered, least of
/// all the credentials.
///
/// # Two shapes carry the same status code
///
/// A 429 or a 403 reaches this function as *either* an enveloped
/// `RegistryError` or a bare `ServerError`, depending on which fork code path
/// produced it. `validate_registry_response` parses the OCI error envelope out
/// of any 4xx body, but the push path's `extract_location_header` turns every
/// non-`202` answer into a `ServerError` without ever looking at the body — so
/// a 401 rejecting a chunk `PATCH` arrives as `ServerError { code: 401 }`.
/// Both shapes must be classified or half the wire surface falls to the
/// catch-all.
///
/// # Why auth is checked before rate limiting on an envelope
///
/// An envelope may carry several codes. Auth wins because the costs are
/// asymmetric under retry: retrying a denial spends the budget and can trip an
/// account lockout, while reading a rate limit as an auth failure only misnames
/// a wait the caller was going to take anyway.
///
/// # Why only 429 / 502 / 503 / 504 are transient
///
/// The set is deliberately issue-scoped, not "every 5xx". A 500 is a server
/// bug — a rerun hits the same bug, so it stays 69. A 408 is rare from a
/// registry, and the case it would cover is already transient by another
/// route: reqwest surfaces a client-side timeout as `RequestError`, matched
/// above.
fn registry_error(e: oci_client::errors::OciDistributionError) -> ClientError {
    use oci_client::errors::OciDistributionError::{
        AuthenticationFailure, RegistryError, RequestError, ServerError, UnauthorizedError,
    };
    use oci_client::errors::OciErrorCode;
    match &e {
        RequestError(request) if request.is_timeout() || request.is_connect() => {
            ClientError::RegistryTransient(Box::new(e))
        }
        AuthenticationFailure(_) | UnauthorizedError { .. } => ClientError::Authentication(Box::new(e)),
        ServerError { code: 401 | 403, .. } => ClientError::Authentication(Box::new(e)),
        ServerError {
            code: 429 | 502 | 503 | 504,
            ..
        } => ClientError::RegistryTransient(Box::new(e)),
        RegistryError { envelope, .. } => {
            let has = |wanted: OciErrorCode| envelope.errors.iter().any(|err| err.code == wanted);
            if has(OciErrorCode::Unauthorized) || has(OciErrorCode::Denied) {
                ClientError::Authentication(Box::new(e))
            } else if has(OciErrorCode::Toomanyrequests) {
                ClientError::RegistryTransient(Box::new(e))
            } else {
                ClientError::Registry(Box::new(e))
            }
        }
        _ => ClientError::Registry(Box::new(e)),
    }
}

/// Maps OCI distribution errors to [`ClientError::ManifestNotFound`] when the
/// registry indicates the manifest does not exist (404 / MANIFEST_UNKNOWN),
/// and defers to [`registry_error`] for everything else.
fn manifest_not_found_or_registry_error(
    e: oci_client::errors::OciDistributionError,
    image: &oci::native::Reference,
) -> ClientError {
    use oci_client::errors::OciDistributionError::{ImageManifestNotFoundError, RegistryError, ServerError};
    use oci_client::errors::OciErrorCode;
    let is_not_found = match &e {
        ImageManifestNotFoundError(_) | ServerError { code: 404, .. } => true,
        RegistryError { envelope, .. } => envelope.errors.iter().any(|err| {
            matches!(
                err.code,
                OciErrorCode::ManifestUnknown | OciErrorCode::NotFound | OciErrorCode::NameUnknown
            )
        }),
        _ => false,
    };
    if is_not_found {
        ClientError::ManifestNotFound(image.to_string())
    } else {
        registry_error(e)
    }
}

/// Maps OCI distribution errors to [`ClientError::RepositoryNotFound`] when the
/// registry indicates the repository does not exist (404 / NAME_UNKNOWN),
/// and defers to [`registry_error`] for everything else.
///
/// Used by `list_tags` so callers can distinguish an authoritative
/// "repository absent" (legitimately empty, e.g. before the first publish)
/// from a transient failure — treating the two alike is the fail-open
/// hazard behind issue #157.
fn repository_not_found_or_registry_error(
    e: oci_client::errors::OciDistributionError,
    image: &oci::native::Reference,
) -> ClientError {
    use oci_client::errors::OciDistributionError::{RegistryError, ServerError};
    use oci_client::errors::OciErrorCode;
    let is_not_found = match &e {
        ServerError { code: 404, .. } => true,
        RegistryError { envelope, .. } => envelope
            .errors
            .iter()
            .any(|err| matches!(err.code, OciErrorCode::NotFound | OciErrorCode::NameUnknown)),
        _ => false,
    };
    if is_not_found {
        ClientError::RepositoryNotFound(format!("{}/{}", image.registry(), image.repository()))
    } else {
        registry_error(e)
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

/// Whole-blob push restarts allowed after a transient registry fault — three
/// total attempts.
///
/// Each *request* is bounded by
/// [`REGISTRY_READ_TIMEOUT`](super::builder::REGISTRY_READ_TIMEOUT) (120 s),
/// not each attempt: a restart re-uploads the whole blob, so an attempt is as
/// many bounded requests as the blob has chunks. The worst case before a push
/// finally gives up is therefore three attempts, each of which may re-upload
/// the whole blob and then stall for the full 120 s read deadline, plus the
/// 1 s + 2 s backoff. That is the ceiling being bought; anything larger stops
/// looking like resilience and starts looking like a hang.
const PUSH_RETRY_ATTEMPTS: u8 = 2;

/// Wait before the first restart, doubled per attempt (the house pattern from
/// `project::resolve::retry_fetch`).
///
/// No jitter: two retries across at most four concurrent layers is not a
/// thundering herd, and the spread jitter buys would be invisible against the
/// per-request timeout.
const PUSH_RETRY_INITIAL_BACKOFF: std::time::Duration = std::time::Duration::from_secs(1);

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
    /// own chunked-then-monolithic retry, no progress) — but only for a blob that
    /// still fits in one request. That fallback ends in a single `PUT` carrying the
    /// whole blob, so above [`MAX_UPLOAD_REQUEST_BYTES`] it is rejected by the very
    /// cap the chunking exists to respect: falling back there would trade a
    /// diagnosable spec violation for the `416` this chunking was written to avoid.
    /// Above the cap the violation is propagated instead.
    ///
    /// # Restart on a transient fault
    ///
    /// A [`ClientError::RegistryTransient`] mid-upload restarts the whole blob,
    /// up to [`PUSH_RETRY_ATTEMPTS`] times. Each attempt begins with a fresh
    /// `POST`, which abandons whatever the failed session stored — a registry
    /// discards an unreferenced upload session, so the restart needs no range
    /// reconciliation to be safe. Progress rewinds with it, harmlessly: the bar
    /// is driven by absolute `set_position`, so a restart moves it backwards
    /// rather than double-counting.
    ///
    /// A `SpecViolationError` is *not* transient and keeps its existing
    /// behaviour exactly — one monolithic fallback, itself never retried. The
    /// registry disagreed with the client about what it stored; sending the
    /// same bytes again does not resolve a disagreement.
    ///
    /// Deliberately skipped: re-checking `blob_exists` between attempts. It
    /// would only pay off in the narrow case where the committing `PUT`
    /// succeeded server-side and the response timed out. Add it when a log
    /// shows that happening.
    // ponytail: whole-blob restart. Per-chunk retry needs a fork change
    // (buffer the chunk into Bytes for a replayable body) — do it when a
    // re-sent layer measurably costs more than the fork PR.
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
        // `Bytes` clones are refcounted, so each attempt's fresh body — and the
        // fallback's — costs a pointer, not a copy of the blob.
        let data = Bytes::from(data);

        let mut attempt: u8 = 0;
        let mut backoff = PUSH_RETRY_INITIAL_BACKOFF;
        loop {
            let body = progress_body_stream(data.clone(), Arc::clone(&on_progress));
            match self
                .client
                .push_blob_stream(image, body, digest_str.as_str(), Some(total as usize))
                .await
            {
                Ok(url) => {
                    // The final frame already reported `total`; repeat so callers still
                    // see completion for a zero-length blob (which yields no frames).
                    on_progress(total);
                    return Ok(url);
                }
                Err(error @ oci_client::errors::OciDistributionError::SpecViolationError(_)) => {
                    log::warn!("Registry spec violation during streamed chunked push: {}", error);
                    if total > MAX_UPLOAD_REQUEST_BYTES as u64 {
                        log::warn!(
                            "Not falling back to buffered push: it ends in a single {total}-byte request, \
                             over the {MAX_UPLOAD_REQUEST_BYTES}-byte per-request cap, so the registry would \
                             reject it too"
                        );
                        return Err(registry_error(error));
                    }
                    log::warn!("Falling back to buffered push (chunked-then-monolithic retry, no progress)");
                    return self
                        .client
                        .push_blob(image, data.clone(), digest_str.as_str())
                        .await
                        .map_err(registry_error);
                }
                Err(e) => {
                    let mapped = registry_error(e);
                    if !matches!(mapped, ClientError::RegistryTransient(_)) || attempt >= PUSH_RETRY_ATTEMPTS {
                        return Err(mapped);
                    }
                    attempt += 1;
                    log::warn!(
                        "Transient registry failure uploading blob {digest_str} ({mapped}); \
                         restarting the upload in {backoff:?} (attempt {attempt} of {PUSH_RETRY_ATTEMPTS})"
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = backoff.saturating_mul(2);
                }
            }
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
    use std::sync::Mutex;

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

    fn server_error(code: u16) -> OciDistributionError {
        OciDistributionError::ServerError {
            code,
            url: "https://registry.test/v2/mirror/cmake/tags/list".to_string(),
            message: String::new(),
        }
    }

    /// Regression tests for issue #157 — `list_tags` errors must distinguish
    /// an authoritative "repository absent" from a transient registry failure
    /// so discover callers can stay fail-safe.
    mod repository_not_found_mapping {
        use super::*;

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
            let mapped = repository_not_found_or_registry_error(server_error(404), &reference());
            assert!(matches!(mapped, ClientError::RepositoryNotFound(_)), "got {mapped:?}");
        }

        #[test]
        fn server_5xx_is_transient_registry_failure() {
            let mapped = repository_not_found_or_registry_error(server_error(503), &reference());
            assert!(matches!(mapped, ClientError::RegistryTransient(_)), "got {mapped:?}");
        }

        #[test]
        fn rate_limit_envelope_is_transient_registry_failure() {
            let mapped =
                repository_not_found_or_registry_error(envelope_error(OciErrorCode::Toomanyrequests), &reference());
            assert!(matches!(mapped, ClientError::RegistryTransient(_)), "got {mapped:?}");
        }
    }

    /// Bug 12: a genuine 401/403 (surfaced by `oci_client` as
    /// `AuthenticationFailure` / `UnauthorizedError`, never `RequestError`) is a
    /// credentials failure — stays `Authentication` (exit 80).
    #[test]
    fn genuine_auth_rejection_stays_authentication() {
        let failure = OciDistributionError::AuthenticationFailure("bad token".to_string());
        assert!(
            matches!(registry_error(failure), ClientError::Authentication(_)),
            "AuthenticationFailure must classify as Authentication"
        );
        let unauthorized = OciDistributionError::UnauthorizedError {
            url: "https://registry.test/v2/".to_string(),
        };
        assert!(
            matches!(registry_error(unauthorized), ClientError::Authentication(_)),
            "UnauthorizedError must classify as Authentication"
        );
    }

    /// The push path never sees an enveloped 401/403: `extract_location_header`
    /// turns every non-`202` into a bare `ServerError`, so a chunk `PATCH`
    /// rejected for credentials arrives as `ServerError { code: 401 }`. Reading
    /// only the envelope shape would classify it as a plain registry failure
    /// and hand a 69 to a caller whose credentials are the actual problem.
    #[test]
    fn server_401_and_403_are_authentication() {
        for code in [401u16, 403] {
            let mapped = registry_error(server_error(code));
            assert!(
                matches!(mapped, ClientError::Authentication(_)),
                "a {code} answer must classify as Authentication, got {mapped:?}"
            );
        }
    }

    /// `DENIED` is the enveloped form of "your credentials do not cover this",
    /// and the envelope check runs before the rate-limit one: under retry the
    /// costs are asymmetric — retrying a denial burns the budget and can trip
    /// an account lockout, while surfacing a rate limit as an auth failure only
    /// misnames a wait.
    #[test]
    fn denied_envelope_is_authentication() {
        let mapped = registry_error(envelope_error(OciErrorCode::Denied));
        assert!(
            matches!(mapped, ClientError::Authentication(_)),
            "a DENIED envelope must classify as Authentication, got {mapped:?}"
        );
    }

    /// Bug 15: a token-endpoint 5xx / 429 (tagged `ServerError` in the patched
    /// `authenticate`) is never a credentials failure — it is transient
    /// (`RegistryTransient` → 75). An unparseable token body is neither: it
    /// falls to the catch-all `Registry` (69), which is what proves the
    /// catch-all still exists after the classification table grew.
    #[test]
    fn token_service_faults_are_never_authentication() {
        for code in [503u16, 429] {
            let mapped = registry_error(server_error(code));
            assert!(
                matches!(mapped, ClientError::RegistryTransient(_)),
                "token-service {code} must classify as RegistryTransient, got {mapped:?}"
            );
        }
        let decode = OciDistributionError::RegistryTokenDecodeError("bad json".to_string());
        let mapped = registry_error(decode);
        assert!(
            matches!(mapped, ClientError::Registry(_)),
            "an unparseable token body must fall through to Registry, got {mapped:?}"
        );
    }

    /// Bug 12 root cause: a connection-refused auth ping (the registry never
    /// answered) must NOT classify as `Authentication` (80). It is transient
    /// (`RegistryTransient` → 75): nothing about the credentials was ever
    /// tested, and the same command may succeed once the host is reachable.
    /// Port 1 on loopback is closed, so the connect fails immediately and
    /// deterministically.
    #[tokio::test]
    async fn connect_refused_auth_ping_is_transient_not_authentication() {
        let transport = NativeTransport::new(
            oci::native::Client::new(oci::native::ClientConfig::default()),
            crate::auth::Auth::new(),
        );
        let reference = oci::native::Reference::try_from("127.0.0.1:1/ocx/probe:latest").expect("valid reference");
        let result = transport.authenticate(&reference, oci::RegistryOperation::Pull).await;
        assert!(
            matches!(result, Err(ClientError::RegistryTransient(_))),
            "a refused connection must be RegistryTransient (TempFail/75), got {result:?}"
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
}
