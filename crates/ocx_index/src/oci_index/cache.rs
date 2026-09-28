// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::RwLock;

use ocx_util::singleflight;

/// Max keys a tag group admits — a memory backstop, never a throughput limit.
///
/// Keys accrue one per repository or tag a sync walks, so a small cap would fail
/// a large-registry sync with `CapacityExceeded` (exit 75) on successes alone.
const TAG_SINGLEFLIGHT_MAX_KEYS: usize = 1 << 20;

/// How long a coalesced caller blocks for the leader's registry call: four read
/// timeouts, since the leader's shortest path is auth plus a listing or `HEAD`.
const TAG_SINGLEFLIGHT_TIMEOUT: Duration = ocx_oci::client::REGISTRY_READ_TIMEOUT.saturating_mul(4);

// A waiter expiring within one request's bound turns a slow link into exit 75 for every caller but the leader.
const _: () = assert!(
    TAG_SINGLEFLIGHT_TIMEOUT.as_secs() > ocx_oci::client::REGISTRY_READ_TIMEOUT.as_secs(),
    "the coalescing timeout must exceed one registry request's own bound"
);

/// In-memory index data shared by every clone of an [`OciIndex`](super::OciIndex).
pub struct Cache {
    repositories: RwLock<HashMap<String, Vec<String>>>,
    tags: RwLock<HashMap<ocx_oci::PackageRef, Vec<String>>>,
    tag_digests: RwLock<HashMap<ocx_oci::PackageRef, ocx_oci::Digest>>,
    /// Coalesces concurrent misses on [`Self::get_tags`]; beside the map, never
    /// around it, or it reintroduces the outer lock [`SharedCache`] avoids.
    tag_group: singleflight::Group<ocx_oci::PackageRef, Vec<String>>,
    /// Coalesces concurrent misses on [`Self::get_tag_digest`].
    tag_digest_group: singleflight::Group<ocx_oci::PackageRef, ocx_oci::Digest>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            repositories: RwLock::default(),
            tags: RwLock::default(),
            tag_digests: RwLock::default(),
            tag_group: singleflight::Group::new(TAG_SINGLEFLIGHT_MAX_KEYS, TAG_SINGLEFLIGHT_TIMEOUT),
            tag_digest_group: singleflight::Group::new(TAG_SINGLEFLIGHT_MAX_KEYS, TAG_SINGLEFLIGHT_TIMEOUT),
        }
    }
}

impl Cache {
    /// The coalescing group guarding [`Self::get_tags`]' misses.
    pub fn tag_group(&self) -> &singleflight::Group<ocx_oci::PackageRef, Vec<String>> {
        &self.tag_group
    }

    /// The coalescing group guarding [`Self::get_tag_digest`]' misses.
    pub fn tag_digest_group(&self) -> &singleflight::Group<ocx_oci::PackageRef, ocx_oci::Digest> {
        &self.tag_digest_group
    }

    pub async fn get_repositories(&self, registry: &str) -> Option<Vec<String>> {
        self.repositories.read().await.get(registry).cloned()
    }

    pub async fn set_repositories(&self, registry: String, repositories: Vec<String>) {
        self.repositories.write().await.insert(registry, repositories);
    }

    pub async fn get_tags(&self, identifier: &ocx_oci::PackageRef) -> Option<Vec<String>> {
        self.tags.read().await.get(identifier).cloned()
    }

    pub async fn set_tags(&self, identifier: ocx_oci::PackageRef, tags: Vec<String>) {
        self.tags.write().await.insert(identifier, tags);
    }

    pub async fn get_tag_digest(&self, identifier: &ocx_oci::PackageRef) -> Option<ocx_oci::Digest> {
        self.tag_digests.read().await.get(identifier).cloned()
    }

    pub async fn set_tag_digest(&self, identifier: &ocx_oci::PackageRef, digest: ocx_oci::Digest) {
        self.tag_digests.write().await.insert(identifier.clone(), digest);
    }
}

/// Shared handle to the in-memory [`OciIndex`](super::OciIndex) cache.
///
/// No outer lock: a writer would hold it across an inner `.await` and block every reader.
pub type SharedCache = std::sync::Arc<Cache>;
