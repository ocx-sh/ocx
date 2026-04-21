// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::log;
use crate::package::tag::Tag;
use crate::{oci, prelude::*};

pub mod error;
pub mod snapshot;

pub use local_index::Config as LocalConfig;
pub use local_index::LocalIndex;
pub use remote_index::Config as RemoteConfig;
pub use remote_index::Index as RemoteIndex;

mod chained_index;
mod index_impl;
mod local_index;
mod remote_index;

/// Re-export the private `IndexImpl` trait for sibling-module tests.
///
/// Sibling modules (e.g. `project::resolve` unit tests) that need to
/// construct an `Index` from a hand-rolled mock must implement
/// `IndexImpl`. Production code reaches `Index` only via
/// [`Index::from_chained`] / [`Index::from_remote`].
#[cfg(test)]
pub(crate) use index_impl::IndexImpl;

/// Routing policy for a [`ChainedIndex`](chained_index::ChainedIndex).
///
/// Threaded through `Index::from_chained` and on into the chained index so
/// that callers can pick the right cache/source policy without changing the
/// `IndexImpl` trait. The parameter is threaded end-to-end; each variant
/// below documents its own cache/source routing behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChainMode {
    /// Cache-first for all lookups. Write-through on cache-miss source
    /// fetches. Used for default (no flag) online operation.
    Default,
    /// Mutable lookups (tags, catalog) bypass cache and go straight to
    /// source. Digest-addressed (immutable) lookups still use cache +
    /// write-through. Used for `--remote`.
    Remote,
    /// Cache only. Source list is empty or never consulted; cache misses
    /// return `None` from `fetch_manifest`. Used for `--offline`.
    Offline,
}

/// The result of a platform-aware package selection.
#[non_exhaustive]
pub enum SelectResult {
    /// Exactly one candidate matched.
    Found(oci::Identifier),
    /// Multiple candidates matched — the caller must decide how to handle the
    /// ambiguity (e.g. ask the user or report an error).
    Ambiguous(Vec<oci::Identifier>),
    /// No candidates matched the requested platforms (or the package was not
    /// found in the index at all).
    NotFound,
}

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

    /// Inject an arbitrary `IndexImpl` implementation.
    ///
    /// Used exclusively in unit tests to wrap `TestIndex` fakes without
    /// exposing `IndexImpl` as a public trait.  Not available in production
    /// builds.
    ///
    /// Visibility is `pub(crate)` under `#[cfg(test)]` so sibling modules
    /// (e.g. `project::resolve`) can inject their own mock index
    /// implementations without going through the heavier `from_chained`
    /// construction path.
    #[cfg(test)]
    pub(crate) fn from_impl(inner: impl index_impl::IndexImpl + 'static) -> Self {
        Self { inner: Box::new(inner) }
    }

    /// Construct an index that reads from `cache` first, falling through to
    /// `sources` in order on miss. Successful source fetches are persisted
    /// into `cache` via `update_tag`.
    ///
    /// `mode` controls cache/source routing — see [`ChainMode`] for each
    /// variant's behaviour.
    pub fn from_chained(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            inner: Box::new(chained_index::ChainedIndex::new(cache, sources, mode)),
        }
    }

    /// List all repositories available in the given registry.
    pub async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        log::debug!("Listing repositories for registry '{}'.", registry);
        self.inner.list_repositories(registry).await
    }

    /// List all tags available for the given identifier.
    ///
    /// Internal tags (prefixed with `__ocx.`) are automatically filtered out.
    /// Returns `None` when the package is not known to this index.
    pub async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        log::debug!("Listing tags for '{}'.", identifier);
        self.inner.list_tags(identifier).await.map(|opt| {
            opt.map(|tags| {
                tags.into_iter()
                    .filter(|t| !Tag::is_internal_str(t))
                    .collect::<Vec<_>>()
                    .sorted()
            })
        })
    }

    /// Fetch the manifest for the given identifier.
    ///
    /// Returns `None` when the manifest is not available.
    pub async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        log::trace!("Fetching candidates for identifier '{}'.", identifier);
        self.inner.fetch_manifest(identifier).await
    }

    /// Find the manifest digest for the given identifier and tag.
    ///
    /// Returns `None` when the identifier cannot be resolved.
    pub async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<Option<oci::Digest>> {
        self.inner.fetch_manifest_digest(identifier).await
    }

    pub async fn fetch_candidates(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<Vec<(oci::Identifier, oci::Platform)>>> {
        let Some((digest, manifest)) = self.fetch_manifest(identifier).await? else {
            return Ok(None);
        };
        log::trace!(
            "Fetched manifest for identifier '{}'. Determining candidates based on manifest type.",
            identifier
        );

        match manifest {
            oci::Manifest::Image(_) => Ok(Some(vec![(
                identifier.clone_with_digest(digest),
                oci::Platform::default(),
            )])),
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
                Ok(Some(candidates))
            }
        }
    }

    pub async fn select(&self, identifier: &oci::Identifier, platforms: Vec<oci::Platform>) -> Result<SelectResult> {
        log::debug!("Selecting package '{}' for platforms {:?}.", identifier, platforms);

        let Some(candidates) = self.fetch_candidates(identifier).await? else {
            log::debug!("No candidates found for '{}'.", identifier);
            return Ok(SelectResult::NotFound);
        };

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

        let result = match matching_candidates.len() {
            0 => SelectResult::NotFound,
            1 => SelectResult::Found(matching_candidates.into_iter().next().expect("len checked above")),
            _ => SelectResult::Ambiguous(matching_candidates),
        };

        match &result {
            SelectResult::Found(id) => log::debug!("Selected '{}'.", id),
            SelectResult::Ambiguous(ids) => {
                log::debug!("Selection ambiguous for '{}': {} candidates.", identifier, ids.len())
            }
            SelectResult::NotFound => log::debug!(
                "No matching platform for '{}' among {} candidate(s).",
                identifier,
                candidates.len()
            ),
        }

        Ok(result)
    }
}

impl Clone for Index {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.box_clone(),
        }
    }
}
