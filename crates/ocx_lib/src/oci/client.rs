use crate::{
    ACCEPTED_MANIFEST_MEDIA_TYPES, Error, ErrorExt, MEDIA_TYPE_OCI_IMAGE_INDEX, MEDIA_TYPE_OCI_IMAGE_MANIFEST,
    MEDIA_TYPE_PACKAGE_METADATA_V1, MEDIA_TYPE_PACKAGE_V1, Result, archive, auth, compression, file_lock, log,
    media_type_file_ext, media_type_from_path, media_type_select, media_type_select_some, oci,
    package::{self, info::Info, install_info, install_status, metadata},
    prelude::SerdeExt,
    utility,
};

use super::{Digest, Identifier};

mod builder;

pub use builder::ClientBuilder;

#[derive(Clone)]
pub struct Client {
    pub(super) auth: auth::Auth,
    pub(super) client: oci::native::Client,
    pub(super) lock_timeout: std::time::Duration,
    pub(super) tag_chunk_size: usize,
    pub(super) repository_chunk_size: usize,
}

impl Client {
    /// Lists the tags for the given image reference.
    /// There is no validation that the tags correspond to valid package versions.
    pub async fn list_tags(&self, identifier: Identifier) -> Result<Vec<String>> {
        let image = &identifier.reference;
        let mut tags = Vec::new();

        loop {
            let last_tag = match tags.last().map(|tag: &String| tag.as_str()) {
                Some(tag) => Some(tag),
                // Using none with chunk-size will yield invalid response, so we use empty string to get the first chunk.
                None => Some(""),
            };
            let response = self
                .client
                .list_tags(
                    image,
                    &self.auth.get_or_fallback(identifier.registry()).await,
                    Some(self.tag_chunk_size),
                    last_tag,
                )
                .await
                .map_to_undefined_error()?;

            let tags_len = response.tags.len();
            tags.extend(response.tags);

            if tags_len < self.tag_chunk_size {
                break;
            }
        }

        log::info!("Listed tags for {}: {:?}", identifier, tags);
        Ok(tags)
    }

    pub async fn list_repositories(&self, registry: impl Into<String>) -> Result<Vec<String>> {
        let registry = registry.into();
        let image = oci::native::Reference::with_tag(registry.clone(), "n/a".into(), "latest".into());
        let auth = self.auth.get_or_fallback(image.registry()).await;
        let chunk_size = self.repository_chunk_size;

        let mut repositories = Vec::new();
        let mut last = None;
        loop {
            let repositories_page = self
                .client
                .catalog(&image, &auth, Some(chunk_size), last)
                .await
                .map_to_undefined_error()?;
            let page_len = repositories_page.len();
            repositories.extend(repositories_page);
            if page_len < chunk_size {
                break;
            }
            last = repositories.last().map(|s| s.as_str());
        }
        Ok(repositories)
    }

    /// Fetches the digest of a manifest from the remote, trying to avoid pulling the entire manifest if possible.
    pub async fn fetch_manifest_digest(&self, identifier: &Identifier) -> Result<oci::Digest> {
        let image = &identifier.reference;
        let digest = self
            .client
            .fetch_manifest_digest(image, &self.auth.get_or_fallback(identifier.registry()).await)
            .await
            .map_to_undefined_error()?;
        digest.try_into()
    }

    /// Fetches the manifest for the given image reference, returning both the manifest and its digest.
    pub async fn fetch_manifest(&self, identifier: &Identifier) -> Result<(Digest, oci::Manifest)> {
        let image = &identifier.reference;
        self.do_authenticate(image, oci::RegistryOperation::Pull).await?;
        let (manifest, digest) = self.do_pull_manifest(&identifier.reference).await?;
        let digest = digest.try_into()?;
        Ok((digest, manifest))
    }

    pub async fn copy_manifest(&self, source_identifier: &Identifier, target: impl Into<String>) -> Result<()> {
        let (_, source_manifest) = self.fetch_manifest(source_identifier).await?;
        let target_identifier = source_identifier.clone_with_tag(target);

        self.do_authenticate(&target_identifier.reference, oci::RegistryOperation::Push)
            .await?;
        self.client
            .push_manifest(&target_identifier.reference, &source_manifest)
            .await
            .map_to_undefined_error()?;
        Ok(())
    }

    pub async fn pull_package(
        &self,
        identifier: Identifier,
        output_path: impl AsRef<std::path::Path>,
    ) -> Result<install_info::InstallInfo> {
        log::debug!("Pulling package {} to {}", identifier, output_path.as_ref().display());
        let identifier_digest = match identifier.reference.digest() {
            Some(digest) => digest,
            None => return Err(Error::Undefined),
        };

        let output_path = output_path.as_ref().to_path_buf();
        let metadata_path = output_path.join("metadata.json");
        let content_path = output_path.join("content");
        let manifest_path = output_path.join("manifest.json");
        let install_path = output_path.join("install.json");
        let lock_path = output_path.join("install.lock");

        std::fs::create_dir_all(&output_path).map_to_undefined_error()?;
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

        let image = &identifier.reference;
        self.do_authenticate(image, oci::RegistryOperation::Pull).await?;
        let (manifest, digest) = self.do_pull_manifest(image).await?;
        if digest != identifier_digest {
            return Err(Error::Undefined);
        }
        let manifest = match manifest {
            oci::Manifest::Image(manifest) => manifest,
            _ => return Err(Error::Undefined),
        };

        media_type_select(&manifest.config.media_type, &[MEDIA_TYPE_PACKAGE_METADATA_V1])?;
        media_type_select_some(&manifest.artifact_type, &[MEDIA_TYPE_PACKAGE_V1])?;

        if manifest.layers.len() != 1 {
            return Err(Error::Undefined);
        }
        let blob_layer = &manifest.layers[0];
        let blob_compression = match compression::CompressionAlgorithm::from_media_type(&blob_layer.media_type) {
            Some(algorithm) => algorithm,
            None => return Err(Error::Undefined),
        };
        let blob_file_ext = media_type_file_ext(&blob_layer.media_type).unwrap_or("blob");

        log::debug!(
            "Acquiring lock for pulling package {} to {}",
            identifier,
            output_path.display()
        );
        let lock = file_lock::FileLock::lock_exclusive_with_timeout(
            std::fs::File::create(&lock_path).map_to_undefined_error()?,
            self.lock_timeout,
        )
        .await
        .map_to_undefined_error()?;

        let install_status = package::install_status::InstallStatus::new();
        install_status.write_json_to_path(&install_path)?;

        let blob_path = content_path.with_added_extension(blob_file_ext);
        let _drop_file = utility::drop_file::DropFile::new(blob_path.clone());
        log::info!(
            "Pulling package {} with digest {} to {}",
            identifier,
            digest,
            output_path.display()
        );
        self.do_pull_blob(image, &manifest.config.digest, &metadata_path)
            .await?;
        self.do_pull_blob(image, &blob_layer.digest, &blob_path).await?;

        let metadata = metadata::Metadata::read_json_from_path(&metadata_path)?;
        match metadata {
            metadata::Metadata::Bundle(bundle) => {
                log::debug!("Extracting bundle package {} to {}", identifier, content_path.display());
                let extract_options = archive::ExtractOptions {
                    algorithm: Some(blob_compression),
                    strip_components: bundle.strip_components.unwrap_or(0).into(),
                };
                archive::Archive::extract_with_options(blob_path, &content_path, Some(extract_options)).await?;
            }
        }

        manifest.write_json_to_path(&manifest_path)?;
        install_status.ok().write_json_to_path(install_path)?;
        drop(lock);
        Ok(install_info::InstallInfo {
            identifier,
            metadata: metadata::Metadata::read_json_from_path(&metadata_path)?,
            content: content_path,
        })
    }

    pub async fn push_package(&self, package_info: Info, file: impl AsRef<std::path::Path>) -> Result<Digest> {
        let path = file.as_ref();
        log::debug!(
            "Pushing package {} from file {}",
            package_info.identifier,
            path.display()
        );

        let image = &package_info.identifier.reference;
        let platform = Some(package_info.platform.clone().into());

        let package_media_type = match media_type_from_path(path) {
            Some(media_type) => Ok(media_type.to_string()),
            _ => Err(Error::UnsupportedArchive(path.display().to_string())),
        }?;

        // Super heavy operation, we have to read the entire file into memory to calculate the digest.
        // In future we may want to optimize this by streaming the file and calculating the digest on the fly.
        // For now we keep it simple and just read the entire file into memory.
        let package_data = tokio::fs::read(&file).await.unwrap();
        let package_data_len = package_data.len();
        let package_digest = Digest::sha256(&package_data).to_string();

        log::trace!("Calculated package digest: {}", package_digest);
        self.do_authenticate(image, oci::RegistryOperation::Push).await?;
        self.push_blob(image, package_data, &package_digest).await?;

        let config_data = serde_json::to_vec(&package_info.metadata).unwrap();
        let config_data_len = config_data.len();
        let config_sha256 = Digest::sha256(&config_data).to_string();
        log::trace!("Calculated meta layer digest: {}", config_sha256);
        self.push_blob(image, config_data, &config_sha256).await?;

        let config_descriptor = oci::Descriptor {
            media_type: MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
            digest: config_sha256,
            size: config_data_len as i64,
            urls: None,
            annotations: None,
        };
        let package_descriptor = oci::Descriptor {
            media_type: package_media_type,
            digest: package_digest,
            size: package_data_len as i64,
            urls: None,
            annotations: None,
        };

        let manifest = oci::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
            config: config_descriptor,
            layers: vec![package_descriptor],
            ..Default::default()
        };

        let manifest_data = serde_json::to_vec(&manifest).unwrap();
        let manifest_data_len = manifest_data.len();
        let manifest_sha256 = Digest::sha256(&manifest_data).to_string();
        let canonical_image = image.clone_with_digest(manifest_sha256.clone());

        let pushed_digest = self
            .client
            .push_manifest_raw(
                &canonical_image,
                manifest_data,
                MEDIA_TYPE_OCI_IMAGE_MANIFEST.parse().unwrap(),
            )
            .await
            .map_to_undefined_error()?;
        log::info!("Pushed manifest with digest '{}', updating index...", pushed_digest);
        let mut index = match self
            .client
            .pull_manifest_raw(
                image,
                &self.auth.get_or_fallback(package_info.identifier.registry()).await,
                &[MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_OCI_IMAGE_INDEX],
            )
            .await
        {
            Ok((blob, _)) => {
                let manifest = serde_json::from_slice::<oci::Manifest>(&blob).unwrap();
                match manifest {
                    oci::Manifest::Image(manifest) => {
                        let manifest_index_entry = oci::ImageIndexEntry {
                            media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
                            digest: manifest.config.digest.clone(),
                            size: manifest.config.size,
                            platform: None,
                            annotations: None,
                        };
                        oci::ImageIndex {
                            schema_version: oci::INDEX_SCHEMA_VERSION,
                            media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                            artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                            manifests: vec![manifest_index_entry],
                            annotations: None,
                        }
                    }
                    oci::Manifest::ImageIndex(index) => index,
                }
            }
            Err(e) => {
                log::debug!(
                    "No existing manifest or index found for image {}, starting with new index. Error: {}",
                    image,
                    e
                );
                oci::ImageIndex {
                    schema_version: oci::INDEX_SCHEMA_VERSION,
                    media_type: Some(MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()),
                    artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
                    manifests: vec![],
                    annotations: None,
                }
            }
        };

        index.manifests.retain(|index_entry| index_entry.platform != platform);
        index.manifests.push(oci::ImageIndexEntry {
            media_type: MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string(),
            digest: manifest_sha256,
            size: manifest_data_len as i64,
            platform,
            annotations: None,
        });

        let index_data = serde_json::to_vec(&index).unwrap();
        let index_digest = Digest::sha256(&index_data);
        self.client
            .push_manifest_raw(image, index_data, MEDIA_TYPE_OCI_IMAGE_INDEX.parse().unwrap())
            .await
            .unwrap();
        log::info!("Successfully updated index for image {}", image);

        Ok(index_digest)
    }

    /// Authenticates with the registry for the given image and operation.
    /// This will adhere to the authentication configuration and may involve credential helpers.
    async fn do_authenticate(&self, image: &oci::Reference, operation: oci::RegistryOperation) -> Result<()> {
        self.client
            .auth(image, &self.auth.get_or_fallback(image.registry()).await, operation)
            .await
            .map_to_undefined_error()?;
        Ok(())
    }

    /// Pulls the manifest for the given image reference and parses it into an OCI manifest struct.
    async fn do_pull_manifest(&self, image: &oci::Reference) -> Result<(oci::Manifest, String)> {
        log::debug!("Pulling manifest for image {}", image);
        let (manifest_data, digest) = self
            .client
            .pull_manifest_raw(
                image,
                &self.auth.get_or_fallback(image.registry()).await,
                ACCEPTED_MANIFEST_MEDIA_TYPES,
            )
            .await
            .map_to_undefined_error()?;
        let manifest = serde_json::from_slice::<oci::Manifest>(&manifest_data).map_to_undefined_error()?;
        Ok((manifest, digest))
    }

    /// Pulls a blob for the given image reference and digest, saving it to the specified output path.
    async fn do_pull_blob(
        &self,
        image: &oci::Reference,
        digest: &str,
        output: impl AsRef<std::path::Path>,
    ) -> Result<()> {
        log::debug!("Pulling blob with digest {} for image {}", digest, image);
        let path = output.as_ref();
        match path.parent() {
            Some(parent) => std::fs::create_dir_all(parent).map_to_undefined_error()?,
            None => return Err(Error::Undefined),
        }
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
            .map_to_undefined_error()?;
        self.client
            .pull_blob(image, digest, file)
            .await
            .map_to_undefined_error()
    }

    /// Pushes a blob to the registry, checking first if it already exists.
    async fn push_blob(&self, image: &oci::Reference, data: Vec<u8>, digest: &str) -> Result<()> {
        log::debug!("Checking if blob with digest {} already exists in registry", digest);
        match self.client.blob_exists(image, digest).await {
            Ok(true) => {
                log::debug!(
                    "Blob with digest {} already exists in registry, skipping upload",
                    digest
                );
                return Ok(());
            }
            Ok(false) => {
                log::debug!("Blob with digest {} does not exist in registry, uploading", digest);
            }
            Err(e) => {
                log::warn!(
                    "Failed to check if blob with digest {} exists, will attempt to upload anyway: {}",
                    digest,
                    e
                );
            }
        }
        match self.client.push_blob(image, data, digest).await {
            Ok(digest) => {
                log::debug!("Successfully pushed blob with digest {}", digest);
                Ok(())
            }
            Err(e) => {
                log::error!("Failed to push blob with digest {}: {}", digest, e);
                Err(Error::Undefined)
            }
        }
    }
}
