// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use tokio::io::{AsyncRead, ReadBuf};

use super::error::ClientError;
use crate::oci;
use crate::oci::verify::DiscoveryMethod;
use crate::package::tag::referrer_fallback_tag;

pub type Result<T> = std::result::Result<T, ClientError>;

/// Progress callback for transfer operations.
pub type ProgressFn = Arc<dyn Fn(u64) + Send + Sync>;

/// Returns a no-op progress callback for callers that don't need progress.
pub fn no_progress() -> ProgressFn {
    Arc::new(|_| {})
}

/// Byte ceiling on a fallback referrers index body.
///
/// The fork applies the same 4 MiB bound to a native referrers response
/// (`MAX_REFERRERS_INDEX_BYTES`, `external/rust-oci-client/src/client.rs`), but
/// both fork constants are private and `pull_manifest_raw` — the only route to a
/// tag-addressed document — carries no bound of its own. So this is declared
/// here and checked **after** the read: it bounds what is parsed and
/// re-published, not what is allocated. Pre-read bounding needs a `limit`
/// threaded into the fork's `_pull_manifest_raw`, which every tag-addressed read
/// in this crate would want too.
const MAX_FALLBACK_INDEX_BYTES: usize = 4 * 1024 * 1024;

/// Descriptor-count ceiling on a fallback referrers index, mirroring the fork's
/// private `MAX_REFERRERS_DESCRIPTORS`. The byte cap alone does not bound the
/// work a caller does per entry: a compact descriptor is ~200 bytes, so 4 MiB of
/// them is tens of thousands of signatures to fetch and verify for one subject.
const MAX_FALLBACK_DESCRIPTORS: usize = 4096;

/// Read-modify-write attempts before an append gives up.
///
/// Optimistic, with **no fairness guarantee**. Surviving your own PUT and
/// surviving until your own read-back are different events, so a writer can be
/// interposed on every attempt: with three writers, W3's PUT can land between
/// W1's read-back and W1's re-read, leaving W1 to overwrite W3 from a stale
/// base — nobody converges that round. Two writers is the case that *is*
/// provable (the one whose PUT landed last reads its own descriptor back and
/// stops), and it is the one `two_writers_racing_one_fallback_index_both_land`
/// pins. Beyond it this converges with high probability at realistic fan-out
/// and otherwise fails loudly and retryably — never with a silent drop. There
/// is no backoff: the loser re-reads immediately.
const MAX_FALLBACK_ATTEMPTS: usize = 5;

/// A referrer listing plus how it was found.
///
/// `via` is what a caller reports as `signatures[].discovery_method`, and it is
/// also the difference between a registry-computed answer and a mutable tag
/// anyone with push access authored — worth surfacing rather than flattening.
#[derive(Debug, Clone)]
pub struct ReferrersListing {
    /// The referrer descriptors, already filtered by artifact type.
    pub descriptors: Vec<oci::Descriptor>,
    /// Which mechanism answered.
    pub via: DiscoveryMethod,
}

/// Outcome of appending a descriptor to a fallback referrers index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackAppend {
    /// The index was rewritten with the descriptor appended.
    Written,
    /// An entry with that digest was already there; nothing was pushed.
    AlreadyPresent,
}

/// An empty OCI image index, the starting point for a fallback tag that 404s.
fn empty_fallback_index() -> oci::ImageIndex {
    oci::ImageIndex {
        schema_version: oci::INDEX_SCHEMA_VERSION,
        media_type: Some(crate::media_type::MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
        manifests: Vec::new(),
        artifact_type: None,
        annotations: None,
    }
}

/// Parses and validates fallback-index bytes read from `tag`.
fn decode_fallback_index(bytes: &[u8], tag: &str) -> Result<oci::ImageIndex> {
    if bytes.len() > MAX_FALLBACK_INDEX_BYTES {
        return Err(ClientError::InvalidManifest(format!(
            "referrers fallback index {tag} is {} bytes, above the {MAX_FALLBACK_INDEX_BYTES} limit",
            bytes.len()
        )));
    }
    // Parsed as the index shape directly rather than as `oci::Manifest`: the
    // untagged manifest enum accepts an image manifest here, and spec step 2
    // says a non-index at this tag is a failure to report, not a shape to
    // interpret.
    let index: oci::ImageIndex = serde_json::from_slice(bytes).map_err(|_| ClientError::UnexpectedManifestType)?;
    oci::manifest::validate_image_index(&index)?;
    if index.manifests.len() > MAX_FALLBACK_DESCRIPTORS {
        // `InvalidManifest` rather than `InvalidImageIndex`: the latter's inner
        // type has a private field and no constructor, and both classify to
        // `ExitCode::DataError` anyway.
        return Err(ClientError::InvalidManifest(format!(
            "referrers fallback index {tag} lists {} descriptors, above the {MAX_FALLBACK_DESCRIPTORS} limit",
            index.manifests.len()
        )));
    }
    Ok(index)
}

/// Whether `index` already lists an entry with `digest`.
fn index_carries(index: &oci::ImageIndex, digest: &str) -> bool {
    index.manifests.iter().any(|entry| entry.digest == digest)
}

/// Builds the index to push: a fresh header plus every surviving entry
/// re-emitted field by field, with `descriptor` appended.
///
/// Nothing is echoed. The bytes at the tag were authored by whoever can push to
/// the repository, and this is the document the caller signs its own credentials
/// against — so the header is reconstructed (no inherited index-level
/// `annotations`, no `artifactType`), and each entry is re-emitted field by
/// field rather than moved across whole. No field is dropped today; the
/// property is that adding one to `oci::ImageIndexEntry` is a compile error
/// here, which is where the decision to carry it belongs.
fn rebuild_with(index: oci::ImageIndex, descriptor: &oci::Descriptor) -> oci::ImageIndex {
    let mut manifests: Vec<oci::ImageIndexEntry> = index
        .manifests
        .into_iter()
        .map(|entry| oci::ImageIndexEntry {
            media_type: entry.media_type,
            digest: entry.digest,
            size: entry.size,
            platform: entry.platform,
            annotations: entry.annotations,
            artifact_type: entry.artifact_type,
        })
        .collect();
    // Spec step 5: `artifactType` MUST be set to the pushed manifest's, and all
    // its annotations MUST be copied. cosign's own fallback write loses both
    // (sigstore/cosign#4641); getting them right is the point of this method.
    manifests.push(oci::ImageIndexEntry {
        media_type: descriptor.media_type.clone(),
        digest: descriptor.digest.clone(),
        size: descriptor.size,
        platform: None,
        annotations: descriptor.annotations.clone(),
        artifact_type: descriptor.artifact_type.clone(),
    });
    oci::ImageIndex {
        manifests,
        ..empty_fallback_index()
    }
}

/// Converts fallback-index entries to descriptors, applying the artifact-type
/// filter the tag schema has no server-side equivalent for.
///
/// `urls` is pinned to `None`: the field is a registry-dereferenced redirect, and
/// nothing in this crate reads one. `ImageIndexEntry` does not model it, so
/// today it cannot survive the round trip anyway — stated here so it stays a
/// decision rather than an accident of an upstream struct.
fn fallback_descriptors(index: oci::ImageIndex, artifact_type: Option<&str>) -> Vec<oci::Descriptor> {
    index
        .manifests
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
            artifact_type: entry.artifact_type,
            annotations: entry.annotations,
        })
        .collect()
}

/// Extracts the `artifactType` and annotations a referrer descriptor must carry,
/// from the manifest bytes that were pushed.
///
/// OCI tag-schema write step 5, verbatim: *"The value of the `artifactType` MUST
/// be set to the `artifactType` value in the pushed manifest, if present. If the
/// `artifactType` is empty or missing in a pushed image manifest, the value of
/// `artifactType` MUST be set to the config descriptor `mediaType` value. All
/// annotations from the pushed manifest MUST be copied to this descriptor."*
///
/// Bytes that do not parse as an image manifest yield `(None, None)` rather than
/// an error: this describes a push that already succeeded, so refusing here
/// would fail an operation that landed. The descriptor is then merely less
/// informative, which is the pre-existing behaviour.
pub(super) fn referrer_descriptor_facets(
    manifest_bytes: &[u8],
) -> (Option<String>, Option<std::collections::BTreeMap<String, String>>) {
    let manifest = match serde_json::from_slice::<oci::ImageManifest>(manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            // Silence here is the failure this whole path exists to prevent,
            // arriving quietly: the descriptor loses exactly the facets spec
            // step 5 requires, and the fallback index records the loss.
            crate::log::warn!(
                "referrer manifest did not parse, descriptor loses its artifactType and annotations: {error}"
            );
            return (None, None);
        }
    };
    let artifact_type = manifest
        .artifact_type
        .filter(|value| !value.is_empty())
        .or_else(|| Some(manifest.config.media_type.clone()).filter(|value| !value.is_empty()));
    (artifact_type, manifest.annotations)
}

/// Decides whether a failed fallback-index PUT means the registry refused to
/// hold the index.
///
/// Exit 84 is "the Referrers API is absent **and** the fallback tag write was
/// refused" — a capability verdict. A registry that answered and declined the
/// document earns it; a credential problem or a transient fault does not, and
/// keeps its own code so `case $?` still tells an operator which one happened.
///
/// `ClientError::Registry` is **not** enough on its own to earn it.
/// `native_transport::registry_error` re-routes 401/403, 429 and 502/503/504,
/// but its catch-all arm folds every remaining `ServerError` into
/// `ClientError::Registry` — a plain 500 included. Reporting that as 84 would
/// tell an operator the endpoint is not served and a rerun can never change
/// that, about a fault where a rerun is exactly the right move. So the status
/// is read back out: only 400, 405 or 422, or a structured OCI error envelope
/// (the registry understood the document and named its objection), is a
/// decline.
fn fallback_write_refused(error: ClientError, image: &oci::native::Reference) -> ClientError {
    let declined = match &error {
        ClientError::Registry(source) => registry_declined(source.as_ref()),
        // A manifest PUT answered with something that is not a manifest response
        // is an endpoint that does not serve this. `InvalidManifest` is
        // deliberately absent: it says ocx built a bad document, and blaming the
        // registry for that is the same mistake in the other direction.
        ClientError::UnexpectedManifestType | ClientError::NotAManifest(_) => true,
        _ => false,
    };
    if declined {
        // The declining error is dropped here — `ReferrersUnsupported` carries
        // only a registry — so the status the registry actually answered has
        // nowhere else to go. Without this line an operator cannot tell a 400
        // from a 422 from a non-manifest response.
        crate::log::debug!("registry declined the referrers fallback index, reported as unsupported: {error}");
        return ClientError::ReferrersUnsupported {
            registry: image.resolve_registry().to_string(),
        };
    }
    error
}

/// The fallback index cannot hold this referrer, and no rerun changes that.
///
/// Exit 84, the same verdict a refused PUT earns. Not 75 — nothing here is
/// transient. Not 65 either: 65 would say the *caller's* document is malformed,
/// when what is full is the registry-side index. The honest reading of 84 for
/// this case is "this registry cannot serve referrers for this subject", and
/// the remedy 84 already tells an operator — use a registry with the Referrers
/// API — is the correct one, since the API has no such ceiling.
///
/// The variant carries only a registry, so which limit was hit is logged at the
/// call site rather than squeezed into the message.
fn index_cannot_hold_it(image: &oci::native::Reference) -> ClientError {
    ClientError::ReferrersUnsupported {
        registry: image.resolve_registry().to_string(),
    }
}

/// Whether a boxed transport error is the registry *declining* the document
/// rather than failing to handle it.
///
/// Anything that is not a recognisable `oci_client` answer is not a decline:
/// the conservative direction here is to leave the error its own exit code,
/// because 84 is the one that tells a script to stop retrying.
fn registry_declined(source: &(dyn std::error::Error + 'static)) -> bool {
    use oci_client::errors::OciDistributionError;
    match source.downcast_ref::<OciDistributionError>() {
        // The statuses that mean *answered and declined* a manifest PUT. Not
        // every 4xx: 401/403 never arrive here (`registry_error` routes them to
        // `Authentication`), and a 404 on a PUT is a repository problem, not a
        // verdict on the document.
        Some(OciDistributionError::ServerError { code, .. }) => matches!(code, 400 | 405 | 422),
        // An error envelope is the registry naming its objection in OCI's own
        // vocabulary. The two envelope codes that are not objections —
        // unauthorized/denied and too-many-requests — never reach here:
        // `registry_error` maps them to `Authentication` and
        // `RegistryTransient` before this sees them.
        //
        // **No PUT reaches this arm today.** The fork builds `RegistryError`
        // only in `validate_registry_response`, and the manifest-PUT route
        // (`push_manifest_raw` → `extract_location_header`) raises `ServerError`
        // unconditionally instead. So the effective decline set is `400 | 405 |
        // 422` alone; this arm is here for a future push path that does parse
        // the envelope, and nothing tests it because nothing can produce it.
        Some(OciDistributionError::RegistryError { .. }) => true,
        _ => false,
    }
}

/// Outcome of a cross-repository blob mount attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountOutcome {
    /// The registry mounted the blob into the target repository; no upload
    /// is needed.
    Mounted,
    /// The registry declined the mount (spec-legal 202 miss, transport error,
    /// or a transport that doesn't implement mounting); the caller must
    /// upload the blob through the normal path.
    UploadRequired,
}

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

    /// Pulls a blob into memory, returning the raw bytes.
    ///
    /// Suitable for small blobs (config, metadata) where writing to disk
    /// and reading back would be wasteful.
    async fn pull_blob(&self, image: &oci::native::Reference, digest: &oci::Digest) -> Result<Vec<u8>>;

    /// Pulls a blob and writes it to the specified file path.
    async fn pull_blob_to_file(&self, image: &oci::native::Reference, digest: &oci::Digest, path: &Path) -> Result<()>;

    /// HEAD a blob to verify existence and retrieve its content length.
    ///
    /// Returns `Ok(size)` if the blob exists, `Err(ClientError::BlobNotFound)` if not.
    async fn head_blob(&self, image: &oci::native::Reference, digest: &oci::Digest) -> Result<u64>;

    /// Streams the RAW (compressed) blob bytes from the registry.
    ///
    /// Returns an [`AsyncRead`] over the compressed bytes exactly as served by
    /// the registry. No decompression, hashing, or progress reporting is
    /// performed here — those concerns are assembled by the caller
    /// (`Client::pull_layer`). This keeps the transport boundary wire-level
    /// (SRP: decompression depends on `archive/` and `utility/` which must not
    /// leak into the transport).
    ///
    /// # Default implementation
    ///
    /// The default implementation downloads the blob to a temporary file via
    /// [`Self::pull_blob_to_file`] and then streams the file back, so an
    /// implementor gets a working stream from `pull_blob_to_file` alone. It has
    /// no `VerifyingStream` — `HashingAsyncReader` in `pull_layer` is the sole
    /// verifier there. Overriding it is what gives an implementor control over
    /// read boundaries (`NativeTransport` streams from the registry;
    /// [`StubTransport`](super::test_transport::StubTransport) replays a
    /// per-digest chunk plan).
    ///
    /// # Errors (from the returned reader)
    ///
    /// - [`ClientError::BlobNotFound`] — blob absent at call time.
    /// - `io::Error` with fork `DigestError` source at stream end when
    ///   `NativeTransport` is used (caller maps to
    ///   [`ClientError::DigestMismatch`]).
    async fn pull_blob_streaming(
        &self,
        image: &oci::native::Reference,
        digest: &oci::Digest,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin + 'static>> {
        // Default: download to a temp file, then open and stream it back.
        // Real implementations (NativeTransport) override this to stream
        // directly from the registry without touching disk.
        let temp_file = tempfile::NamedTempFile::new().map_err(|e| ClientError::Io {
            path: std::path::PathBuf::from("<tempfile>"),
            source: e,
        })?;
        let temp_path = temp_file.path().to_path_buf();
        self.pull_blob_to_file(image, digest, &temp_path).await?;
        let file = tokio::fs::File::open(&temp_path).await.map_err(|e| ClientError::Io {
            path: temp_path.clone(),
            source: e,
        })?;
        // Keep the NamedTempFile alive by leaking it into the reader via a
        // combination of tokio::io::BufReader and a guard struct so the file
        // is not deleted before the reader is done.
        //
        // Simple approach: convert the temp file into a regular file handle
        // that outlives the path reference, then use a wrapper that holds
        // both the `File` and the `NamedTempFile` for cleanup.
        let reader = TempFileReader {
            file,
            _guard: temp_file,
        };
        Ok(Box::new(reader))
    }

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
    ///
    /// The implementation streams the blob to the registry, invoking
    /// `on_progress` with the cumulative byte count as data reaches the wire.
    /// Pass [`no_progress()`] when progress reporting is not needed.
    async fn push_blob(
        &self,
        image: &oci::native::Reference,
        data: Vec<u8>,
        digest: &oci::Digest,
        on_progress: ProgressFn,
    ) -> Result<String>;

    /// Pushes a blob whose bytes live in a file, without holding them in RAM.
    ///
    /// Exists for transfers where the blob never needed to be in memory in the
    /// first place — a registry-to-registry copy spools the source blob to disk
    /// and hands the path here, so a 200 MB layer costs a file handle rather
    /// than a 200 MB allocation per concurrent layer.
    ///
    /// # No default
    ///
    /// Required, deliberately. The obvious default — read the file, delegate to
    /// [`Self::push_blob`] — allocates the whole blob, which is the one thing
    /// this method exists to avoid, so a future transport that never noticed the
    /// method would compile and quietly reintroduce the allocation. A test
    /// double that genuinely does not care writes one line:
    /// [`push_blob_buffered`], where buffering is stated rather than inherited.
    ///
    /// Contrast [`Self::mount_blob`], whose default *is* correct for a transport
    /// with no mounting — that is what earns a default.
    ///
    /// [`NativeTransport`]: super::native_transport::NativeTransport
    async fn push_blob_from_path(
        &self,
        image: &oci::native::Reference,
        path: &Path,
        digest: &oci::Digest,
        on_progress: ProgressFn,
    ) -> Result<String>;

    /// Attempts to mount `digest` from `source_repository` into `image`'s
    /// repository, avoiding a redundant upload when the blob is already
    /// present elsewhere in the registry.
    ///
    /// # Default implementation
    ///
    /// Always returns [`MountOutcome::UploadRequired`]. Mounting is a
    /// registry-side optimization, not a correctness requirement — a
    /// transport that doesn't implement it (or a test double) falls back
    /// to the normal upload path unchanged.
    async fn mount_blob(
        &self,
        image: &oci::native::Reference,
        source_repository: &str,
        digest: &oci::Digest,
    ) -> Result<MountOutcome> {
        let _ = (image, source_repository, digest);
        Ok(MountOutcome::UploadRequired)
    }

    // ── Referrer operations (OCI 1.1) ────────────────────────────────

    /// Pushes a referrer manifest with a `subject` descriptor pointing at
    /// `subject_digest`.
    ///
    /// The returned descriptor identifies the pushed referrer manifest
    /// (digest + size + media type), suitable for embedding in subsequent
    /// Referrers-API responses.
    async fn push_referrer_manifest(
        &self,
        image: &oci::native::Reference,
        subject_digest: &oci::Digest,
        manifest_bytes: &[u8],
        media_type: &str,
    ) -> Result<oci::Descriptor>;

    /// Lists referrers of the given subject digest via the
    /// `/v2/<name>/referrers/<digest>` endpoint, optionally filtered to a
    /// single artifact type.
    ///
    /// When `artifact_type` is `Some`, implementations SHOULD apply it as a
    /// server-side query filter AND MUST also filter the returned
    /// descriptors client-side — the OCI spec permits a server to ignore the
    /// filter, so callers can only rely on the client-side pass.
    ///
    /// Returns `ClientError::ReferrersUnsupported` when the registry returns
    /// 404 on the referrers endpoint (distinguished from a subject with zero
    /// referrers, which returns an empty list).
    async fn list_referrers(
        &self,
        image: &oci::native::Reference,
        subject_digest: &oci::Digest,
        artifact_type: Option<&str>,
    ) -> Result<Vec<oci::Descriptor>>;

    /// Lists referrers, falling back to the OCI referrers tag schema when the
    /// registry has no Referrers API.
    ///
    /// The fallback-capable sibling of [`Self::list_referrers`], which stays
    /// native-only. The split is not redundancy: the capability probe
    /// (`ReferrersApiCapability::probe`) asks "is the API there?" and reads
    /// [`ClientError::ReferrersUnsupported`] as its answer, so a reader that
    /// swallows the 404 cannot serve it. Callers that want a *verdict* use
    /// `list_referrers`; callers that want the *referrers* use this.
    ///
    /// **This method never returns [`ClientError::ReferrersUnsupported`].** No
    /// Referrers API and no fallback tag is an empty listing tagged
    /// [`DiscoveryMethod::FallbackTag`] — "no signatures found", not "cannot
    /// look". Any other transport error propagates untouched: a 401 or a 500 on
    /// the native endpoint is not a capability verdict, and must not silently
    /// substitute a tag anyone with push access can author.
    ///
    /// # Default implementation
    ///
    /// Delegates to [`Self::list_referrers`] and, on the unsupported verdict, to
    /// [`Self::pull_referrer_fallback_index`] — both already correct for every
    /// implementor, so nothing overrides this.
    async fn list_referrers_with_fallback(
        &self,
        image: &oci::native::Reference,
        subject_digest: &oci::Digest,
        artifact_type: Option<&str>,
    ) -> Result<ReferrersListing> {
        match self.list_referrers(image, subject_digest, artifact_type).await {
            Ok(descriptors) => Ok(ReferrersListing {
                descriptors,
                via: DiscoveryMethod::ReferrersApi,
            }),
            Err(ClientError::ReferrersUnsupported { .. }) => {
                let index = self.pull_referrer_fallback_index(image, subject_digest).await?;
                Ok(ReferrersListing {
                    descriptors: fallback_descriptors(index, artifact_type),
                    via: DiscoveryMethod::FallbackTag,
                })
            }
            Err(other) => Err(other),
        }
    }

    /// Reads the OCI referrers fallback index parked at
    /// `<algorithm>-<encoded truncated to 64>` for `subject_digest`.
    ///
    /// Spec step 1 of the tag-schema write procedure, and the read half of the
    /// fallback on its own. An absent tag is an **empty index**, per spec step 3
    /// ("if the tag returns a 404, the client MUST begin with an empty image
    /// index") — that is the only path here that yields one. Every other refusal
    /// is an error, because the caller that appends must be able to tell "there
    /// is nothing there" from "I could not read what is there": treating the
    /// second as the first would republish an empty index over every sibling
    /// referrer this client did not author.
    ///
    /// # Errors
    ///
    /// - [`ClientError::InvalidManifest`] — the body exceeds
    ///   [`MAX_FALLBACK_INDEX_BYTES`].
    /// - [`ClientError::UnexpectedManifestType`] — the tag holds something that
    ///   is not an image index (spec step 2, "SHOULD report a failure").
    /// - [`ClientError::InvalidImageIndex`] — it fails
    ///   [`oci::manifest::validate_image_index`].
    /// - [`ClientError::InvalidManifest`] — it lists more than
    ///   [`MAX_FALLBACK_DESCRIPTORS`] descriptors.
    ///
    /// # Default implementation
    ///
    /// Built on [`Self::pull_manifest_raw`], so every implementor — including a
    /// test double — gets a real one over whatever manifest store it has.
    async fn pull_referrer_fallback_index(
        &self,
        image: &oci::native::Reference,
        subject_digest: &oci::Digest,
    ) -> Result<oci::ImageIndex> {
        let tag = referrer_fallback_tag(subject_digest);
        let target = super::sibling_tag_reference(image, tag.clone());
        let bytes = match self
            .pull_manifest_raw(&target, crate::media_type::ACCEPTED_MANIFEST_MEDIA_TYPES)
            .await
        {
            Ok((bytes, _digest)) => bytes,
            Err(ClientError::ManifestNotFound(_)) => return Ok(empty_fallback_index()),
            Err(other) => return Err(other),
        };
        decode_fallback_index(&bytes, &tag)
    }

    /// Appends `descriptor` to the fallback referrers index for `subject_digest`,
    /// preserving its `artifactType` and annotations.
    ///
    /// Spec steps 4–6 of the tag-schema write procedure. There is **no
    /// conditional manifest PUT anywhere in the OCI distribution spec**, so this
    /// is optimistic rather than atomic: read, append, write, read back, retry.
    /// Two writers converge provably: the one whose PUT landed last reads its
    /// own descriptor back and stops. Three do not — W3's PUT can land between
    /// W1's failed read-back and W1's re-read, leaving W1 to overwrite W3 from a
    /// stale base — so there is no bound on one writer's failures below its own
    /// attempt budget. See [`MAX_FALLBACK_ATTEMPTS`]. Exhaustion is loud and
    /// retryable, never a silent drop, which is the property that makes the
    /// missing guarantee tolerable.
    ///
    /// The read-back checks for **this call's own descriptor**, not for a
    /// successful PUT. A PUT that a concurrent writer immediately clobbered
    /// returns `Ok` and loses the descriptor; only re-reading catches it. That
    /// distinction is the whole mechanism.
    ///
    /// The written document is **constructed, never echoed**: the index at the
    /// tag is authored by anyone with push access to the repository, and this
    /// call re-publishes it under the caller's own credentials. Its header is
    /// rebuilt and its entries are re-serialised from values that passed
    /// validation, so an unmodelled field cannot ride through.
    ///
    /// # The reference must be the write reference
    ///
    /// `image` is propagated verbatim by [`super::sibling_tag_reference`], which
    /// only swaps the tag, so this PUTs to whatever host it is handed. It must
    /// therefore come from `Client::transport_write_reference` — the canonical
    /// registry — never from a mirrored read reference: a mirror is read-only,
    /// and deciding from one host while writing to another is the CWE-345/367
    /// class `client.rs` already names at its addressing seams.
    ///
    /// # Errors
    ///
    /// - Whatever [`Self::pull_referrer_fallback_index`] refuses — a refused read
    ///   aborts the append and leaves the tag untouched.
    /// - [`ClientError::ReferrersUnsupported`] — appending would push the index
    ///   past [`MAX_FALLBACK_DESCRIPTORS`] or [`MAX_FALLBACK_INDEX_BYTES`], so
    ///   the fallback cannot hold this referrer and a rerun cannot change that.
    ///   Exit 84, and nothing is pushed. The limit itself goes to the log,
    ///   because the error carries only the registry.
    /// - [`ClientError::ReferrersUnsupported`] — the registry **declined** to hold
    ///   the index (it answered, and said no). Exit 84.
    /// - [`ClientError::RegistryTransient`] — the retries were exhausted by
    ///   concurrent writers. Nothing was refused and a rerun converges, so this is
    ///   exit 75, not 84. Never an `Ok` that drops the descriptor.
    ///
    /// # Default implementation
    ///
    /// Built on [`Self::pull_referrer_fallback_index`] and
    /// [`Self::push_manifest_raw`], for the same reason as its sibling.
    async fn append_referrer_fallback_index(
        &self,
        image: &oci::native::Reference,
        subject_digest: &oci::Digest,
        descriptor: &oci::Descriptor,
    ) -> Result<FallbackAppend> {
        let tag = referrer_fallback_tag(subject_digest);
        let target = super::sibling_tag_reference(image, tag.clone());
        for _attempt in 0..MAX_FALLBACK_ATTEMPTS {
            let index = self.pull_referrer_fallback_index(image, subject_digest).await?;
            if index_carries(&index, &descriptor.digest) {
                return Ok(FallbackAppend::AlreadyPresent);
            }
            let next = rebuild_with(index, descriptor);
            // The caps above are the read's, and until here nothing applied
            // them to a write: an index already at the ceiling would be pushed
            // one entry past it, the PUT would land, and every later read —
            // this method's own read-back first — would refuse the document.
            // That is a permanent denial of signature discovery for this
            // subject, written under OCX's own credentials, with no path in
            // OCX that can shrink it again. Refuse before the PUT, with the
            // code the read side gives the same document.
            if next.manifests.len() > MAX_FALLBACK_DESCRIPTORS {
                crate::log::warn!(
                    "referrers fallback index {tag} already lists {} descriptors; appending would pass the \
                     {MAX_FALLBACK_DESCRIPTORS} limit and leave a document this client refuses to read",
                    next.manifests.len() - 1
                );
                return Err(index_cannot_hold_it(image));
            }
            let bytes = serde_json::to_vec(&next).map_err(ClientError::Serialization)?;
            if bytes.len() > MAX_FALLBACK_INDEX_BYTES {
                crate::log::warn!(
                    "referrers fallback index {tag} would be {} bytes with this descriptor appended, above the \
                     {MAX_FALLBACK_INDEX_BYTES}-byte limit this client will read back",
                    bytes.len()
                );
                return Err(index_cannot_hold_it(image));
            }
            self.push_manifest_raw(&target, bytes, crate::media_type::MEDIA_TYPE_OCI_IMAGE_INDEX)
                .await
                .map_err(|error| fallback_write_refused(error, image))?;
            // The read-back is the only evidence the PUT survived: a concurrent
            // writer that read the same base index and pushed after us produced
            // an `Ok` above while dropping this descriptor.
            //
            // A read-back that *errors* aborts the append without telling the
            // caller whether the PUT landed. That ambiguity is benign only
            // because the append is idempotent: a rerun re-reads, finds the
            // descriptor via `index_carries`, and returns `AlreadyPresent`
            // without a second PUT. An edit that drops that check breaks this
            // too.
            let after = self.pull_referrer_fallback_index(image, subject_digest).await?;
            if index_carries(&after, &descriptor.digest) {
                return Ok(FallbackAppend::Written);
            }
        }
        Err(ClientError::RegistryTransient(
            format!(
                "referrers fallback index {tag} was overwritten by a concurrent writer \
                 {MAX_FALLBACK_ATTEMPTS} times; the descriptor was not appended"
            )
            .into(),
        ))
    }

    // ── Clone support ────────────────────────────────────────────────

    /// Clones the transport into a boxed trait object.
    fn box_clone(&self) -> Box<dyn OciTransport>;
}

// ── Default-impl helpers and tests ───────────────────────────────────────────

/// Reads `path` into memory and pushes it through [`OciTransport::push_blob`].
///
/// The body a test double gives [`OciTransport::push_blob_from_path`] when it
/// has no streaming to preserve — buffering stated at the call site instead of
/// inherited from a default nobody read. A production transport must not use
/// it: the point of the file-backed push is that a 200 MB layer costs a file
/// handle rather than a 200 MB allocation, several times over concurrently —
/// which is why it is `cfg(test)`: production code cannot reach it.
#[cfg(test)]
pub(crate) async fn push_blob_buffered<T: OciTransport + ?Sized>(
    transport: &T,
    image: &oci::native::Reference,
    path: &Path,
    digest: &oci::Digest,
    on_progress: ProgressFn,
) -> Result<String> {
    let data = tokio::fs::read(path).await.map_err(|e| ClientError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    transport.push_blob(image, data, digest, on_progress).await
}

/// RAII wrapper that holds a temporary file open for reading while keeping the
/// [`tempfile::NamedTempFile`] guard alive so the underlying path is not
/// deleted until the reader is dropped.
///
/// Used by the default implementation of
/// [`OciTransport::pull_blob_streaming`] to stream an already-downloaded blob
/// back as `AsyncRead`.
struct TempFileReader {
    file: tokio::fs::File,
    /// Keeps the temp file on disk until this reader is dropped.
    _guard: tempfile::NamedTempFile,
}

impl AsyncRead for TempFileReader {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.file).poll_read(cx, buf)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;
    use crate::oci::{Algorithm, RegistryOperation};
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, RwLock};

    // ── Minimal OciTransport impl for testing default pull_blob_streaming ──

    /// In-memory stub OciTransport that only implements `pull_blob_to_file` using
    /// a simple byte-map. Used to exercise the DEFAULT implementation of
    /// `pull_blob_streaming` without pulling in StubTransport from the test module.
    struct InlineStub {
        blobs: Arc<RwLock<HashMap<String, Vec<u8>>>>,
    }

    impl InlineStub {
        fn new(blobs: HashMap<String, Vec<u8>>) -> Self {
            Self {
                blobs: Arc::new(RwLock::new(blobs)),
            }
        }

        fn box_clone_inner(&self) -> Self {
            Self {
                blobs: Arc::clone(&self.blobs),
            }
        }
    }

    #[async_trait]
    impl OciTransport for InlineStub {
        async fn ensure_auth(&self, _image: &oci::native::Reference, _op: RegistryOperation) -> Result<()> {
            Ok(())
        }

        async fn list_tags(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn catalog(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn fetch_manifest_digest(&self, _image: &oci::native::Reference) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_manifest_raw(
            &self,
            _image: &oci::native::Reference,
            _accepted_media_types: &[&str],
        ) -> Result<(Vec<u8>, String)> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_blob(&self, _image: &oci::native::Reference, _digest: &oci::Digest) -> Result<Vec<u8>> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_blob_to_file(
            &self,
            _image: &oci::native::Reference,
            digest: &oci::Digest,
            path: &Path,
        ) -> Result<()> {
            use super::super::error::ClientError;
            let key = digest.to_string();
            let inner = self.blobs.read().unwrap();
            let bytes = inner.get(&key).cloned().unwrap_or_default();
            drop(inner);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ClientError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }
            std::fs::write(path, &bytes).map_err(|e| ClientError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
            Ok(())
        }

        async fn head_blob(&self, _image: &oci::native::Reference, _digest: &oci::Digest) -> Result<u64> {
            Ok(0)
        }

        async fn push_manifest(&self, _image: &oci::native::Reference, _manifest: &oci::Manifest) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_manifest_raw(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _media_type: &str,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_blob(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _digest: &oci::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_blob_from_path(
            &self,
            _image: &oci::native::Reference,
            _path: &Path,
            _digest: &oci::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_referrer_manifest(
            &self,
            _image: &oci::native::Reference,
            _subject_digest: &oci::Digest,
            _manifest_bytes: &[u8],
            _media_type: &str,
        ) -> Result<oci::Descriptor> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn list_referrers(
            &self,
            _image: &oci::native::Reference,
            _subject_digest: &oci::Digest,
            _artifact_type: Option<&str>,
        ) -> Result<Vec<oci::Descriptor>> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(self.box_clone_inner())
        }
    }

    fn test_reference() -> oci::native::Reference {
        oci::native::Reference::try_from("example.com/test/pkg:1.0").expect("valid reference")
    }

    // ── pull_blob_streaming default impl ─────────────────────────────

    /// spec §OciTransport::pull_blob_streaming default impl:
    /// delegates to pull_blob_to_file into temp file then streams file back.
    /// The returned AsyncRead must yield the same bytes as stored in the blob map.
    #[tokio::test]
    async fn default_pull_blob_streaming_yields_blob_content() {
        let blob_content = b"compressed layer bytes for testing".to_vec();
        let digest = Algorithm::Sha256.hash(&blob_content);

        let mut blobs = HashMap::new();
        blobs.insert(digest.to_string(), blob_content.clone());
        let transport = InlineStub::new(blobs);

        let reference = test_reference();
        let mut stream = transport.pull_blob_streaming(&reference, &digest).await.unwrap();

        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut received)
            .await
            .unwrap();

        assert_eq!(
            received, blob_content,
            "default pull_blob_streaming must yield the same bytes as pull_blob_to_file"
        );
    }

    /// spec §OciTransport::pull_blob_streaming default impl:
    /// empty blob returns empty stream (not an error).
    #[tokio::test]
    async fn default_pull_blob_streaming_empty_blob_yields_empty_stream() {
        let blob_content: Vec<u8> = vec![];
        let digest = Algorithm::Sha256.hash(&blob_content);

        let mut blobs = HashMap::new();
        blobs.insert(digest.to_string(), blob_content.clone());
        let transport = InlineStub::new(blobs);

        let reference = test_reference();
        let mut stream = transport.pull_blob_streaming(&reference, &digest).await.unwrap();

        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut received)
            .await
            .unwrap();

        assert!(
            received.is_empty(),
            "empty blob must yield empty stream from default impl"
        );
    }

    /// spec §OciTransport::pull_blob_streaming default impl:
    /// default path has NO VerifyingStream — HashingAsyncReader in pull_layer is
    /// the sole verifier. This test confirms the default impl does not itself
    /// verify the digest (it just streams bytes as-is from the temp file).
    /// A corrupted blob served via InlineStub flows through unchanged —
    /// the CALLER (pull_layer + HashingAsyncReader) detects the mismatch.
    #[tokio::test]
    async fn default_pull_blob_streaming_passes_through_bytes_without_verifying() {
        // Store bytes that do NOT match the declared digest.
        // The default impl must stream them as-is (no verification at transport layer).
        let honest_content = b"honest bytes".to_vec();
        let evil_content = b"evil bytes corrupted".to_vec();
        let honest_digest = Algorithm::Sha256.hash(&honest_content);

        // Register evil bytes under the honest digest key.
        let mut blobs = HashMap::new();
        blobs.insert(honest_digest.to_string(), evil_content.clone());
        let transport = InlineStub::new(blobs);

        let reference = test_reference();
        let mut stream = transport.pull_blob_streaming(&reference, &honest_digest).await.unwrap();

        let mut received = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut received)
            .await
            .unwrap();

        // Default impl does NOT verify — bytes flow through unchanged.
        // The mismatch is the caller's responsibility (HashingAsyncReader).
        assert_eq!(
            received, evil_content,
            "default pull_blob_streaming must not verify digest; bytes flow through as-is for caller verification"
        );
    }

    // ── Referrers fallback tag (OCI tag schema) ───────────────────────────

    /// One latch on one call: wait on the first gate before the operation, fire
    /// the second after it.
    type Gate = (Option<Arc<tokio::sync::Notify>>, Option<Arc<tokio::sync::Notify>>);

    /// The manifest PUTs a `FallbackRegistry` recorded, in order.
    type PushLog = Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;

    /// A registry with no Referrers API and one mutable manifest store, whose
    /// reads and writes can be gated by call number.
    ///
    /// It models exactly what makes the fallback tag hard: a manifest PUT that
    /// unconditionally replaces whatever was at the tag, with no compare-and-set
    /// anywhere in the OCI distribution spec to prevent it. The per-call gates
    /// exist because a lost update cannot be produced by hoping the scheduler
    /// interleaves two tasks the right way — the same `Notify`-hold idiom
    /// `oci/index/local_index.rs`'s `ScriptedSource` uses to force a completion
    /// order, indexed by call so a read-back can be held apart from the read
    /// that preceded it.
    /// How the fixture's Referrers API fails.
    ///
    /// `Unsupported` is the fixture's whole reason to exist; the other two are a
    /// registry that *has* the endpoint and answered badly — which must not be
    /// read as a capability verdict.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ReferrersApiFault {
        Unsupported,
        Unauthorized,
        ServerFault,
    }

    #[derive(Clone)]
    struct FallbackRegistry {
        manifests: Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>>,
        pushes: PushLog,
        /// What `list_referrers` answers with.
        referrers_fault: ReferrersApiFault,
        /// When set, a PUT is logged and answered `Ok` but never stored — a
        /// concurrent writer that wins every single round.
        swallow_pushes: bool,
        /// When set, a PUT is answered with this HTTP status, boxed the way
        /// `registry_error` boxes it. Drives the real classification path
        /// rather than handing `fallback_write_refused` a value by hand.
        push_status: Option<u16>,
        /// Gate for read *n*; beyond the end of the vec, reads are ungated.
        read_gates: Vec<Gate>,
        /// Gate for push *n*; beyond the end of the vec, pushes are ungated.
        push_gates: Vec<Gate>,
        reads: Arc<std::sync::atomic::AtomicUsize>,
        writes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FallbackRegistry {
        fn new() -> Self {
            Self::with_referrers_fault(ReferrersApiFault::Unsupported)
        }

        /// The same registry, with its Referrers API failing some other way.
        fn with_referrers_fault(referrers_fault: ReferrersApiFault) -> Self {
            Self {
                manifests: Arc::new(std::sync::Mutex::new(HashMap::new())),
                pushes: Arc::new(std::sync::Mutex::new(Vec::new())),
                referrers_fault,
                swallow_pushes: false,
                push_status: None,
                read_gates: Vec::new(),
                push_gates: Vec::new(),
                reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                writes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        /// A second client of the same registry, with its own call counters.
        fn second_client(&self) -> Self {
            Self {
                manifests: self.manifests.clone(),
                pushes: self.pushes.clone(),
                referrers_fault: self.referrers_fault,
                swallow_pushes: self.swallow_pushes,
                push_status: self.push_status,
                read_gates: Vec::new(),
                push_gates: Vec::new(),
                reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                writes: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn seed(&self, reference: &str, bytes: Vec<u8>) {
            self.manifests.lock().unwrap().insert(reference.to_string(), bytes);
        }

        fn stored(&self, reference: &str) -> Option<Vec<u8>> {
            self.manifests.lock().unwrap().get(reference).cloned()
        }

        fn pushed_tags(&self) -> Vec<String> {
            self.pushes.lock().unwrap().iter().map(|(r, _)| r.clone()).collect()
        }

        /// Takes the gate for call `n`, if the script has one.
        fn gate(gates: &[Gate], n: usize) -> Option<Gate> {
            gates.get(n).cloned()
        }

        async fn enter(gate: &Option<Gate>) {
            if let Some((Some(wait), _)) = gate {
                wait.notified().await;
            }
        }

        fn leave(gate: &Option<Gate>) {
            if let Some((_, Some(signal))) = gate {
                signal.notify_one();
            }
        }
    }

    #[async_trait]
    impl OciTransport for FallbackRegistry {
        async fn ensure_auth(&self, _image: &oci::native::Reference, _operation: RegistryOperation) -> Result<()> {
            Ok(())
        }

        async fn list_tags(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn catalog(
            &self,
            _image: &oci::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn fetch_manifest_digest(&self, _image: &oci::native::Reference) -> Result<String> {
            unimplemented!("the fallback-index tests never fetch a bare digest")
        }

        async fn pull_manifest_raw(
            &self,
            image: &oci::native::Reference,
            _accepted_media_types: &[&str],
        ) -> Result<(Vec<u8>, String)> {
            let n = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let gate = Self::gate(&self.read_gates, n);
            Self::enter(&gate).await;
            let key = image.to_string();
            let answer = match self.manifests.lock().unwrap().get(&key) {
                Some(bytes) => {
                    let digest = Algorithm::Sha256.hash(bytes).to_string();
                    Ok((bytes.clone(), digest))
                }
                None => Err(ClientError::ManifestNotFound(key)),
            };
            Self::leave(&gate);
            answer
        }

        async fn pull_blob(&self, _image: &oci::native::Reference, _digest: &oci::Digest) -> Result<Vec<u8>> {
            unimplemented!("the fallback-index tests never pull a blob")
        }

        async fn pull_blob_to_file(
            &self,
            _image: &oci::native::Reference,
            _digest: &oci::Digest,
            _path: &Path,
        ) -> Result<()> {
            unimplemented!("the fallback-index tests never pull a blob")
        }

        async fn head_blob(&self, _image: &oci::native::Reference, _digest: &oci::Digest) -> Result<u64> {
            unimplemented!("the fallback-index tests never head a blob")
        }

        async fn push_manifest(&self, _image: &oci::native::Reference, _manifest: &oci::Manifest) -> Result<String> {
            unimplemented!("the fallback index is pushed through push_manifest_raw")
        }

        async fn push_manifest_raw(
            &self,
            image: &oci::native::Reference,
            data: Vec<u8>,
            _media_type: &str,
        ) -> Result<String> {
            let n = self.writes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let gate = Self::gate(&self.push_gates, n);
            Self::enter(&gate).await;
            if let Some(status) = self.push_status {
                Self::leave(&gate);
                return Err(ClientError::Registry(Box::new(server_error(status))));
            }
            let key = image.to_string();
            let digest = Algorithm::Sha256.hash(&data).to_string();
            // Last writer wins, unconditionally. There is no If-Match on a
            // manifest PUT anywhere in the OCI distribution spec, so this is the
            // real behaviour, not a simplification.
            self.pushes.lock().unwrap().push((key.clone(), data.clone()));
            if !self.swallow_pushes {
                self.manifests.lock().unwrap().insert(key, data);
            }
            Self::leave(&gate);
            Ok(digest)
        }

        async fn push_blob(
            &self,
            _image: &oci::native::Reference,
            _data: Vec<u8>,
            _digest: &oci::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("the fallback-index tests never push a blob")
        }

        async fn push_blob_from_path(
            &self,
            _image: &oci::native::Reference,
            _path: &Path,
            _digest: &oci::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("the fallback-index tests never push a blob")
        }

        async fn push_referrer_manifest(
            &self,
            _image: &oci::native::Reference,
            _subject_digest: &oci::Digest,
            _manifest_bytes: &[u8],
            _media_type: &str,
        ) -> Result<oci::Descriptor> {
            unimplemented!("the fallback-index tests append a descriptor directly")
        }

        async fn list_referrers(
            &self,
            image: &oci::native::Reference,
            _subject_digest: &oci::Digest,
            _artifact_type: Option<&str>,
        ) -> Result<Vec<oci::Descriptor>> {
            Err(match self.referrers_fault {
                // The whole point of the fixture: no OCI 1.1 Referrers API.
                ReferrersApiFault::Unsupported => ClientError::ReferrersUnsupported {
                    registry: image.resolve_registry().to_string(),
                },
                ReferrersApiFault::Unauthorized => ClientError::Authentication("401 on the referrers endpoint".into()),
                ReferrersApiFault::ServerFault => {
                    ClientError::RegistryTransient("500 on the referrers endpoint".into())
                }
            })
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(self.clone())
        }
    }

    fn subject() -> oci::Digest {
        oci::Digest::Sha256("a".repeat(64))
    }

    fn image() -> oci::native::Reference {
        // Parsed rather than direct-constructed: the direct constructors are
        // gated to the seams in client.rs by
        // `native_reference_direct_construction_restricted_to_seams`, whose
        // scan is over source text — so naming the gated spelling here, even
        // in a comment, would trip it.
        "registry.test/acme/tool:1.0".parse().expect("a valid reference")
    }

    fn fallback_reference() -> String {
        format!("registry.test/acme/tool:{}", referrer_fallback_tag(&subject()))
    }

    /// An `oci_client` HTTP-status error, the shape `registry_error`'s
    /// catch-all boxes into [`ClientError::Registry`].
    fn server_error(code: u16) -> oci_client::errors::OciDistributionError {
        oci_client::errors::OciDistributionError::ServerError {
            code,
            url: "https://registry.test/v2/acme/tool/manifests/sha256-x".into(),
            message: format!("HTTP {code}"),
        }
    }

    fn descriptor(hex: &str) -> oci::Descriptor {
        oci::Descriptor {
            media_type: "application/vnd.oci.image.manifest.v1+json".into(),
            digest: format!("sha256:{}", hex.repeat(64)),
            size: 123,
            urls: None,
            artifact_type: Some("application/vnd.dev.sigstore.bundle.v0.3+json".into()),
            annotations: Some(
                [("dev.sigstore.bundle.content".to_string(), "dsse-envelope".to_string())]
                    .into_iter()
                    .collect(),
            ),
        }
    }

    /// The heart of the fallback write: two writers race one index, and **both**
    /// descriptors survive.
    ///
    /// The interleave is scripted, not hoped for. Writer A reads the empty index
    /// and then blocks before its push; writer B reads the same empty index,
    /// pushes `[b]`, and releases A; A pushes `[a]`, clobbering B. A's read-back
    /// finds its own descriptor and stops. B's read-back does not find `b`, so B
    /// re-reads `[a]`, appends, and pushes `[a, b]`.
    ///
    /// A single-writer test proves none of this: the lost update only exists
    /// when a second writer's PUT lands between the first's read and its write.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_writers_racing_one_fallback_index_both_land() {
        // The interleave is scripted, not hoped for. `a_read` releases B's first
        // read only after A has taken the same empty base; `b_pushed` holds A's
        // write until B's has landed, so A's write is the clobber; `a_pushed`
        // holds B's read-back until the clobber is visible, which is the only
        // moment at which B can learn it lost.
        //
        //   A read []  ->  B read []  ->  B write [b]  ->  A write [a] (clobber)
        //     -> A read-back [a], sees itself, done
        //     -> B read-back [a], does NOT see itself, re-reads and appends
        //
        // A single-writer test proves none of this, and neither does an
        // unscripted one: without the third gate B verifies before the clobber
        // and reports success on a descriptor that is already gone.
        let a_read = Arc::new(tokio::sync::Notify::new());
        let b_pushed = Arc::new(tokio::sync::Notify::new());
        let a_pushed = Arc::new(tokio::sync::Notify::new());

        let mut writer_a = FallbackRegistry::new();
        writer_a.read_gates = vec![(None, Some(a_read.clone()))];
        writer_a.push_gates = vec![(Some(b_pushed.clone()), Some(a_pushed.clone()))];

        let mut writer_b = writer_a.second_client();
        writer_b.read_gates = vec![(Some(a_read.clone()), None), (Some(a_pushed.clone()), None)];
        writer_b.push_gates = vec![(None, Some(b_pushed.clone()))];

        let registry = writer_a.clone();
        let descriptor_a = descriptor("a");
        let descriptor_b = descriptor("b");

        // A hang here means one writer never reached the call its gate is
        // waiting on — a real failure, and one that must not present as a
        // stalled suite.
        let raced = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(
                {
                    let descriptor_a = descriptor_a.clone();
                    async move {
                        writer_a
                            .append_referrer_fallback_index(&image(), &subject(), &descriptor_a)
                            .await
                    }
                },
                {
                    let descriptor_b = descriptor_b.clone();
                    async move {
                        writer_b
                            .append_referrer_fallback_index(&image(), &subject(), &descriptor_b)
                            .await
                    }
                },
            )
        })
        .await;
        let (result_a, result_b) = raced.expect("both writers must finish; a timeout means a gate was never released");

        assert!(
            matches!(result_a, Ok(FallbackAppend::Written)),
            "writer A: {result_a:?}"
        );
        assert!(
            matches!(result_b, Ok(FallbackAppend::Written)),
            "writer B: {result_b:?}"
        );

        let bytes = registry
            .stored(&fallback_reference())
            .expect("the fallback index must exist after two successful appends");
        let index: oci::ImageIndex = serde_json::from_slice(&bytes).expect("a valid image index");
        let digests: Vec<&str> = index.manifests.iter().map(|entry| entry.digest.as_str()).collect();
        assert!(
            digests.contains(&descriptor_a.digest.as_str()),
            "writer A's descriptor was lost: {digests:?}"
        );
        assert!(
            digests.contains(&descriptor_b.digest.as_str()),
            "writer B's descriptor was lost — the loser of the race did not re-read and retry: {digests:?}"
        );
    }

    /// The inverted S1-F tape. The ADR claimed a test asserting **no**
    /// `sha256-<hex>` manifest write; this asserts the write happens, at the
    /// spec-derived tag, carrying the two fields cosign's own fallback write
    /// loses (sigstore/cosign#4641).
    #[tokio::test]
    async fn the_fallback_write_lands_at_the_spec_tag_with_artifact_type_and_annotations() {
        let registry = FallbackRegistry::new();
        let signature = descriptor("c");

        let outcome = registry
            .append_referrer_fallback_index(&image(), &subject(), &signature)
            .await
            .expect("the append must succeed against an empty registry");
        assert_eq!(outcome, FallbackAppend::Written);

        let expected_tag = format!("sha256-{}", "a".repeat(64));
        assert_eq!(
            registry.pushed_tags(),
            vec![format!("registry.test/acme/tool:{expected_tag}")],
            "exactly one manifest PUT, at the spec-derived fallback tag"
        );

        let bytes = registry
            .stored(&fallback_reference())
            .expect("the index must be stored");
        let index: oci::ImageIndex = serde_json::from_slice(&bytes).expect("a valid image index");
        let entry = index.manifests.first().expect("one appended entry");
        assert_eq!(
            entry.artifact_type.as_deref(),
            Some("application/vnd.dev.sigstore.bundle.v0.3+json"),
            "artifactType must survive the fallback write"
        );
        assert_eq!(
            entry
                .annotations
                .as_ref()
                .and_then(|a| a.get("dev.sigstore.bundle.content")),
            Some(&"dsse-envelope".to_string()),
            "annotations must survive the fallback write"
        );
        assert_eq!(
            index.media_type.as_deref(),
            Some("application/vnd.oci.image.index.v1+json")
        );
    }

    /// Appending a descriptor that is already listed pushes nothing at all.
    #[tokio::test]
    async fn appending_an_already_listed_descriptor_writes_nothing() {
        let registry = FallbackRegistry::new();
        let signature = descriptor("d");
        registry
            .append_referrer_fallback_index(&image(), &subject(), &signature)
            .await
            .expect("first append");
        let after_first = registry.pushed_tags().len();

        let outcome = registry
            .append_referrer_fallback_index(&image(), &subject(), &signature)
            .await
            .expect("second append");

        assert_eq!(outcome, FallbackAppend::AlreadyPresent);
        assert_eq!(
            registry.pushed_tags().len(),
            after_first,
            "a re-append must issue no manifest PUT"
        );
    }

    /// No Referrers API and no fallback tag is *no signatures*, not *cannot
    /// look*. Returning `ReferrersUnsupported` here is what made exit 84 the
    /// answer to a question the operator did not ask.
    #[tokio::test]
    async fn a_missing_api_and_a_missing_tag_read_as_an_empty_fallback_listing() {
        let registry = FallbackRegistry::new();

        let listing = registry
            .list_referrers_with_fallback(&image(), &subject(), None)
            .await
            .expect("an absent fallback tag is an empty listing, never an error");

        assert!(listing.descriptors.is_empty());
        assert_eq!(listing.via, DiscoveryMethod::FallbackTag);
    }

    /// Losing every round is a loud, retryable failure — never an `Ok` that
    /// dropped the descriptor.
    ///
    /// Exhaustion is exit 75, not 84: nothing was refused, and a rerun against
    /// a quieter registry converges. `MAX_FALLBACK_ATTEMPTS` and the whole
    /// exhaustion arm have no other test — the racing test converges, which is
    /// the opposite branch.
    #[tokio::test]
    async fn an_append_that_loses_every_round_fails_loudly_as_transient() {
        use crate::cli::ClassifyExitCode;

        let mut registry = FallbackRegistry::new();
        // Every PUT is answered `Ok` and thrown away: the read-back never sees
        // this descriptor, which is exactly what a writer that lost the race
        // observes.
        registry.swallow_pushes = true;

        let error = registry
            .append_referrer_fallback_index(&image(), &subject(), &descriptor("a"))
            .await
            .expect_err("a descriptor that never lands must not report success");

        assert!(
            matches!(error, ClientError::RegistryTransient(_)),
            "exhaustion is a lost race, not a refused capability: {error}"
        );
        assert_eq!(error.classify(), Some(crate::cli::ExitCode::TempFail));
        assert_eq!(
            registry.pushed_tags().len(),
            MAX_FALLBACK_ATTEMPTS,
            "the loop must spend its whole budget before giving up"
        );
    }

    /// An index at the descriptor ceiling is refused **before** the PUT.
    ///
    /// Without the pre-PUT check the append pushes 4097 entries, the PUT lands,
    /// and every later read — including this method's own read-back — refuses
    /// the document. Nothing in OCX can shrink it again, so the tag is
    /// permanently undiscoverable, bricked under OCX's own credentials.
    #[tokio::test]
    async fn appending_to_a_full_index_is_refused_before_anything_is_written() {
        use crate::cli::ClassifyExitCode;

        let registry = FallbackRegistry::new();
        let mut full = empty_fallback_index();
        full.manifests = (0..MAX_FALLBACK_DESCRIPTORS)
            .map(|n| oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".into(),
                digest: format!("sha256:{n:064x}"),
                size: 2,
                platform: None,
                annotations: None,
                artifact_type: None,
            })
            .collect();
        let seeded = serde_json::to_vec(&full).unwrap();
        registry.seed(&fallback_reference(), seeded.clone());

        let error = registry
            .append_referrer_fallback_index(&image(), &subject(), &descriptor("a"))
            .await
            .expect_err("appending past the ceiling must be refused");

        assert_eq!(error.classify(), Some(crate::cli::ExitCode::ReferrersUnsupported));
        assert!(
            registry.pushed_tags().is_empty(),
            "nothing may be PUT once the append is refused"
        );
        assert_eq!(
            registry.stored(&fallback_reference()).as_deref(),
            Some(seeded.as_slice()),
            "the tag must be byte-identical to what was there before"
        );
    }

    /// The count cap is not the only way past the byte cap: one descriptor with
    /// a large enough annotation carries the document over on its own.
    #[tokio::test]
    async fn a_descriptor_that_alone_exceeds_the_byte_cap_is_refused_before_the_put() {
        use crate::cli::ClassifyExitCode;

        let registry = FallbackRegistry::new();
        let mut fat = descriptor("a");
        fat.annotations = Some(
            [("dev.example.blob".to_string(), "a".repeat(MAX_FALLBACK_INDEX_BYTES + 1))]
                .into_iter()
                .collect(),
        );

        let error = registry
            .append_referrer_fallback_index(&image(), &subject(), &fat)
            .await
            .expect_err("a single over-cap entry must be refused");

        assert_eq!(error.classify(), Some(crate::cli::ExitCode::ReferrersUnsupported));
        assert!(registry.pushed_tags().is_empty(), "nothing may be PUT");
    }

    /// The exit code an append gives is decided by what the registry actually
    /// answered — driven through the append, not handed to the mapper.
    ///
    /// The unit test above hands `fallback_write_refused` a value it built
    /// itself, so it is structurally incapable of catching a mis-mapping in the
    /// path that produces those values. This one PUTs against a registry that
    /// answers a real status.
    #[tokio::test]
    async fn the_status_the_registry_answered_decides_the_append_exit_code() {
        use crate::cli::ClassifyExitCode;

        for (status, expected) in [
            // Answered and declined: the index is not something it will hold.
            (400, crate::cli::ExitCode::ReferrersUnsupported),
            (405, crate::cli::ExitCode::ReferrersUnsupported),
            (422, crate::cli::ExitCode::ReferrersUnsupported),
            // A fault. `registry_error`'s transient arm is the literal list
            // 429|502|503|504, so a 500 lands in the `Registry` catch-all with
            // every parse error — reporting it as 84 would tell a CI wrapper to
            // stop retrying a sign that would have succeeded.
            (500, crate::cli::ExitCode::Unavailable),
            (501, crate::cli::ExitCode::Unavailable),
        ] {
            let mut registry = FallbackRegistry::new();
            registry.push_status = Some(status);

            let error = registry
                .append_referrer_fallback_index(&image(), &subject(), &descriptor("a"))
                .await
                .expect_err("the fixture answers every PUT with an error");

            assert_eq!(error.classify(), Some(expected), "HTTP {status} classified wrongly");
        }
    }

    /// A registry that *has* a Referrers API and answered badly is not a
    /// registry without one.
    ///
    /// C-004: only [`ClientError::ReferrersUnsupported`] opens the fallback. A
    /// 401 or a 500 propagates with its own exit code, and the fallback tag is
    /// never read — substituting a tag anyone with push access can author for a
    /// endpoint that merely refused the caller's credentials would answer a
    /// security question with an attacker-writable document.
    #[tokio::test]
    async fn a_native_referrers_failure_propagates_and_never_reads_the_fallback_tag() {
        use crate::cli::ClassifyExitCode;
        use std::sync::atomic::Ordering;

        for (fault, expected) in [
            (ReferrersApiFault::Unauthorized, crate::cli::ExitCode::AuthError),
            (ReferrersApiFault::ServerFault, crate::cli::ExitCode::TempFail),
        ] {
            let registry = FallbackRegistry::with_referrers_fault(fault);
            // Seed the tag, so a fallback read would *succeed* and return a
            // descriptor. Without this the test could not tell "did not fall
            // back" from "fell back and found nothing".
            let seeded = rebuild_with(empty_fallback_index(), &descriptor("e"));
            registry.seed(&fallback_reference(), serde_json::to_vec(&seeded).unwrap());

            let error = registry
                .list_referrers_with_fallback(&image(), &subject(), None)
                .await
                .expect_err("a native referrers failure is not an empty listing");

            assert_eq!(error.classify(), Some(expected));
            assert_eq!(
                registry.reads.load(Ordering::SeqCst),
                0,
                "the fallback tag must not be read when the Referrers API merely failed"
            );
        }
    }

    /// A signature parked in a fallback index by some other tool is discovered,
    /// and reported as having come from the tag rather than the API.
    #[tokio::test]
    async fn a_seeded_fallback_index_is_discovered_and_reports_its_discovery_method() {
        let registry = FallbackRegistry::new();
        let seeded = rebuild_with(empty_fallback_index(), &descriptor("e"));
        registry.seed(&fallback_reference(), serde_json::to_vec(&seeded).unwrap());

        let listing = registry
            .list_referrers_with_fallback(&image(), &subject(), None)
            .await
            .expect("a seeded fallback index must be readable");

        assert_eq!(listing.descriptors.len(), 1);
        assert_eq!(listing.via, DiscoveryMethod::FallbackTag);
        assert_eq!(
            listing.descriptors[0].artifact_type.as_deref(),
            Some("application/vnd.dev.sigstore.bundle.v0.3+json")
        );
        assert!(
            listing.descriptors[0].urls.is_none(),
            "`ImageIndexEntry` models no `urls`, so this cannot fail today — it is the tripwire for an \
             upstream that adds one, not coverage of current behaviour"
        );
    }

    /// The artifact-type filter has no server-side equivalent on the tag schema,
    /// so the client-side pass is the only one there is.
    #[tokio::test]
    async fn the_fallback_listing_filters_by_artifact_type_client_side() {
        let registry = FallbackRegistry::new();
        let mut other = descriptor("f");
        other.artifact_type = Some("application/vnd.example.other".into());
        let seeded = rebuild_with(rebuild_with(empty_fallback_index(), &descriptor("e")), &other);
        registry.seed(&fallback_reference(), serde_json::to_vec(&seeded).unwrap());

        let listing = registry
            .list_referrers_with_fallback(&image(), &subject(), Some("application/vnd.example.other"))
            .await
            .expect("a seeded fallback index must be readable");

        assert_eq!(listing.descriptors.len(), 1);
        assert_eq!(listing.descriptors[0].digest, other.digest);
    }

    /// A non-index at the fallback tag refuses the read **and aborts the write**,
    /// leaving the tag byte-identical.
    ///
    /// The alternative — degrading a refused read to an empty index — would have
    /// this client republish `[]` over every sibling referrer it did not author,
    /// under its own credentials. That is the suppression attack, performed by
    /// the victim.
    #[tokio::test]
    async fn a_non_index_at_the_fallback_tag_refuses_the_read_and_aborts_the_write() {
        let registry = FallbackRegistry::new();
        let intruder = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.empty.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#.to_vec();
        registry.seed(&fallback_reference(), intruder.clone());

        let read = registry.pull_referrer_fallback_index(&image(), &subject()).await;
        assert!(
            matches!(read, Err(ClientError::UnexpectedManifestType)),
            "a non-index must be refused, got {read:?}"
        );

        let write = registry
            .append_referrer_fallback_index(&image(), &subject(), &descriptor("a"))
            .await;
        assert!(
            matches!(write, Err(ClientError::UnexpectedManifestType)),
            "a refused read must abort the append, got {write:?}"
        );
        assert!(registry.pushed_tags().is_empty(), "an aborted append must push nothing");
        assert_eq!(
            registry.stored(&fallback_reference()),
            Some(intruder),
            "the tag must be left byte-identical"
        );
    }

    /// An index above the descriptor cap is refused rather than parsed and
    /// re-published — the byte cap alone does not bound the work a caller does
    /// per entry.
    #[tokio::test]
    async fn an_over_cap_fallback_index_is_refused() {
        let registry = FallbackRegistry::new();
        let mut index = empty_fallback_index();
        index.manifests = (0..=MAX_FALLBACK_DESCRIPTORS)
            .map(|i| oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".into(),
                digest: format!("sha256:{i:064x}"),
                size: 1,
                platform: None,
                annotations: None,
                artifact_type: None,
            })
            .collect();
        registry.seed(&fallback_reference(), serde_json::to_vec(&index).unwrap());

        let read = registry.pull_referrer_fallback_index(&image(), &subject()).await;
        assert!(
            matches!(read, Err(ClientError::InvalidManifest(_))),
            "an over-cap index must be refused, got {read:?}"
        );
    }

    /// An index-level `annotations` map the caller did not author must not ride
    /// through into what this client re-publishes under its own credentials.
    #[test]
    fn the_written_index_header_is_rebuilt_not_echoed() {
        let mut hostile = empty_fallback_index();
        hostile.annotations = Some([("attacker".to_string(), "value".to_string())].into_iter().collect());
        hostile.artifact_type = Some("application/vnd.attacker".into());

        let rebuilt = rebuild_with(hostile, &descriptor("a"));

        assert!(
            rebuilt.annotations.is_none(),
            "index-level annotations must not survive"
        );
        assert!(
            rebuilt.artifact_type.is_none(),
            "index-level artifactType must not survive"
        );
        assert_eq!(rebuilt.schema_version, oci::INDEX_SCHEMA_VERSION);
    }

    /// A registry that answers and declines the index earns exit 84; a
    /// credential problem keeps exit 77, and a transient fault keeps its own.
    #[test]
    fn only_a_registry_refusal_of_the_index_becomes_referrers_unsupported() {
        use crate::cli::ClassifyExitCode;
        let target = image();

        let refused = fallback_write_refused(ClientError::Registry(Box::new(server_error(400))), &target);
        assert!(matches!(refused, ClientError::ReferrersUnsupported { .. }));
        assert_eq!(refused.classify(), Some(crate::cli::ExitCode::ReferrersUnsupported));

        // `registry_error`'s catch-all folds a plain 500 into `Registry` — the
        // same variant a 400 arrives in. Reporting it as 84 would tell a script
        // the endpoint is not served and retrying is pointless, about the one
        // failure where retrying is the answer.
        let fault = fallback_write_refused(ClientError::Registry(Box::new(server_error(500))), &target);
        assert!(
            matches!(fault, ClientError::Registry(_)),
            "a server fault is not a capability verdict"
        );
        assert_eq!(fault.classify(), Some(crate::cli::ExitCode::Unavailable));

        // A 404 on a PUT is a repository problem, not a verdict on the document.
        let missing = fallback_write_refused(ClientError::Registry(Box::new(server_error(404))), &target);
        assert!(matches!(missing, ClientError::Registry(_)));

        // Nothing that is not a recognisable registry answer earns 84 either.
        let opaque = fallback_write_refused(ClientError::Registry("declined".into()), &target);
        assert!(matches!(opaque, ClientError::Registry(_)));

        // ocx building an invalid document is ocx's fault, not a capability verdict.
        let ours = fallback_write_refused(ClientError::InvalidManifest("bad".into()), &target);
        assert!(matches!(ours, ClientError::InvalidManifest(_)));

        let unauthorized = fallback_write_refused(ClientError::Authentication("bad token".into()), &target);
        assert!(
            matches!(unauthorized, ClientError::Authentication(_)),
            "a credential problem is not a missing capability"
        );
        assert_eq!(unauthorized.classify(), Some(crate::cli::ExitCode::AuthError));

        let transient = fallback_write_refused(ClientError::RegistryTransient("503".into()), &target);
        assert_eq!(transient.classify(), Some(crate::cli::ExitCode::TempFail));
    }

    /// Spec write step 5: `artifactType` falls back to the config descriptor's
    /// `mediaType` when the pushed manifest declares none, and every annotation
    /// is copied.
    #[test]
    fn the_referrer_descriptor_takes_its_facets_from_the_pushed_manifest() {
        let declared = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","artifactType":"application/vnd.dev.sigstore.bundle.v0.3+json","config":{"mediaType":"application/vnd.oci.empty.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[],"annotations":{"dev.sigstore.bundle.content":"dsse-envelope"}}"#;
        let (artifact_type, annotations) = referrer_descriptor_facets(declared);
        assert_eq!(
            artifact_type.as_deref(),
            Some("application/vnd.dev.sigstore.bundle.v0.3+json")
        );
        assert_eq!(
            annotations.as_ref().and_then(|a| a.get("dev.sigstore.bundle.content")),
            Some(&"dsse-envelope".to_string())
        );

        let undeclared = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.example.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;
        let (fallback, _) = referrer_descriptor_facets(undeclared);
        assert_eq!(
            fallback.as_deref(),
            Some("application/vnd.example.config.v1+json"),
            "spec step 5: an absent artifactType falls back to the config descriptor's mediaType"
        );

        let (none, _) = referrer_descriptor_facets(b"not a manifest");
        assert!(
            none.is_none(),
            "unparseable bytes must not fail a push that already landed"
        );
    }
}
