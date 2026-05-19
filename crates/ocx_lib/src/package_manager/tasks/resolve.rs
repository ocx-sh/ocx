// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;

use tokio::task::JoinSet;

use crate::{
    oci,
    oci::index::{IndexOperation, SelectResult},
    package::install_info::InstallInfo,
    package_manager::{self, composer, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

/// What a [`ChainBlob`] is in OCI terms — disambiguates the otherwise
/// opaque digest list so `inspect` can label each entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainRole {
    /// The multi-platform image index (only present for multi-platform tags).
    Index,
    /// The platform-selected image manifest.
    Manifest,
    /// The OCX metadata config blob the manifest points at.
    Config,
}

impl std::fmt::Display for ChainRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            ChainRole::Index => "index",
            ChainRole::Manifest => "manifest",
            ChainRole::Config => "config",
        };
        f.write_str(text)
    }
}

/// One blob in the resolution chain, carrying enough descriptor context
/// (role, media type, byte size) for callers to render it the same way
/// layers are rendered. `size` is `-1` only when it could not be
/// determined (a manifest blob whose on-disk file is unexpectedly absent
/// despite the on-disk invariant); descriptor-backed entries always have a
/// real size.
#[derive(Debug, Clone)]
pub struct ChainBlob {
    /// The blob pinned by its own digest.
    pub identifier: oci::PinnedIdentifier,
    /// What this blob is in the OCI walk.
    pub role: ChainRole,
    /// The blob's media type (descriptor `mediaType`, or the spec default
    /// for the role when the manifest omits it).
    pub media_type: String,
    /// Size in bytes, or `-1` when undeterminable.
    pub size: i64,
}

/// The full resolution output for a single identifier.
///
/// `chain` lists every blob the resolved package depends on as a raw
/// blob in `blobs/`: manifest entries (image-index where present,
/// image-manifest) followed by the trailing OCX metadata config blob.
/// Manifest entries land on disk via `ChainedIndex` write-through during
/// `resolve`; the trailing config-blob entry is **not** guaranteed on
/// disk by `resolve` alone — `pull::setup_owned` materializes it via
/// `common::fetch_or_get_blob` before `ReferenceManager::link_blobs`
/// runs. `link_blobs` tolerates dangling targets (eventual consistency;
/// GC collects). `final_manifest` is the platform-selected image
/// manifest (never an image index).
#[derive(Debug, Clone)]
pub struct ResolvedChain {
    /// The platform-selected pinned identifier — same value the old
    /// `resolve` method returned.
    pub pinned: oci::PinnedIdentifier,
    /// Walk-order chain blobs the resolver touched, backed by on-disk blob
    /// files (config blob materialized later by the pull pipeline).
    pub chain: Vec<ChainBlob>,
    /// The platform-selected image manifest used by the pull pipeline for
    /// layer extraction. Never an image index.
    pub final_manifest: oci::ImageManifest,
}

impl ResolvedChain {
    /// Walk-order pinned identifiers for every chain blob — the input
    /// `ReferenceManager::link_blobs` consumes to populate `refs/blobs/`.
    pub fn blobs(&self) -> impl Iterator<Item = &oci::PinnedIdentifier> {
        self.chain.iter().map(|blob| &blob.identifier)
    }
}

impl PackageManager {
    /// Resolves an identifier through the index (tag → digest, platform
    /// matching), returning the pinned identifier plus the full chain of
    /// blobs that backed the resolution.
    pub async fn resolve(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
    ) -> Result<ResolvedChain, PackageErrorKind> {
        // Walk the manifest chain through ChainedIndex. Each `fetch_manifest`
        // returns cache-first with write-through persistence, so every digest
        // the walk touches is backed by an on-disk blob by the time it lands
        // in `chain` — that is the `ResolvedChain` invariant.
        //
        // The tag/digest top-id derivation + not-found-vs-offline split is
        // shared with `inspect` via `common::resolve_top_manifest`; this
        // method keeps the divergent chain-building below.
        let (top_pinned, top_manifest) =
            super::common::resolve_top_manifest(self.index(), package, IndexOperation::Resolve).await?;
        // Reconstruct the tag-form top identifier the divergent chain-building
        // below operates on (digest dropped — `clone_with_tag`/`select` derive
        // their own digests, and `select` must see the unpinned index ref).
        let top_id = if package.digest().is_some() {
            package.clone()
        } else {
            package.clone_with_tag(package.tag_or_latest())
        };
        match top_manifest {
            // Flat image manifest: the chain is a single entry and the
            // top-level digest IS the pinned identifier. Platform filtering
            // does not apply here — a single-platform package always matches.
            oci::Manifest::Image(img) => {
                let top_size = blob_data_size(self.file_structure(), &top_pinned).await;
                let top_media = img
                    .media_type
                    .clone()
                    .unwrap_or_else(|| oci::OCI_IMAGE_MEDIA_TYPE.to_string());
                let mut chain = vec![ChainBlob {
                    identifier: top_pinned.clone(),
                    role: ChainRole::Manifest,
                    media_type: top_media,
                    size: top_size,
                }];

                let config_digest =
                    oci::Digest::try_from(img.config.digest.as_str()).map_err(|_| PackageErrorKind::DigestMissing)?;
                let config_pinned = oci::PinnedIdentifier::try_from(top_id.clone_with_digest(config_digest))
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                chain.push(ChainBlob {
                    identifier: config_pinned,
                    role: ChainRole::Config,
                    media_type: img.config.media_type.clone(),
                    size: img.config.size,
                });
                Ok(ResolvedChain {
                    pinned: top_pinned,
                    chain,
                    final_manifest: img,
                })
            }
            // Image index: defer platform selection to `Index::select`, then
            // fetch the selected child to append it to the chain and return
            // its manifest as `final_manifest`.
            oci::Manifest::ImageIndex(index) => {
                let top_size = blob_data_size(self.file_structure(), &top_pinned).await;
                let top_media = index
                    .media_type
                    .clone()
                    .unwrap_or_else(|| oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string());
                let mut chain = vec![ChainBlob {
                    identifier: top_pinned.clone(),
                    role: ChainRole::Index,
                    media_type: top_media,
                    size: top_size,
                }];

                let pinned = match self.index().select(&top_id, platforms, IndexOperation::Resolve).await {
                    Ok(SelectResult::Found(id)) => {
                        oci::PinnedIdentifier::try_from(id).map_err(|_| PackageErrorKind::DigestMissing)?
                    }
                    Ok(SelectResult::Ambiguous(v)) => return Err(PackageErrorKind::SelectionAmbiguous(v)),
                    Ok(SelectResult::NotFound) => return Err(PackageErrorKind::NotFound),
                    Err(e) => return Err(PackageErrorKind::Internal(e)),
                };

                let child_id = top_id.clone_with_digest(pinned.digest());
                let (child_digest, child_manifest) = match self
                    .index()
                    .fetch_manifest(&child_id, IndexOperation::Resolve)
                    .await
                    .map_err(PackageErrorKind::Internal)?
                {
                    Some(result) => result,
                    None => {
                        // Child manifest blob missing but the parent was
                        // located via an image-index entry — treat as the
                        // offline-missing case so the user knows to re-pull.
                        return Err(PackageErrorKind::OfflineManifestMissing(Box::new(
                            package_manager::error::OfflineManifestMissing {
                                identifier: child_id,
                                digest: pinned.digest(),
                            },
                        )));
                    }
                };

                let final_manifest = match child_manifest {
                    oci::Manifest::Image(img) => img,
                    oci::Manifest::ImageIndex(_) => {
                        return Err(PackageErrorKind::Internal(
                            oci::index::error::Error::NestedImageIndex { digest: child_digest }.into(),
                        ));
                    }
                };
                let child_pinned = oci::PinnedIdentifier::try_from(child_id.clone_with_digest(child_digest))
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                // The image-index entry that selected this child carries its
                // authoritative descriptor (media type + size) — no extra
                // blob stat needed.
                let child_descriptor = index
                    .manifests
                    .iter()
                    .find(|entry| entry.digest == child_pinned.digest().to_string());
                let (child_media, child_size) = match child_descriptor {
                    Some(entry) => (entry.media_type.clone(), entry.size),
                    None => (
                        oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                        blob_data_size(self.file_structure(), &child_pinned).await,
                    ),
                };
                chain.push(ChainBlob {
                    identifier: child_pinned,
                    role: ChainRole::Manifest,
                    media_type: child_media,
                    size: child_size,
                });

                let config_digest = oci::Digest::try_from(final_manifest.config.digest.as_str())
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                let config_pinned = oci::PinnedIdentifier::try_from(top_id.clone_with_digest(config_digest))
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                chain.push(ChainBlob {
                    identifier: config_pinned,
                    role: ChainRole::Config,
                    media_type: final_manifest.config.media_type.clone(),
                    size: final_manifest.config.size,
                });

                Ok(ResolvedChain {
                    pinned,
                    chain,
                    final_manifest,
                })
            }
        }
    }

    /// Resolves multiple identifiers in parallel, preserving input order.
    pub async fn resolve_all(
        &self,
        packages: &[oci::Identifier],
        platforms: Vec<oci::Platform>,
    ) -> Result<Vec<ResolvedChain>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let _spin = self.progress().spinner(format!("Resolving '{}'", packages[0]));
            let pinned = self.resolve(&packages[0], platforms).await.map_err(|kind| {
                package_manager::error::Error::ResolveFailed(vec![PackageError::new(packages[0].clone(), kind)])
            })?;
            return Ok(vec![pinned]);
        }

        let mut tasks = JoinSet::new();
        for package in packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            tasks.spawn(async move {
                let _spin = mgr.progress().spinner(format!("Resolving '{package}'"));
                let result = mgr.resolve(&package, platforms).await;
                (package, result)
            });
        }

        super::common::drain_package_tasks(packages, tasks, package_manager::error::Error::ResolveFailed).await
    }

    /// Resolve the composed env for the given roots.
    ///
    /// `self_view = true` selects the private surface (matches `--self`);
    /// `self_view = false` selects the interface surface (default exec).
    ///
    /// Delegates to [`composer::compose`] which iterates each root's
    /// pre-built TC flatly with cross-root dedup and per-surface gating.
    pub async fn resolve_env(
        &self,
        packages: &[Arc<InstallInfo>],
        self_view: bool,
    ) -> crate::Result<Vec<crate::package::metadata::env::entry::Entry>> {
        composer::compose(packages, &self.file_structure().packages, self_view).await
    }
}

/// Size of a chain blob's on-disk `data` file, or `-1` when it cannot be
/// stat'd. Manifest entries are guaranteed on disk by the `ResolvedChain`
/// invariant (`ChainedIndex` write-through), so this only meaningfully
/// returns `-1` for the trailing config blob — which the callers above
/// never pass here (config size comes from its descriptor).
///
/// A bare `metadata()` (not a `BlobGuard`-locked read) is deliberate: the
/// value is cosmetic (inspect display only, never correctness-bearing) and
/// the store is content-addressed, so a concurrent rewrite of the same
/// digest writes byte-identical content — a race cannot yield a wrong size.
async fn blob_data_size(file_structure: &crate::file_structure::FileStructure, pinned: &oci::PinnedIdentifier) -> i64 {
    let path = file_structure.blobs.data(pinned.registry(), &pinned.digest());
    match tokio::fs::metadata(&path).await {
        Ok(meta) => i64::try_from(meta.len()).unwrap_or(i64::MAX),
        Err(error) => {
            crate::log::debug!("Could not stat chain blob '{}': {error}.", path.display());
            -1
        }
    }
}

// ── Specification tests — plan_resolution_chain_refs.md (revised) ────────
//
// These tests replace the deleted `chain_walk` module's tests 33-38. They
// exercise `PackageManager::resolve` — now returning `ResolvedChain` — and
// the chain-accumulation invariants promised by the design record.
#[cfg(test)]
mod spec_tests {
    use tempfile::TempDir;

    use super::ChainRole;
    use crate::{
        file_structure::{BlobStore, FileStructure, TagStore},
        oci::index::{Index, LocalConfig, LocalIndex},
        oci::{self, Digest, Identifier},
        package_manager::PackageManager,
    };

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";
    const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn tagged_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }
    fn digest_a() -> Digest {
        Digest::Sha256(HEX_A.to_string())
    }
    fn digest_b() -> Digest {
        Digest::Sha256(HEX_B.to_string())
    }

    fn linux_amd64() -> oci::Platform {
        "linux/amd64".parse().unwrap()
    }

    /// Build a `PackageManager` whose local index already has the tag +
    /// blob files seeded on disk.
    fn make_manager(dir: &TempDir) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                tag_store: TagStore::new(dir.path().join("tags")),
                blob_store: BlobStore::new(dir.path().join("blobs")),
            }),
            Vec::new(),
            crate::oci::index::ChainMode::Offline,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// Writes a `TagLock`-shaped JSON file at `tag_path` mapping `TAG → digest`.
    /// Mirrors the on-disk format `LocalIndex` expects (see `tag_lock.rs`).
    fn write_tag_lock(tag_path: &std::path::Path, digest: &Digest) {
        std::fs::create_dir_all(tag_path.parent().unwrap()).unwrap();
        let json = format!(r#"{{"version":1,"repository":"{REGISTRY}/{REPO}","tags":{{"{TAG}":"{digest}"}}}}"#);
        std::fs::write(tag_path, json).unwrap();
    }

    /// Seed a flat `ImageManifest` tag + blob pair (single-entry chain).
    fn seed_flat_manifest(dir: &TempDir, digest: &Digest) {
        let tag_store = TagStore::new(dir.path().join("tags"));
        let id = Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG);
        write_tag_lock(&tag_store.tags(&id), digest);

        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let blob_path = blob_store.data(REGISTRY, digest);
        std::fs::create_dir_all(blob_path.parent().unwrap()).unwrap();
        let manifest_json = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;
        std::fs::write(&blob_path, manifest_json).unwrap();
    }

    /// Seed tag + top-level `ImageIndex` + child `ImageManifest` (two-entry chain).
    fn seed_image_index(dir: &TempDir, top_digest: &Digest, child_digest: &Digest) {
        let tag_store = TagStore::new(dir.path().join("tags"));
        let id = Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG);
        write_tag_lock(&tag_store.tags(&id), top_digest);

        let blob_store = BlobStore::new(dir.path().join("blobs"));

        let index_blob_path = blob_store.data(REGISTRY, top_digest);
        std::fs::create_dir_all(index_blob_path.parent().unwrap()).unwrap();
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":1,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#
        );
        std::fs::write(&index_blob_path, index_json).unwrap();

        let child_blob_path = blob_store.data(REGISTRY, child_digest);
        std::fs::create_dir_all(child_blob_path.parent().unwrap()).unwrap();
        let manifest_json = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;
        std::fs::write(&child_blob_path, manifest_json).unwrap();
    }

    /// `resolve` against a flat `ImageManifest` yields a `ResolvedChain`
    /// with two entries — the top-level manifest digest followed by the
    /// config-blob digest.
    #[tokio::test]
    async fn resolve_single_image_returns_two_chain_entries() {
        let dir = TempDir::new().unwrap();
        seed_flat_manifest(&dir, &digest_a());
        let mgr = make_manager(&dir);
        let result = mgr.resolve(&tagged_id(), vec![linux_amd64()]).await.unwrap();
        assert_eq!(
            result.chain.len(),
            2,
            "flat ImageManifest must produce manifest + config chain entries"
        );
        assert_eq!(result.pinned.digest(), digest_a());
        assert_eq!(result.chain[0].role, ChainRole::Manifest);
        assert_eq!(
            result.chain[1].identifier.digest().to_string(),
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "second entry must be the manifest's config-blob digest"
        );
        assert_eq!(result.chain[1].role, ChainRole::Config);
        assert_eq!(
            result.chain[1].size, 2,
            "config size must come from the manifest's config descriptor"
        );
    }

    /// `resolve` against an `ImageIndex` yields a `ResolvedChain` with three
    /// entries — the top-level index, the platform-selected child manifest,
    /// and the trailing config-blob digest.
    #[tokio::test]
    async fn resolve_image_index_returns_three_chain_entries() {
        let dir = TempDir::new().unwrap();
        seed_image_index(&dir, &digest_a(), &digest_b());
        let mgr = make_manager(&dir);
        let result = mgr.resolve(&tagged_id(), vec![linux_amd64()]).await.unwrap();
        assert_eq!(
            result.chain.len(),
            3,
            "ImageIndex must produce 3 chain entries (top + selected platform + config)"
        );
        assert_eq!(
            result.chain[0].identifier.digest(),
            digest_a(),
            "first entry must be the top-level index digest"
        );
        assert_eq!(
            result.chain[1].identifier.digest(),
            digest_b(),
            "second entry must be the platform-selected child digest"
        );
        assert_eq!(
            result.chain[2].identifier.digest().to_string(),
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "third entry must be the child manifest's config-blob digest"
        );
        assert_eq!(result.pinned.digest(), digest_b());
    }

    /// Nested image indexes (index pointing at another index) are rejected
    /// with a clear error — unsupported OCI shape.
    #[tokio::test]
    async fn resolve_rejects_nested_image_index() {
        let dir = TempDir::new().unwrap();

        let tag_store = TagStore::new(dir.path().join("tags"));
        let id = Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG);
        write_tag_lock(&tag_store.tags(&id), &digest_a());

        let blob_store = BlobStore::new(dir.path().join("blobs"));

        let blob_path = blob_store.data(REGISTRY, &digest_a());
        std::fs::create_dir_all(blob_path.parent().unwrap()).unwrap();
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.index.v1+json","digest":"{b}","size":1}}]}}"#,
            b = digest_b()
        );
        std::fs::write(&blob_path, index_json).unwrap();

        let child_path = blob_store.data(REGISTRY, &digest_b());
        std::fs::create_dir_all(child_path.parent().unwrap()).unwrap();
        let child_json = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        std::fs::write(&child_path, child_json).unwrap();

        let mgr = make_manager(&dir);
        let result = mgr.resolve(&tagged_id(), vec![linux_amd64()]).await;
        assert!(result.is_err(), "nested ImageIndex must be rejected with an error");
    }

    /// Property guarantee: every `(registry, digest)` entry in a successful
    /// `ResolvedChain` has an on-disk `data` file at the CAS-sharded path.
    #[tokio::test]
    async fn resolve_result_every_entry_has_on_disk_blob_file() {
        let dir = TempDir::new().unwrap();
        seed_flat_manifest(&dir, &digest_a());
        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let mgr = make_manager(&dir);
        let result = mgr.resolve(&tagged_id(), vec![linux_amd64()]).await.unwrap();

        // Manifest entries (all chain entries except the trailing config blob)
        // must be on disk — ChainedIndex write-through guarantees that.
        // The trailing config-blob entry is materialised later by
        // pull::setup_owned via common::fetch_or_get_blob, not by resolve.
        let manifest_entries = &result.chain[..result.chain.len() - 1];
        for blob in manifest_entries {
            let pinned = &blob.identifier;
            let blob_path = blob_store.data(pinned.registry(), &pinned.digest());
            assert!(
                blob_path.exists(),
                "property violated: manifest chain entry {pinned} has no on-disk blob at {}",
                blob_path.display()
            );
        }
    }
}
