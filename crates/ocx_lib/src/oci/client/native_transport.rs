// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt as _;
use tokio::io::AsyncWriteExt as _;

use super::builder::MAX_UPLOAD_REQUEST_BYTES;
use super::error::ClientError;
use super::progress_reader::ProgressReader;
use super::transport::{MountOutcome, OciTransport, ProgressFn, Result};
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
///
/// # Why two wire failures leave the registry buckets entirely
///
/// A content-type refusal and a digest verification failure both say the same
/// thing: the bytes that arrived are not the bytes that were asked for. Exit 65
/// says exactly that, and says a rerun cannot fix it. Exit 69 says the opposite
/// -- it invites a retry wrapper to fetch the same wrong bytes again, and it
/// reports a mis-routed mirror as an unavailable registry.
///
/// # Why there is no `_` arm
///
/// The fork's `OciDistributionError` is not `#[non_exhaustive]` and is
/// path-vendored, so ocx controls the version. A catch-all here would ship
/// every future guard the fork grows silently mis-classified -- which is
/// exactly how `CrossHostRefused` and `InsecureAuthRealm` first landed on
/// "registry unreachable, retry with backoff". An exhaustive match turns that
/// into a compile error at the next submodule bump, which is the only
/// compile-time guarantee available.
pub(crate) fn registry_error(e: oci_client::errors::OciDistributionError) -> ClientError {
    use oci_client::errors::DigestError as WireDigestError;
    use oci_client::errors::OciDistributionError::{
        AuthenticationFailure, ConfigConversionError, CrossHostRefused, DigestError, GenericError, HeaderValueError,
        ImageIndexParsingNoPlatformResolverError, ImageManifestNotFoundError, IncompatibleLayerMediaTypeError,
        InsecureAuthRealm, IoError, JsonError, ManifestEncodingError, ManifestParsingError, PullNoLayersError,
        PushLayerNoDataError, PushNoDataError, RegistryError, RegistryNoDigestError, RegistryNoLocationError,
        RegistryTokenDecodeError, RequestError, ResponseTooLargeError, ServerError, SpecViolationError,
        UnauthorizedError, UnexpectedContentType, UnsupportedMediaTypeError, UnsupportedSchemaVersionError,
        UrlParseError, VersionedParsingError,
    };
    use oci_client::errors::OciErrorCode;
    match &e {
        // A connect that never completed against an `https://` URL is the shape
        // a plain-HTTP registry produces, and the scheme is itself the proof
        // that the host is NOT in the plain-HTTP allowance -- a host that were
        // would have been contacted over `http`. So the remediation can be
        // named without knowing the allowance set here. It stays TempFail: a
        // refused connection and a DNS failure are the common causes and both
        // are retryable, and mis-labelling those as terminal would also stop
        // `push_blob`'s transient-only retry.
        RequestError(request) if request.is_connect() => match https_allowance_name(request.url()) {
            Some(host) => ClientError::RegistryTransient(Box::new(PlainHttpAllowanceHint { host, source: e })),
            None => ClientError::RegistryTransient(Box::new(e)),
        },
        RequestError(request) if request.is_timeout() => ClientError::RegistryTransient(Box::new(e)),
        // The two typed guards -- the only two shapes that name a destination and
        // refuse it. See `ClientError::UnsafeDestination` for why 65.
        CrossHostRefused { .. } | InsecureAuthRealm { .. } => ClientError::UnsafeDestination(Box::new(e)),
        // The fork's redirect policy ending a chain with `attempt.error(..)`:
        // either an `https` -> `http` hop, or the hop count exceeding
        // `MAX_REDIRECTS`. A closure inside reqwest, so a reqwest error rather
        // than a typed variant.
        //
        // The limit branch reaches here only because it errors rather than
        // stopping: `attempt.stop()` hands the 3xx back as an `Ok` response,
        // which `error_for_status_ref` reads as success -- a blob behind an
        // over-long chain then answered "exists" to a HEAD and its layer was
        // never uploaded.
        RequestError(request) if request.is_redirect() => ClientError::UnfollowedRedirect(Box::new(e)),
        AuthenticationFailure(_) | UnauthorizedError { .. } => ClientError::Authentication(Box::new(e)),
        ServerError { code: 401 | 403, .. } => ClientError::Authentication(Box::new(e)),
        // A 3xx that reached ocx as a *status* is a redirect no client acted on,
        // from either of two paths:
        //
        // - the upload path, which issues its registry-supplied session URLs on a
        //   `Policy::none()` client precisely so a mid-session handoff surfaces
        //   instead of replaying the blob body to a foreign host (CWE-918);
        // - a redirect `tower-http` could not follow at all -- a missing or
        //   unparseable `Location`, or a body it cannot clone -- which it hands
        //   back as the 3xx itself on ANY client, whatever the policy.
        //
        // The two are indistinguishable here: both arrive as `ServerError`, and
        // the second is a malformed answer, not a refusal. Both are terminal --
        // a rerun walks the same chain to the same answer.
        ServerError { code: 301..=308, .. } => ClientError::UnfollowedRedirect(Box::new(e)),
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
        // A manifest request answered with HTML is a mis-delivered response, not
        // an unavailable registry: 65 says the bytes were wrong, 69 would tell a
        // retry wrapper the registry might answer differently next time.
        UnexpectedContentType { .. } => ClientError::NotAManifest(Box::new(e)),
        // The wire proved the bytes are not what the digest claimed. Same 65,
        // same reason. Only this one `DigestError` variant moves: the others
        // (an unusable algorithm, a malformed header) are the registry giving a
        // bad answer rather than serving corrupted content, so they stay 69.
        DigestError(WireDigestError::VerificationError { expected, actual }) => ClientError::DigestMismatch {
            expected: expected.clone(),
            actual: actual.clone(),
        },
        // Everything the registry answered that ocx cannot use, enumerated so
        // the next fork variant is a compile error rather than a silent 69.
        ConfigConversionError(_)
        | DigestError(_)
        | GenericError(_)
        | HeaderValueError(_)
        | ImageManifestNotFoundError(_)
        | ImageIndexParsingNoPlatformResolverError
        | IncompatibleLayerMediaTypeError(_)
        | IoError(_)
        | JsonError(_)
        | ManifestEncodingError(_)
        | ManifestParsingError(_)
        | PushNoDataError
        | PushLayerNoDataError
        | PullNoLayersError
        | RegistryNoDigestError
        | RegistryNoLocationError
        | RegistryTokenDecodeError(_)
        | RequestError(_)
        | ServerError { .. }
        | ResponseTooLargeError { .. }
        | SpecViolationError(_)
        | UrlParseError(_)
        | UnsupportedMediaTypeError(_)
        | UnsupportedSchemaVersionError(_)
        | VersionedParsingError(_) => ClientError::Registry(Box::new(e)),
    }
}

/// The `[registries."<name>"]` key that would have licensed plain HTTP for the
/// host this failed request was addressed to, when the request used `https`.
///
/// `None` for any other scheme or a URL with no host: the hint is only correct
/// when TLS is what was attempted.
///
/// # One case where the hint under-quotes
///
/// This is the only one of the four plain-HTTP messages that DERIVES the name
/// instead of quoting the configured string, and the two sides normalise
/// differently. The `url` crate drops a scheme-default port at parse time, so
/// `https://mirror.corp:443/` yields `mirror.corp` -- but `config::mirror`'s
/// `parse_url` keeps the authority verbatim, and `resolve_registry()` passes it
/// through unchanged, so a name configured as `mirror.corp:443` is what the gate
/// actually compares. For that one redundant spelling the hint names a key that
/// would grant nothing. `registry_error` holds only the post-parse `Url` and
/// cannot recover the configured form, so this is stated rather than fixed.
///
/// Twin on the other side of the submodule boundary: the fork's `url_authority`
/// derives the same `host[:port]` and decides whether a plaintext realm is
/// ADMITTED, where this one tells the operator what to write to admit one. A
/// crate boundary rules out sharing the function, so a change to either wants a
/// look at the other.
fn https_allowance_name(url: Option<&reqwest::Url>) -> Option<String> {
    let url = url.filter(|url| url.scheme() == "https")?;
    let host = url.host_str()?;
    Some(match url.port() {
        // `Url::port` is `None` on the scheme's default port, which is exactly
        // when the allowance is written as the bare host.
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Carries the plain-HTTP remediation on a failed HTTPS connect.
///
/// A registry that speaks plain HTTP answers a TLS `ClientHello` with an HTTP
/// response, which rustls reports as "received corrupt message of type
/// InvalidContentType" -- legible only to someone who has met it before, and
/// naming neither of the two ways to allow plaintext. Wrapping rather than
/// adding a `ClientError` variant keeps the transient bucket, and with it
/// `push_blob`'s retry and `via_mirror`'s annotation, exactly as they were.
#[derive(Debug, thiserror::Error)]
#[error(
    "could not connect to '{host}' over https; if it serves plain HTTP, set insecure = true \
     under [registries.\"{host}\"] or add the host to OCX_INSECURE_REGISTRIES"
)]
struct PlainHttpAllowanceHint {
    host: String,
    #[source]
    source: oci_client::errors::OciDistributionError,
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
            // Preserve the referrer's artifactType. The Referrers-API index
            // carries it per descriptor; discarding it here blinds every
            // consumer's client-side artifactType check (the verify pipeline
            // re-filters to Sigstore bundles), which would drop server-matched
            // referrers as if none existed.
            artifact_type: entry.artifact_type,
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
        self.do_push_blob(image, BlobBody::Memory(Bytes::from(data)), digest, on_progress)
            .await
    }

    async fn push_blob_from_path(
        &self,
        image: &oci::native::Reference,
        path: &Path,
        digest: &oci::Digest,
        on_progress: ProgressFn,
    ) -> Result<String> {
        let size = tokio::fs::metadata(path)
            .await
            .map_err(|e| ClientError::Io {
                path: path.to_path_buf(),
                source: e,
            })?
            .len();
        self.do_push_blob(
            image,
            BlobBody::File {
                path: path.to_path_buf(),
                size,
            },
            digest,
            on_progress,
        )
        .await
    }

    async fn mount_blob(
        &self,
        image: &oci::native::Reference,
        source_repository: &str,
        digest: &oci::Digest,
    ) -> Result<MountOutcome> {
        let source = oci::identifier::mount_source_reference(image.registry(), source_repository);
        let digest_str = digest.to_string();
        log::debug!(
            "Attempting to mount blob {} from {} into {}",
            digest_str,
            source_repository,
            image
        );
        // A 202 mount-miss is spec-legal (registry declined and opened a regular
        // upload session instead) — that session is deliberately abandoned here;
        // `push_multi_layer_manifest`'s existing fallback re-uploads through the
        // normal `push_blob` path. A genuine transport error is likewise mapped
        // to `UploadRequired` rather than propagated: mounting is purely an
        // upload-avoidance optimization, so declining is never itself fatal —
        // what the caller's fallback then finds is the caller's business.
        match self.client.mount_blob(image, &source, digest_str.as_str()).await {
            Ok(oci_client::client::BlobMountResponse::Mounted) => Ok(MountOutcome::Mounted),
            Ok(oci_client::client::BlobMountResponse::UploadSessionOpened(_)) => Ok(MountOutcome::UploadRequired),
            Err(e) => {
                log::warn!(
                    "Mount of blob {} from {} into {} declined, falling back to upload: {}",
                    digest_str,
                    source_repository,
                    image,
                    e
                );
                Ok(MountOutcome::UploadRequired)
            }
        }
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
        // Native-only referrers lookup: a 404 on `/v2/<name>/referrers/<digest>`
        // means the registry lacks the OCI 1.1 Referrers API — surfaced as
        // `None` here and mapped to `ReferrersUnsupported` (exit 84), NOT
        // silently swallowed into an empty list (which would misreport as
        // "no signatures found", exit 79). See `pull_referrers_native`.
        match self
            .client
            .pull_referrers_native(&target, artifact_type)
            .await
            .map_err(|e| referrers_unsupported_or_registry_error(e, image))?
        {
            Some(index) => Ok(filter_and_convert_referrers(index.manifests, artifact_type)),
            None => Err(ClientError::ReferrersUnsupported {
                registry: image.resolve_registry().to_string(),
            }),
        }
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
        body_source: BlobBody,
        digest: &oci::Digest,
        on_progress: ProgressFn,
    ) -> Result<String> {
        let digest_str = digest.to_string();
        log::debug!("Checking if blob {} already exists in registry", digest_str);
        match self.client.blob_exists(image, digest_str.as_str()).await {
            Ok(true) => {
                log::debug!("Blob {} already exists, skipping upload", digest_str);
                on_progress(body_source.size());
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

        let total = body_source.size();
        // Checked, not `as`: a narrowing cast would hand the fork a short length
        // on a 32-bit target, its `while remaining > 0` loop would upload a
        // prefix, and the failure would surface as a rejected committing PUT
        // rather than as the size problem it is (PKG-03).
        let total_len = usize::try_from(total).map_err(|_| ClientError::LayerSizeExceeded {
            // A file length never exceeds i64::MAX, so both saturations are
            // unreachable; they exist so the error stays total.
            declared: i64::try_from(total).unwrap_or(i64::MAX),
            maximum: u64::try_from(usize::MAX).unwrap_or(u64::MAX),
        })?;

        let mut attempt: u8 = 0;
        let mut backoff = PUSH_RETRY_INITIAL_BACKOFF;
        loop {
            let body = body_source.stream(Arc::clone(&on_progress)).await?;
            match self
                .client
                .push_blob_stream(image, body, digest_str.as_str(), Some(total_len))
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
                    // Bounded by the cap checked directly above: the fallback ends in one
                    // request, so reading a file-backed body into RAM here can never exceed
                    // what that single request was already going to carry.
                    let data = body_source.read_all().await?;
                    return self
                        .client
                        .push_blob(image, data, digest_str.as_str())
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

/// Where a blob's bytes come from for a streamed push.
///
/// Both arms produce a *fresh* body on every attempt, which is what makes the
/// whole-blob restart in [`NativeTransport::do_push_blob`] replayable: an
/// in-memory body re-reads a refcounted [`Bytes`], a file-backed body re-opens
/// the file. Neither ever holds a second copy of the blob.
enum BlobBody {
    /// Bytes already in RAM — the ordinary publish path, where the caller built
    /// them.
    Memory(Bytes),
    /// Bytes on disk, streamed frame by frame. Used by transfers (a
    /// registry-to-registry copy) that spooled the source blob to a file rather
    /// than buffering a whole layer per concurrent transfer.
    File { path: PathBuf, size: u64 },
}

impl BlobBody {
    fn size(&self) -> u64 {
        match self {
            Self::Memory(data) => data.len() as u64,
            Self::File { size, .. } => *size,
        }
    }

    /// Opens a fresh progress-reporting body stream for one upload attempt.
    async fn stream(&self, on_progress: ProgressFn) -> Result<BlobBodyStream> {
        match self {
            Self::Memory(data) => Ok(progress_body_stream(std::io::Cursor::new(data.clone()), on_progress)),
            Self::File { path, .. } => {
                let file = tokio::fs::File::open(path).await.map_err(|e| ClientError::Io {
                    path: path.clone(),
                    source: e,
                })?;
                Ok(progress_body_stream(file, on_progress))
            }
        }
    }

    /// Materializes the whole blob in RAM, for the buffered fallback only.
    async fn read_all(&self) -> Result<Bytes> {
        match self {
            Self::Memory(data) => Ok(data.clone()),
            Self::File { path, .. } => {
                let data = tokio::fs::read(path).await.map_err(|e| ClientError::Io {
                    path: path.clone(),
                    source: e,
                })?;
                Ok(Bytes::from(data))
            }
        }
    }
}

/// The body stream handed to the fork's `push_blob_stream`.
///
/// Boxed because [`BlobBody::stream`] returns one of two concrete stream types
/// from a single call site.
type BlobBodyStream =
    futures::stream::BoxStream<'static, std::result::Result<Bytes, oci_client::errors::OciDistributionError>>;

/// Frame size for the streamed push body — the granularity at which upload
/// progress advances. Small enough that progress looks smooth, large enough that
/// per-frame overhead stays negligible against the blob size.
const UPLOAD_FRAME_SIZE: usize = 128 * 1024;

/// Wraps a blob's byte source as a progress-reporting stream for a streamed push.
///
/// The source is teed through [`ProgressReader`] (cumulative byte count on every
/// read), then framed into [`UPLOAD_FRAME_SIZE`] chunks by
/// [`ReaderStream`](tokio_util::io::ReaderStream). The fork's `push_blob_stream`
/// pulls from this stream only as the socket accepts more of each streamed PATCH
/// body (backpressure), so `ProgressReader` fires per [`UPLOAD_FRAME_SIZE`] frame
/// as it is pulled for the wire — progress leads the actual socket hand-off by at
/// most one frame. This mirrors the pull path (`Client::pull_layer`), which wraps
/// the fork's streaming reader in the same [`ProgressReader`].
fn progress_body_stream<R>(source: R, on_progress: ProgressFn) -> BlobBodyStream
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    let reader = ProgressReader::new(source, on_progress);
    tokio_util::io::ReaderStream::with_capacity(reader, UPLOAD_FRAME_SIZE)
        .map(|frame| {
            // A read error is real for a file-backed body and impossible for a
            // `Cursor`; either way this reconciles it with the fork's stream item.
            frame.map_err(|error| oci_client::errors::OciDistributionError::GenericError(Some(error.to_string())))
        })
        .boxed()
}

#[cfg(test)]
mod tests {
    use super::super::transport::no_progress;
    use super::*;
    use futures::StreamExt;
    use futures::stream;
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

    /// Bind a loopback listener that answers exactly one connection with `raw`,
    /// returning the port and the task handle so the caller can abort it.
    async fn one_shot_http(raw: &'static [u8]) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt as _;
                let _ = socket.write_all(raw).await;
                let _ = socket.flush().await;
            }
        });
        (port, server)
    }

    /// The two typed guards are terminal refusals, not availability faults.
    /// Before this they fell through the `_` arm to `Registry` (exit 69), which
    /// tells a CI retry wrapper to re-issue the push against the same hostile
    /// `Location` — the one thing the guard exists to prevent. Asserting they
    /// are NOT `Registry` is what pins the arm; `UnsafeDestination` alone would
    /// also hold if the whole taxonomy collapsed.
    #[test]
    fn the_forks_local_refusals_are_unsafe_destinations_not_registry_faults() {
        let cross_host = OciDistributionError::CrossHostRefused {
            url: "https://evil.example/v2/x/blobs/uploads/1".to_string(),
            registry: "registry.test".to_string(),
        };
        let plaintext_realm = OciDistributionError::InsecureAuthRealm {
            realm: "http://collector.example/token".to_string(),
        };

        for wire in [cross_host, plaintext_realm] {
            let label = format!("{wire:?}");
            let mapped = registry_error(wire);
            assert!(
                matches!(mapped, ClientError::UnsafeDestination(_)),
                "{label} must map to UnsafeDestination, got {mapped:?}"
            );
        }
    }

    /// A declined hop reaches ocx as a reqwest *error*, not a typed fork
    /// variant, because the policy is a closure inside reqwest. Driven through
    /// a real client so the `is_redirect()` predicate is exercised against the
    /// error reqwest actually builds, not an assumption about it.
    ///
    /// Loopback only — one accept, one hand-written `302`, no TLS needed: the
    /// policy under test here is ocx's classification of the refusal, and the
    /// fork owns the downgrade decision itself.
    #[tokio::test]
    async fn a_declined_redirect_hop_is_an_unfollowed_redirect_not_a_registry_fault() {
        let (port, server) =
            one_shot_http(b"HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\nContent-Length: 0\r\n\r\n").await;

        let wire = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                attempt.error(std::io::Error::other("refusing this hop"))
            }))
            .build()
            .expect("client")
            .get(format!("http://127.0.0.1:{port}/v2/"))
            .send()
            .await
            .expect_err("a refused redirect must fail the request");
        server.abort();

        assert!(wire.is_redirect(), "precondition: reqwest must report a redirect error");

        let mapped = registry_error(wire.into());
        assert!(
            matches!(mapped, ClientError::UnfollowedRedirect(_)),
            "a declined hop must not read as an unavailable registry, got {mapped:?}"
        );
    }

    /// A `Location`-less 3xx is handed back as the 3xx itself, by a client whose
    /// policy would gladly have followed it — the shape that made the old
    /// `UnsafeDestination` name a lie, since nothing was named and nothing was
    /// refused. The precondition assertion is the load-bearing half: it proves
    /// the arm is reachable on the ordinary follow-redirects client, so the
    /// classification below is not a statement about the upload path alone.
    #[tokio::test]
    async fn a_location_less_redirect_is_an_unfollowed_redirect_on_a_following_client() {
        let (port, server) = one_shot_http(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n").await;

        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .expect("client")
            .get(format!("http://127.0.0.1:{port}/v2/"))
            .send()
            .await
            .expect("a Location-less 3xx is handed back as a successful response, not an error");
        server.abort();

        assert_eq!(
            response.status().as_u16(),
            302,
            "precondition: a following client cannot act on a Location-less redirect, so it \
             surfaces the 3xx as the response"
        );

        // The fork's status handler turns any unhandled status into `ServerError`,
        // carrying nothing that distinguishes this from the upload path's refusal.
        let mapped = registry_error(server_error(response.status().as_u16()));
        assert!(
            matches!(mapped, ClientError::UnfollowedRedirect(_)),
            "a malformed redirect must not read as a refused unsafe destination, got {mapped:?}"
        );
    }

    /// The `host[:port]` the plain-HTTP allowance would be written as, derived
    /// from the URL the failed request carried. `Url::port` is `None` on the
    /// scheme default, which is exactly when the allowance is the bare host.
    #[test]
    fn the_allowance_name_is_derived_only_from_an_https_url() {
        let name = |raw: &str| {
            let url = reqwest::Url::parse(raw).expect("valid url");
            https_allowance_name(Some(&url))
        };

        assert_eq!(
            name("https://registry.corp:5000/v2/"),
            Some("registry.corp:5000".into())
        );
        assert_eq!(name("https://registry.corp/v2/"), Some("registry.corp".into()));
        assert_eq!(
            name("https://registry.corp:443/v2/"),
            Some("registry.corp".into()),
            "the default port is not part of the name the allowance is written as"
        );
        assert_eq!(
            name("http://registry.corp:5000/v2/"),
            None,
            "the hint is only correct when TLS is what was attempted"
        );
        assert_eq!(https_allowance_name(None), None);
    }

    /// End to end on a real `reqwest` connect failure: the refusal an operator
    /// actually sees must name the config key and the env var. Loopback only —
    /// the port is one the OS just told us is free, so the connect is refused
    /// locally with no network and no DNS.
    #[tokio::test]
    async fn a_failed_https_connect_names_both_ways_to_allow_plain_http() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind a free port")
            .local_addr()
            .expect("local addr")
            .port();

        let wire = reqwest::Client::builder()
            .build()
            .expect("client")
            .get(format!("https://127.0.0.1:{port}/v2/"))
            .send()
            .await
            .expect_err("a connect to a closed port must fail");
        assert!(wire.is_connect(), "precondition: this must be a connect failure");

        let mapped = registry_error(wire.into());
        let rendered = mapped.to_string();

        assert!(
            matches!(mapped, ClientError::RegistryTransient(_)),
            "the bucket must not move — a refused connect is still retryable, got {mapped:?}"
        );
        assert!(
            rendered.contains(&format!("[registries.\"127.0.0.1:{port}\"]")),
            "the refusal must quote the exact config key that would grant it: {rendered}"
        );
        assert!(
            rendered.contains("insecure = true"),
            "the refusal must name the config spelling: {rendered}"
        );
        assert!(
            rendered.contains("OCX_INSECURE_REGISTRIES"),
            "the refusal must name the env spelling too: {rendered}"
        );
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

        let frames: Vec<Bytes> = progress_body_stream(std::io::Cursor::new(Bytes::from(blob)), on_progress)
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

    /// The response-shape split, pinned end to end: a manifest request answered
    /// with a login portal exits 65 (the bytes were wrong, rerunning changes
    /// nothing), not 69 (the registry is unavailable). Before the fork read the
    /// content type this arrived as a digest mismatch; before this arm it fell
    /// to the catch-all and exited 69.
    #[test]
    fn an_html_manifest_response_classifies_as_a_data_error() {
        use crate::cli::{ClassifyExitCode as _, ExitCode};

        let mapped = registry_error(OciDistributionError::UnexpectedContentType {
            content_type: "text/html; charset=utf-8".to_string(),
            url: "https://portal.example.com/login".to_string(),
        });

        assert!(matches!(mapped, ClientError::NotAManifest(_)), "got {mapped:?}");
        assert_eq!(mapped.classify(), Some(ExitCode::DataError));
        let rendered = format!("{:#}", anyhow::Error::from(mapped));
        assert!(
            rendered.contains("text/html") && rendered.contains("portal.example.com"),
            "the chain must name what arrived and where from, got: {rendered}"
        );
    }

    /// The wire told us the bytes are not what they claimed to be. That is a
    /// data fault (65) -- rerunning fetches the same wrong bytes -- where the
    /// catch-all's 69 tells a retry wrapper the opposite.
    #[test]
    fn a_wire_digest_mismatch_maps_to_digest_mismatch() {
        use crate::cli::{ClassifyExitCode as _, ExitCode};
        use oci_client::errors::DigestError as WireDigestError;

        let mapped = registry_error(OciDistributionError::DigestError(WireDigestError::VerificationError {
            expected: format!("sha256:{}", "a".repeat(64)),
            actual: format!("sha256:{}", "b".repeat(64)),
        }));

        let ClientError::DigestMismatch { expected, actual } = &mapped else {
            panic!("expected DigestMismatch, got {mapped:?}");
        };
        assert_eq!(expected, &format!("sha256:{}", "a".repeat(64)));
        assert_eq!(actual, &format!("sha256:{}", "b".repeat(64)));
        assert_eq!(mapped.classify(), Some(ExitCode::DataError));
    }

    /// The other half of the split: a digest header naming an algorithm we
    /// cannot compute is a bad answer, not corrupted content, so it stays a
    /// registry failure. Pinning both directions is what keeps the arm from
    /// widening to every `DigestError` on the next edit.
    #[test]
    fn an_unusable_digest_header_stays_a_registry_failure() {
        use crate::cli::{ClassifyExitCode as _, ExitCode};
        use oci_client::errors::DigestError as WireDigestError;

        let mapped = registry_error(OciDistributionError::DigestError(
            WireDigestError::UnsupportedAlgorithm("md5".to_string()),
        ));

        assert!(matches!(mapped, ClientError::Registry(_)), "got {mapped:?}");
        assert_eq!(mapped.classify(), Some(ExitCode::Unavailable));
    }

    /// A file-backed body must be byte-identical to the in-memory one, and must
    /// stay replayable: `do_push_blob`'s transient-failure path restarts the whole
    /// blob, so `stream()` is called again on the same `BlobBody` and the second
    /// body has to carry the same bytes as the first. A source consumed by its
    /// first read would upload a truncated blob on the retry and only fail later,
    /// at the registry's digest check.
    #[tokio::test]
    async fn file_backed_body_streams_the_same_bytes_as_memory_and_replays() {
        // Larger than one frame, so the equality is over a real multi-frame stream
        // rather than a single chunk that any source shape would get right.
        let blob: Vec<u8> = (0..(UPLOAD_FRAME_SIZE * 2 + 7)).map(|i| (i % 251) as u8).collect();

        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("layer.bin");
        std::fs::write(&path, &blob).expect("write blob");

        let memory = BlobBody::Memory(Bytes::from(blob.clone()));
        let file = BlobBody::File {
            path: path.clone(),
            size: blob.len() as u64,
        };
        assert_eq!(memory.size(), file.size());

        async fn drain(body: &BlobBody) -> Vec<u8> {
            let frames: Vec<Bytes> = body
                .stream(no_progress())
                .await
                .expect("open body")
                .map(|frame| frame.expect("frames never error here"))
                .collect()
                .await;
            reassemble(&frames)
        }

        assert_eq!(drain(&memory).await, blob, "in-memory body");
        assert_eq!(drain(&file).await, blob, "first file-backed attempt");
        assert_eq!(drain(&file).await, blob, "restarted file-backed attempt");
    }

    /// The buffered fallback reads the whole blob back, so a file-backed body has
    /// to materialize the same bytes the streamed path would have sent.
    #[tokio::test]
    async fn file_backed_body_reads_back_the_whole_blob() {
        let blob: Vec<u8> = (0..5000).map(|i| (i % 97) as u8).collect();
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("layer.bin");
        std::fs::write(&path, &blob).expect("write blob");

        let body = BlobBody::File {
            path,
            size: blob.len() as u64,
        };
        assert_eq!(body.read_all().await.expect("read back"), Bytes::from(blob));
    }
}
