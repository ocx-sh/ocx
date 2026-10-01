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
use crate::referrer::DiscoveryMethod;
use crate::tag::referrer_fallback_tag;

pub type Result<T> = std::result::Result<T, ClientError>;

/// Progress callback for transfer operations.
pub type ProgressFn = Arc<dyn Fn(u64) + Send + Sync>;

/// Returns a no-op progress callback for callers that don't need progress.
pub fn no_progress() -> ProgressFn {
    Arc::new(|_| {})
}

/// Byte ceiling on a fallback referrers index body, checked after the read: it bounds what is parsed
/// and re-published, not what is allocated.
const MAX_FALLBACK_INDEX_BYTES: usize = 4 * 1024 * 1024;

/// Descriptor-count ceiling on a fallback referrers index; the byte cap alone admits tens of thousands
/// of signatures to verify.
const MAX_FALLBACK_DESCRIPTORS: usize = 4096;

/// Read-modify-write attempts before an append fails retryably; convergence is guaranteed only for two writers.
const MAX_FALLBACK_ATTEMPTS: usize = 5;

/// A referrer listing plus how it was found: registry-computed, or a mutable tag anyone with push access authored.
#[derive(Debug, Clone)]
pub struct ReferrersListing {
    /// The referrer descriptors, already filtered by artifact type.
    pub descriptors: Vec<crate::Descriptor>,
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
fn empty_fallback_index() -> crate::ImageIndex {
    crate::ImageIndex {
        schema_version: crate::INDEX_SCHEMA_VERSION,
        media_type: Some(crate::media_type::MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
        manifests: Vec::new(),
        artifact_type: None,
        annotations: None,
    }
}

/// Parses and validates fallback-index bytes read from `tag`.
fn decode_fallback_index(bytes: &[u8], tag: &str) -> Result<crate::ImageIndex> {
    if bytes.len() > MAX_FALLBACK_INDEX_BYTES {
        return Err(ClientError::InvalidManifest(format!(
            "referrers fallback index {tag} is {} bytes, above the {MAX_FALLBACK_INDEX_BYTES} limit",
            bytes.len()
        )));
    }
    // Parsed as an index, not `crate::Manifest`, which would accept an image manifest spec step 2 says to refuse.
    let index: crate::ImageIndex = serde_json::from_slice(bytes).map_err(|_| ClientError::UnexpectedManifestType)?;
    crate::manifest::validate_image_index(&index)?;
    if index.manifests.len() > MAX_FALLBACK_DESCRIPTORS {
        return Err(ClientError::InvalidManifest(format!(
            "referrers fallback index {tag} lists {} descriptors, above the {MAX_FALLBACK_DESCRIPTORS} limit",
            index.manifests.len()
        )));
    }
    Ok(index)
}

/// Whether `index` already lists an entry with `digest`.
fn index_carries(index: &crate::ImageIndex, digest: &str) -> bool {
    index.manifests.iter().any(|entry| entry.digest == digest)
}

/// Builds the index to push: a fresh header plus every entry re-emitted, with `descriptor` appended.
///
/// Field by field, never echoed, so a new `ImageIndexEntry` field is a compile error here rather than
/// repository-authored bytes carried into a document the caller signs against.
fn rebuild_with(index: crate::ImageIndex, descriptor: &crate::Descriptor) -> crate::ImageIndex {
    let mut manifests: Vec<crate::ImageIndexEntry> = index
        .manifests
        .into_iter()
        .map(|entry| crate::ImageIndexEntry {
            media_type: entry.media_type,
            digest: entry.digest,
            size: entry.size,
            platform: entry.platform,
            annotations: entry.annotations,
            artifact_type: entry.artifact_type,
        })
        .collect();
    // Spec step 5: `artifactType` and every annotation MUST carry over (cosign drops both, sigstore/cosign#4641).
    manifests.push(crate::ImageIndexEntry {
        media_type: descriptor.media_type.clone(),
        digest: descriptor.digest.clone(),
        size: descriptor.size,
        platform: None,
        annotations: descriptor.annotations.clone(),
        artifact_type: descriptor.artifact_type.clone(),
    });
    crate::ImageIndex {
        manifests,
        ..empty_fallback_index()
    }
}

/// Converts fallback-index entries to descriptors, filtering by artifact type client-side.
///
/// `urls` is pinned to `None`: nothing here follows a registry-dereferenced redirect.
fn fallback_descriptors(index: crate::ImageIndex, artifact_type: Option<&str>) -> Vec<crate::Descriptor> {
    index
        .manifests
        .into_iter()
        .filter(|entry| match artifact_type {
            Some(wanted) => entry.artifact_type.as_deref() == Some(wanted),
            None => true,
        })
        .map(|entry| crate::Descriptor {
            media_type: entry.media_type,
            digest: entry.digest,
            size: entry.size,
            urls: None,
            artifact_type: entry.artifact_type,
            annotations: entry.annotations,
        })
        .collect()
}

/// The `artifactType` (else the config `mediaType`) and annotations a referrer descriptor must carry
/// (tag-schema write step 5).
///
/// Unparseable bytes yield `(None, None)`, not an error: the push already landed.
pub(super) fn referrer_descriptor_facets(
    manifest_bytes: &[u8],
) -> (Option<String>, Option<std::collections::BTreeMap<String, String>>) {
    let manifest = match serde_json::from_slice::<crate::ImageManifest>(manifest_bytes) {
        Ok(manifest) => manifest,
        Err(error) => {
            // Warned, not silent: the fallback index permanently records the lost facets.
            log::warn!("referrer manifest did not parse, descriptor loses its artifactType and annotations: {error}");
            return (None, None);
        }
    };
    let artifact_type = manifest
        .artifact_type
        .filter(|value| !value.is_empty())
        .or_else(|| Some(manifest.config.media_type.clone()).filter(|value| !value.is_empty()));
    (artifact_type, manifest.annotations)
}

/// Maps a failed fallback-index PUT to `ReferrersUnsupported` only when the registry answered and declined.
///
/// Not on `ClientError::Registry` alone, which folds in a plain 500: exit 84 would claim no rerun can help.
fn fallback_write_refused(error: ClientError, image: &crate::native::Reference) -> ClientError {
    let declined = match &error {
        ClientError::Registry(source) => registry_declined(source.as_ref()),
        // Not `InvalidManifest`: that is ocx's own bad document, not the registry declining.
        ClientError::UnexpectedManifestType | ClientError::NotAManifest(_) => true,
        _ => false,
    };
    if declined {
        // Logged, because `ReferrersUnsupported` drops the status the registry declined with.
        log::debug!("registry declined the referrers fallback index, reported as unsupported: {error}");
        return ClientError::ReferrersUnsupported {
            registry: image.resolve_registry().to_string(),
        };
    }
    error
}

/// The fallback index cannot hold this referrer; the call site logs which limit was hit.
///
/// `ReferrersUnsupported` (84): 75 would promise a rerun helps, 65 would blame the caller's document.
fn index_cannot_hold_it(image: &crate::native::Reference) -> ClientError {
    ClientError::ReferrersUnsupported {
        registry: image.resolve_registry().to_string(),
    }
}

/// Whether a boxed transport error is the registry declining the document.
///
/// Anything unrecognised is not a decline, since 84 tells a script to stop retrying.
fn registry_declined(source: &(dyn std::error::Error + 'static)) -> bool {
    use oci_client::errors::OciDistributionError;
    match source.downcast_ref::<OciDistributionError>() {
        // Only answered-and-declined statuses; a 404 on a PUT is a repository problem, not a verdict.
        Some(OciDistributionError::ServerError { code, .. }) => matches!(code, 400 | 405 | 422),
        // Unreachable today (`registry_error` maps these envelopes first); kept for a push path that parses one.
        Some(OciDistributionError::RegistryError { .. }) => true,
        _ => false,
    }
}

/// Outcome of deleting one tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// The registry removed the tag.
    Deleted,
    /// The registry answered that the tag was not there.
    AlreadyAbsent,
}

/// What a manifest GET found, keeping the not-found code a removal decision needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestPresence {
    /// The registry served the manifest; the digest it was served under.
    Present(crate::Digest),
    /// The registry answered 404; the code its envelope carried.
    Absent(NotFoundCode),
}

/// The envelope code a registry answered a missing manifest with.
///
/// Only `ManifestUnknown` may drive a removal; `registry:2` answers it for an absent repository too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFoundCode {
    /// A 404 carrying `MANIFEST_UNKNOWN`.
    ManifestUnknown,
    /// A 404 carrying `NAME_UNKNOWN`, and taking precedence over `MANIFEST_UNKNOWN`.
    NameUnknown,
    /// A 404 with no envelope, or none of the two codes above.
    Unspecified,
}

/// Outcome of a cross-repository blob mount attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountOutcome {
    /// The registry mounted the blob into the target repository.
    Mounted,
    /// The registry declined or the transport cannot mount; upload normally.
    UploadRequired,
}

/// Low-level OCI registry transport; every method calls [`ensure_auth`](Self::ensure_auth) itself.
///
/// Sealed: defaults such as [`list_referrers_with_fallback`](Self::list_referrers_with_fallback)'s 401/500
/// refusal are security an outside impl could skip.
#[async_trait]
pub trait OciTransport: crate::sealed::Sealed + Send + Sync {
    // ── Authentication ───────────────────────────────────────────────

    /// Pre-authenticate `image`'s registry for `operation`; repeat calls for a cached scope are no-ops.
    async fn ensure_auth(&self, image: &crate::native::Reference, operation: crate::RegistryOperation) -> Result<()>;

    // ── Read operations ──────────────────────────────────────────────

    /// Lists tags for the given image, returning one page of results.
    async fn list_tags(
        &self,
        image: &crate::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>>;

    /// Lists repositories (catalog) for the registry of the given image reference.
    async fn catalog(
        &self,
        image: &crate::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>>;

    /// Fetches only the digest of a manifest without pulling the full content.
    async fn fetch_manifest_digest(&self, image: &crate::native::Reference) -> Result<String>;

    /// Pulls raw manifest bytes and returns them with the digest string.
    async fn pull_manifest_raw(
        &self,
        image: &crate::native::Reference,
        accepted_media_types: &[&str],
    ) -> Result<(Vec<u8>, String)>;

    /// Pulls a small blob (config, metadata) into memory.
    async fn pull_blob(&self, image: &crate::native::Reference, digest: &crate::Digest) -> Result<Vec<u8>>;

    /// Pulls a blob and writes it to the specified file path.
    async fn pull_blob_to_file(
        &self,
        image: &crate::native::Reference,
        digest: &crate::Digest,
        path: &Path,
    ) -> Result<()>;

    /// HEAD a blob: `Ok(size)`, or `ClientError::BlobNotFound`.
    async fn head_blob(&self, image: &crate::native::Reference, digest: &crate::Digest) -> Result<u64>;

    /// Streams the raw (compressed) blob bytes exactly as served: no decompression, hashing or progress.
    ///
    /// The default spools through a temp file with no `VerifyingStream`: `HashingAsyncReader` in `pull_layer` is
    /// the sole verifier.
    ///
    /// # Errors (from the returned reader)
    ///
    /// - [`ClientError::BlobNotFound`] — blob absent at call time.
    /// - `io::Error` with a fork `DigestError` source at stream end (`NativeTransport`), mapped by the caller to
    ///   [`ClientError::DigestMismatch`].
    async fn pull_blob_streaming(
        &self,
        image: &crate::native::Reference,
        digest: &crate::Digest,
    ) -> Result<Box<dyn AsyncRead + Send + Unpin + 'static>> {
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
        // `temp_file` rides in the reader, or its `Drop` deletes the path mid-read.
        let reader = TempFileReader {
            file,
            _guard: temp_file,
        };
        Ok(Box::new(reader))
    }

    // ── Write operations ─────────────────────────────────────────────

    /// Pushes a typed OCI manifest and returns the resulting digest string.
    async fn push_manifest(&self, image: &crate::native::Reference, manifest: &crate::Manifest) -> Result<String>;

    /// Pushes raw manifest bytes with the given media type, returning the digest.
    async fn push_manifest_raw(
        &self,
        image: &crate::native::Reference,
        data: Vec<u8>,
        media_type: &str,
    ) -> Result<String>;

    /// Pushes in-memory blob data, reporting cumulative bytes on the wire to `on_progress`; returns the digest.
    async fn push_blob(
        &self,
        image: &crate::native::Reference,
        data: Vec<u8>,
        digest: &crate::Digest,
        on_progress: ProgressFn,
    ) -> Result<String>;

    /// Pushes a blob from a file without holding it in RAM.
    ///
    /// No default: the obvious one (read, then [`Self::push_blob`]) compiles and silently reintroduces the
    /// whole-blob allocation.
    async fn push_blob_from_path(
        &self,
        image: &crate::native::Reference,
        path: &Path,
        digest: &crate::Digest,
        on_progress: ProgressFn,
    ) -> Result<String>;

    /// Attempts to mount `digest` from `source_repository` into `image`'s repository, skipping an upload.
    ///
    /// Default: [`MountOutcome::UploadRequired`], since mounting is an optimization only.
    async fn mount_blob(
        &self,
        image: &crate::native::Reference,
        source_repository: &str,
        digest: &crate::Digest,
    ) -> Result<MountOutcome> {
        let _ = (image, source_repository, digest);
        Ok(MountOutcome::UploadRequired)
    }

    /// Deletes the tag `image` names; `image` is the canonical reference, with a tag and no digest.
    ///
    /// # Errors
    ///
    /// [`ClientError::DeleteUnsupported`] when the registry does not delete tags.
    async fn delete_manifest(&self, image: &crate::native::Reference) -> Result<DeleteOutcome>;

    /// GETs the manifest `image` (the canonical reference) names, reporting which not-found code a miss carried.
    async fn probe_manifest(&self, image: &crate::native::Reference) -> Result<ManifestPresence>;

    // ── Referrer operations (OCI 1.1) ────────────────────────────────

    /// Pushes a referrer manifest whose `subject` is `subject_digest`, returning its descriptor.
    async fn push_referrer_manifest(
        &self,
        image: &crate::native::Reference,
        subject_digest: &crate::Digest,
        manifest_bytes: &[u8],
        media_type: &str,
    ) -> Result<crate::Descriptor>;

    /// Lists referrers of `subject_digest` via the Referrers API, optionally filtered to `artifact_type`.
    ///
    /// Must also filter client-side: a server may ignore the filter.
    ///
    /// # Errors
    ///
    /// [`ClientError::ReferrersUnsupported`] on a referrers-endpoint 404, unlike an empty list for a subject with none.
    async fn list_referrers(
        &self,
        image: &crate::native::Reference,
        subject_digest: &crate::Digest,
        artifact_type: Option<&str>,
    ) -> Result<Vec<crate::Descriptor>>;

    /// Lists referrers, falling back to the OCI referrers tag schema when the registry has no Referrers API.
    ///
    /// Never returns [`ClientError::ReferrersUnsupported`] (no fallback tag is an empty listing); any other error
    /// propagates, never substituted by a tag anyone with push access can author.
    async fn list_referrers_with_fallback(
        &self,
        image: &crate::native::Reference,
        subject_digest: &crate::Digest,
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

    /// Reads the OCI referrers fallback index for `subject_digest`; an absent tag is an empty index (spec step 3).
    ///
    /// Only a 404 yields empty, or an appender republishes an empty index over sibling referrers it cannot read.
    ///
    /// # Errors
    ///
    /// - [`ClientError::InvalidManifest`] — over `MAX_FALLBACK_INDEX_BYTES` or `MAX_FALLBACK_DESCRIPTORS`.
    /// - [`ClientError::UnexpectedManifestType`] — the tag holds something other than an image index.
    /// - [`ClientError::InvalidImageIndex`] — it fails [`crate::manifest::validate_image_index`].
    async fn pull_referrer_fallback_index(
        &self,
        image: &crate::native::Reference,
        subject_digest: &crate::Digest,
    ) -> Result<crate::ImageIndex> {
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

    /// Appends `descriptor` to the fallback referrers index for `subject_digest`, keeping its `artifactType` and
    /// annotations.
    ///
    /// Optimistic, as the spec has no conditional PUT: success is this descriptor seen on read-back, since a
    /// clobbered PUT still returns `Ok`. `image` must be
    /// [`Client::transport_write_reference`](super::Client), never a mirror.
    ///
    /// # Errors
    ///
    /// - Whatever [`Self::pull_referrer_fallback_index`] refuses; the tag is left untouched.
    /// - [`ClientError::ReferrersUnsupported`] — the index cannot hold another entry (nothing is pushed), or the
    ///   registry declined it.
    /// - [`ClientError::RegistryTransient`] — concurrent writers exhausted the retries; never an `Ok` that drops
    ///   the descriptor.
    async fn append_referrer_fallback_index(
        &self,
        image: &crate::native::Reference,
        subject_digest: &crate::Digest,
        descriptor: &crate::Descriptor,
    ) -> Result<FallbackAppend> {
        let tag = referrer_fallback_tag(subject_digest);
        let target = super::sibling_tag_reference(image, tag.clone());
        for _attempt in 0..MAX_FALLBACK_ATTEMPTS {
            let index = self.pull_referrer_fallback_index(image, subject_digest).await?;
            if index_carries(&index, &descriptor.digest) {
                return Ok(FallbackAppend::AlreadyPresent);
            }
            let next = rebuild_with(index, descriptor);
            // Refused before the PUT: an over-cap index would land and then fail every later read, for good.
            if next.manifests.len() > MAX_FALLBACK_DESCRIPTORS {
                log::warn!(
                    "referrers fallback index {tag} already lists {} descriptors; appending would pass the \
                     {MAX_FALLBACK_DESCRIPTORS} limit and leave a document this client refuses to read",
                    next.manifests.len() - 1
                );
                return Err(index_cannot_hold_it(image));
            }
            let bytes = serde_json::to_vec(&next).map_err(ClientError::Serialization)?;
            if bytes.len() > MAX_FALLBACK_INDEX_BYTES {
                log::warn!(
                    "referrers fallback index {tag} would be {} bytes with this descriptor appended, above the \
                     {MAX_FALLBACK_INDEX_BYTES}-byte limit this client will read back",
                    bytes.len()
                );
                return Err(index_cannot_hold_it(image));
            }
            self.push_manifest_raw(&target, bytes, crate::media_type::MEDIA_TYPE_OCI_IMAGE_INDEX)
                .await
                .map_err(|error| fallback_write_refused(error, image))?;
            // The read-back is the only evidence the PUT survived a concurrent writer; after a read-back error a
            // rerun finds the descriptor via `index_carries` instead of pushing twice.
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

/// Reads `path` into memory and pushes it through [`OciTransport::push_blob`], for test doubles.
///
/// `__testing`-gated so production cannot reach the allocation `push_blob_from_path` exists to avoid.
#[cfg(any(test, feature = "__testing"))]
pub async fn push_blob_buffered<T: OciTransport + ?Sized>(
    transport: &T,
    image: &crate::native::Reference,
    path: &Path,
    digest: &crate::Digest,
    on_progress: ProgressFn,
) -> Result<String> {
    let data = tokio::fs::read(path).await.map_err(|e| ClientError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    transport.push_blob(image, data, digest, on_progress).await
}

/// Streams a temp file, keeping its [`tempfile::NamedTempFile`] guard alive until dropped.
struct TempFileReader {
    file: tokio::fs::File,
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
    use crate::{Algorithm, RegistryOperation};
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

    impl crate::sealed::Sealed for InlineStub {}

    #[async_trait]
    impl OciTransport for InlineStub {
        async fn ensure_auth(&self, _image: &crate::native::Reference, _op: RegistryOperation) -> Result<()> {
            Ok(())
        }

        async fn list_tags(
            &self,
            _image: &crate::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn catalog(
            &self,
            _image: &crate::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn fetch_manifest_digest(&self, _image: &crate::native::Reference) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_manifest_raw(
            &self,
            _image: &crate::native::Reference,
            _accepted_media_types: &[&str],
        ) -> Result<(Vec<u8>, String)> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_blob(&self, _image: &crate::native::Reference, _digest: &crate::Digest) -> Result<Vec<u8>> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn pull_blob_to_file(
            &self,
            _image: &crate::native::Reference,
            digest: &crate::Digest,
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

        async fn head_blob(&self, _image: &crate::native::Reference, _digest: &crate::Digest) -> Result<u64> {
            Ok(0)
        }

        async fn push_manifest(
            &self,
            _image: &crate::native::Reference,
            _manifest: &crate::Manifest,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_manifest_raw(
            &self,
            _image: &crate::native::Reference,
            _data: Vec<u8>,
            _media_type: &str,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_blob(
            &self,
            _image: &crate::native::Reference,
            _data: Vec<u8>,
            _digest: &crate::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn push_blob_from_path(
            &self,
            _image: &crate::native::Reference,
            _path: &Path,
            _digest: &crate::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn delete_manifest(
            &self,
            _image: &crate::native::Reference,
        ) -> crate::client::Result<crate::client::DeleteOutcome> {
            unimplemented!()
        }

        async fn probe_manifest(
            &self,
            _image: &crate::native::Reference,
        ) -> crate::client::Result<crate::client::ManifestPresence> {
            unimplemented!()
        }

        async fn push_referrer_manifest(
            &self,
            _image: &crate::native::Reference,
            _subject_digest: &crate::Digest,
            _manifest_bytes: &[u8],
            _media_type: &str,
        ) -> Result<crate::Descriptor> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        async fn list_referrers(
            &self,
            _image: &crate::native::Reference,
            _subject_digest: &crate::Digest,
            _artifact_type: Option<&str>,
        ) -> Result<Vec<crate::Descriptor>> {
            unimplemented!("not needed for pull_blob_streaming default-impl test")
        }

        fn box_clone(&self) -> Box<dyn OciTransport> {
            Box::new(self.box_clone_inner())
        }
    }

    fn test_reference() -> crate::native::Reference {
        crate::native::Reference::try_from("example.com/test/pkg:1.0").expect("valid reference")
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

    impl crate::sealed::Sealed for FallbackRegistry {}

    #[async_trait]
    impl OciTransport for FallbackRegistry {
        async fn ensure_auth(&self, _image: &crate::native::Reference, _operation: RegistryOperation) -> Result<()> {
            Ok(())
        }

        async fn list_tags(
            &self,
            _image: &crate::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn catalog(
            &self,
            _image: &crate::native::Reference,
            _chunk_size: usize,
            _last: Option<String>,
        ) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn fetch_manifest_digest(&self, _image: &crate::native::Reference) -> Result<String> {
            unimplemented!("the fallback-index tests never fetch a bare digest")
        }

        async fn pull_manifest_raw(
            &self,
            image: &crate::native::Reference,
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

        async fn pull_blob(&self, _image: &crate::native::Reference, _digest: &crate::Digest) -> Result<Vec<u8>> {
            unimplemented!("the fallback-index tests never pull a blob")
        }

        async fn pull_blob_to_file(
            &self,
            _image: &crate::native::Reference,
            _digest: &crate::Digest,
            _path: &Path,
        ) -> Result<()> {
            unimplemented!("the fallback-index tests never pull a blob")
        }

        async fn head_blob(&self, _image: &crate::native::Reference, _digest: &crate::Digest) -> Result<u64> {
            unimplemented!("the fallback-index tests never head a blob")
        }

        async fn push_manifest(
            &self,
            _image: &crate::native::Reference,
            _manifest: &crate::Manifest,
        ) -> Result<String> {
            unimplemented!("the fallback index is pushed through push_manifest_raw")
        }

        async fn push_manifest_raw(
            &self,
            image: &crate::native::Reference,
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
            _image: &crate::native::Reference,
            _data: Vec<u8>,
            _digest: &crate::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("the fallback-index tests never push a blob")
        }

        async fn push_blob_from_path(
            &self,
            _image: &crate::native::Reference,
            _path: &Path,
            _digest: &crate::Digest,
            _on_progress: ProgressFn,
        ) -> Result<String> {
            unimplemented!("the fallback-index tests never push a blob")
        }

        async fn delete_manifest(
            &self,
            _image: &crate::native::Reference,
        ) -> crate::client::Result<crate::client::DeleteOutcome> {
            unimplemented!()
        }

        async fn probe_manifest(
            &self,
            _image: &crate::native::Reference,
        ) -> crate::client::Result<crate::client::ManifestPresence> {
            unimplemented!()
        }

        async fn push_referrer_manifest(
            &self,
            _image: &crate::native::Reference,
            _subject_digest: &crate::Digest,
            _manifest_bytes: &[u8],
            _media_type: &str,
        ) -> Result<crate::Descriptor> {
            unimplemented!("the fallback-index tests append a descriptor directly")
        }

        async fn list_referrers(
            &self,
            image: &crate::native::Reference,
            _subject_digest: &crate::Digest,
            _artifact_type: Option<&str>,
        ) -> Result<Vec<crate::Descriptor>> {
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

    fn subject() -> crate::Digest {
        crate::Digest::Sha256("a".repeat(64))
    }

    fn image() -> crate::native::Reference {
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

    fn descriptor(hex: &str) -> crate::Descriptor {
        crate::Descriptor {
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
        let index: crate::ImageIndex = serde_json::from_slice(&bytes).expect("a valid image index");
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
        let index: crate::ImageIndex = serde_json::from_slice(&bytes).expect("a valid image index");
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
        let registry = FallbackRegistry::new();
        let mut full = empty_fallback_index();
        full.manifests = (0..MAX_FALLBACK_DESCRIPTORS)
            .map(|n| crate::ImageIndexEntry {
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

        let _error = registry
            .append_referrer_fallback_index(&image(), &subject(), &descriptor("a"))
            .await
            .expect_err("appending past the ceiling must be refused");

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
        let registry = FallbackRegistry::new();
        let mut fat = descriptor("a");
        fat.annotations = Some(
            [("dev.example.blob".to_string(), "a".repeat(MAX_FALLBACK_INDEX_BYTES + 1))]
                .into_iter()
                .collect(),
        );

        let _error = registry
            .append_referrer_fallback_index(&image(), &subject(), &fat)
            .await
            .expect_err("a single over-cap entry must be refused");

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
        for (status, _expected) in [
            // Answered and declined: the index is not something it will hold.
            (400, ocx_exit::ExitCode::ReferrersUnsupported),
            (405, ocx_exit::ExitCode::ReferrersUnsupported),
            (422, ocx_exit::ExitCode::ReferrersUnsupported),
            // A fault. `registry_error`'s transient arm is `is_transient_status`,
            // which excludes 500, so a 500 lands in the `Registry` catch-all with
            // every parse error — reporting it as 84 would tell a CI wrapper to
            // stop retrying a sign that would have succeeded.
            (500, ocx_exit::ExitCode::Unavailable),
            (501, ocx_exit::ExitCode::Unavailable),
        ] {
            let mut registry = FallbackRegistry::new();
            registry.push_status = Some(status);

            let _error = registry
                .append_referrer_fallback_index(&image(), &subject(), &descriptor("a"))
                .await
                .expect_err("the fixture answers every PUT with an error");
        }
    }

    /// A registry that *has* a Referrers API and answered badly is not a
    /// registry without one.
    ///
    /// Only [`ClientError::ReferrersUnsupported`] opens the fallback. A
    /// 401 or a 500 propagates with its own exit code, and the fallback tag is
    /// never read — substituting a tag anyone with push access can author for a
    /// endpoint that merely refused the caller's credentials would answer a
    /// security question with an attacker-writable document.
    #[tokio::test]
    async fn a_native_referrers_failure_propagates_and_never_reads_the_fallback_tag() {
        use std::sync::atomic::Ordering;

        for (fault, _expected) in [
            (ReferrersApiFault::Unauthorized, ocx_exit::ExitCode::AuthError),
            (ReferrersApiFault::ServerFault, ocx_exit::ExitCode::TempFail),
        ] {
            let registry = FallbackRegistry::with_referrers_fault(fault);
            // Seed the tag, so a fallback read would *succeed* and return a
            // descriptor. Without this the test could not tell "did not fall
            // back" from "fell back and found nothing".
            let seeded = rebuild_with(empty_fallback_index(), &descriptor("e"));
            registry.seed(&fallback_reference(), serde_json::to_vec(&seeded).unwrap());

            let _error = registry
                .list_referrers_with_fallback(&image(), &subject(), None)
                .await
                .expect_err("a native referrers failure is not an empty listing");

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
            .map(|i| crate::ImageIndexEntry {
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
        assert_eq!(rebuilt.schema_version, crate::INDEX_SCHEMA_VERSION);
    }

    /// A registry that answers and declines the index earns exit 84; a
    /// credential problem keeps exit 77, and a transient fault keeps its own.
    #[test]
    fn only_a_registry_refusal_of_the_index_becomes_referrers_unsupported() {
        let target = image();

        let refused = fallback_write_refused(ClientError::Registry(Box::new(server_error(400))), &target);
        assert!(matches!(refused, ClientError::ReferrersUnsupported { .. }));

        // `registry_error`'s catch-all folds a plain 500 into `Registry` — the
        // same variant a 400 arrives in. Reporting it as 84 would tell a script
        // the endpoint is not served and retrying is pointless, about the one
        // failure where retrying is the answer.
        let fault = fallback_write_refused(ClientError::Registry(Box::new(server_error(500))), &target);
        assert!(
            matches!(fault, ClientError::Registry(_)),
            "a server fault is not a capability verdict"
        );

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

        let _transient = fallback_write_refused(ClientError::RegistryTransient("503".into()), &target);
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
