// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Writes the restricted index an export directory carries: one root per package, the dispatch objects its tags
//! name, the catalog over them, and `config.json`.

use crate::error::{Error, Result};
use crate::local_index::TagStatus;
use crate::{IndexStore, LocalIndex};

/// One tag of an exported root and what writing it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRow {
    pub tag: String,
    pub digest: ocx_oci::Digest,
    /// `Added`, `Replaced` or `Unchanged`; an export never reports a conflict.
    pub status: TagStatus,
}

/// Writes `identifier`'s root into `out`, restricted to exactly `tags`, with the dispatch objects those tags
/// name byte for byte and a catalog entry over the restricted bytes; ensures `config.json`.
///
/// A root already in `out` is merged by union of rows: a tag it pins elsewhere moves to the home's digest and
/// is reported `Replaced`. Call once per package; the catalog is re-authored after every root of the run.
///
/// # Errors
///
/// A tag the home does not pin; I/O or index-store failures.
pub async fn write_export_index(
    home: &LocalIndex,
    out: &IndexStore,
    identifier: &ocx_oci::PackageRef,
    tags: &[String],
) -> Result<Vec<TagRow>> {
    let registry = identifier.registry();
    let repository = identifier.repository();
    let not_pinned = |tag: &str| Error::RemoteManifestNotFound(identifier.clone_with_tag(tag).to_string());

    let home_root: serde_json::Value = match home
        .index_store()
        .read_root_document_bytes(registry, repository)
        .await?
    {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => return tags.first().map_or(Ok(Vec::new()), |tag| Err(not_pinned(tag))),
    };

    // The lock covers the read of an existing export root and its rewrite, so two exports into one
    // directory union instead of overwriting each other.
    let mut transaction = out.begin_catalog_transaction(registry).await?;
    let mut root = match out.read_root_document_bytes(registry, repository).await? {
        Some(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)?,
        None => serde_json::json!({}),
    };
    if !root["tags"].is_object() {
        root["tags"] = serde_json::json!({});
    }
    root["repository"] = serde_json::Value::String(format!("oci://{registry}/{repository}"));

    let mut rows = Vec::with_capacity(tags.len());
    for tag in tags {
        let entry = home_root["tags"].get(tag).cloned().ok_or_else(|| not_pinned(tag))?;
        let digest: ocx_oci::Digest = serde_json::from_value(entry["content"].clone())?;
        let object = home
            .index_store()
            .read_dispatch_object(registry, repository, &digest)
            .await?
            .ok_or_else(|| not_pinned(tag))?;
        out.write_dispatch_object(registry, repository, &digest, &object)
            .await?;

        let status = match root["tags"].get(tag).and_then(|row| row["content"].as_str()) {
            None => TagStatus::Added,
            Some(existing) if existing == digest.to_string() => TagStatus::Unchanged,
            Some(_) => TagStatus::Replaced,
        };
        root["tags"][tag] = entry;
        rows.push(TagRow {
            tag: tag.clone(),
            digest,
            status,
        });
    }

    // Roots before the catalog: a crash leaves a catalog that lags its roots, which a read recovers.
    let bytes = serde_json::to_vec_pretty(&root)?;
    transaction
        .write_root(repository, &bytes, |root| {
            crate::parse_repository_pointer(&root.repository).map(|_| ())
        })
        .await?;
    transaction.commit().await?;
    out.ensure_source_config(registry).await?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use tempfile::TempDir;

    use super::*;
    use crate::error::Result;
    use crate::{Index, IndexOperation, LocalConfig, index_impl};
    use ocx_oci::{Algorithm, Digest, Manifest, PackageRef};

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";

    fn id() -> PackageRef {
        PackageRef::new_registry(REPO, REGISTRY)
    }

    /// Verbatim, pretty-printed image index bytes over one leaf: not the canonical serde form, so a
    /// re-serialising writer cannot pass.
    fn object_bytes(leaf: char) -> (Vec<u8>, Digest) {
        let leaf = Digest::Sha256(leaf.to_string().repeat(64));
        let json = format!(
            "{{\n  \"schemaVersion\": 2,\n  \"mediaType\": \"application/vnd.oci.image.index.v1+json\",\n  \
             \"manifests\": [\n    {{ \"mediaType\": \"application/vnd.oci.image.manifest.v1+json\", \
             \"digest\": \"{leaf}\", \"size\": 42, \"platform\": {{ \"architecture\": \"amd64\", \"os\": \"linux\" }} }}\n  ]\n}}\n"
        );
        let bytes = json.into_bytes();
        let digest = Algorithm::Sha256.hash(&bytes);
        (bytes, digest)
    }

    /// A derived source: tag → leaf char.
    #[derive(Clone)]
    struct Registry(Vec<(&'static str, char)>);

    #[async_trait]
    impl index_impl::IndexImpl for Registry {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(self.0.iter().map(|(t, _)| (*t).to_owned()).collect()))
        }
        async fn fetch_manifest(&self, id: &PackageRef, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, d, m)| (d, m)))
        }
        async fn fetch_manifest_digest(&self, id: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, d, _)| d))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(&self, id: &PackageRef) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            let Some((_, leaf)) = self.0.iter().find(|(t, _)| Some(*t) == id.tag()) else {
                return Ok(None);
            };
            let (bytes, digest) = object_bytes(*leaf);
            let manifest = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, digest, manifest)))
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// A home that pins every tag in `tags` through a derived refresh.
    async fn home_pinning(dir: &TempDir, tags: &[(&'static str, char)]) -> LocalIndex {
        let home = LocalIndex::new(LocalConfig {
            index_store: IndexStore::new(dir.path().join("home")),
        });
        let source = Index::from_impl(Registry(tags.to_vec()));
        home.refresh_tags(&id(), &source).await.unwrap();
        home
    }

    fn out_store(dir: &TempDir) -> IndexStore {
        IndexStore::new(dir.path().join("out"))
    }

    fn names(out: &IndexStore) -> Vec<String> {
        let bytes = std::fs::read(out.root_document_path(REGISTRY, REPO)).unwrap();
        let root: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        root["tags"].as_object().unwrap().keys().cloned().collect()
    }

    fn strings(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|t| (*t).to_owned()).collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn root_restricted_to_tags() {
        let dir = TempDir::new().unwrap();
        let home = home_pinning(&dir, &[("1", 'a'), ("2", 'b'), ("3", 'c')]).await;
        let out = out_store(&dir);

        let rows = write_export_index(&home, &out, &id(), &strings(&["1", "3"]))
            .await
            .unwrap();

        assert_eq!(names(&out), ["1", "3"], "the root lists exactly the exported tags");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.status == TagStatus::Added));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_reauthored_last() {
        let dir = TempDir::new().unwrap();
        let home = home_pinning(&dir, &[("1", 'a')]).await;
        let out = out_store(&dir);

        write_export_index(&home, &out, &id(), &strings(&["1"])).await.unwrap();

        let root = std::fs::read(out.root_document_path(REGISTRY, REPO)).unwrap();
        let catalog = out
            .read_source_catalog(REGISTRY)
            .await
            .unwrap()
            .expect("catalog written");
        let document = serde_json::to_value(&catalog).unwrap();
        assert!(
            document.to_string().contains(&IndexStore::root_catalog_entry(&root)),
            "the catalog entry hashes the restricted root's bytes, not the home's"
        );
        assert!(
            std::fs::read(out.source_config_path(REGISTRY)).is_ok(),
            "config.json is ensured beside the catalog"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reexport_replaces_moved_tag() {
        let dir = TempDir::new().unwrap();
        let out = out_store(&dir);
        let first = home_pinning(&dir, &[("1", 'a')]).await;
        write_export_index(&first, &out, &id(), &strings(&["1"])).await.unwrap();
        let other = TempDir::new().unwrap();
        let moved = home_pinning(&other, &[("1", 'b'), ("2", 'c')]).await;

        let rows = write_export_index(&moved, &out, &id(), &strings(&["1", "2"]))
            .await
            .unwrap();

        let status = |tag: &str| rows.iter().find(|row| row.tag == tag).map(|row| row.status);
        assert_eq!(status("1"), Some(TagStatus::Replaced), "a tag pinned elsewhere moves");
        assert_eq!(status("2"), Some(TagStatus::Added));
        assert_eq!(names(&out), ["1", "2"], "rows already in the export are kept (union)");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_object_verbatim() {
        let dir = TempDir::new().unwrap();
        let home = home_pinning(&dir, &[("1", 'a')]).await;
        let out = out_store(&dir);
        let (bytes, digest) = object_bytes('a');

        write_export_index(&home, &out, &id(), &strings(&["1"])).await.unwrap();

        let written = std::fs::read(out.dispatch_object_path(REGISTRY, REPO, &digest)).unwrap();
        assert_eq!(written, bytes, "the dispatch object is copied byte for byte");
    }
}
