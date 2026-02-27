use std::collections::HashMap;

use tokio::sync::RwLock;

use crate::oci;

/// Internal cache for index data.
/// This is shared across all instances of the index, and is used to avoid redundant file reads.
#[derive(Default)]
pub struct Cache {
    tags: RwLock<HashMap<oci::Identifier, Vec<String>>>,
    tag_digests: RwLock<HashMap<oci::Identifier, oci::Digest>>,
}

impl Cache {
    pub async fn get_tags(&self, identifier: &oci::Identifier) -> Option<Vec<String>> {
        self.tags.read().await.get(identifier).cloned()
    }

    pub async fn set_tags(&self, identifier: oci::Identifier, tags: Vec<String>) {
        self.tags.write().await.insert(identifier, tags);
    }

    pub async fn get_tag_digest(&self, identifier: &oci::Identifier) -> Option<oci::Digest> {
        self.tag_digests.read().await.get(identifier).cloned()
    }

    pub async fn set_tag_digest(&self, identifier: &oci::Identifier, digest: oci::Digest) {
        self.tag_digests.write().await.insert(identifier.clone(), digest);
    }
}

pub type SharedCache = std::sync::Arc<tokio::sync::RwLock<Cache>>;
