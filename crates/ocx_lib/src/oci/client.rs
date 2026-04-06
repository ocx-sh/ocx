// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    ACCEPTED_MANIFEST_MEDIA_TYPES, MEDIA_TYPE_DESCRIPTION_V1, MEDIA_TYPE_MARKDOWN, MEDIA_TYPE_OCI_EMPTY_CONFIG,
    MEDIA_TYPE_OCI_IMAGE_INDEX, MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_PACKAGE_METADATA_V1, MEDIA_TYPE_PACKAGE_V1,
    MEDIA_TYPE_PNG, MEDIA_TYPE_SVG, Result, archive, compression, log, media_type_file_ext, media_type_from_path,
    media_type_select, media_type_select_some, oci,
    package::{self, info::Info, metadata, tag::InternalTag},
    prelude::SerdeExt,
    utility,
};

use tracing::Instrument;

use super::{Digest, Identifier, native};

mod builder;
pub mod error;
pub(crate) mod native_transport;
mod progress_writer;
#[cfg(test)]
pub(crate) mod test_transport;
mod transport;

pub use builder::ClientBuilder;
pub use transport::OciTransport;

use error::ClientError;

pub struct Client {
    transport: Box<dyn OciTransport>,
    pub(super) lock_timeout: std::time::Duration,
    pub(super) tag_chunk_size: usize,
    pub(super) repository_chunk_size: usize,
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.box_clone(),
            lock_timeout: self.lock_timeout,
            tag_chunk_size: self.tag_chunk_size,
            repository_chunk_size: self.repository_chunk_size,
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
        }
    }

    // ── Authentication ─────────────────────────────────────────────

    /// Pre-authenticate against the registry for `identifier` with the
    /// given operation scope.
    ///
    /// Call at the start of a command or task to fail fast on credential
    /// issues (expired tokens, GPG agent prompts, missing env vars)
    /// before beginning any real work.
    pub async fn ensure_auth(&self, identifier: &Identifier, operation: oci::RegistryOperation) -> Result<()> {
        let image = native::Reference::from(identifier);
        self.transport.ensure_auth(&image, operation).await?;
        Ok(())
    }

    // ── Index operations ─────────────────────────────────────────────

    /// Lists the tags for the given image reference.
    /// There is no validation that the tags correspond to valid package versions.
    pub async fn list_tags(&self, identifier: Identifier) -> Result<Vec<String>> {
        let image = native::Reference::from(&identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        let chunk_size = self.tag_chunk_size;
        let tags = paginate(chunk_size, |cs, last| self.transport.list_tags(&image, cs, last)).await?;
        log::trace!("Listed tags for {}: {:?}", identifier, tags);
        Ok(tags)
    }

    pub async fn list_repositories(&self, registry: impl Into<String>) -> Result<Vec<String>> {
        let registry = registry.into();
        let image = oci::native::Reference::with_tag(registry.clone(), "n/a".into(), "latest".into());
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;
        let chunk_size = self.repository_chunk_size;
        let repositories = paginate(chunk_size, |cs, last| self.transport.catalog(&image, cs, last)).await?;
        log::trace!("Listed repositories for {}: {:?}", registry, repositories);
        Ok(repositories)
    }

    /// Fetches the digest of a manifest from the remote, trying to avoid pulling the entire manifest if possible.
    pub async fn fetch_manifest_digest(&self, identifier: &Identifier) -> Result<oci::Digest> {
        let ref_ = native::Reference::from(identifier);
        self.transport.ensure_auth(&ref_, oci::RegistryOperation::Pull).await?;
        let digest = self.transport.fetch_manifest_digest(&ref_).await?;
        log::trace!("Fetched manifest digest for {}: {}", identifier, digest);
        Ok(digest.try_into()?)
    }

    /// Fetches the manifest for the given image reference, returning both the manifest and its digest.
    pub async fn fetch_manifest(&self, identifier: &Identifier) -> Result<(Digest, oci::Manifest)> {
        let ref_ = native::Reference::from(identifier);
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
        let ref_ = native::Reference::from(&target_identifier);
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
                        let entry = oci::ImageIndexEntry {
                            media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                            digest: digest_str,
                            size: blob.len() as i64,
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
        let index_digest = Digest::sha256(&index_data);
        self.transport
            .push_manifest_raw(&ref_, index_data, MEDIA_TYPE_OCI_IMAGE_INDEX)
            .await?;
        log::debug!("Successfully merged platform entry into index for {}", ref_);

        Ok((index_digest, index))
    }

    // ── Package pull ─────────────────────────────────────────────────
    //
    // Three composable methods for fetching a package from a registry:
    //
    //   pull_manifest  → ImageManifest   (validate digest, media types, layers)
    //   pull_metadata  → Metadata        (fetch config blob — deps, env, etc.)
    //   pull_content   → TempAcquireResult (download content blob, extract, codesign)
    //
    // Both `pull_metadata` and `pull_content` accept an optional manifest.
    // If `None`, they call `pull_manifest` internally.

    /// Fetches and validates the OCI manifest for a pinned package.
    ///
    /// Verifies the manifest digest matches the identifier.
    /// Returns the [`ImageManifest`](oci::ImageManifest) without asserting media types.
    pub async fn pull_manifest(
        &self,
        identifier: &oci::PinnedIdentifier,
    ) -> std::result::Result<oci::ImageManifest, ClientError> {
        let expected_digest = identifier.digest().to_string();
        let image = native::Reference::from(&**identifier);

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

    /// Fetches the package metadata from the config blob (~1KB).
    ///
    /// If `manifest` is `None`, fetches it via [`pull_manifest`](Self::pull_manifest).
    pub async fn pull_metadata(
        &self,
        identifier: &oci::PinnedIdentifier,
        manifest: Option<&oci::ImageManifest>,
    ) -> std::result::Result<metadata::Metadata, ClientError> {
        let owned;
        let manifest = match manifest {
            Some(m) => m,
            None => {
                owned = self.pull_manifest(identifier).await?;
                &owned
            }
        };

        media_type_select(&manifest.config.media_type, &[MEDIA_TYPE_PACKAGE_METADATA_V1])
            .map_err(|e| ClientError::InvalidManifest(e.to_string()))?;

        let image = native::Reference::from(&**identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Pull).await?;

        let bytes = self.transport.pull_blob(&image, &manifest.config.digest).await?;
        serde_json::from_slice(&bytes).map_err(ClientError::Serialization)
    }

    /// Downloads the content blob and extracts it into a temp directory.
    ///
    /// Takes ownership of `temp` — on error, the temp directory is cleaned up
    /// automatically. On success, returns the [`TempAcquireResult`] back.
    ///
    /// The temp directory will contain after success:
    /// - `metadata.json` (serialized metadata)
    /// - `content/` (extracted archive)
    /// - `manifest.json` (OCI manifest for audit)
    ///
    /// If `manifest` is `None`, fetches it via [`pull_manifest`](Self::pull_manifest).
    pub async fn pull_content(
        &self,
        identifier: &oci::PinnedIdentifier,
        manifest: Option<&oci::ImageManifest>,
        metadata: &metadata::Metadata,
        output_dir: &std::path::Path,
    ) -> std::result::Result<(), ClientError> {
        let owned;
        let manifest = match manifest {
            Some(m) => m,
            None => {
                owned = self.pull_manifest(identifier).await?;
                &owned
            }
        };

        media_type_select_some(&manifest.artifact_type, &[MEDIA_TYPE_PACKAGE_V1])
            .map_err(|e| ClientError::InvalidManifest(e.to_string()))?;
        if manifest.layers.is_empty() {
            return Err(ClientError::InvalidManifest("manifest has no layers".to_string()));
        }

        let mut temp_guard = utility::fs::DropFile::new(output_dir.to_path_buf());
        let image = native::Reference::from(&**identifier);

        // Write metadata.json to temp so it's available for the final move.
        metadata
            .write_json(output_dir.join("metadata.json"))
            .await
            .map_err(ClientError::internal)?;

        let blob_layer = &manifest.layers[0];
        let blob_compression =
            compression::CompressionAlgorithm::from_media_type(&blob_layer.media_type).ok_or_else(|| {
                ClientError::InvalidManifest(format!("unsupported layer media type: {}", blob_layer.media_type))
            })?;
        let blob_file_ext = media_type_file_ext(&blob_layer.media_type).unwrap_or("blob");
        let content_path = output_dir.join("content");
        let blob_path = content_path.with_added_extension(blob_file_ext);
        let blob_total_size = u64::try_from(blob_layer.size).unwrap_or(0);

        log::info!("Downloading package {} to temp {}", identifier, output_dir.display());

        let bar = crate::cli::progress::ProgressBar::bytes(
            tracing::info_span!("Downloading", package = %identifier),
            blob_total_size,
            identifier,
        );
        let on_progress = bar.callback();

        {
            let _guard = bar.enter();
            self.transport
                .pull_blob_to_file(&image, &blob_layer.digest, &blob_path, blob_total_size, on_progress)
                .await?;
        }

        self.extract_to_temp(identifier, metadata, blob_compression, blob_file_ext, output_dir)
            .await?;

        // Write manifest.json for audit.
        manifest
            .write_json(output_dir.join("manifest.json"))
            .await
            .map_err(ClientError::internal)?;

        temp_guard.retain();
        Ok(())
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

        let image = native::Reference::from(&**identifier);

        log::info!(
            "Downloading layer {} to {}",
            &layer.digest[..std::cmp::min(19, layer.digest.len())],
            output_dir.display()
        );

        let bar = crate::cli::progress::ProgressBar::bytes(
            tracing::info_span!("Downloading", package = %identifier),
            blob_total_size,
            identifier,
        );
        let on_progress = bar.callback();

        {
            let _guard = bar.enter();
            self.transport
                .pull_blob_to_file(&image, &layer.digest, &blob_path, blob_total_size, on_progress)
                .await?;
        }

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

                let extract_span = crate::cli::progress::spinner_span(
                    tracing::info_span!("Extracting", package = %identifier),
                    identifier,
                );

                let extract_options = archive::ExtractOptions {
                    algorithm: Some(blob_compression),
                    strip_components: bundle.strip_components.unwrap_or(0).into(),
                };
                async {
                    archive::Archive::extract_with_options(&blob_path, &temp_content_path, Some(extract_options))
                        .await
                        .map_err(ClientError::internal)
                }
                .instrument(extract_span)
                .await?;
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
        file: impl AsRef<std::path::Path>,
    ) -> Result<(Digest, oci::Manifest)> {
        let path = file.as_ref();
        log::debug!(
            "Pushing package {} from file {}",
            package_info.identifier,
            path.display()
        );

        let image = native::Reference::from(&package_info.identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Push).await?;

        let (manifest, manifest_data, manifest_sha256) = self.push_image_manifest(&package_info, path).await?;

        let (index_digest, index) = self
            .update_image_index(&package_info, &manifest_data, &manifest_sha256)
            .await?;

        drop(manifest);
        Ok((index_digest, oci::Manifest::ImageIndex(index)))
    }

    /// Pushes config blob + package blob + image manifest. Returns the manifest,
    /// its serialized bytes, and its SHA-256 digest string.
    pub(crate) async fn push_image_manifest(
        &self,
        package_info: &Info,
        path: &std::path::Path,
    ) -> std::result::Result<(oci::ImageManifest, Vec<u8>, String), ClientError> {
        let image = native::Reference::from(&package_info.identifier);

        let package_media_type = media_type_from_path(path)
            .map(|mt| mt.to_string())
            .ok_or_else(|| ClientError::InvalidManifest(format!("unsupported archive: {}", path.display())))?;

        let package_data = tokio::fs::read(path).await.map_err(|e| ClientError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let package_data_len = package_data.len();
        let package_digest = Digest::sha256(&package_data).to_string();

        log::trace!("Calculated package digest: {}", package_digest);

        {
            let bar = crate::cli::progress::ProgressBar::bytes(
                tracing::info_span!("Uploading", package = %package_info.identifier),
                package_data_len as u64,
                &package_info.identifier,
            );
            let on_progress = bar.callback();

            let _guard = bar.enter();
            self.transport
                .push_blob(&image, package_data, &package_digest, on_progress)
                .await?;
        }

        // Config blob — tiny, no progress needed.
        let config_data = serde_json::to_vec(&package_info.metadata).map_err(ClientError::Serialization)?;
        let config_data_len = config_data.len();
        let config_sha256 = Digest::sha256(&config_data).to_string();
        log::trace!("Calculated config digest: {}", config_sha256);
        self.transport
            .push_blob(&image, config_data, &config_sha256, transport::no_progress())
            .await?;

        let manifest = oci::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
            config: oci::Descriptor {
                media_type: MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                digest: config_sha256,
                size: config_data_len as i64,
                urls: None,
                annotations: None,
            },
            layers: vec![oci::Descriptor {
                media_type: package_media_type,
                digest: package_digest,
                size: package_data_len as i64,
                urls: None,
                annotations: None,
            }],
            annotations: None,
            ..Default::default()
        };

        let manifest_data = serde_json::to_vec(&manifest).map_err(ClientError::Serialization)?;
        let manifest_sha256 = Digest::sha256(&manifest_data).to_string();
        let canonical_image = image.clone_with_digest(manifest_sha256.clone());

        let pushed_digest = self
            .transport
            .push_manifest_raw(&canonical_image, manifest_data.clone(), MEDIA_TYPE_OCI_IMAGE_MANIFEST)
            .await?;
        log::debug!("Pushed manifest with digest '{}'", pushed_digest);

        Ok((manifest, manifest_data, manifest_sha256))
    }

    /// Fetches (or creates) the image index, adds the new manifest entry for the
    /// package platform, and pushes the updated index.
    ///
    /// Delegates to [`merge_platform_into_index`](Self::merge_platform_into_index).
    async fn update_image_index(
        &self,
        package_info: &Info,
        manifest_data: &[u8],
        manifest_sha256: &str,
    ) -> std::result::Result<(Digest, oci::ImageIndex), ClientError> {
        let tag = package_info.identifier.tag_or_latest().to_string();
        let manifest_size = manifest_data.len() as i64;

        let (digest, index) = self
            .merge_platform_into_index(
                &package_info.identifier,
                &tag,
                &package_info.platform,
                manifest_sha256,
                manifest_size,
            )
            .await
            .map_err(ClientError::internal)?;

        Ok((digest, index))
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
        let image = super::native::Reference::from(&desc_identifier);
        self.transport.ensure_auth(&image, oci::RegistryOperation::Push).await?;

        let config_data = b"{}".to_vec();
        let config_digest = Digest::sha256(&config_data).to_string();
        self.transport
            .push_blob(&image, config_data, &config_digest, transport::no_progress())
            .await?;

        let readme_bytes = description.readme.as_bytes();
        let readme_len = readme_bytes.len();
        let readme_digest = Digest::sha256(readme_bytes).to_string();
        self.transport
            .push_blob(&image, readme_bytes.to_vec(), &readme_digest, transport::no_progress())
            .await?;

        let mut layers = vec![oci::Descriptor {
            media_type: MEDIA_TYPE_MARKDOWN.to_string(),
            digest: readme_digest,
            size: readme_len as i64,
            urls: None,
            annotations: Some([(oci::annotations::TITLE.to_string(), "README.md".to_string())].into()),
        }];

        if let Some(logo) = &description.logo {
            let logo_len = logo.data.len();
            let logo_digest = Digest::sha256(&logo.data).to_string();
            self.transport
                .push_blob(&image, logo.data.clone(), &logo_digest, transport::no_progress())
                .await?;

            let ext = match logo.media_type {
                MEDIA_TYPE_PNG => "png",
                MEDIA_TYPE_SVG => "svg",
                _ => "bin",
            };
            layers.push(oci::Descriptor {
                media_type: logo.media_type.to_string(),
                digest: logo_digest,
                size: logo_len as i64,
                urls: None,
                annotations: Some([(oci::annotations::TITLE.to_string(), format!("logo.{ext}"))].into()),
            });
        }

        let manifest_annotations = if description.annotations.is_empty() {
            None
        } else {
            Some(description.annotations.clone())
        };

        let manifest = oci::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(MEDIA_TYPE_DESCRIPTION_V1.to_string()),
            config: oci::Descriptor {
                media_type: MEDIA_TYPE_OCI_EMPTY_CONFIG.to_string(),
                digest: config_digest,
                size: 2,
                urls: None,
                annotations: None,
            },
            layers,
            annotations: manifest_annotations,
            ..Default::default()
        };

        let manifest_data = serde_json::to_vec(&manifest).map_err(ClientError::Serialization)?;

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
        let image = super::native::Reference::from(&desc_identifier);
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
            self.transport
                .pull_blob_to_file(&image, &layer.digest, &blob_path, 0, transport::no_progress())
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
    fn make_image_manifest(config_digest: &str, layer_digest: &str) -> oci::ImageManifest {
        oci::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
            config: oci::Descriptor {
                media_type: MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                digest: config_digest.to_string(),
                size: 100,
                urls: None,
                annotations: None,
            },
            layers: vec![oci::Descriptor {
                media_type: crate::MEDIA_TYPE_TAR_GZ.to_string(),
                digest: layer_digest.to_string(),
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
        let digest = Digest::sha256(&data).to_string();
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
        let manifest = oci::Manifest::Image(make_image_manifest("sha256:cfg", "sha256:layer"));
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
        let manifest = oci::Manifest::Image(make_image_manifest("sha256:cfg", "sha256:layer"));
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
        assert!(err_msg.contains("digest mismatch"), "got: {}", err_msg);
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
        let mut m = make_image_manifest("sha256:cfg", "sha256:layer");
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

    // ── pull_metadata tests ─────────────────────────────────────

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
    async fn pull_metadata_success() {
        let metadata_json = br#"{"type":"bundle","version":1}"#;
        let data = StubTransportData::new();
        let manifest = make_image_manifest("sha256:cfg", "sha256:layer");
        let id = setup_manifest_and_blob(&data, manifest, metadata_json);
        let client = stub(&data);

        let result = client.pull_metadata(&id, None).await;
        assert!(result.is_ok(), "expected success, got: {:?}", result.err());
    }

    #[tokio::test]
    async fn pull_metadata_rejects_wrong_config_media_type() {
        let metadata_json = br#"{"type":"bundle","version":1}"#;
        let mut manifest = make_image_manifest("sha256:cfg", "sha256:layer");
        manifest.config.media_type = "application/vnd.wrong".to_string();

        let data = StubTransportData::new();
        let id = setup_manifest_and_blob(&data, manifest.clone(), metadata_json);
        let client = stub(&data);

        let result = client.pull_metadata(&id, Some(&manifest)).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Invalid manifest"), "got: {}", err_msg);
    }

    // ── pull_content tests ──────────────────────────────────────

    #[tokio::test]
    async fn pull_content_rejects_wrong_artifact_type() {
        let mut manifest = make_image_manifest("sha256:cfg", "sha256:layer");
        manifest.artifact_type = Some("application/vnd.wrong".to_string());

        let data = StubTransportData::new();
        let id = setup_manifest_and_blob(&data, manifest.clone(), b"{}");
        let client = stub(&data);

        let metadata: metadata::Metadata = serde_json::from_str(r#"{"type":"bundle","version":1}"#).unwrap();
        let dir = tempfile::tempdir().unwrap();

        match client.pull_content(&id, Some(&manifest), &metadata, dir.path()).await {
            Err(e) => assert!(e.to_string().contains("Invalid manifest"), "got: {}", e),
            Ok(_) => panic!("should reject wrong artifact type"),
        }
    }

    #[tokio::test]
    async fn pull_content_rejects_empty_layers() {
        let mut manifest = make_image_manifest("sha256:cfg", "sha256:layer");
        manifest.layers.clear();

        let data = StubTransportData::new();
        let id = setup_manifest_and_blob(&data, manifest.clone(), b"{}");
        let client = stub(&data);

        let metadata: metadata::Metadata = serde_json::from_str(r#"{"type":"bundle","version":1}"#).unwrap();
        let dir = tempfile::tempdir().unwrap();

        match client.pull_content(&id, Some(&manifest), &metadata, dir.path()).await {
            Err(e) => assert!(e.to_string().contains("no layers"), "got: {}", e),
            Ok(_) => panic!("should reject empty layers"),
        }
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
                .get(&native::Reference::from(&id).to_string())
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
            let existing_digest = oci::Digest::sha256(&existing_bytes).to_string();
            data.write().manifests.insert(
                native::Reference::from(&id).to_string(),
                (existing_bytes, existing_digest),
            );

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
            let existing_digest = oci::Digest::sha256(&existing_bytes).to_string();
            data.write().manifests.insert(
                native::Reference::from(&id).to_string(),
                (existing_bytes, existing_digest),
            );

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
            let manifest_digest = oci::Digest::sha256(&manifest_bytes).to_string();
            data.write().manifests.insert(
                native::Reference::from(&id).to_string(),
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
            let manifest = oci::Manifest::Image(make_image_manifest("sha256:cfg", "sha256:layer"));
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
                }),
                platform: "linux/amd64".parse().unwrap(),
            };

            let _ = client.push_package(info, &archive_path).await;
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
                }),
                platform: "linux/amd64".parse().unwrap(),
            };

            let _ = client.push_package(info, &archive_path).await;

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
}
