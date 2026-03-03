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

pub type SharedCache = std::sync::Arc<tokio::sync::RwLock<Cache>>;
