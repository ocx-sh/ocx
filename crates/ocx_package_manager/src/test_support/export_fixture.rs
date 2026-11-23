// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! One bundle package (`example.com/cmake:3.28`, `linux/amd64`, one layer or none) in the three shapes the export and
//! import tests need: a registry stand-in, a home that already holds it, and a hand-built export directory.
//!
//! The layer archive is a descriptor only: its bytes are never written, so a test that needs the layer present
//! writes them itself. Export directories are built by hand, not by the writer under test.

use std::path::Path;

use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
use ocx_oci::media_type::{MEDIA_TYPE_PACKAGE_METADATA_V1, MEDIA_TYPE_PACKAGE_V1};
use ocx_oci::{Algorithm, Digest, PackageRef};
use ocx_store::file_structure::{BlobStore, FileStructure};
use ocx_util::prelude::StringExt as _;

use super::manifest_source::FakeManifestSource;
use crate::PackageManager;

pub const REGISTRY: &str = "example.com";
pub const REPO: &str = "cmake";
pub const TAG: &str = "3.28";
pub const LAYER_BYTES: &[u8] = b"layer archive bytes";
const METADATA_JSON: &str = r#"{"type":"bundle","version":1,"env":[{"key":"PATH","type":"path","value":"${installPath}/bin","visibility":"public"}],"dependencies":[],"entrypoints":{}}"#;

pub struct Package {
    pub config: Digest,
    /// The layer archive; `None` for a package that extracts nothing.
    pub layer: Option<Digest>,
    pub leaf: Digest,
    pub leaf_bytes: Vec<u8>,
    pub dispatch: Digest,
    pub dispatch_bytes: Vec<u8>,
}

pub fn tagged() -> PackageRef {
    PackageRef::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
}

pub fn linux_amd64() -> ocx_oci::Platform {
    "linux/amd64".parse().expect("platform")
}

impl Default for Package {
    fn default() -> Self {
        Self::new()
    }
}

impl Package {
    /// The package with its one layer archive.
    pub fn new() -> Self {
        Self::build(true)
    }

    /// The package with no layer: a pull has nothing to extract, so a test can import it from bytes it owns.
    pub fn layerless() -> Self {
        Self::build(false)
    }

    fn build(with_layer: bool) -> Self {
        let config = Algorithm::Sha256.hash(METADATA_JSON.as_bytes());
        let layer = with_layer.then(|| Algorithm::Sha256.hash(LAYER_BYTES));
        let layers = layer.as_ref().map_or_else(String::new, |digest| {
            format!(
                r#"{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"{digest}","size":{}}}"#,
                LAYER_BYTES.len()
            )
        });
        let leaf_bytes = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config}","size":{}}},"layers":[{layers}],"artifactType":"{MEDIA_TYPE_PACKAGE_V1}"}}"#,
            METADATA_JSON.len(),
        )
        .into_bytes();
        let leaf = Algorithm::Sha256.hash(&leaf_bytes);
        let dispatch_bytes = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{leaf}","size":{},"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
            leaf_bytes.len()
        )
        .into_bytes();
        let dispatch = Algorithm::Sha256.hash(&dispatch_bytes);
        Self {
            config,
            layer,
            leaf,
            leaf_bytes,
            dispatch,
            dispatch_bytes,
        }
    }

    pub fn leaf_id(&self) -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::try_from(
            PackageRef::new_registry(REPO, REGISTRY).clone_with_digest(self.leaf.clone()),
        )
        .expect("pinned")
    }

    /// A registry stand-in serving the package under `TAG`.
    pub fn source(&self) -> FakeManifestSource {
        FakeManifestSource::default()
            .with(TAG, &self.dispatch_bytes)
            .with(&self.leaf.to_string(), &self.leaf_bytes)
            .with_blob(&self.config.to_string(), METADATA_JSON.as_bytes())
    }

    /// Writes the leaf manifest and config into `blobs` under `REGISTRY`, the layer when `with_layer`.
    pub async fn write_blobs(&self, blobs: &BlobStore, with_layer: bool) {
        blobs
            .write_blob(REGISTRY, &self.leaf, &self.leaf_bytes)
            .await
            .expect("leaf blob");
        blobs
            .write_blob(REGISTRY, &self.config, METADATA_JSON.as_bytes())
            .await
            .expect("config blob");
        if let (true, Some(layer)) = (with_layer, &self.layer) {
            blobs
                .write_blob(REGISTRY, layer, LAYER_BYTES)
                .await
                .expect("layer blob");
        }
    }

    /// A home under `dir` that already pins `TAG` and holds the leaf manifest and config, but not the layer:
    /// the state an install leaves after an interrupted extraction.
    pub async fn seed_home(&self, dir: &Path) {
        let manager = online_manager(dir, self.source());
        manager
            .index()
            .fetch_manifest(&tagged(), ocx_index::IndexOperation::Resolve)
            .await
            .expect("resolve commits the pin")
            .expect("source serves the tag");
        self.write_blobs(&manager.file_structure().blobs, false).await;
    }

    /// Builds an export directory at `dir`: marker, a root pinning `TAG` (with the exporting machine's
    /// pointer, not this registry's), its dispatch object, the catalog and `config.json`, and the blobs.
    pub async fn write_export_dir(&self, dir: &Path, with_layer: bool) {
        let slug = REGISTRY.to_relaxed_slug();
        std::fs::create_dir_all(dir).expect("export dir");
        std::fs::write(
            dir.join("ocx-export.json"),
            format!(r#"{{"format_version":1,"registries":{{"{slug}":"{REGISTRY}"}}}}"#),
        )
        .expect("marker");
        let store = IndexStore::new(dir.join("index"));
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/{REPO}","tags":{{"{TAG}":{{"content":"{}"}}}}}}"#,
            self.dispatch
        );
        store
            .write_dispatch_object(REGISTRY, REPO, &self.dispatch, &self.dispatch_bytes)
            .await
            .expect("dispatch object");
        let mut transaction = store.begin_catalog_transaction(REGISTRY).await.expect("transaction");
        transaction
            .write_root(REPO, root.as_bytes(), |_| Ok(()))
            .await
            .expect("root");
        transaction.commit().await.expect("catalog");
        std::fs::write(store.root().join(&slug).join("config.json"), "{\"format_version\":1}\n")
            .expect("source config");
        self.write_blobs(&BlobStore::new(dir.join("blobs")), with_layer).await;
    }
}

/// A manager over a fresh home at `dir` whose chain reads `source`.
pub fn online_manager(dir: &Path, source: FakeManifestSource) -> PackageManager {
    let fs = FileStructure::with_root(dir.to_path_buf());
    let index = Index::from_chained_with_content_store(
        LocalIndex::new(LocalConfig {
            index_store: IndexStore::machine_local(&fs),
        }),
        vec![Index::from_impl(source)],
        ChainMode::Default,
        fs.blobs.clone(),
    );
    PackageManager::new(fs, index, None, REGISTRY)
}

/// A manager over the home at `dir` with no source and no client: every read that needs a network fails.
pub fn offline_manager(dir: &Path) -> PackageManager {
    let fs = FileStructure::with_root(dir.to_path_buf());
    let index = Index::from_chained_with_content_store(
        LocalIndex::new(LocalConfig {
            index_store: IndexStore::machine_local(&fs),
        }),
        Vec::new(),
        ChainMode::Offline,
        fs.blobs.clone(),
    );
    PackageManager::new(fs, index, None, REGISTRY)
}
