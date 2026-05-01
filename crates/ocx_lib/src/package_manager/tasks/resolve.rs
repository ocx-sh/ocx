// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;

use tokio::task::JoinSet;
use tracing::{Instrument, info_span};

use crate::{
    oci,
    oci::index::SelectResult,
    package::install_info::InstallInfo,
    package_manager::{self, composer, error::PackageError, error::PackageErrorKind},
};

use super::super::PackageManager;

/// The full resolution output for a single identifier.
///
/// `chain` lists every pinned identifier the resolver touched — one entry
/// per manifest blob, in walk order. Every entry is backed by an on-disk
/// `blobs/{registry}/.../data` file, guaranteed by `ChainedIndex`
/// write-through persistence. `final_manifest` is the platform-selected
/// image manifest (never an image index).
#[derive(Debug, Clone)]
pub struct ResolvedChain {
    /// The platform-selected pinned identifier — same value the old
    /// `resolve` method returned.
    pub pinned: oci::PinnedIdentifier,
    /// Walk-order pinned identifiers for every manifest blob the resolver
    /// touched, backed by on-disk blob files.
    pub chain: Vec<oci::PinnedIdentifier>,
    /// The platform-selected image manifest used by the pull pipeline for
    /// layer extraction. Never an image index.
    pub final_manifest: oci::ImageManifest,
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
        let top_id = if package.digest().is_some() {
            package.clone()
        } else {
            package.clone_with_tag(package.tag_or_latest())
        };
        let (top_digest, top_manifest) = match self
            .index()
            .fetch_manifest(&top_id)
            .await
            .map_err(PackageErrorKind::Internal)?
        {
            Some(result) => result,
            None => {
                // Distinguish "tag truly unknown" (NotFound) from "tag cached
                // locally but manifest blob missing from the cache"
                // (OfflineManifestMissing — requires online re-pull). We ask
                // the index for the tag → digest mapping: if that succeeds,
                // the tag is known, so fetch_manifest returning None implies
                // the blob is missing rather than the tag is unknown.
                if let Some(digest) = self
                    .index()
                    .fetch_manifest_digest(&top_id)
                    .await
                    .map_err(PackageErrorKind::Internal)?
                {
                    return Err(PackageErrorKind::OfflineManifestMissing(Box::new(
                        package_manager::error::OfflineManifestMissing {
                            identifier: top_id.clone(),
                            digest,
                        },
                    )));
                }
                return Err(PackageErrorKind::NotFound);
            }
        };

        let top_pinned = oci::PinnedIdentifier::try_from(top_id.clone_with_digest(top_digest.clone()))
            .map_err(|_| PackageErrorKind::DigestMissing)?;
        let mut chain = vec![top_pinned.clone()];

        match top_manifest {
            // Flat image manifest: the chain is a single entry and the
            // top-level digest IS the pinned identifier. Platform filtering
            // does not apply here — a single-platform package always matches.
            oci::Manifest::Image(img) => Ok(ResolvedChain {
                pinned: top_pinned,
                chain,
                final_manifest: img,
            }),
            // Image index: defer platform selection to `Index::select`, then
            // fetch the selected child to append it to the chain and return
            // its manifest as `final_manifest`.
            oci::Manifest::ImageIndex(_) => {
                let pinned = match self.index().select(&top_id, platforms).await {
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
                    .fetch_manifest(&child_id)
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
                chain.push(child_pinned);

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
            let pinned = self
                .resolve(&packages[0], platforms)
                .instrument(crate::cli::progress::spinner_span(
                    info_span!("Resolving", package = %packages[0]),
                    &packages[0],
                ))
                .await
                .map_err(|kind| {
                    package_manager::error::Error::ResolveFailed(vec![PackageError::new(packages[0].clone(), kind)])
                })?;
            return Ok(vec![pinned]);
        }

        let mut tasks = JoinSet::new();
        for package in packages {
            let mgr = self.clone();
            let package = package.clone();
            let platforms = platforms.clone();
            let span = crate::cli::progress::spinner_span(info_span!("Resolving", package = %package), &package);
            tasks.spawn(
                async move {
                    let result = mgr.resolve(&package, platforms).await;
                    (package, result)
                }
                .instrument(span),
            );
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

// ── Specification tests — plan_resolution_chain_refs.md (revised) ────────
//
// These tests replace the deleted `chain_walk` module's tests 33-38. They
// exercise `PackageManager::resolve` — now returning `ResolvedChain` — and
// the chain-accumulation invariants promised by the design record.
#[cfg(test)]
mod spec_tests {
    use tempfile::TempDir;

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
        oci::Platform::current().unwrap()
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
    /// with exactly one entry — the top-level manifest digest.
    #[tokio::test]
    async fn resolve_single_image_returns_one_chain_entry() {
        let dir = TempDir::new().unwrap();
        seed_flat_manifest(&dir, &digest_a());
        let mgr = make_manager(&dir);
        let result = mgr.resolve(&tagged_id(), vec![linux_amd64()]).await.unwrap();
        assert_eq!(
            result.chain.len(),
            1,
            "flat ImageManifest must produce exactly 1 chain entry"
        );
        assert_eq!(result.pinned.digest(), digest_a());
    }

    /// `resolve` against an `ImageIndex` yields a `ResolvedChain` with two
    /// entries — the top-level index plus the platform-selected child.
    #[tokio::test]
    async fn resolve_image_index_returns_two_chain_entries() {
        let dir = TempDir::new().unwrap();
        seed_image_index(&dir, &digest_a(), &digest_b());
        let mgr = make_manager(&dir);
        let result = mgr.resolve(&tagged_id(), vec![linux_amd64()]).await.unwrap();
        assert_eq!(
            result.chain.len(),
            2,
            "ImageIndex must produce 2 chain entries (top + selected platform)"
        );
        assert_eq!(
            result.chain[0].digest(),
            digest_a(),
            "first entry must be the top-level index digest"
        );
        assert_eq!(
            result.chain[1].digest(),
            digest_b(),
            "second entry must be the platform-selected child digest"
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

        for pinned in &result.chain {
            let blob_path = blob_store.data(pinned.registry(), &pinned.digest());
            assert!(
                blob_path.exists(),
                "property violated: chain entry {pinned} has no on-disk blob at {}",
                blob_path.display()
            );
        }
    }
}
