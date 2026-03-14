// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    ACCEPTED_MANIFEST_MEDIA_TYPES, MEDIA_TYPE_DESCRIPTION_V1, MEDIA_TYPE_MARKDOWN, MEDIA_TYPE_OCI_EMPTY_CONFIG,
    MEDIA_TYPE_OCI_IMAGE_INDEX, MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_PACKAGE_METADATA_V1, MEDIA_TYPE_PACKAGE_V1,
    MEDIA_TYPE_PNG, MEDIA_TYPE_SVG, Result, archive, compression, log, media_type_file_ext, media_type_from_path,
    media_type_select, media_type_select_some, oci,
    package::{self, description::DESCRIPTION_TAG, info::Info, install_info, install_status, metadata},
    prelude::SerdeExt,
    utility,
};

use super::{Digest, Identifier, native};

mod builder;
pub mod error;
pub(crate) mod native_transport;
#[cfg(test)]
pub(crate) mod test_transport;
mod transport;

pub use builder::ClientBuilder;
pub use transport::OciTransport;

use error::ClientError;

/// Result of downloading manifest + blobs to the temp directory.
struct TempDownload {
    manifest: oci::ImageManifest,
    metadata: metadata::Metadata,
    blob_compression: compression::CompressionAlgorithm,
    blob_file_ext: &'static str,
}

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

    // ── Index operations ─────────────────────────────────────────────

    /// Lists the tags for the given image reference.
    /// There is no validation that the tags correspond to valid package versions.
    pub async fn list_tags(&self, identifier: Identifier) -> Result<Vec<String>> {
        let image = native::Reference::from(&identifier);
        let chunk_size = self.tag_chunk_size;
        let tags = paginate(chunk_size, |cs, last| self.transport.list_tags(&image, cs, last)).await?;
        log::trace!("Listed tags for {}: {:?}", identifier, tags);
        Ok(tags)
    }

    pub async fn list_repositories(&self, registry: impl Into<String>) -> Result<Vec<String>> {
        let registry = registry.into();
        let image = oci::native::Reference::with_tag(registry.clone(), "n/a".into(), "latest".into());
        let chunk_size = self.repository_chunk_size;
        let repositories = paginate(chunk_size, |cs, last| self.transport.catalog(&image, cs, last)).await?;
        log::trace!("Listed repositories for {}: {:?}", registry, repositories);
        Ok(repositories)
    }

    /// Fetches the digest of a manifest from the remote, trying to avoid pulling the entire manifest if possible.
    pub async fn fetch_manifest_digest(&self, identifier: &Identifier) -> Result<oci::Digest> {
        let ref_ = native::Reference::from(identifier);
        let digest = self.transport.fetch_manifest_digest(&ref_).await?;
        log::trace!("Fetched manifest digest for {}: {}", identifier, digest);
        digest.try_into()
    }

    /// Fetches the manifest for the given image reference, returning both the manifest and its digest.
    pub async fn fetch_manifest(&self, identifier: &Identifier) -> Result<(Digest, oci::Manifest)> {
        let ref_ = native::Reference::from(identifier);
        let (manifest, digest_str) = self.pull_manifest(&ref_).await?;
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
        let platform = Some(platform.clone().into());

        log::debug!("Merging platform entry into index for {}", ref_);
        let mut index = match self
            .transport
            .pull_manifest_raw(&ref_, &[MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_OCI_IMAGE_INDEX])
            .await
        {
            Ok((blob, digest_str)) => {
                let existing: oci::Manifest =
                    serde_json::from_slice(&blob).map_err(|e| ClientError::Serialization(e.to_string()))?;
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

        let index_data = serde_json::to_vec(&index).map_err(|e| ClientError::Serialization(e.to_string()))?;
        let index_digest = Digest::sha256(&index_data);
        self.transport
            .push_manifest_raw(&ref_, index_data, MEDIA_TYPE_OCI_IMAGE_INDEX)
            .await?;
        log::debug!("Successfully merged platform entry into index for {}", ref_);

        Ok((index_digest, index))
    }

    // ── Package pull ─────────────────────────────────────────────────

    pub async fn pull_package(
        &self,
        identifier: Identifier,
        output_path: impl AsRef<std::path::Path>,
        temp: crate::file_structure::TempAcquireResult,
    ) -> Result<install_info::InstallInfo> {
        let output_path = output_path.as_ref().to_path_buf();
        let temp_path = &temp.dir.dir;
        log::debug!("Pulling package {} to {}", identifier, output_path.display());

        let identifier_digest = identifier
            .digest()
            .map(|d| d.to_string())
            .ok_or_else(|| ClientError::InvalidManifest("identifier must carry a digest".into()))?;

        // Check if already installed at the final output path.
        let metadata_path = output_path.join("metadata.json");
        let content_path = output_path.join("content");
        let install_path = output_path.join("install.json");
        let lock_path = output_path.join("install.lock");

        if let (true, Some(status)) =
            install_status::check_install_status(&install_path, &lock_path, self.lock_timeout).await
        {
            log::debug!(
                "Package '{}' is already installed (ok={}, timestamp={})",
                identifier,
                status.ok,
                status.timestamp
            );
            let metadata = metadata::Metadata::read_json_from_path(&metadata_path)?;
            return Ok(install_info::InstallInfo {
                identifier,
                metadata,
                content: content_path,
            });
        }

        let mut temp_guard = utility::drop_file::DropFile::new(temp_path.to_path_buf());
        let download = self
            .download_to_temp(&identifier, &identifier_digest, temp_path)
            .await?;
        self.extract_and_finalize(&identifier, download, temp_path, &output_path)
            .await?;

        // Clean up temp directory — files have been moved to the output path.
        temp_guard.unlink().ok();
        drop(temp);

        let metadata = metadata::Metadata::read_json_from_path(output_path.join("metadata.json"))?;
        Ok(install_info::InstallInfo {
            identifier,
            metadata,
            content: output_path.join("content"),
        })
    }

    /// Downloads manifest and blobs to the temp directory, validating the manifest.
    async fn download_to_temp(
        &self,
        identifier: &Identifier,
        expected_digest: &str,
        temp_path: &std::path::Path,
    ) -> std::result::Result<TempDownload, ClientError> {
        let image = native::Reference::from(identifier);

        let (manifest, digest_str) = self.pull_manifest(&image).await?;
        if digest_str != expected_digest {
            return Err(ClientError::DigestMismatch {
                expected: expected_digest.to_string(),
                actual: digest_str,
            });
        }
        let manifest = match manifest {
            oci::Manifest::Image(m) => m,
            _ => return Err(ClientError::UnexpectedManifestType),
        };

        media_type_select(&manifest.config.media_type, &[MEDIA_TYPE_PACKAGE_METADATA_V1])
            .map_err(|e| ClientError::InvalidManifest(e.to_string()))?;
        media_type_select_some(&manifest.artifact_type, &[MEDIA_TYPE_PACKAGE_V1])
            .map_err(|e| ClientError::InvalidManifest(e.to_string()))?;

        if manifest.layers.len() != 1 {
            return Err(ClientError::InvalidManifest(format!(
                "expected exactly 1 layer, got {}",
                manifest.layers.len()
            )));
        }
        let blob_layer = &manifest.layers[0];
        let blob_compression =
            compression::CompressionAlgorithm::from_media_type(&blob_layer.media_type).ok_or_else(|| {
                ClientError::InvalidManifest(format!("unsupported layer media type: {}", blob_layer.media_type))
            })?;
        let blob_file_ext = media_type_file_ext(&blob_layer.media_type).unwrap_or("blob");

        let metadata_path = temp_path.join("metadata.json");
        let content_path = temp_path.join("content");
        let blob_path = content_path.with_added_extension(blob_file_ext);

        log::info!(
            "Pulling package {} with digest {} to temp {}",
            identifier,
            expected_digest,
            temp_path.display()
        );
        self.transport
            .pull_blob_to_file(&image, &manifest.config.digest, &metadata_path)
            .await?;
        self.transport
            .pull_blob_to_file(&image, &blob_layer.digest, &blob_path)
            .await?;

        let metadata = metadata::Metadata::read_json_from_path(&metadata_path)
            .map_err(|e| ClientError::Serialization(e.to_string()))?;

        Ok(TempDownload {
            manifest,
            metadata,
            blob_compression,
            blob_file_ext,
        })
    }

    /// Extracts the downloaded archive and moves files from temp to the final output path.
    async fn extract_and_finalize(
        &self,
        identifier: &Identifier,
        download: TempDownload,
        temp_path: &std::path::Path,
        output_path: &std::path::Path,
    ) -> std::result::Result<(), ClientError> {
        let temp_content_path = temp_path.join("content");
        let blob_path = temp_content_path.with_added_extension(download.blob_file_ext);
        let _drop_blob = utility::drop_file::DropFile::new(blob_path.clone());

        match &download.metadata {
            metadata::Metadata::Bundle(bundle) => {
                log::debug!(
                    "Extracting bundle package {} to {}",
                    identifier,
                    temp_content_path.display()
                );
                let extract_options = archive::ExtractOptions {
                    algorithm: Some(download.blob_compression),
                    strip_components: bundle.strip_components.unwrap_or(0).into(),
                };
                archive::Archive::extract_with_options(&blob_path, &temp_content_path, Some(extract_options))
                    .await
                    .map_err(|e| ClientError::Io(blob_path.clone(), std::io::Error::other(e.to_string())))?;
            }
        }

        // Move temp contents to the final output path.
        // Wrap in DropFile so partial output is cleaned up on failure.
        std::fs::create_dir_all(output_path).map_err(|e| ClientError::Io(output_path.to_path_buf(), e))?;
        let mut output_guard = utility::drop_file::DropFile::new(output_path.to_path_buf());

        let final_metadata = output_path.join("metadata.json");
        let final_content = output_path.join("content");
        let final_manifest = output_path.join("manifest.json");
        let final_install = output_path.join("install.json");

        Self::move_path(&temp_path.join("metadata.json"), &final_metadata)?;
        Self::move_path(&temp_content_path, &final_content)?;

        crate::codesign::sign_extracted_content(&final_content)
            .await
            .map_err(|e| ClientError::Internal(Box::new(e)))?;

        download
            .manifest
            .write_json_to_path(&final_manifest)
            .map_err(|e| ClientError::Serialization(e.to_string()))?;

        let install_status = package::install_status::InstallStatus::new().ok();
        install_status
            .write_json_to_path(&final_install)
            .map_err(|e| ClientError::Serialization(e.to_string()))?;

        // All steps succeeded — keep the output directory.
        output_guard.retain();
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

        // Read file and calculate digest for content-addressing.
        let package_data = tokio::fs::read(path)
            .await
            .map_err(|e| ClientError::Io(path.to_path_buf(), e))?;
        let package_data_len = package_data.len();
        let package_digest = Digest::sha256(&package_data).to_string();

        log::trace!("Calculated package digest: {}", package_digest);
        self.transport.push_blob(&image, package_data, &package_digest).await?;

        let config_data =
            serde_json::to_vec(&package_info.metadata).map_err(|e| ClientError::Serialization(e.to_string()))?;
        let config_data_len = config_data.len();
        let config_sha256 = Digest::sha256(&config_data).to_string();
        log::trace!("Calculated config digest: {}", config_sha256);
        self.transport.push_blob(&image, config_data, &config_sha256).await?;

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
            ..Default::default()
        };

        let manifest_data = serde_json::to_vec(&manifest).map_err(|e| ClientError::Serialization(e.to_string()))?;
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
            .map_err(|e| ClientError::Registry(e.to_string()))?;

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
        let desc_identifier = identifier.clone_with_tag(DESCRIPTION_TAG);
        let image = super::native::Reference::from(&desc_identifier);

        // Push empty config blob.
        let config_data = b"{}".to_vec();
        let config_digest = Digest::sha256(&config_data).to_string();
        self.transport.push_blob(&image, config_data, &config_digest).await?;

        // Push README blob.
        let readme_bytes = description.readme.as_bytes();
        let readme_len = readme_bytes.len();
        let readme_digest = Digest::sha256(readme_bytes).to_string();
        self.transport
            .push_blob(&image, readme_bytes.to_vec(), &readme_digest)
            .await?;

        let mut layers = vec![oci::Descriptor {
            media_type: MEDIA_TYPE_MARKDOWN.to_string(),
            digest: readme_digest,
            size: readme_len as i64,
            urls: None,
            annotations: Some([(oci::annotations::TITLE.to_string(), "README.md".to_string())].into()),
        }];

        // Push logo blob if provided.
        if let Some(logo) = &description.logo {
            let logo_len = logo.data.len();
            let logo_digest = Digest::sha256(&logo.data).to_string();
            self.transport
                .push_blob(&image, logo.data.clone(), &logo_digest)
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

        let manifest_data = serde_json::to_vec(&manifest).map_err(|e| ClientError::Serialization(e.to_string()))?;

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
        let desc_identifier = identifier.clone_with_tag(DESCRIPTION_TAG);
        let image = super::native::Reference::from(&desc_identifier);

        let (manifest, _digest) = match self.pull_manifest(&image).await {
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

        // Validate artifact type.
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

        // Pull layers by media type into temp files.
        let mut readme: Option<String> = None;
        let mut logo: Option<package::description::Logo> = None;

        for (i, layer) in image_manifest.layers.iter().enumerate() {
            let blob_path = temp_dir.join(format!("layer_{i}"));
            self.transport
                .pull_blob_to_file(&image, &layer.digest, &blob_path)
                .await?;

            match layer.media_type.as_str() {
                MEDIA_TYPE_MARKDOWN => {
                    let data = tokio::fs::read(&blob_path)
                        .await
                        .map_err(|e| ClientError::Io(blob_path, e))?;
                    readme = Some(
                        String::from_utf8(data)
                            .map_err(|e| ClientError::Serialization(format!("README is not valid UTF-8: {e}")))?,
                    );
                }
                MEDIA_TYPE_PNG | MEDIA_TYPE_SVG => {
                    let data = tokio::fs::read(&blob_path)
                        .await
                        .map_err(|e| ClientError::Io(blob_path, e))?;
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
    async fn pull_manifest(&self, image: &oci::Reference) -> std::result::Result<(oci::Manifest, String), ClientError> {
        log::debug!("Pulling manifest for image {}", image);
        let (data, digest) = self
            .transport
            .pull_manifest_raw(image, ACCEPTED_MANIFEST_MEDIA_TYPES)
            .await?;
        let manifest: oci::Manifest =
            serde_json::from_slice(&data).map_err(|e| ClientError::Serialization(e.to_string()))?;
        Ok((manifest, digest))
    }

    /// Moves a file or directory from `src` to `dst` (rename on same filesystem).
    fn move_path(src: &std::path::Path, dst: &std::path::Path) -> std::result::Result<(), ClientError> {
        if src.exists() {
            std::fs::rename(src, dst).map_err(|e| ClientError::Io(src.to_path_buf(), e))?;
        }
        Ok(())
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

    use crate::file_lock;
    use crate::file_structure::{TempAcquireResult, TempDir, TempStore};
    use crate::prelude::SerdeExt;

    // ── Test helpers ─────────────────────────────────────────────────

    fn stub(data: &StubTransportData) -> Client {
        Client::with_transport(Box::new(StubTransport::new(data.clone())))
    }

    /// Acquire a temp directory for use in pull_package tests.
    fn test_acquire(path: &std::path::Path) -> TempAcquireResult {
        std::fs::create_dir_all(path).unwrap();
        let lock_path = path.join("install.lock");
        let file = std::fs::File::create(&lock_path).unwrap();
        let lock = file_lock::FileLock::try_exclusive(file).unwrap();
        TempAcquireResult {
            dir: TempDir {
                dir: path.to_path_buf(),
            },
            lock,
            was_cleaned: false,
        }
    }

    fn test_identifier(tag: &str) -> Identifier {
        Identifier::new_registry("test/pkg", "example.com").clone_with_tag(tag)
    }

    fn test_identifier_with_digest(digest_hex: &str) -> Identifier {
        let digest = oci::Digest::Sha256(digest_hex.to_string());
        Identifier::new_registry("test/pkg", "example.com").clone_with_digest(digest)
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

    /// Write install.json + metadata.json at `output_path` to simulate
    /// a completed installation, so `pull_package` will return early.
    fn write_completed_install(output_path: &std::path::Path) {
        std::fs::create_dir_all(output_path).unwrap();
        let status = package::install_status::InstallStatus::new().ok();
        status.write_json_to_path(output_path.join("install.json")).unwrap();
        let metadata = metadata::Metadata::Bundle(package::metadata::bundle::Bundle {
            version: package::metadata::bundle::Version::V1,
            strip_components: None,
            env: Default::default(),
        });
        metadata.write_json_to_path(output_path.join("metadata.json")).unwrap();
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

    // ── pull_package tests ───────────────────────────────────────────

    #[tokio::test]
    async fn pull_package_digest_mismatch() {
        let manifest = oci::Manifest::Image(make_image_manifest("sha256:cfg", "sha256:layer"));
        let (manifest_data, _real_digest) = serialize_manifest(&manifest);
        let wrong_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

        let id = test_identifier_with_digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");

        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, wrong_digest.to_string()));
        let client = stub(&data);

        let dir = tempfile::tempdir().unwrap();
        let temp = test_acquire(&dir.path().join("temp"));
        let result = client.pull_package(id, dir.path().join("output"), temp).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("digest mismatch"), "got: {}", err_msg);
    }

    #[tokio::test]
    async fn pull_package_unexpected_manifest_type() {
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
        let id = test_identifier_with_digest(digest_hex);

        let data = StubTransportData::new();
        data.write()
            .manifests
            .insert(id.to_string(), (manifest_data, digest_str));
        let client = stub(&data);

        let dir = tempfile::tempdir().unwrap();
        let temp = test_acquire(&dir.path().join("temp"));
        let result = client.pull_package(id, dir.path().join("out"), temp).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("image manifest") || err_msg.contains("image index"),
            "got: {}",
            err_msg
        );
    }

    #[tokio::test]
    async fn pull_package_no_digest_in_identifier() {
        let data = StubTransportData::new();
        let client = stub(&data);
        let id = test_identifier("1.0");

        let dir = tempfile::tempdir().unwrap();
        let temp = test_acquire(&dir.path().join("temp"));
        let result = client.pull_package(id, dir.path().join("out"), temp).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("digest"), "got: {}", err_msg);
    }

    // ── Re-entrant install tests ────────────────────────────────────

    #[tokio::test]
    async fn pull_package_skips_download_when_already_installed() {
        let id = test_identifier_with_digest("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
        let data = StubTransportData::new();
        let client = stub(&data);

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        write_completed_install(&output);

        let temp = test_acquire(&dir.path().join("temp"));
        let info = client.pull_package(id.clone(), &output, temp).await.unwrap();

        // Verify no transport calls were made — install was short-circuited.
        assert!(
            data.read().calls.is_empty(),
            "expected no transport calls, got: {:?}",
            data.read().calls
        );
        assert_eq!(info.content, output.join("content"));
        assert_eq!(info.identifier, id);
    }

    #[tokio::test]
    async fn pull_package_reentrant_second_call_skips_download() {
        // Simulate two sequential "installs" of the same package to the same
        // output path. The first fails (no manifest data configured), but we
        // manually write a completed install status. The second call should
        // detect the completed install and skip the download entirely.
        let id = test_identifier_with_digest("cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc");
        let data = StubTransportData::new();
        let client = stub(&data);
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");

        // First "install" — fails because no manifest is configured.
        let temp1 = test_acquire(&dir.path().join("temp1"));
        let result = client.pull_package(id.clone(), &output, temp1).await;
        assert!(result.is_err());
        assert_eq!(data.read().calls.len(), 1); // pull_manifest_raw was called

        // Simulate successful install by writing status files.
        write_completed_install(&output);

        // Second "install" — should detect existing install and skip download.
        let temp2 = test_acquire(&dir.path().join("temp2"));
        let info = client.pull_package(id.clone(), &output, temp2).await.unwrap();

        // Only the one call from the first attempt; no new transport calls.
        assert_eq!(data.read().calls.len(), 1);
        assert_eq!(info.content, output.join("content"));
    }

    #[tokio::test]
    async fn temp_acquire_cleans_leftover_before_download() {
        let id = test_identifier_with_digest("dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd");
        let data = StubTransportData::new();
        let client = stub(&data);

        let dir = tempfile::tempdir().unwrap();
        let temp_root = dir.path().join("temp_root");
        let temp_path = temp_root.join("some_hash");

        // Simulate leftover artifacts from a crashed download.
        std::fs::create_dir_all(&temp_path).unwrap();
        std::fs::write(temp_path.join("install.lock"), b"").unwrap();
        std::fs::write(temp_path.join("metadata.json"), b"stale").unwrap();
        std::fs::create_dir(temp_path.join("content")).unwrap();

        let store = TempStore::new(&temp_root);
        let acquired = store.try_acquire(&temp_path).unwrap().unwrap();

        // Verify artifacts were cleaned.
        assert!(acquired.was_cleaned);
        assert!(!temp_path.join("metadata.json").exists());
        assert!(!temp_path.join("content").exists());
        assert!(temp_path.join("install.lock").exists());

        // The cleaned temp can be passed to pull_package (which will fail
        // because no manifest is configured, but the point is it accepts it).
        let result = client.pull_package(id, dir.path().join("output"), acquired).await;
        assert!(result.is_err());
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
}
