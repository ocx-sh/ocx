use std::path::Path;

use async_trait::async_trait;

use super::error::ClientError;
use super::transport::{OciTransport, Result};
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
            .map_err(|e| ClientError::Authentication(e.to_string()))?;
        Ok(())
    }
}

fn registry_error(e: impl std::fmt::Display) -> ClientError {
    ClientError::Registry(e.to_string())
}

fn io_error(path: &Path, e: impl Into<std::io::Error>) -> ClientError {
    ClientError::Io(path.to_path_buf(), e.into())
}

#[async_trait]
impl OciTransport for NativeTransport {
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
            .map_err(registry_error)?;
        Ok(response.tags)
    }

    async fn catalog(
        &self,
        image: &oci::native::Reference,
        chunk_size: usize,
        last: Option<String>,
    ) -> Result<Vec<String>> {
        let auth = self.auth_for(image).await;
        self.client
            .catalog(image, &auth, Some(chunk_size), last.as_deref())
            .await
            .map_err(registry_error)
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
            .map_err(registry_error)?;
        Ok((data.to_vec(), digest))
    }

    async fn pull_blob_to_file(&self, image: &oci::native::Reference, digest: &str, path: &Path) -> Result<()> {
        log::debug!("Pulling blob {} for image {} to {}", digest, image, path.display());
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
        self.client.pull_blob(image, digest, file).await.map_err(registry_error)
    }

    async fn push_manifest(&self, image: &oci::native::Reference, manifest: &oci::Manifest) -> Result<String> {
        self.authenticate(image, oci::RegistryOperation::Push).await?;
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

    async fn push_blob(&self, image: &oci::native::Reference, data: Vec<u8>, digest: &str) -> Result<String> {
        self.do_push_blob(image, data, digest).await
    }

    fn box_clone(&self) -> Box<dyn OciTransport> {
        Box::new(self.clone())
    }
}

impl NativeTransport {
    /// Shared push-blob logic: checks existence, then uploads.
    async fn do_push_blob(&self, image: &oci::native::Reference, data: Vec<u8>, digest: &str) -> Result<String> {
        log::debug!("Checking if blob {} already exists in registry", digest);
        match self.client.blob_exists(image, digest).await {
            Ok(true) => {
                log::debug!("Blob {} already exists, skipping upload", digest);
                return Ok(digest.to_string());
            }
            Ok(false) => {
                log::debug!("Blob {} does not exist, uploading", digest);
            }
            Err(e) => {
                log::warn!("Failed to check blob {} existence, will attempt upload: {}", digest, e);
            }
        }
        self.client.push_blob(image, data, digest).await.map_err(registry_error)
    }
}
