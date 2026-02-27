use async_trait::async_trait;

use crate::{Result, oci};

#[async_trait]
pub trait IndexImpl: Send + Sync {
    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Vec<String>>;

    async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<(oci::Digest, oci::Manifest)>;
    async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<oci::Digest>;

    fn box_clone(&self) -> Box<dyn IndexImpl>;
}
