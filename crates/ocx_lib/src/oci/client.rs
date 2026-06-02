// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    ACCEPTED_MANIFEST_MEDIA_TYPES, MEDIA_TYPE_DESCRIPTION_V1, MEDIA_TYPE_MARKDOWN, MEDIA_TYPE_OCI_EMPTY_CONFIG,
    MEDIA_TYPE_OCI_IMAGE_INDEX, MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_PACKAGE_V1, MEDIA_TYPE_PNG, MEDIA_TYPE_SVG,
    Result, archive, compression, log, media_type_file_ext, media_type_from_path, oci,
    package::{self, info::Info, metadata, tag::InternalTag},
    utility,
};

use futures::stream::{self, StreamExt, TryStreamExt};

use super::{Algorithm, Digest, Identifier, native};

/// Maximum number of layer push/verify operations to run concurrently.
///
/// Each `LayerRef::File` reads the full archive into memory before
/// uploading, so unbounded fan-out would OOM on multi-GB layers.
const LAYER_PUSH_CONCURRENCY: usize = 4;

mod builder;
pub mod error;
mod mirror_map;
pub(crate) mod native_transport;
mod progress_writer;
#[cfg(test)]
pub(crate) mod test_transport;
mod transport;

pub use builder::ClientBuilder;
pub use mirror_map::MirrorMap;
pub use transport::OciTransport;

use error::ClientError;

/// Verifies that a blob on disk hashes to its claimed digest.
///
/// Streams the file through the algorithm named by `expected` (SHA-256,
/// SHA-384, or SHA-512 — whichever variant the manifest descriptor
/// declares) and compares against it. On mismatch, removes the blob
/// and returns [`ClientError::DigestMismatch`]. This defends against
/// a compromised or misbehaving registry serving different bytes for
/// the same digest (CWE-345).
///
/// This is the **second line of defense** against bad registry
/// responses. The first line is the archive walker in
/// `utility::fs::assemble`, which validates the on-disk structure
/// (entry count caps, depth caps, symlink containment, overlap
/// detection) during extraction. The walker catches malformed or
/// malicious *archive contents*; this function catches the narrower
/// case of a registry serving different bytes for the same digest —
/// i.e. a digest/bytes mismatch that the walker cannot see because
/// it operates after extraction.
async fn verify_blob_digest(blob_path: &std::path::Path, expected: &Digest) -> std::result::Result<(), ClientError> {
    let actual = expected
        .algorithm()
        .hash_file(blob_path)
        .await
        .map_err(|e| ClientError::Io {
            path: blob_path.to_path_buf(),
            source: e,
        })?;
    if &actual == expected {
        return Ok(());
    }
    // Best-effort cleanup of the tampered blob so a subsequent pull
    // retries from the registry instead of re-reading the bad bytes.
    // The primary error reported to the caller is the digest
    // mismatch; a failure to unlink is logged for diagnostics but
    // must not mask it.
    if let Err(e) = tokio::fs::remove_file(blob_path).await {
        log::debug!(
            "failed to remove tampered blob at {} after digest mismatch: {}",
            blob_path.display(),
            e
        );
    }
    Err(ClientError::DigestMismatch {
        expected: expected.to_string(),
        actual: actual.to_string(),
    })
}

pub struct Client {
    transport: Box<dyn OciTransport>,
    pub(super) lock_timeout: std::time::Duration,
    pub(super) tag_chunk_size: usize,
    pub(super) repository_chunk_size: usize,
    /// Shared progress manager for download/upload bars. Cheap to clone
    /// (an `Arc` handle or a disabled no-op).
    progress: crate::cli::progress::ProgressManager,
    /// Per-upstream-host mirror map. Applied on the read path only, via
    /// [`Client::transport_reference`] / [`Client::transport_registry`].
    /// Empty = identity (no host mirrored). Cheap to clone.
    pub(super) mirrors: MirrorMap,
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.box_clone(),
            lock_timeout: self.lock_timeout,
            tag_chunk_size: self.tag_chunk_size,
            repository_chunk_size: self.repository_chunk_size,
            progress: self.progress.clone(),
            mirrors: self.mirrors.clone(),
        }
    }
}

impl Client {
    pub fn lock_timeout(&self) -> std::time::Duration {
        self.lock_timeout
    }

    #[cfg(test)]
    pub(crate) fn with_transport(transport: Box<dyn OciTransport>) -> Self {
        Client {
            transport,
            lock_timeout: std::time::Duration::from_secs(5),
            tag_chunk_size: 100,
            repository_chunk_size: 100,
            progress: crate::cli::progress::ProgressManager::disabled(),
            mirrors: MirrorMap::default(),
        }
    }

    // ── Mirror transform (single read-path rewrite seam) ───────────

    /// Builds the transport reference for a **read-path** operation, applying
    /// the mirror map.
    ///
    /// When `self.mirrors` has an entry for `identifier.registry()`, the
    /// returned reference targets the mirror host with the repository rewritten
    /// to `<path-prefix>/<repository>` (tag and digest copied verbatim). When
    /// no mirror is configured, the result is identical to the canonical
    /// reference. The returned reference is transport-only and is never
    /// converted back into an [`Identifier`] for storage.
    ///
    /// This is one of the two read seams — every read site builds references
    /// through here (or [`transport_registry`](Self::transport_registry)). There
    /// is no PUBLIC bypass: the `From<&Identifier> for native::Reference` impl is
    /// removed, so no read site can reach for a canonical conversion without
    /// naming an in-crate seam. In-crate read paths must still route through
    /// these seams rather than the `pub(crate)`
    /// [`Identifier::canonical_reference`] (which stays callable in-crate) — that
    /// discipline is enforced by the structural test plus the behavioural
    /// backstop, not by the compiler.
    fn transport_reference(&self, identifier: &Identifier) -> native::Reference {
        let Some((host, repository)) = self
            .mirrors
            .rewrite_repository(identifier.registry(), identifier.repository())
        else {
            // No mirror for this host: identical to the canonical reference.
            return identifier.canonical_reference();
        };
        // Tag and digest are copied verbatim from the canonical identifier; only
        // the host and repository are rewritten. The returned reference is
        // transport-only and never round-trips into storage.
        match (identifier.tag(), identifier.digest()) {
            (Some(tag), Some(digest)) => {
                native::Reference::with_tag_and_digest(host, repository, tag.to_string(), digest.to_string())
            }
            (Some(tag), None) => native::Reference::with_tag(host, repository, tag.to_string()),
            (None, Some(digest)) => native::Reference::with_digest(host, repository, digest.to_string()),
            (None, None) => native::Reference::with_tag(host, repository, "latest".into()),
        }
    }

    /// Builds the transport reference for a registry-scoped read operation
    /// (the catalog `list_repositories` call), applying the mirror map to the
    /// registry host.
    ///
    /// Sibling of [`transport_reference`](Self::transport_reference) for the
    /// case where there is no full identifier — only a registry string and a
    /// placeholder repository.
    fn transport_registry(&self, registry: &str) -> native::Reference {
        // The catalog **URL** is built from `registry()` alone (`/v2/_catalog`),
        // so the repository never reaches the path. The catalog **auth scope**,
        // however, is `repository:<repository>:pull` (oci-client `_auth`), so the
        // repository value still has to be well-formed. An empty repository (no
        // mirror) keeps the host verbatim and the repository empty; when a mirror
        // exists, the host is rewritten and the placeholder repository becomes the
        // mirror's path prefix verbatim — `rewrite_repository` returns the prefix
        // with no trailing slash for the empty-repository case, so the auth scope
        // is `repository:<prefix>:pull`, not the malformed `repository:<prefix>/:pull`.
        let (host, repository) = self
            .mirrors
            .rewrite_repository(registry, "")
            .unwrap_or_else(|| (registry.to_string(), String::new()));
        native::Reference::with_tag(host, repository, "latest".into())
    }

    // ── Authentication ─────────────────────────────────────────────

    /// Pre-authenticate against the registry for `identifier` with the
    /// given operation scope.
    ///
    /// Call at the start of a command or task to fail fast on credential
    /// issues (expired tokens, GPG agent prompts, missing env vars)
    /// before beginning any real work.
    ///
    /// `ensure_auth` is shared by the read path and the push path. A `Push`
    /// scope authenticates against the **canonical** host (remote/proxy mirrors
    /// are read-only, ADR Q5), so it builds the reference via
    /// [`Identifier::canonical_reference`]; every other scope is a read and
    /// keys auth off the mirror host via
    /// [`transport_reference`](Self::transport_reference).
    pub async fn ensure_auth(&self, identifier: &Identifier, operation: oci::RegistryOperation) -> Result<()> {
        // Exhaustive over `RegistryOperation` so a future upstream variant is a
        // compile error here, forcing an explicit routing decision rather than
        // silently inheriting the read (mirror-aware) path. `Push` authenticates
        // against the canonical host (remote/proxy mirrors are read-only, ADR Q5);
        // `Pull` is a read and routes through the mirror-aware
        // `transport_reference`. Coupled to the upstream enum in
        // `external/rust-oci-client/src/token_cache.rs`.
        let image = match operation {
            oci::RegistryOperation::Push => identifier.canonical_reference(),
            oci::RegistryOperation::Pull => self.transport_reference(identifier),
        };
        self.transport.ensure_auth(&image, operation).await?;
        Ok(())
    }

    // ── Index operations ─────────────────────────────────────────────

    /// Lists the tags for the given image reference.
    /// There is no validation that the tags correspond to valid package versions.
    pub async fn list_tags(&self, identifier: Identifier) -> Result<Vec<String>> {
        let image = self.transport_reference(&identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        let chunk_size = self.tag_chunk_size;
        let tags = paginate(chunk_size, |cs, last| self.transport.list_tags(&image, cs, last)).await?;
        log::trace!("Listed tags for {}: {:?}", identifier, tags);
        Ok(tags)
    }

    pub async fn list_repositories(&self, registry: impl Into<String>) -> Result<Vec<String>> {
        let registry = registry.into();
        let image = self.transport_registry(&registry);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        let chunk_size = self.repository_chunk_size;
        let repositories = paginate(chunk_size, |cs, last| self.transport.catalog(&image, cs, last)).await?;
        log::trace!("Listed repositories for {}: {:?}", registry, repositories);
        Ok(repositories)
    }

    /// Fetches the digest of a manifest from the remote, trying to avoid pulling the entire manifest if possible.
    pub async fn fetch_manifest_digest(&self, identifier: &Identifier) -> Result<oci::Digest> {
        let ref_ = self.transport_reference(identifier);
        self.transport.ensure_auth(&ref_, oci::RegistryOperation::Pull).await?;
        let digest = self.transport.fetch_manifest_digest(&ref_).await?;
        log::trace!("Fetched manifest digest for {}: {}", identifier, digest);
        Ok(digest.try_into()?)
    }

    /// Fetches the manifest for the given image reference, returning both the manifest and its digest.
    pub async fn fetch_manifest(&self, identifier: &Identifier) -> Result<(Digest, oci::Manifest)> {
        let ref_ = self.transport_reference(identifier);
        self.transport.ensure_auth(&ref_, oci::RegistryOperation::Pull).await?;
        let (manifest, digest_str) = self.fetch_manifest_raw(&ref_).await?;
        let digest = digest_str.try_into()?;
        Ok((digest, manifest))
    }

    // ── Platform-aware cascade merge ─────────────────────────────────

    /// Fetches (or creates) the image index at `target_tag`, removes any existing
    /// entry for `platform`, inserts the new manifest entry, and pushes the
    /// updated index.
    ///
    /// Used by `package push --cascade` to merge a single-platform manifest into
    /// each rolling tag without destroying entries for other platforms.
    ///
    /// Returns the digest and data of the pushed index.
    pub(crate) async fn merge_platform_into_index(
        &self,
        source_identifier: &Identifier,
        target_tag: impl Into<String>,
        platform: &oci::Platform,
        manifest_sha256: &str,
        manifest_size: i64,
    ) -> Result<(Digest, oci::ImageIndex)> {
        let target_identifier = source_identifier.clone_with_tag(target_tag);
        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let ref_ = target_identifier.canonical_reference();
        self.transport.ensure_auth(&ref_, oci::RegistryOperation::Push).await?;
        let platform = Some(platform.clone().into());

        log::debug!("Merging platform entry into index for {}", ref_);
        let mut index = match self
            .transport
            .pull_manifest_raw(&ref_, &[MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_OCI_IMAGE_INDEX])
            .await
        {
            Ok((blob, digest_str)) => {
                let existing: oci::Manifest = serde_json::from_slice(&blob).map_err(ClientError::Serialization)?;
                match existing {
                    oci::Manifest::Image(_) => {
                        let blob_size = i64::try_from(blob.len()).map_err(|_| {
                            ClientError::InvalidManifest(format!(
                                "existing manifest blob size {} exceeds i64::MAX",
                                blob.len()
                            ))
                        })?;
                        let entry = oci::ImageIndexEntry {
                            media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                            digest: digest_str,
                            size: blob_size,
                            platform: None,
                            annotations: None,
                        };
                        oci::ImageIndex {
                            schema_version: oci::INDEX_SCHEMA_VERSION,
                            media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                            artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                            manifests: vec![entry],
                            annotations: None,
                        }
                    }
                    oci::Manifest::ImageIndex(idx) => idx,
                }
            }
            Err(ClientError::ManifestNotFound(_)) => {
                log::debug!("No existing manifest/index for {}, starting fresh", ref_);
                oci::ImageIndex {
                    schema_version: oci::INDEX_SCHEMA_VERSION,
                    media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                    artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                    manifests: vec![],
                    annotations: None,
                }
            }
            Err(e) => return Err(e.into()),
        };

        index.manifests.retain(|entry| entry.platform != platform);
        index.manifests.push(oci::ImageIndexEntry {
            media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
            digest: manifest_sha256.to_string(),
            size: manifest_size,
            platform,
            annotations: None,
        });

        let index_data = serde_json::to_vec(&index).map_err(ClientError::Serialization)?;
        let index_digest = Algorithm::Sha256.hash(&index_data);
        self.transport
            .push_manifest_raw(&ref_, index_data, MEDIA_TYPE_OCI_IMAGE_INDEX)
            .await?;
        log::debug!("Successfully merged platform entry into index for {}", ref_);

        Ok((index_digest, index))
    }

    // ── Blob introspection ────────────────────────────────────────────

    /// HEAD a blob to verify its existence and retrieve its content length.
    ///
    /// Returns `Ok(size_bytes)` when the blob exists in the registry.
    /// Returns `Err(ClientError::BlobNotFound)` when the blob is absent.
    ///
    /// Used by `pull_local` to capture the real byte count for a
    /// `LayerRef::Digest` layer before pulling it, so the synthesized
    /// OCI descriptor has the same size as the manifest produced by
    /// `package push`.
    pub async fn head_blob(&self, identifier: &Identifier, digest: &Digest) -> Result<u64> {
        let image = self.transport_reference(identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        let size = self.transport.head_blob(&image, digest).await?;
        Ok(size)
    }

    // ── Package pull ─────────────────────────────────────────────────
    //
    // Composable methods for fetching a package from a registry:
    //
    //   pull_manifest  → ImageManifest   (validate digest, media types, layers)
    //   pull_blob      → Vec<u8>         (raw OCI blob fetch by digest)
    //   pull_layer     → extracted dir   (download one layer blob, extract, codesign)
    //
    // Higher-level metadata fetch (with local-CAS caching) lives in
    // `package_manager::tasks::common::fetch_or_get_blob`.

    /// Fetches and validates the OCI manifest for a pinned package.
    ///
    /// Verifies the manifest digest matches the identifier.
    /// Returns the [`ImageManifest`](oci::ImageManifest) without asserting media types.
    pub async fn pull_manifest(
        &self,
        identifier: &oci::PinnedIdentifier,
    ) -> std::result::Result<oci::ImageManifest, ClientError> {
        let expected_digest = identifier.digest().to_string();
        let image = self.transport_reference(identifier);

        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;

        let (manifest, digest_str) = self.fetch_manifest_raw(&image).await?;
        if digest_str != expected_digest {
            return Err(ClientError::DigestMismatch {
                expected: expected_digest,
                actual: digest_str,
            });
        }
        let manifest = match manifest {
            oci::Manifest::Image(m) => m,
            _ => return Err(ClientError::UnexpectedManifestType),
        };

        Ok(manifest)
    }

    /// Fetches a single blob from the registry.
    ///
    /// `blob_ref` carries `(registry, repo)` for the OCI blob endpoint and
    /// the blob's own digest for content addressing. Generic OCI blob fetch
    /// — no media-type validation, no parsing. Caller is responsible for
    /// content interpretation.
    pub async fn pull_blob(&self, blob_ref: &oci::PinnedIdentifier) -> std::result::Result<Vec<u8>, ClientError> {
        let image = self.transport_reference(blob_ref);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        self.transport.pull_blob(&image, &blob_ref.digest()).await
    }

    /// Downloads and extracts a single OCI layer to the specified directory.
    ///
    /// Creates `{output_dir}/content/` with the extracted files and runs
    /// code-signing on macOS. The downloaded blob archive is removed after
    /// extraction.
    ///
    /// Callers are responsible for creating `output_dir` and writing the
    /// digest marker file.
    pub async fn pull_layer(
        &self,
        identifier: &oci::PinnedIdentifier,
        layer: &oci::Descriptor,
        metadata: &metadata::Metadata,
        output_dir: &std::path::Path,
    ) -> std::result::Result<(), ClientError> {
        let blob_compression =
            compression::CompressionAlgorithm::from_media_type(&layer.media_type).ok_or_else(|| {
                ClientError::InvalidManifest(format!("unsupported layer media type: {}", layer.media_type))
            })?;
        let blob_file_ext = media_type_file_ext(&layer.media_type).unwrap_or("blob");
        let content_path = output_dir.join("content");
        let blob_path = content_path.with_added_extension(blob_file_ext);
        let blob_total_size = u64::try_from(layer.size).unwrap_or(0);

        let image = self.transport_reference(identifier);

        let layer_digest = Digest::try_from(layer.digest.as_str())
            .map_err(|e| ClientError::InvalidManifest(format!("layer digest '{}' is malformed: {e}", layer.digest)))?;

        log::info!(
            "Downloading layer {} to {}",
            layer_digest.to_short_string(),
            output_dir.display()
        );

        let bar = self
            .progress
            .bytes(format!("Downloading '{identifier}'"), blob_total_size);
        let on_progress = bar.callback();

        self.transport
            .pull_blob_to_file(&image, &layer_digest, &blob_path, blob_total_size, on_progress)
            .await?;
        drop(bar);

        verify_blob_digest(&blob_path, &layer_digest).await?;

        // Extract archive + codesign.
        self.extract_to_temp(identifier, metadata, blob_compression, blob_file_ext, output_dir)
            .await?;

        Ok(())
    }

    /// Extracts the downloaded archive within the temp directory and signs content.
    async fn extract_to_temp(
        &self,
        identifier: &oci::PinnedIdentifier,
        metadata: &metadata::Metadata,
        blob_compression: compression::CompressionAlgorithm,
        blob_file_ext: &str,
        output_dir: &std::path::Path,
    ) -> std::result::Result<(), ClientError> {
        let temp_content_path = output_dir.join("content");
        let blob_path = temp_content_path.with_added_extension(blob_file_ext);
        let _drop_blob = utility::fs::DropFile::new(blob_path.clone());

        match metadata {
            metadata::Metadata::Bundle(bundle) => {
                log::debug!(
                    "Extracting bundle package {} to {}",
                    identifier,
                    temp_content_path.display()
                );

                let _spin = self.progress.spinner(format!("Extracting '{identifier}'"));

                let extract_options = archive::ExtractOptions {
                    algorithm: Some(blob_compression),
                    strip_components: bundle.strip_components.unwrap_or(0).into(),
                };
                archive::Archive::extract_with_options(&blob_path, &temp_content_path, Some(extract_options))
                    .await
                    .map_err(ClientError::internal)?;
            }
        }

        crate::codesign::sign_extracted_content(&temp_content_path)
            .await
            .map_err(ClientError::internal)?;

        Ok(())
    }

    // ── Package push ─────────────────────────────────────────────────

    pub async fn push_package(
        &self,
        package_info: Info,
        layers: &[crate::publisher::LayerRef],
    ) -> Result<(Digest, oci::Manifest)> {
        let (index_digest, index) = self.push_manifest_and_merge_tags(&package_info, layers, &[]).await?;
        Ok((index_digest, oci::Manifest::ImageIndex(index)))
    }

    /// Pushes the package manifest and merges the resulting platform entry
    /// into the primary tag's image index plus each tag in `extra_tags`.
    ///
    /// The manifest is pushed once and its digest reused across every
    /// `merge_platform_into_index` call, so a cascade or multi-tag push
    /// never re-serializes or re-uploads the manifest. `extra_tags` is
    /// the rolling/cascade tag set (e.g. `["3.28", "3", "latest"]`);
    /// pass `&[]` for a plain single-tag push.
    ///
    /// Returns the digest + data of the primary tag's image index.
    pub(crate) async fn push_manifest_and_merge_tags(
        &self,
        package_info: &Info,
        layers: &[crate::publisher::LayerRef],
        extra_tags: &[String],
    ) -> Result<(Digest, oci::ImageIndex)> {
        log::debug!(
            "Pushing package {} with {} layer(s)",
            package_info.identifier,
            layers.len()
        );

        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let image = package_info.identifier.canonical_reference();
        self.transport.ensure_auth(&image, oci::RegistryOperation::Push).await?;

        let (_manifest, manifest_data, manifest_sha256) = self.push_multi_layer_manifest(package_info, layers).await?;
        let manifest_size = i64::try_from(manifest_data.len()).map_err(|_| {
            ClientError::InvalidManifest(format!("manifest size {} exceeds i64::MAX", manifest_data.len()))
        })?;

        let primary_tag = package_info.identifier.tag_or_latest().to_string();
        let (index_digest, index) = self
            .merge_platform_into_index(
                &package_info.identifier,
                &primary_tag,
                &package_info.platform,
                &manifest_sha256,
                manifest_size,
            )
            .await?;

        for tag in extra_tags {
            log::debug!("Cascading to {tag}");
            self.merge_platform_into_index(
                &package_info.identifier,
                tag.clone(),
                &package_info.platform,
                &manifest_sha256,
                manifest_size,
            )
            .await?;
        }

        Ok((index_digest, index))
    }

    /// Pushes config blob + N layer blobs + image manifest.
    ///
    /// For `LayerRef::File` layers: reads file, computes digest, uploads blob.
    /// For `LayerRef::Digest` layers: HEADs the blob to verify existence
    /// and learn its size, and uses the caller-supplied `media_type`
    /// for the manifest descriptor. The OCI spec does not expose a
    /// layer's media type via blob HEAD, so the caller is responsible
    /// for declaring it at the CLI (see `LayerRef::FromStr`).
    /// Returns the manifest, its serialized bytes, and its SHA-256 digest string.
    pub(crate) async fn push_multi_layer_manifest(
        &self,
        package_info: &Info,
        layers: &[crate::publisher::LayerRef],
    ) -> std::result::Result<(oci::ImageManifest, Vec<u8>, String), ClientError> {
        use crate::publisher::LayerRef;

        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let image = package_info.identifier.canonical_reference();

        let total_layers = layers.len();
        // Upload file layers and verify digest layers concurrently, preserving
        // input order so manifest descriptors match the caller-supplied order.
        // Bounded by `LAYER_PUSH_CONCURRENCY` to cap in-memory archive buffers.
        let layer_descriptors: Vec<oci::Descriptor> = stream::iter(layers.iter().enumerate())
            .map(|(index, layer)| {
                // `async move` owns its captures, so each concurrent future needs
                // its own copy of the image reference; clones are cheap
                // (a few short strings) and are outweighed by avoiding a
                // lifetime gymnastics around the stream combinator.
                let image = image.clone();
                async move {
                    let progress_label = format!("{}/{}", index + 1, total_layers);
                    match layer {
                        LayerRef::File(path) => {
                            let package_media_type =
                                media_type_from_path(path).map(|mt| mt.to_string()).ok_or_else(|| {
                                    ClientError::InvalidManifest(format!("unsupported archive: {}", path.display()))
                                })?;

                            // BOUNDED: LAYER_PUSH_CONCURRENCY caps simultaneous
                            // in-memory archives at 4 × (layer size). Do not raise
                            // the constant without either switching to a streaming
                            // push path or auditing the RSS budget for the largest
                            // layers callers ship.
                            //
                            // Single disk pass: read and hash are interleaved in
                            // 64 KiB chunks, so the SHA-256 finalization happens
                            // without a second traversal of the buffer.
                            let (package_data, digest) =
                                Algorithm::Sha256
                                    .hash_file_read(path)
                                    .await
                                    .map_err(|e| ClientError::Io {
                                        path: path.to_path_buf(),
                                        source: e,
                                    })?;
                            let package_data_len = package_data.len();

                            log::trace!(
                                "Layer {progress_label} {}: digest={}, size={}",
                                path.display(),
                                digest,
                                package_data_len
                            );

                            let bar = self.progress.bytes(
                                format!("Uploading {progress_label} {}", path.display()),
                                package_data_len as u64,
                            );
                            let on_progress = bar.callback();
                            self.transport
                                .push_blob(&image, package_data, &digest, on_progress)
                                .await?;
                            drop(bar);

                            let size = i64::try_from(package_data_len).map_err(|_| {
                                ClientError::InvalidManifest(format!("blob size {package_data_len} exceeds i64::MAX"))
                            })?;
                            Ok::<oci::Descriptor, ClientError>(oci::Descriptor {
                                media_type: package_media_type,
                                digest: digest.to_string(),
                                size,
                                urls: None,
                                annotations: None,
                            })
                        }
                        LayerRef::Digest { digest, media_type } => {
                            // The caller supplies `media_type` because the OCI
                            // distribution spec does not expose a layer's media
                            // type via blob HEAD — only the blob bytes and
                            // Content-Length. See `LayerRef::FromStr` for the
                            // `sha256:<hex>.<ext>` CLI syntax that carries this
                            // information from the user to here.
                            log::info!("Reusing layer {progress_label} {digest} ({media_type})");
                            let size = self.transport.head_blob(&image, digest).await?;

                            log::trace!(
                                "Layer {progress_label} {digest}: verified, media_type={media_type}, size={size}"
                            );

                            let size = i64::try_from(size).map_err(|_| {
                                ClientError::InvalidManifest(format!("blob size {size} exceeds i64::MAX"))
                            })?;
                            Ok(oci::Descriptor {
                                media_type: media_type.as_media_type().to_string(),
                                digest: digest.to_string(),
                                size,
                                urls: None,
                                annotations: None,
                            })
                        }
                    }
                }
            })
            .buffered(LAYER_PUSH_CONCURRENCY)
            .try_collect()
            .await?;

        // Assemble the manifest from the resolved descriptors (pure, no I/O).
        // Shared with `pull_local` so the two paths produce byte-identical manifests.
        let parts = super::manifest_builder::build_package_manifest(&package_info.metadata, layer_descriptors)?;
        log::trace!("Config digest: {}", parts.config_digest);

        // Push config blob — tiny, no progress needed.
        self.transport
            .push_blob(
                &image,
                parts.config_bytes,
                &parts.config_digest,
                transport::no_progress(),
            )
            .await?;

        let manifest_sha256 = parts.manifest_digest.to_string();
        let canonical_image = image.clone_with_digest(manifest_sha256.clone());

        let pushed_digest = self
            .transport
            .push_manifest_raw(
                &canonical_image,
                parts.manifest_bytes.clone(),
                MEDIA_TYPE_OCI_IMAGE_MANIFEST,
            )
            .await?;
        log::debug!("Pushed manifest with digest '{}'", pushed_digest);

        Ok((parts.manifest, parts.manifest_bytes, manifest_sha256))
    }

    // ── Description operations ────────────────────────────────────────

    /// Pushes a description artifact to the `__ocx.desc` tag.
    ///
    /// Builds an OCI ImageManifest with `artifact_type` set to the description media type,
    /// an empty config blob, layers for the README (and optional logo), and manifest-level
    /// annotations for catalog metadata (title, description, keywords).
    pub async fn push_description(
        &self,
        identifier: &Identifier,
        description: &package::description::Description,
    ) -> std::result::Result<(), ClientError> {
        let desc_identifier = identifier.clone_with_tag(InternalTag::DESCRIPTION_TAG);
        // Push stays canonical (mirror-free): remote/proxy mirrors are read-only.
        let image = desc_identifier.canonical_reference();
        self.transport.ensure_auth(&image, oci::RegistryOperation::Push).await?;

        let config_data = b"{}".to_vec();
        let config_digest = Algorithm::Sha256.hash(&config_data);
        self.transport
            .push_blob(&image, config_data, &config_digest, transport::no_progress())
            .await?;

        let readme_bytes = description.readme.as_bytes();
        let readme_len = readme_bytes.len();
        let readme_digest = Algorithm::Sha256.hash(readme_bytes);
        self.transport
            .push_blob(&image, readme_bytes.to_vec(), &readme_digest, transport::no_progress())
            .await?;

        let readme_size = i64::try_from(readme_len)
            .map_err(|_| ClientError::InvalidManifest(format!("readme blob size {readme_len} exceeds i64::MAX")))?;
        let mut layers = vec![oci::Descriptor {
            media_type: MEDIA_TYPE_MARKDOWN.to_string(),
            digest: readme_digest.to_string(),
            size: readme_size,
            urls: None,
            annotations: Some([(oci::annotations::TITLE.to_string(), "README.md".to_string())].into()),
        }];

        if let Some(logo) = &description.logo {
            let logo_len = logo.data.len();
            let logo_digest = Algorithm::Sha256.hash(&logo.data);
            self.transport
                .push_blob(&image, logo.data.clone(), &logo_digest, transport::no_progress())
                .await?;

            let ext = match logo.media_type {
                MEDIA_TYPE_PNG => "png",
                MEDIA_TYPE_SVG => "svg",
                _ => "bin",
            };
            let logo_size = i64::try_from(logo_len)
                .map_err(|_| ClientError::InvalidManifest(format!("logo blob size {logo_len} exceeds i64::MAX")))?;
            layers.push(oci::Descriptor {
                media_type: logo.media_type.to_string(),
                digest: logo_digest.to_string(),
                size: logo_size,
                urls: None,
                annotations: Some([(oci::annotations::TITLE.to_string(), format!("logo.{ext}"))].into()),
            });
        }

        let mut builder = super::manifest_builder::ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_DESCRIPTION_V1)
            .config_bytes(MEDIA_TYPE_OCI_EMPTY_CONFIG, b"{}".to_vec())
            .layers(layers);
        if !description.annotations.is_empty() {
            builder = builder.annotations(description.annotations.clone());
        }
        let parts = builder.build()?;
        // Sanity: the empty-config blob digest computed by the builder must
        // match the one we already pushed above.
        debug_assert_eq!(parts.config_digest.to_string(), config_digest.to_string());
        let manifest_data = parts.manifest_bytes;

        // Push to the tag reference directly (not by digest) so the tag is created.
        self.transport
            .push_manifest_raw(&image, manifest_data, MEDIA_TYPE_OCI_IMAGE_MANIFEST)
            .await?;

        log::debug!("Pushed description for {}", identifier);
        Ok(())
    }

    /// Pulls the description artifact from the `__ocx.desc` tag.
    ///
    /// Returns `Ok(None)` if no description tag exists for the identifier.
    /// Uses a temporary directory to download blobs before reading them into memory.
    pub async fn pull_description(
        &self,
        identifier: &Identifier,
        temp_dir: &std::path::Path,
    ) -> std::result::Result<Option<package::description::Description>, ClientError> {
        let desc_identifier = identifier.clone_with_tag(InternalTag::DESCRIPTION_TAG);
        let image = self.transport_reference(&desc_identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;

        let (manifest, _digest) = match self.fetch_manifest_raw(&image).await {
            Ok(result) => result,
            Err(ClientError::ManifestNotFound(_)) => return Ok(None),
            Err(e) => return Err(e),
        };

        let image_manifest = match manifest {
            oci::Manifest::Image(m) => m,
            oci::Manifest::ImageIndex(_) => {
                return Err(ClientError::InvalidManifest(
                    "expected image manifest for description, got image index".to_string(),
                ));
            }
        };

        match &image_manifest.artifact_type {
            Some(at) if at == MEDIA_TYPE_DESCRIPTION_V1 => {}
            other => {
                return Err(ClientError::InvalidManifest(format!(
                    "expected artifact_type '{}', got '{}'",
                    MEDIA_TYPE_DESCRIPTION_V1,
                    other.as_deref().unwrap_or("<none>")
                )));
            }
        }

        let mut readme: Option<String> = None;
        let mut logo: Option<package::description::Logo> = None;

        for (i, layer) in image_manifest.layers.iter().enumerate() {
            let blob_path = temp_dir.join(format!("layer_{i}"));
            let layer_digest = Digest::try_from(layer.digest.as_str()).map_err(|e| {
                ClientError::InvalidManifest(format!("description layer digest '{}' is malformed: {e}", layer.digest))
            })?;
            self.transport
                .pull_blob_to_file(&image, &layer_digest, &blob_path, 0, transport::no_progress())
                .await?;

            match layer.media_type.as_str() {
                MEDIA_TYPE_MARKDOWN => {
                    let data = tokio::fs::read(&blob_path).await.map_err(|e| ClientError::Io {
                        path: blob_path,
                        source: e,
                    })?;
                    readme = Some(String::from_utf8(data).map_err(ClientError::InvalidEncoding)?);
                }
                MEDIA_TYPE_PNG | MEDIA_TYPE_SVG => {
                    let data = tokio::fs::read(&blob_path).await.map_err(|e| ClientError::Io {
                        path: blob_path,
                        source: e,
                    })?;
                    logo = Some(package::description::Logo {
                        data,
                        media_type: if layer.media_type == MEDIA_TYPE_PNG {
                            MEDIA_TYPE_PNG
                        } else {
                            MEDIA_TYPE_SVG
                        },
                    });
                }
                _ => {
                    log::debug!("Ignoring unknown description layer media type: {}", layer.media_type);
                }
            }
        }

        let readme = readme
            .ok_or_else(|| ClientError::InvalidManifest("description manifest has no markdown layer".to_string()))?;

        let annotations = image_manifest.annotations.unwrap_or_default();

        Ok(Some(package::description::Description {
            readme,
            logo,
            annotations,
        }))
    }

    // ── Internal helpers ─────────────────────────────────────────────

    /// Pulls and parses a manifest from the registry.
    async fn fetch_manifest_raw(
        &self,
        image: &oci::Reference,
    ) -> std::result::Result<(oci::Manifest, String), ClientError> {
        log::debug!("Pulling manifest for image {}", image);
        let (data, digest) = self
            .transport
            .pull_manifest_raw(image, ACCEPTED_MANIFEST_MEDIA_TYPES)
            .await?;
        let manifest: oci::Manifest = serde_json::from_slice(&data).map_err(ClientError::Serialization)?;
        Ok((manifest, digest))
    }
}

// ── Pagination ───────────────────────────────────────────────────────

/// Generic paginated fetch: calls `fetch` repeatedly until the returned page
/// is smaller than `chunk_size`, concatenating all results.
///
/// The first call uses `Some("")` as the `last` cursor (not `None`)
/// because some registries return invalid responses when `n` is set without `last`.
async fn paginate<F, Fut>(chunk_size: usize, fetch: F) -> std::result::Result<Vec<String>, ClientError>
where
    F: Fn(usize, Option<String>) -> Fut,
    Fut: std::future::Future<Output = std::result::Result<Vec<String>, ClientError>>,
{
    let mut items = Vec::new();
    loop {
        let last = if items.is_empty() {
            Some(String::new())
        } else {
            items.last().cloned()
        };
        let page = fetch(chunk_size, last).await?;
        let page_len = page.len();
        items.extend(page);
        if page_len < chunk_size {
            break;
        }
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::test_transport::{StubTransport, StubTransportData};
    use super::*;
    use crate::MEDIA_TYPE_PACKAGE_METADATA_V1;
    use crate::oci;

    use std::sync::Mutex;

    use crate::file_structure::TempStore;

    // ── Test helpers ─────────────────────────────────────────────────

    fn stub(data: &StubTransportData) -> Client {
        Client::with_transport(Box::new(StubTransport::new(data.clone())))
    }

    fn test_identifier(tag: &str) -> Identifier {
        Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
    }

    fn test_identifier_with_digest(digest_hex: &str) -> Identifier {
        let digest = oci::Digest::Sha256(digest_hex.to_string());
        Identifier::new_registry("test/pkg", "example.com").clone_with_digest(digest)
    }

    fn test_pinned(digest_hex: &str) -> oci::PinnedIdentifier {
        oci::PinnedIdentifier::try_from(test_identifier_with_digest(digest_hex)).unwrap()
    }

    /// Build a valid image manifest with the given config and layer digests.
    /// Pads any short hex suffix up to 64 hex characters so the result parses as a real `Digest`.
    fn make_image_manifest(config_digest: &str, layer_digest: &str) -> oci::ImageManifest {
        fn normalize(d: &str) -> String {
            match d.strip_prefix("sha256:") {
                Some(rest) if rest.len() < 64 => {
                    let padding = "a".repeat(64 - rest.len());
                    format!("sha256:{rest}{padding}")
                }
                _ => d.to_string(),
            }
        }
        oci::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
            config: oci::Descriptor {
                media_type: MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                digest: normalize(config_digest),
                size: 100,
                urls: None,
                annotations: None,
            },
            layers: vec![oci::Descriptor {
                media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
                digest: normalize(layer_digest),
                size: 200,
                urls: None,
                annotations: None,
            }],
            ..Default::default()
        }
    }

    /// Serialize a manifest and compute its digest, returning (bytes, digest_string).
    fn serialize_manifest(manifest: &oci::Manifest) -> (Vec<u8>, String) {
        let data = serde_json::to_vec(manifest).unwrap();
        let digest = Algorithm::Sha256.hash(&data).to_string();
        (data, digest)
    }

    // ── Pagination tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn list_tags_single_page() {
        let data = StubTransportData::new();
        data.write().tags = vec![vec!["1.0".into(), "2.0".into()]];
        let client = stub(&data);

        let tags = client.list_tags(test_identifier("latest")).await.unwrap();
        assert_eq!(tags, vec!["1.0", "2.0"]);
    }

    #[tokio::test]
    async fn list_tags_multi_page() {
        let page1: Vec<String> = (0..100).map(|i| format!("tag-{:03}", i)).collect();
        let page2 = vec!["tag-100".to_string(), "tag-101".to_string()];

        let data = StubTransportData::new();
        data.write().tags = vec![page1, page2];
        let client = stub(&data);

        let tags = client.list_tags(test_identifier("latest")).await.unwrap();
        assert_eq!(tags.len(), 102);
        assert_eq!(tags[0], "tag-000");
        assert_eq!(tags[101], "tag-101");
    }

    #[tokio::test]
    async fn list_repositories_pagination() {
        let page1: Vec<String> = (0..100).map(|i| format!("repo-{:03}", i)).collect();
        let page2 = vec!["repo-100".to_string()];

        let data = StubTransportData::new();
        data.write().repositories = vec![page1, page2];
        let client = stub(&data);

        let repos = client.list_repositories("example.com").await.unwrap();
        assert_eq!(repos.len(), 101);
    }

    // ── Manifest fetch tests ─────────────────────────────────────────

    #[tokio::test]
    async fn fetch_manifest_digest_success() {
        let digest_str = "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let data = StubTransportData::new();
        data.write().digest = Some(digest_str.to_string());
        let client = stub(&data);

        let id = test_identifier("1.0");
        let digest = client.fetch_manifest_digest(&id).await.unwrap();
        assert_eq!(digest.to_string(), digest_str);
    }

    #[tokio::test]
    async fn fetch_manifest_success() {
        let manifest = oci::Manifest::Image(make_image_manifest("sha256:cff", "sha256:1a0e"));
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let id = test_identifier("1.0");
        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str.clone()));
        let client = stub(&data);

        let (digest, fetched) = client.fetch_manifest(&id).await.unwrap();
        assert_eq!(digest.to_string(), digest_str);
        assert!(matches!(fetched, oci::Manifest::Image(_)));
    }

    // ── pull_manifest tests ─────────────────────────────────────

    #[tokio::test]
    async fn pull_manifest_digest_mismatch() {
        let manifest = oci::Manifest::Image(make_image_manifest("sha256:cff", "sha256:1a0e"));
        let (manifest_data, _real_digest) = serialize_manifest(&manifest);
        let wrong_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        let id = test_pinned("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, wrong_digest.to_string()));
        let client = stub(&data);

        let result = client.pull_manifest(&id).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.to_lowercase().contains("digest mismatch"), "got: {}", err_msg);
    }

    #[tokio::test]
    async fn pull_manifest_unexpected_manifest_type() {
        let index = oci::ImageIndex {
            schema_version: 2,
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
            artifact_type: None,
            manifests: vec![],
            annotations: None,
        };
        let manifest = oci::Manifest::ImageIndex(index);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);

        let digest_hex = digest_str.strip_prefix("sha256:").unwrap();
        let id = test_pinned(digest_hex);

        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client.pull_manifest(&id).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("image manifest") || err_msg.contains("image index"),
            "got: {}",
            err_msg
        );
    }

    // ── pull_manifest: no longer validates media types ────────────

    #[tokio::test]
    async fn pull_manifest_accepts_any_media_types() {
        let mut m = make_image_manifest("sha256:cff", "sha256:1a0e");
        m.config.media_type = "application/vnd.other.config".to_string();
        m.artifact_type = Some("application/vnd.other.artifact".to_string());
        let manifest = oci::Manifest::Image(m);
        let (manifest_data, digest_str) = serialize_manifest(&manifest);
        let digest_hex = digest_str.strip_prefix("sha256:").unwrap();
        let id = test_pinned(digest_hex);

        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let result = client.pull_manifest(&id).await;
        assert!(result.is_ok(), "pull_manifest should not validate media types");
    }

    // ── pull_blob tests ─────────────────────────────────────────

    /// Helper: register a manifest + config blob in the stub, returning the pinned ID.
    fn setup_manifest_and_blob(
        data: &StubTransportData,
        manifest: oci::ImageManifest,
        config_blob: &[u8],
    ) -> oci::PinnedIdentifier {
        let config_digest = &manifest.config.digest;
        data.write()
            .blobs
            .insert(config_digest.to_string(), config_blob.to_vec());

        let oci_manifest = oci::Manifest::Image(manifest);
        let (manifest_data, digest_str) = serialize_manifest(&oci_manifest);
        let digest_hex = digest_str.strip_prefix("sha256:").unwrap();
        let id = test_pinned(digest_hex);

        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        id
    }

    #[tokio::test]
    async fn pull_blob_returns_raw_bytes() {
        let metadata_json = br#"{"type":"bundle","version":1}"#;
        let data = StubTransportData::new();
        let manifest = make_image_manifest("sha256:cff", "sha256:1a0e");
        let id = setup_manifest_and_blob(&data, manifest.clone(), metadata_json);
        let client = stub(&data);

        let config_digest = Digest::try_from(manifest.config.digest.as_str()).unwrap();
        let blob_ref = id.clone_with_digest(config_digest);
        let bytes = client
            .pull_blob(&blob_ref)
            .await
            .expect("pull_blob should return registered bytes");
        assert_eq!(bytes.as_slice(), metadata_json.as_slice());

        // Round-trip parse confirms the bytes are intact.
        let parsed: metadata::Metadata = serde_json::from_slice(&bytes).expect("returned bytes must parse as Metadata");
        let _ = parsed;
    }

    // ── verify_blob_digest tests ────────────────────────────────

    #[tokio::test]
    async fn verify_blob_digest_accepts_matching_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        tokio::fs::write(&path, b"hello world").await.unwrap();

        let expected = Algorithm::Sha256.hash(b"hello world");
        verify_blob_digest(&path, &expected).await.unwrap();
        assert!(path.exists(), "matching blob must not be deleted");
    }

    #[tokio::test]
    async fn verify_blob_digest_rejects_tampered_content_and_deletes_blob() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        tokio::fs::write(&path, b"evil bytes").await.unwrap();

        // Claim the digest of different bytes — simulating a registry
        // that served different content for the same digest (CWE-345).
        let expected = Algorithm::Sha256.hash(b"honest bytes");
        let expected_str = expected.to_string();

        let err = verify_blob_digest(&path, &expected).await.unwrap_err();
        match err {
            ClientError::DigestMismatch { expected: e, actual } => {
                assert_eq!(e, expected_str);
                assert_ne!(actual, expected_str);
            }
            other => panic!("expected DigestMismatch, got {other:?}"),
        }
        assert!(!path.exists(), "tampered blob must be deleted");
    }

    #[tokio::test]
    async fn verify_blob_digest_accepts_sha512_match() {
        // Regression for algorithm-blind verify: before the fix, any
        // non-SHA256 expected digest would be hashed with SHA-256 and
        // produce a spurious DigestMismatch even on honest bytes.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        let data = b"hello world".to_vec();
        tokio::fs::write(&path, &data).await.unwrap();

        let expected = Digest::Sha512(hex::encode(<sha2::Sha512 as sha2::Digest>::digest(&data)));
        verify_blob_digest(&path, &expected).await.unwrap();
        assert!(path.exists(), "matching blob must not be deleted");
    }

    #[tokio::test]
    async fn verify_blob_digest_rejects_sha512_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blob");
        tokio::fs::write(&path, b"evil bytes").await.unwrap();

        // A real SHA-512 of "honest bytes" — the verify path must
        // compute SHA-512 of the file (not SHA-256) and still report
        // a clean mismatch when the bytes differ.
        let expected = Digest::Sha512(hex::encode(<sha2::Sha512 as sha2::Digest>::digest(b"honest bytes")));
        let err = verify_blob_digest(&path, &expected).await.unwrap_err();
        match err {
            ClientError::DigestMismatch { expected: e, actual } => {
                assert!(e.starts_with("sha512:"), "expected algorithm preserved in error: {e}");
                assert!(
                    actual.starts_with("sha512:"),
                    "actual must also be SHA-512, got: {actual}"
                );
                assert_ne!(e, actual);
            }
            other => panic!("expected DigestMismatch, got {other:?}"),
        }
        assert!(!path.exists(), "tampered blob must be deleted");
    }

    // ── pull_layer tests ────────────────────────────────────────

    #[tokio::test]
    async fn pull_layer_rejects_bytes_not_matching_descriptor_digest() {
        // Claim a digest for bytes that hash to something else — simulates
        // a registry serving different content for the declared digest
        // (CWE-345). `pull_layer` must surface `DigestMismatch` and leave
        // no blob file on disk (verify_blob_digest's unlink invariant).
        let claimed_digest = format!("sha256:{}", "a".repeat(64));
        let evil_bytes = b"bytes that definitely do not hash to all-a".to_vec();

        let data = StubTransportData::new();
        data.write().blobs.insert(claimed_digest.clone(), evil_bytes);
        let client = stub(&data);

        let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: claimed_digest.clone(),
            size: 0,
            urls: None,
            annotations: None,
        };
        let metadata: metadata::Metadata = serde_json::from_str(r#"{"type":"bundle","version":1}"#).unwrap();
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, &metadata, dir.path()).await;
        match result {
            Err(ClientError::DigestMismatch { expected, actual }) => {
                assert_eq!(expected, claimed_digest);
                assert_ne!(actual, claimed_digest);
            }
            other => panic!("expected DigestMismatch, got {other:?}"),
        }

        let blob_path = dir.path().join("content.tar.gz");
        assert!(
            !blob_path.exists(),
            "tampered layer blob must be unlinked after digest mismatch"
        );
    }

    /// T-A4: `pull_layer` rejects a tampered blob even when the client has a
    /// non-empty `MirrorMap` configured for the identifier's registry.
    ///
    /// This pins the security invariant that the mirror transform (host + repo
    /// rewrite) does NOT bypass `verify_blob_digest`. A compromised or
    /// misbehaving mirror that serves different bytes for the declared digest
    /// must be caught by the same CWE-345 gate that catches a compromised
    /// canonical registry.
    ///
    /// Setup mirrors the existing `pull_layer_rejects_bytes_not_matching_descriptor_digest`
    /// test, then adds a `MirrorMap` for `example.com` before calling `pull_layer`.
    #[tokio::test]
    async fn pull_layer_rejects_tampered_blob_under_configured_mirror() {
        use crate::config::mirror::ParsedMirror;

        let claimed_digest = format!("sha256:{}", "a".repeat(64));
        let evil_bytes = b"bytes that definitely do not hash to all-a".to_vec();

        let data = StubTransportData::new();
        data.write().blobs.insert(claimed_digest.clone(), evil_bytes);
        let mut client = stub(&data);

        // Install a non-empty MirrorMap pointing example.com (the test
        // identifier's registry) to a corporate mirror. This proves that the
        // mirror rewrite path also funnels through verify_blob_digest.
        client.mirrors = MirrorMap::new([(
            "example.com".to_string(),
            ParsedMirror {
                protocol: "https".to_string(),
                host: "mirror.corp".to_string(),
                path_prefix: "oci-proxy".to_string(),
            },
        )]);

        let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let layer = oci::Descriptor {
            media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
            digest: claimed_digest.clone(),
            size: 0,
            urls: None,
            annotations: None,
        };
        let metadata: metadata::Metadata = serde_json::from_str(r#"{"type":"bundle","version":1}"#).unwrap();
        let dir = tempfile::tempdir().unwrap();

        let result = client.pull_layer(&id, &layer, &metadata, dir.path()).await;
        match result {
            Err(ClientError::DigestMismatch { expected, actual }) => {
                assert_eq!(
                    expected, claimed_digest,
                    "DigestMismatch must report the declared (claimed) digest"
                );
                assert_ne!(
                    actual, claimed_digest,
                    "DigestMismatch actual must differ from the claimed digest"
                );
            }
            other => panic!("expected DigestMismatch from pull_layer under mirror, got {other:?}"),
        }

        let blob_path = dir.path().join("content.tar.gz");
        assert!(
            !blob_path.exists(),
            "tampered layer blob must be unlinked even when a MirrorMap is configured"
        );
    }

    // ── TempStore tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn temp_acquire_cleans_leftover_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let temp_root = dir.path().join("temp_root");
        let temp_path = temp_root.join("some_hash");

        // Simulate leftover artifacts from a crashed download.
        tokio::fs::create_dir_all(&temp_path).await.unwrap();
        tokio::fs::write(temp_path.join("metadata.json"), b"stale")
            .await
            .unwrap();
        tokio::fs::create_dir(temp_path.join("content")).await.unwrap();

        let store = TempStore::new(&temp_root);
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();

        // Verify artifacts were cleaned.
        assert!(acquired.was_cleaned);
        assert!(!temp_path.join("metadata.json").exists());
        assert!(!temp_path.join("content").exists());
        // Lock file is a sibling, not inside the dir.
        assert!(TempStore::lock_path_for(&temp_path).exists());
    }

    // ── Paginate unit test ───────────────────────────────────────────

    #[tokio::test]
    async fn paginate_empty() {
        let result = paginate(100, |_cs, _last| async { Ok(vec![]) }).await;
        assert_eq!(result.unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn paginate_first_page_uses_empty_string() {
        let lasts = std::sync::Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let lasts_clone = lasts.clone();
        let result = paginate(100, move |_cs, last| {
            let lasts = lasts_clone.clone();
            async move {
                lasts.lock().unwrap().push(last);
                Ok(vec!["a".to_string()])
            }
        })
        .await;
        assert!(result.is_ok());
        let captured = lasts.lock().unwrap();
        assert_eq!(captured[0], Some(String::new()));
    }

    // ── merge_platform_into_index tests ─────────────────────────────

    mod merge_platform {
        use super::*;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn platform(s: &str) -> oci::Platform {
            s.parse().unwrap()
        }

        /// Read back the pushed index from the stub and parse it.
        fn read_pushed_index(data: &StubTransportData, tag: &str) -> oci::ImageIndex {
            let id = test_identifier(tag);
            let inner = data.read();
            let (bytes, _) = inner
                .manifests
                .get(&id.canonical_reference().to_string())
                .expect("no pushed manifest");
            let manifest: oci::Manifest = serde_json::from_slice(bytes).unwrap();
            match manifest {
                oci::Manifest::ImageIndex(idx) => idx,
                _ => panic!("expected ImageIndex, got ImageManifest"),
            }
        }

        #[tokio::test]
        async fn fresh_tag_creates_new_index() {
            let data = StubTransportData::new();
            let client = stub_with_capture(&data);
            let id = test_identifier("3.28");

            client
                .merge_platform_into_index(&id, "3.28", &platform("linux/amd64"), "sha256:abc", 100)
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(index.manifests.len(), 1);
            assert_eq!(index.manifests[0].digest, "sha256:abc");
            assert_eq!(index.manifests[0].size, 100);
            let entry_plat: oci::Platform = index.manifests[0].platform.clone().unwrap().try_into().unwrap();
            assert_eq!(entry_plat, platform("linux/amd64"));
        }

        #[tokio::test]
        async fn existing_index_adds_platform() {
            let data = StubTransportData::new();

            // Seed an existing index with arm64.
            let id = test_identifier("3.28");
            let existing = oci::ImageIndex {
                schema_version: 2,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                manifests: vec![oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: "sha256:arm64_digest".to_string(),
                    size: 50,
                    platform: Some(platform("linux/arm64").into()),
                    annotations: None,
                }],
                annotations: None,
            };
            let existing_bytes = serde_json::to_vec(&oci::Manifest::ImageIndex(existing)).unwrap();
            let existing_digest = oci::Algorithm::Sha256.hash(&existing_bytes).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (existing_bytes, existing_digest));

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(&id, "3.28", &platform("linux/amd64"), "sha256:amd64_new", 200)
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(index.manifests.len(), 2);
            let digests: Vec<&str> = index.manifests.iter().map(|e| e.digest.as_str()).collect();
            assert!(digests.contains(&"sha256:arm64_digest"));
            assert!(digests.contains(&"sha256:amd64_new"));
        }

        #[tokio::test]
        async fn existing_index_replaces_same_platform() {
            let data = StubTransportData::new();

            let id = test_identifier("3.28");
            let existing = oci::ImageIndex {
                schema_version: 2,
                media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                manifests: vec![oci::ImageIndexEntry {
                    media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                    digest: "sha256:old_amd64".to_string(),
                    size: 50,
                    platform: Some(platform("linux/amd64").into()),
                    annotations: None,
                }],
                annotations: None,
            };
            let existing_bytes = serde_json::to_vec(&oci::Manifest::ImageIndex(existing)).unwrap();
            let existing_digest = oci::Algorithm::Sha256.hash(&existing_bytes).to_string();
            data.write()
                .manifests
                .insert(id.canonical_reference().to_string(), (existing_bytes, existing_digest));

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(&id, "3.28", &platform("linux/amd64"), "sha256:new_amd64", 200)
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            assert_eq!(index.manifests.len(), 1);
            assert_eq!(index.manifests[0].digest, "sha256:new_amd64");
            assert_eq!(index.manifests[0].size, 200);
        }

        #[tokio::test]
        async fn existing_image_manifest_upgrades_to_index() {
            let data = StubTransportData::new();

            // Seed an existing plain ImageManifest (not an index).
            let id = test_identifier("3.28");
            let image_manifest = oci::ImageManifest {
                config: oci::Descriptor {
                    media_type: "application/vnd.oci.image.config.v1+json".to_string(),
                    digest: "sha256:old_config".to_string(),
                    size: 42,
                    urls: None,
                    annotations: None,
                },
                ..Default::default()
            };
            let manifest = oci::Manifest::Image(image_manifest);
            let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
            let manifest_digest = oci::Algorithm::Sha256.hash(&manifest_bytes).to_string();
            data.write().manifests.insert(
                id.canonical_reference().to_string(),
                (manifest_bytes.clone(), manifest_digest.clone()),
            );

            let client = stub_with_capture(&data);
            client
                .merge_platform_into_index(&id, "3.28", &platform("linux/amd64"), "sha256:new_manifest", 300)
                .await
                .unwrap();

            let index = read_pushed_index(&data, "3.28");
            // Should have 2 entries: old manifest (no platform) + new (amd64).
            assert_eq!(index.manifests.len(), 2);
            let old_entry = index
                .manifests
                .iter()
                .find(|e| e.platform.is_none())
                .expect("old entry missing");
            // Fixed: uses the manifest digest (not config.digest) and manifest size (not config.size).
            assert_eq!(old_entry.digest, manifest_digest);
            assert_eq!(old_entry.size, manifest_bytes.len() as i64);
            let new_entry = index
                .manifests
                .iter()
                .find(|e| e.platform.is_some())
                .expect("new entry missing");
            assert_eq!(new_entry.digest, "sha256:new_manifest");
        }

        #[tokio::test]
        async fn non_404_error_propagates_instead_of_starting_fresh() {
            let data = StubTransportData::new();
            // Inject a registry error (e.g. auth failure, network issue) for missing manifests.
            data.write().pull_manifest_error_override = Some("connection reset".into());
            data.write().capture_pushes = true;
            let client = Client::with_transport(Box::new(StubTransport::new(data.clone())));
            let id = test_identifier("3.28");

            let result = client
                .merge_platform_into_index(&id, "3.28", &platform("linux/amd64"), "sha256:abc", 100)
                .await;

            assert!(result.is_err(), "expected error to propagate, got Ok");
            // Verify no manifest was pushed (no silent overwrite).
            let inner = data.read();
            assert!(
                inner.manifests.is_empty(),
                "no manifest should have been pushed on error"
            );
        }
    }

    // ── ensure_auth tests ───────────────────────────────────────────

    mod ensure_auth {
        use super::*;
        use oci::RegistryOperation;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn platform(s: &str) -> oci::Platform {
            s.parse().unwrap()
        }

        fn auth_calls(data: &StubTransportData) -> Vec<(String, RegistryOperation)> {
            data.read().auth_calls.clone()
        }

        #[tokio::test]
        async fn client_ensure_auth_delegates_to_transport() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let id = test_identifier("1.0");

            client.ensure_auth(&id, RegistryOperation::Pull).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].0, "example.com");
            assert!(matches!(calls[0].1, RegistryOperation::Pull));

            client.ensure_auth(&id, RegistryOperation::Push).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 2);
            assert!(matches!(calls[1].1, RegistryOperation::Push));
        }

        #[tokio::test]
        async fn list_tags_authenticates_with_pull() {
            let data = StubTransportData::new();
            data.write().tags = vec![vec!["1.0".into()]];
            let client = stub(&data);

            client.list_tags(test_identifier("latest")).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn list_repositories_authenticates_with_pull() {
            let data = StubTransportData::new();
            let client = stub(&data);

            client.list_repositories("example.com").await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn fetch_manifest_digest_authenticates_with_pull() {
            let data = StubTransportData::new();
            data.write().digest =
                Some("sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890".into());
            let client = stub(&data);
            let id = test_identifier("1.0");

            client.fetch_manifest_digest(&id).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn fetch_manifest_authenticates_with_pull() {
            let manifest = oci::Manifest::Image(make_image_manifest("sha256:cff", "sha256:1a0e"));
            let (manifest_data, digest_str) = serialize_manifest(&manifest);

            let id = test_identifier("1.0");
            let data = StubTransportData::new();
            data.write()
                .manifests
                .insert(id.to_string(), (manifest_data, digest_str));
            let client = stub(&data);

            client.fetch_manifest(&id).await.unwrap();
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn pull_manifest_authenticates_with_pull() {
            let id = test_pinned("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            let data = StubTransportData::new();
            let client = stub(&data);

            // Will fail (no manifest), but auth should have been called first.
            let _ = client.pull_manifest(&id).await;
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn push_package_authenticates_with_push() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = stub_with_capture(&data);

            let id = test_identifier("1.0");
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            tokio::fs::write(&archive_path, b"fake-archive").await.unwrap();

            let info = Info {
                identifier: id,
                metadata: metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                    version: package::metadata::bundle::Version::V1,
                    strip_components: None,
                    env: Default::default(),
                    dependencies: Default::default(),
                    entrypoints: Default::default(),
                }),
                platform: "linux/amd64".parse().unwrap(),
            };

            let layers = [crate::publisher::LayerRef::File(archive_path)];
            let _ = client.push_package(info, &layers).await;
            let calls = auth_calls(&data);
            // Must authenticate with Push before any blob/manifest operations.
            assert!(!calls.is_empty(), "push_package must call ensure_auth");
            assert!(matches!(calls[0].1, RegistryOperation::Push));
        }

        #[tokio::test]
        async fn push_description_authenticates_with_push() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let id = test_identifier("1.0");

            let desc = package::description::Description {
                readme: "# Test".to_string(),
                logo: None,
                annotations: Default::default(),
            };

            let _ = client.push_description(&id, &desc).await;
            let calls = auth_calls(&data);
            assert!(!calls.is_empty(), "push_description must call ensure_auth");
            assert!(matches!(calls[0].1, RegistryOperation::Push));
        }

        #[tokio::test]
        async fn pull_description_authenticates_with_pull() {
            let data = StubTransportData::new();
            let client = stub(&data);
            let id = test_identifier("1.0");

            let dir = tempfile::tempdir().unwrap();
            let _ = client.pull_description(&id, dir.path()).await;
            let calls = auth_calls(&data);
            assert_eq!(calls.len(), 1);
            assert!(matches!(calls[0].1, RegistryOperation::Pull));
        }

        #[tokio::test]
        async fn merge_platform_into_index_authenticates_with_push() {
            let data = StubTransportData::new();
            let client = stub_with_capture(&data);
            let id = test_identifier("3.28");

            let _ = client
                .merge_platform_into_index(&id, "3.28", &platform("linux/amd64"), "sha256:abc", 100)
                .await;
            let calls = auth_calls(&data);
            assert!(!calls.is_empty(), "merge_platform_into_index must call ensure_auth");
            assert!(matches!(calls[0].1, RegistryOperation::Push));
        }

        #[tokio::test]
        async fn ensure_auth_precedes_transport_calls_for_push() {
            let data = StubTransportData::new();
            data.write().capture_pushes = true;
            let client = stub_with_capture(&data);

            let id = test_identifier("1.0");
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            tokio::fs::write(&archive_path, b"fake-archive").await.unwrap();

            let info = Info {
                identifier: id,
                metadata: metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                    version: package::metadata::bundle::Version::V1,
                    strip_components: None,
                    env: Default::default(),
                    dependencies: Default::default(),
                    entrypoints: Default::default(),
                }),
                platform: "linux/amd64".parse().unwrap(),
            };

            let layers = [crate::publisher::LayerRef::File(archive_path)];
            let _ = client.push_package(info, &layers).await;

            // Verify auth happened before any transport method calls.
            let inner = data.read();
            assert!(!inner.auth_calls.is_empty(), "ensure_auth must have been called");
            assert!(matches!(inner.auth_calls[0].1, RegistryOperation::Push));
            // push_blob should have been called (for the package data).
            assert!(
                inner.calls.iter().any(|c| c.starts_with("push_blob:")),
                "push_blob should follow ensure_auth, calls: {:?}",
                inner.calls
            );
        }
    }

    // ── Multi-layer digest reuse tests ──────────────────────────────
    //
    // Regression test for the fabricated-`tar+gzip` bug on the
    // `LayerRef::Digest` path. Before this fix, the push code
    // unconditionally stamped `application/vnd.oci.image.layer.v1.tar+gzip`
    // on every digest-referenced layer, so reusing a `.tar.xz` or
    // `.zip` layer produced a manifest that broke every consumer's
    // `package pull`.
    //
    // The fix makes the CLI declare the media type alongside the
    // digest (see `LayerRef::FromStr`'s `sha256:<hex>.<ext>` syntax)
    // and threads it straight into the manifest descriptor. These
    // tests assert the supplied media type round-trips unchanged.

    mod multi_layer_digest_resolve {
        use super::*;
        use crate::package::{self, info::Info, metadata};
        use crate::publisher::LayerRef;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn bundle_metadata() -> metadata::Metadata {
            metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                version: package::metadata::bundle::Version::V1,
                strip_components: None,
                env: Default::default(),
                dependencies: Default::default(),
                entrypoints: Default::default(),
            })
        }

        fn info(tag: &str) -> Info {
            Info {
                identifier: test_identifier(tag),
                metadata: bundle_metadata(),
                platform: "linux/amd64".parse().unwrap(),
            }
        }

        /// A digest-referenced layer must carry the media type declared
        /// by the caller, not a fabricated `tar+gzip`. Regression for
        /// the original Bug 2.
        #[tokio::test]
        async fn digest_layer_uses_supplied_media_type_tar_xz() {
            let layer_digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";
            let layer_size: i64 = 4096;

            let data = StubTransportData::new();
            // The stub's `head_blob` returns the length of whatever
            // bytes we seed under this digest, so the size in the
            // resulting manifest descriptor will match.
            data.write()
                .blobs
                .insert(layer_digest.to_string(), vec![0u8; layer_size as usize]);
            let client = stub_with_capture(&data);

            let layers = [LayerRef::Digest {
                digest: oci::Digest::try_from(layer_digest).unwrap(),
                media_type: crate::publisher::ArchiveMediaType::TarXz,
            }];
            let (manifest, _bytes, _digest) = client
                .push_multi_layer_manifest(&info("2.0.0"), &layers)
                .await
                .expect("push_multi_layer_manifest must succeed with a live blob and declared media type");

            assert_eq!(manifest.layers.len(), 1);
            assert_eq!(
                manifest.layers[0].media_type,
                crate::MEDIA_TYPE_TAR_XZ,
                "the manifest must carry the caller-declared media type verbatim — no tar+gzip fabrication"
            );
            assert_eq!(manifest.layers[0].size, layer_size);
            assert_eq!(manifest.layers[0].digest, layer_digest);

            // `head_blob` should still be called — it's the transport-
            // level contract for fetching the blob's size. The Bug 1
            // fix ensures its native implementation reads
            // `Content-Length` from a real HEAD rather than pulling
            // the whole blob into memory.
            let inner = data.read();
            assert!(
                inner.calls.iter().any(|c| c == &format!("head_blob:{layer_digest}")),
                "head_blob should be called exactly once to fetch the layer size, calls: {:?}",
                inner.calls
            );
        }

        /// When the requested digest blob does not exist in the
        /// registry, the push must fail with `BlobNotFound` surfaced by
        /// `head_blob`.
        #[tokio::test]
        async fn digest_layer_not_found_in_registry_errors() {
            let missing_digest = "sha256:3333333333333333333333333333333333333333333333333333333333333333";

            let data = StubTransportData::new();
            let client = stub_with_capture(&data);

            let layers = [LayerRef::Digest {
                digest: oci::Digest::try_from(missing_digest).unwrap(),
                media_type: crate::publisher::ArchiveMediaType::TarGz,
            }];
            let err = client
                .push_multi_layer_manifest(&info("2.0.0"), &layers)
                .await
                .expect_err("push must fail when the referenced blob is absent");

            let msg = err.to_string().to_lowercase();
            assert!(
                msg.contains("not found") || msg.contains("blob"),
                "error message should mention not-found / blob, got: {msg}"
            );
        }

        /// A `LayerRef::File` with an unrecognized extension must be
        /// rejected with `InvalidManifest` before any network I/O.
        /// Without this guard the push path would stamp `media_type = "blob"`
        /// or silently default to tar+gzip, shipping a manifest that no
        /// consumer can extract.
        #[tokio::test]
        async fn unknown_file_extension_is_rejected() {
            let dir = tempfile::tempdir().unwrap();
            let weird_path = dir.path().join("archive.bogus");
            tokio::fs::write(&weird_path, b"irrelevant bytes").await.unwrap();

            let data = StubTransportData::new();
            let client = stub_with_capture(&data);

            let layers = [LayerRef::File(weird_path)];
            let err = client
                .push_multi_layer_manifest(&info("1.0.0"), &layers)
                .await
                .expect_err("unknown extensions must fail before push");

            assert!(
                matches!(err, ClientError::InvalidManifest(_)),
                "expected InvalidManifest, got {err:?}"
            );
        }
    }

    // ── Cascade tag ordering ────────────────────────────────────────

    /// `push_manifest_and_merge_tags` must push the manifest once, then
    /// merge the resulting platform entry into the primary tag's index
    /// and into each `extra_tags` entry in input order. The order of
    /// recorded transport calls is what OCX clients actually observe —
    /// tests over that contract prevent silent reorderings that would
    /// leave earlier tags pointing at stale indexes.
    mod cascade_order {
        use super::*;
        use crate::publisher::LayerRef;

        fn test_identifier(tag: &str) -> Identifier {
            Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
        }

        fn stub_with_capture(data: &StubTransportData) -> Client {
            data.write().capture_pushes = true;
            Client::with_transport(Box::new(StubTransport::new(data.clone())))
        }

        fn info(tag: &str) -> Info {
            Info {
                identifier: test_identifier(tag),
                metadata: metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
                    version: package::metadata::bundle::Version::V1,
                    strip_components: None,
                    env: Default::default(),
                    dependencies: Default::default(),
                    entrypoints: Default::default(),
                }),
                platform: "linux/amd64".parse().unwrap(),
            }
        }

        #[tokio::test]
        async fn push_manifest_and_merge_tags_processes_tags_in_input_order() {
            let dir = tempfile::tempdir().unwrap();
            let archive_path = dir.path().join("pkg.tar.gz");
            tokio::fs::write(&archive_path, b"fake-archive").await.unwrap();

            let data = StubTransportData::new();
            let client = stub_with_capture(&data);

            let layers = [LayerRef::File(archive_path)];
            let extra_tags = ["3".to_string(), "latest".to_string()];
            client
                .push_manifest_and_merge_tags(&info("1.2.3"), &layers, &extra_tags)
                .await
                .expect("push should succeed");

            // Extract only push_manifest / push_manifest_raw / pull_manifest_raw
            // calls from the recorded transport log — those are the ordered
            // high-level operations we care about here.
            let relevant: Vec<String> = data
                .read()
                .calls
                .iter()
                .filter(|c| *c == "push_manifest" || *c == "push_manifest_raw" || *c == "pull_manifest_raw")
                .cloned()
                .collect();

            // Expected cascade: push the image manifest once, then for
            // each tag (primary, "3", "latest") pull (attempt to read
            // existing index) + push_manifest_raw (write updated index).
            // Ordering must be stable across every run.
            let expected = vec![
                "push_manifest_raw", // the image manifest itself
                "pull_manifest_raw", // primary tag existing index lookup
                "push_manifest_raw", // primary tag index push
                "pull_manifest_raw", // extra_tags[0] lookup
                "push_manifest_raw", // extra_tags[0] push
                "pull_manifest_raw", // extra_tags[1] lookup
                "push_manifest_raw", // extra_tags[1] push
            ];
            assert_eq!(relevant, expected, "cascade calls must follow input tag order");
        }
    }

    // ── construction-gating backstop + Step 3.1 specification tests ──────────
    //
    // The PRIMARY guarantee is the compile-time construction-gating from
    // Step 1.3: the read-path `Identifier → native::Reference` conversion has
    // no public `From` impl, so a bypassing read site fails to compile. This
    // behavioural module is defence-in-depth only — it pins
    // `transport_reference` identity-when-empty / rewrite-when-set and the
    // push-path-unchanged invariant.
    mod transport_reference {
        use super::*;
        use crate::config::mirror::ParsedMirror;
        use crate::oci::client::MirrorMap;

        fn make_id_with_tag(registry: &str, repo: &str, tag: &str) -> Identifier {
            Identifier::new_registry(repo, registry).clone_with_tag(tag)
        }

        /// A 64-hex SHA-256 digest for the pinned-install path tests.
        fn test_digest(hex_seed: char) -> oci::Digest {
            oci::Digest::Sha256(std::iter::repeat_n(hex_seed, 64).collect())
        }

        fn make_id_with_digest(registry: &str, repo: &str, digest: oci::Digest) -> Identifier {
            Identifier::new_registry(repo, registry).clone_with_digest(digest)
        }

        fn make_id_with_tag_and_digest(registry: &str, repo: &str, tag: &str, digest: oci::Digest) -> Identifier {
            Identifier::new_registry(repo, registry)
                .clone_with_tag(tag)
                .clone_with_digest(digest)
        }

        fn make_mirror_map(upstream: &str, mirror_host: &str, prefix: &str) -> MirrorMap {
            MirrorMap::new([(
                upstream.to_string(),
                ParsedMirror {
                    protocol: "https".to_string(),
                    host: mirror_host.to_string(),
                    path_prefix: prefix.to_string(),
                },
            )])
        }

        /// `transport_reference` with an empty (identity) map returns a
        /// reference whose host equals the canonical registry.
        ///
        /// Traces: plan Testing Strategy — "identity when no mirror".
        #[test]
        fn transport_reference_is_identity_when_no_mirror() {
            let client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            let id = make_id_with_tag("ghcr.io", "owner/tool", "1.0");
            let reference = client.transport_reference(&id);
            assert_eq!(
                reference.registry(),
                "ghcr.io",
                "empty MirrorMap must leave registry unchanged"
            );
            assert_eq!(reference.repository(), "owner/tool");
        }

        /// `transport_reference` with a mirrored host rewrites to the mirror
        /// host + path-prefix-joined repository.
        ///
        /// Traces: plan Testing Strategy — "transport_reference rewrites a
        /// mirrored read identifier (host+repo+tag/digest verbatim)".
        #[test]
        fn transport_reference_rewrites_mirrored_host_and_repository() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "company.jfrog.io", "ghcr-remote");

            let id = make_id_with_tag("ghcr.io", "owner/tool", "1.0");
            let reference = client.transport_reference(&id);
            assert_eq!(
                reference.registry(),
                "company.jfrog.io",
                "registry must be rewritten to the mirror host"
            );
            assert_eq!(
                reference.repository(),
                "ghcr-remote/owner/tool",
                "repository must be <prefix>/<upstream-repo>"
            );
        }

        /// The returned reference's `registry()` is the MIRROR host — this
        /// proves that auth keys off the mirror host, not the upstream.
        ///
        /// Traces: plan Testing Strategy — "the returned `native::Reference`
        /// `registry()` is the MIRROR host (proves auth keys off mirror)";
        /// ADR F1/R5.
        #[test]
        fn transport_reference_registry_is_mirror_host_for_auth() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "enterprise.artifactory.corp", "ghcr-proxy");

            let id = make_id_with_tag("ghcr.io", "my-org/my-tool", "v2.0");
            let reference = client.transport_reference(&id);

            // This is the host that NativeTransport::auth_for keys off — it
            // must be the mirror host so mirror credentials are used, not
            // upstream credentials.
            assert_eq!(
                reference.registry(),
                "enterprise.artifactory.corp",
                "reference.registry() must be the mirror host so auth resolves against it"
            );
        }

        /// Tag is copied verbatim from the original identifier.
        ///
        /// Traces: plan Testing Strategy — "host+repo+tag/digest verbatim".
        #[test]
        fn transport_reference_tag_copied_verbatim() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "mirror.corp", "proxy");

            let id = make_id_with_tag("ghcr.io", "owner/tool", "3.28.1");
            let reference = client.transport_reference(&id);
            assert_eq!(
                reference.tag(),
                Some("3.28.1"),
                "tag must be copied verbatim to the mirror reference"
            );
        }

        // ── Pinned-install (digest) paths — security-critical ───────────────
        //
        // A pinned install resolves through a digest. The transport reference
        // MUST carry the digest verbatim so `verify_blob_digest` checks the
        // bytes against the SAME digest the caller pinned — under both the
        // mirror and no-mirror paths. A dropped or altered digest here would
        // silently weaken the tamper gate.

        /// Digest-only identifier (pinned install, no tag): the digest is
        /// preserved verbatim in the transport reference under a mirror.
        ///
        /// Traces: coverage gap #3 — digest-only pinned-install path (mirror).
        #[test]
        fn transport_reference_digest_only_preserves_digest_under_mirror() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "company.jfrog.io", "ghcr-remote");

            let digest = test_digest('a');
            let id = make_id_with_digest("ghcr.io", "owner/tool", digest.clone());
            let reference = client.transport_reference(&id);

            assert_eq!(reference.registry(), "company.jfrog.io", "host must be the mirror");
            assert_eq!(
                reference.repository(),
                "ghcr-remote/owner/tool",
                "repo must be prefixed"
            );
            assert_eq!(
                reference.digest(),
                Some(digest.to_string().as_str()),
                "digest must be preserved verbatim — the pinned tamper gate keys off it"
            );
            assert_eq!(reference.tag(), None, "a digest-only identifier carries no tag");
        }

        /// Digest-only identifier with no mirror: identity reference still
        /// preserves the digest verbatim.
        ///
        /// Traces: coverage gap #3 — digest-only pinned-install path (no mirror).
        #[test]
        fn transport_reference_digest_only_preserves_digest_no_mirror() {
            let client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));

            let digest = test_digest('b');
            let id = make_id_with_digest("ghcr.io", "owner/tool", digest.clone());
            let reference = client.transport_reference(&id);

            assert_eq!(reference.registry(), "ghcr.io", "no mirror → canonical host");
            assert_eq!(reference.repository(), "owner/tool", "no mirror → canonical repo");
            assert_eq!(
                reference.digest(),
                Some(digest.to_string().as_str()),
                "digest must be preserved verbatim on the no-mirror identity path"
            );
        }

        /// Tag+digest identifier: BOTH the tag and the digest are preserved
        /// verbatim under a mirror (the digest is what pins the install).
        ///
        /// Traces: coverage gap #3 — tag+digest pinned-install path (mirror).
        #[test]
        fn transport_reference_tag_and_digest_preserved_under_mirror() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "company.jfrog.io", "ghcr-remote");

            let digest = test_digest('c');
            let id = make_id_with_tag_and_digest("ghcr.io", "owner/tool", "3.28.1", digest.clone());
            let reference = client.transport_reference(&id);

            assert_eq!(reference.registry(), "company.jfrog.io", "host must be the mirror");
            assert_eq!(
                reference.repository(),
                "ghcr-remote/owner/tool",
                "repo must be prefixed"
            );
            assert_eq!(reference.tag(), Some("3.28.1"), "tag must be preserved verbatim");
            assert_eq!(
                reference.digest(),
                Some(digest.to_string().as_str()),
                "digest must be preserved verbatim alongside the tag"
            );
        }

        /// Tag+digest identifier with no mirror: identity reference preserves
        /// both tag and digest verbatim.
        ///
        /// Traces: coverage gap #3 — tag+digest pinned-install path (no mirror).
        #[test]
        fn transport_reference_tag_and_digest_preserved_no_mirror() {
            let client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));

            let digest = test_digest('d');
            let id = make_id_with_tag_and_digest("ghcr.io", "owner/tool", "3.28.1", digest.clone());
            let reference = client.transport_reference(&id);

            assert_eq!(reference.registry(), "ghcr.io", "no mirror → canonical host");
            assert_eq!(reference.repository(), "owner/tool", "no mirror → canonical repo");
            assert_eq!(reference.tag(), Some("3.28.1"), "tag must be preserved verbatim");
            assert_eq!(
                reference.digest(),
                Some(digest.to_string().as_str()),
                "digest must be preserved verbatim on the no-mirror identity path"
            );
        }

        /// `transport_registry` rewrites a catalog registry to the mirror host.
        ///
        /// Traces: plan Testing Strategy — "transport_registry rewrites the
        /// catalog registry".
        #[test]
        fn transport_registry_rewrites_catalog_registry() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "catalog-mirror.corp", "ghcr-catalog");

            let reference = client.transport_registry("ghcr.io");
            assert_eq!(
                reference.registry(),
                "catalog-mirror.corp",
                "transport_registry must rewrite the catalog registry to the mirror host"
            );
            // Pin the empty-repository fix (finding #5): the placeholder
            // repository for a registry-scoped catalog call must be the mirror's
            // path prefix VERBATIM — never `"ghcr-catalog/"` with a trailing
            // slash. oci-client's `_auth` stamps `repository()` into the token
            // scope (`repository:<repository>:pull`); a trailing slash there
            // produces a malformed scope that can break catalog auth against a
            // mirror keying tokens off the repo-key path segment.
            assert_eq!(
                reference.repository(),
                "ghcr-catalog",
                "catalog repository must be the path prefix with no trailing slash"
            );
        }

        /// `transport_registry` is identity when no mirror configured.
        ///
        /// Traces: plan Testing Strategy — "identity when no mirror".
        #[test]
        fn transport_registry_is_identity_when_no_mirror() {
            let client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            let reference = client.transport_registry("quay.io");
            assert_eq!(
                reference.registry(),
                "quay.io",
                "empty MirrorMap must leave catalog registry unchanged"
            );
            assert_eq!(
                reference.repository(),
                "",
                "no-mirror catalog repository stays empty (auth scope keys off the registry only)"
            );
        }

        /// T-A3: bare identifier (no tag, no digest) under a configured mirror.
        ///
        /// The `(None, None)` arm in `transport_reference` emits
        /// `native::Reference::with_tag(host, repository, "latest")`. This test
        /// verifies that:
        /// - the host is rewritten to the mirror host (not the canonical registry), and
        /// - the returned reference carries `tag() == Some("latest")` (the OCI default).
        ///
        /// A bare identifier arises when a user runs `ocx package install cmake`
        /// (no pin, no explicit tag). Under a mirror the reference must point at
        /// the mirror and still carry "latest" so the registry fetch resolves
        /// the correct tag.
        #[test]
        fn transport_reference_bare_identifier_resolves_to_latest_under_mirror() {
            let mut client = Client::with_transport(Box::new(test_transport::StubTransport::new(
                test_transport::StubTransportData::new(),
            )));
            client.mirrors = make_mirror_map("ghcr.io", "company.jfrog.io", "ghcr-remote");

            // Bare identifier: no tag, no digest.
            let bare_id = Identifier::new_registry("owner/tool", "ghcr.io");
            assert!(bare_id.tag().is_none(), "pre-condition: bare id has no tag");
            assert!(bare_id.digest().is_none(), "pre-condition: bare id has no digest");

            let reference = client.transport_reference(&bare_id);

            assert_eq!(
                reference.registry(),
                "company.jfrog.io",
                "bare identifier under mirror must use the mirror host, not ghcr.io"
            );
            assert_eq!(
                reference.repository(),
                "ghcr-remote/owner/tool",
                "bare identifier under mirror must prefix the repository"
            );
            assert_eq!(
                reference.tag(),
                Some("latest"),
                "bare identifier (no tag, no digest) must resolve to 'latest'"
            );
            assert!(
                reference.digest().is_none(),
                "bare identifier must carry no digest in the transport reference"
            );
        }

        /// Push path uses `canonical_reference()` — not `transport_reference`.
        /// The canonical reference is NEVER mirrored, even when the client
        /// has a mirror map for the registry.
        ///
        /// Traces: plan Testing Strategy — "push distinct"; ADR Q5 (push not
        /// mirror-redirected).
        #[test]
        fn push_path_uses_canonical_reference_not_mirror() {
            // canonical_reference() is pub(crate); call it directly on the
            // identifier (as push sites do) and confirm it targets the
            // canonical host, not the mirror.
            let id = make_id_with_tag("ghcr.io", "owner/tool", "1.0");
            let canonical = id.canonical_reference();
            assert_eq!(
                canonical.registry(),
                "ghcr.io",
                "canonical_reference must always target the upstream host, never the mirror"
            );
        }
    }

    // ── T-arch-A1: structural gating test ────────────────────────────────────
    //
    // `canonical_reference` is `pub(crate)` and intentionally callable in-crate,
    // but the in-crate discipline is: read paths must route through
    // `Client::transport_reference` / `transport_registry` (the mirror seams), not
    // call `canonical_reference` directly. The compiler cannot enforce this for
    // in-crate call sites, so we promote it to a source-scanning structural test.
    //
    // Any NEW call site of `canonical_reference` outside the allow-list below must
    // fail this test, forcing an explicit decision: either update the allow-list
    // (with a justification comment) or reroute through the mirror seam.
    //
    // Allow-list rationale (only files that ACTUALLY reference the symbol —
    // adding a file that does not use it would create a latent hole, silently
    // permitting a future read-path call there):
    // - `oci/identifier.rs`  — definition + test helpers (canonical home).
    // - `oci/client.rs`      — the two gated seams + `ensure_auth` push path +
    //                          the manifest-cache keys (cache keyed off the
    //                          canonical identity, mirror-independent by design)
    //                          + test helpers in the `transport_reference` module.
    // - `package/cascade.rs` — push-path cascade test spies keying a manifest
    //                          map by canonical reference (test-only).
    #[test]
    fn canonical_reference_only_used_in_allowed_files() {
        use std::fs;
        use std::path::Path;

        // Allow-list: file paths (relative to the ocx_lib src root) that are
        // permitted to reference `canonical_reference`.
        const ALLOWED_SUFFIXES: &[&str] = &["oci/identifier.rs", "oci/client.rs", "package/cascade.rs"];

        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let src_root = Path::new(manifest_dir).join("src");

        // Recursively collect all `.rs` files under the src root.
        fn collect_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect_rs_files(&path, out);
                } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                    out.push(path);
                }
            }
        }

        let mut rs_files = Vec::new();
        collect_rs_files(&src_root, &mut rs_files);
        assert!(
            !rs_files.is_empty(),
            "source scanner found no .rs files under {}",
            src_root.display()
        );

        let mut offenders: Vec<String> = Vec::new();
        for file_path in &rs_files {
            let content = fs::read_to_string(file_path).unwrap_or_default();
            if !content.contains("canonical_reference") {
                continue;
            }

            // Check whether this file is in the allow-list.
            let path_str = file_path.to_string_lossy();
            let allowed = ALLOWED_SUFFIXES.iter().any(|suffix| {
                // Normalise separators so the check works on all platforms.
                path_str.replace('\\', "/").ends_with(suffix)
            });
            if !allowed {
                offenders.push(path_str.into_owned());
            }
        }

        assert!(
            offenders.is_empty(),
            "T-arch-A1: `canonical_reference` found in file(s) outside the allow-list \
             (read paths must route through Client::transport_reference / transport_registry):\n  {}",
            offenders.join("\n  ")
        );
    }
}
