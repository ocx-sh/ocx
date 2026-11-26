// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use super::error::{Error, Result};
use super::{DeclaredPublishers, IndexOperation, Jurisdiction, index_impl::IndexImpl};

/// The chain source for a namespace whose `index_signature = "enforce"` cannot be honoured because a
/// registry-role `[mirrors]` entry pulled its index off.
///
/// It is authoritative for that registry and refuses every read, so the registry catch-all behind it
/// never answers the name through the mirror with nothing checked. It does no I/O.
#[derive(Clone)]
pub(crate) struct RefusedSource {
    namespace: String,
}

impl RefusedSource {
    pub(crate) fn new(namespace: String) -> Self {
        Self { namespace }
    }

    fn refusal(&self) -> Error {
        Error::IndexCatalogThroughMirror {
            namespace: self.namespace.clone(),
        }
    }
}

#[async_trait]
impl IndexImpl for RefusedSource {
    async fn list_repositories(&self, _registry: &str) -> Result<Vec<String>> {
        Err(self.refusal())
    }

    async fn list_tags(&self, _identifier: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
        Err(self.refusal())
    }

    async fn fetch_manifest(
        &self,
        _identifier: &ocx_oci::PackageRef,
        _op: IndexOperation,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        Err(self.refusal())
    }

    async fn fetch_manifest_digest(
        &self,
        _identifier: &ocx_oci::PackageRef,
        _op: IndexOperation,
    ) -> Result<Option<ocx_oci::Digest>> {
        Err(self.refusal())
    }

    async fn fetch_blob(&self, _blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
        Err(self.refusal())
    }

    async fn fetch_manifest_raw_bytes(
        &self,
        _identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        Err(self.refusal())
    }

    async fn fetch_root_document(
        &self,
        _identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, super::IndexRoot)>> {
        Err(self.refusal())
    }

    async fn fetch_catalog_entry(&self, _identifier: &ocx_oci::PackageRef) -> Result<Option<String>> {
        Err(self.refusal())
    }

    async fn physical_reference(&self, _identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        Err(self.refusal())
    }

    // `declared_publishers` and `committed_publishers` read process-held and committed local state only,
    // and `physical_reference_local` never contacts a source, so their defaults stay.
    async fn declared_publishers(&self, _identifier: &ocx_oci::PackageRef) -> Result<DeclaredPublishers> {
        Ok(DeclaredPublishers::Undeclared)
    }

    fn jurisdiction(&self, identifier: &ocx_oci::PackageRef) -> Jurisdiction {
        if identifier.registry() == self.namespace {
            Jurisdiction::Authoritative
        } else {
            Jurisdiction::Outside
        }
    }

    fn serves_registry(&self, registry: &str) -> bool {
        registry == self.namespace
    }

    fn box_clone(&self) -> Box<dyn IndexImpl> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_exit::{ClassifyErrorKind as _, ClassifyExitCode as _, Detail};

    fn name(registry: &str) -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry("cmake", registry)
    }

    fn refused(result: Result<impl Sized>) {
        let error = result.err().expect("a refused source reads nothing");
        assert!(
            matches!(&error, Error::IndexCatalogThroughMirror { namespace } if namespace == "ocx.sh"),
            "got {error:?}"
        );
        let slug = match error.kind_detail() {
            Detail::Fixed(entry) | Detail::Chain { fallback: entry, .. } => entry.slug,
        };
        assert_eq!(error.classify(), Some(ocx_exit::ExitCode::ConfigError));
        assert_eq!(slug, "index_catalog_through_mirror");
    }

    #[tokio::test]
    async fn every_reading_method_refuses_with_the_slug_and_exit_78() {
        let source = RefusedSource::new("ocx.sh".to_string());
        let id = name("ocx.sh");
        refused(source.list_repositories("ocx.sh").await);
        refused(source.list_tags(&id).await);
        refused(source.fetch_manifest(&id, IndexOperation::Query).await);
        refused(source.fetch_manifest(&id, IndexOperation::Resolve).await);
        refused(source.fetch_manifest_digest(&id, IndexOperation::Query).await);
        refused(source.fetch_manifest_raw_bytes(&id).await);
        refused(source.fetch_manifest_raw_bytes_with_origin(&id).await);
        refused(source.fetch_root_document(&id).await);
        refused(source.revalidate_root_document(&id).await);
        refused(source.fetch_catalog_entry(&id).await);
        refused(source.physical_reference(&id).await);
        let blob = ocx_oci::PinnedPackageRef::try_from(id.clone_with_digest(ocx_oci::Digest::Sha256("a".repeat(64))))
            .expect("a digest-pinned reference");
        refused(source.fetch_blob(&blob).await);
    }

    #[test]
    fn it_is_authoritative_for_its_registry_and_outside_every_other() {
        let source = RefusedSource::new("ocx.sh".to_string());
        assert_eq!(source.jurisdiction(&name("ocx.sh")), Jurisdiction::Authoritative);
        assert_eq!(source.jurisdiction(&name("example.com")), Jurisdiction::Outside);
        assert!(source.serves_registry("ocx.sh"));
        assert!(!source.serves_registry("example.com"));
    }
}
