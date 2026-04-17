// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use tokio::sync::RwLock;

use crate::oci;

/// Internal cache for index data.
/// This is shared across all instances of the index, and is used to avoid redundant file reads.
#[derive(Default)]
pub struct Cache {
    tags: RwLock<HashMap<oci::Identifier, HashMap<String, oci::Digest>>>,
    manifest: RwLock<HashMap<(String, oci::Digest), oci::Manifest>>,
}

impl Cache {
    pub async fn get_tags(&self, identifier: &oci::Identifier) -> Option<HashMap<String, oci::Digest>> {
        self.tags.read().await.get(identifier).cloned()
    }

    pub async fn set_tags(&self, identifier: oci::Identifier, tags: HashMap<String, oci::Digest>) {
        self.tags.write().await.insert(identifier, tags);
    }

    pub async fn get_manifest(&self, identifier: &oci::Identifier, digest: &oci::Digest) -> Option<oci::Manifest> {
        // TODO: Optimize
        let key = (identifier.registry().to_string(), digest.clone());
        self.manifest.read().await.get(&key).cloned()
    }

    pub async fn set_manifest(&self, identifier: oci::Identifier, digest: oci::Digest, manifest: oci::Manifest) {
        // TODO: Optimize
        let key = (identifier.registry().to_string(), digest);
        self.manifest.write().await.insert(key, manifest);
    }
}

/// Shared handle to the in-memory index cache.
///
/// The inner fields of [`Cache`] are each independently guarded by a
/// `tokio::sync::RwLock`, so the outer handle is a plain `Arc<Cache>` —
/// no outer lock. A previous revision wrapped this in an outer `RwLock`,
/// which caused writers to hold the outer write-guard across the inner
/// `.await` in `set_tags` / `set_manifest`. Tokio locks allow this in
/// principle but it blocks every other reader for the entire suspension
/// window. Moving to `Arc<Cache>` eliminates the contention at the type
/// level — readers and writers only contend on the specific sub-map they
/// touch, and no guard is ever held across an `.await` on a foreign lock.
pub type SharedCache = std::sync::Arc<Cache>;
