use crate::log;
use crate::{oci, prelude::*};

pub mod snapshot;

pub use local_index::Config as LocalConfig;
pub use local_index::LocalIndex;
pub use remote_index::Config as RemoteConfig;
pub use remote_index::Index as RemoteIndex;

mod index_impl;
mod local_index;
mod remote_index;

/// Note, some operations are cached and the cache is shared between clones of the index.
/// This means that if you clone the index, they will share the same cache and benefit from each other's cached data.
/// On the other hand, if you have a long-running index instance, you may want to periodically clear the cache to avoid memory bloat and ensure that you always have the latest data.
/// The cache is currently never cleared, but expiration or manual clearing may be added in the future if needed.
pub struct Index {
    inner: Box<dyn index_impl::IndexImpl>,
}

impl Index {
    pub fn from_remote(remote_index: RemoteIndex) -> Self {
        Self {
            inner: Box::new(remote_index),
        }
    }

    pub fn from_local(local_index: LocalIndex) -> Self {
        Self {
            inner: Box::new(local_index),
        }
    }

    /// List all tags available for the given identifier.
    pub async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Vec<String>> {
        self.inner.list_tags(identifier).await.map(|v| v.sorted())
    }

    pub async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<(oci::Digest, oci::Manifest)> {
        log::trace!("Fetching candidates for identifier '{}'.", identifier);
        self.inner.fetch_manifest(identifier).await
    }

    /// Find the manifest digest for the given identifier and tag.
    pub async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<oci::Digest> {
        self.inner.fetch_manifest_digest(identifier).await
    }

    pub async fn fetch_candidates(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Vec<(oci::Identifier, oci::Platform)>> {
        let (digest, manifest) = self.fetch_manifest(identifier).await?;
        log::trace!(
            "Fetched manifest for identifier '{}'. Determining candidates based on manifest type.",
            identifier
        );

        match manifest {
            oci::Manifest::Image(_) => Ok(vec![(identifier.clone_with_digest(digest), oci::Platform::default())]),
            oci::Manifest::ImageIndex(index) => {
                let mut candidates = Vec::with_capacity(index.manifests.len());
                for manifest in index.manifests {
                    let digest = manifest.digest.try_into()?;
                    let candidate = identifier.clone_with_digest(digest);
                    let platform = match manifest.platform {
                        Some(platform) => platform.try_into()?,
                        None => oci::Platform::any(),
                    };
                    candidates.push((candidate, platform));
                }
                log::debug!(
                    "Found {} candidate(s) for identifier '{}'.",
                    candidates.len(),
                    identifier
                );
                Ok(candidates)
            }
        }
    }

    pub async fn select(
        &self,
        identifier: &oci::Identifier,
        platforms: Vec<oci::Platform>,
    ) -> Result<Option<oci::Identifier>> {
        let candidates = self.fetch_candidates(identifier).await?;

        let mut matching_candidates = Vec::new();
        for platform in &platforms {
            for (identifier, candidate_platform) in &candidates {
                if platform.matches(candidate_platform) {
                    matching_candidates.push(identifier.clone());
                }
            }
            if !matching_candidates.is_empty() {
                break;
            }
        }

        let first_candidate = match matching_candidates.first() {
            Some(candidate) => candidate,
            None => return Ok(None),
        };

        if matching_candidates.len() > 1 {
            return Err(Error::PackageSelectionAmbiguous(matching_candidates));
        }
        Ok(Some(first_candidate.clone()))
    }
}

impl Clone for Index {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.box_clone(),
        }
    }
}
