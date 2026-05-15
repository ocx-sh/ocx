// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    oci,
    oci::index::IndexOperation,
    package::metadata::ValidMetadata,
    package_manager::{self, error::PackageErrorKind, tasks::resolve::ResolvedChain},
};

use super::super::PackageManager;

/// One child manifest of an image index, surfaced in default (no-`--resolve`)
/// mode so a caller can see which platforms a multi-platform tag offers
/// without committing to one.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The child manifest pinned by its own digest.
    pub identifier: oci::PinnedIdentifier,
    /// Declared platform, or [`oci::Platform::any`] when the entry omits one.
    pub platform: oci::Platform,
    /// The child descriptor's media type.
    pub media_type: String,
    /// The child descriptor's size in bytes.
    pub size: i64,
}

/// Read-only inspection output. The variant is chosen by what sits at the
/// requested reference and whether `--resolve` was given:
///
/// - [`Candidates`](InspectResult::Candidates) — default mode, the ref is an
///   image index: list the platform children, no metadata loaded.
/// - [`Manifest`](InspectResult::Manifest) — default mode, the ref is a single
///   image manifest (flat tag or `@digest`): metadata only, no chain.
/// - [`Resolved`](InspectResult::Resolved) — `--resolve`: platform-select
///   through the index, then metadata plus the full resolution chain.
///
/// No install or symlink side effects occur in any variant.
#[derive(Debug)]
pub enum InspectResult {
    Candidates {
        /// The image-index digest the candidates came from.
        pinned: oci::PinnedIdentifier,
        candidates: Vec<Candidate>,
    },
    Manifest {
        /// The manifest digest at the reference.
        pinned: oci::PinnedIdentifier,
        metadata: ValidMetadata,
    },
    Resolved {
        /// The platform-selected pinned identifier.
        pinned: oci::PinnedIdentifier,
        metadata: ValidMetadata,
        /// Boxed — `ResolvedChain` is large relative to the other variants
        /// (`clippy::large_enum_variant`).
        chain: Box<ResolvedChain>,
    },
}

impl PackageManager {
    /// Inspects `package` without installing or creating symlinks.
    ///
    /// `resolve == false` (default): the manifest at the reference is fetched
    /// **without** platform selection. An image index yields
    /// [`InspectResult::Candidates`] (the available platforms); a single image
    /// manifest yields [`InspectResult::Manifest`] (its declared metadata).
    /// `-p/--platform` does not apply here.
    ///
    /// `resolve == true`: the identifier is resolved through the index with
    /// platform selection (honoring `platforms`), returning
    /// [`InspectResult::Resolved`] with metadata and the resolution chain.
    ///
    /// Accepts a tag or an `@digest` identifier.
    ///
    /// # Errors
    ///
    /// - [`PackageErrorKind::NotFound`] — tag/digest unknown.
    /// - [`PackageErrorKind::OfflineManifestMissing`] — known tag but the
    ///   manifest blob is absent from the local cache in offline mode.
    /// - [`PackageErrorKind::Internal`] — config blob missing offline,
    ///   wrong media type, or metadata validation failure.
    pub async fn inspect(
        &self,
        package: &oci::Identifier,
        platforms: Vec<oci::Platform>,
        resolve: bool,
    ) -> Result<InspectResult, PackageErrorKind> {
        if resolve {
            let resolved = self.resolve(package, platforms).await?;
            let metadata =
                super::common::load_config_metadata(self.index(), &resolved.pinned, &resolved.final_manifest).await?;
            return Ok(InspectResult::Resolved {
                pinned: resolved.pinned.clone(),
                metadata,
                chain: Box::new(resolved),
            });
        }

        // Default mode: fetch the manifest at the reference without platform
        // selection, then adapt the result to its OCI shape.
        let (top_pinned, manifest) = fetch_top_manifest(self, package).await?;
        match manifest {
            oci::Manifest::Image(img) => {
                let metadata = super::common::load_config_metadata(self.index(), &top_pinned, &img).await?;
                Ok(InspectResult::Manifest {
                    pinned: top_pinned,
                    metadata,
                })
            }
            oci::Manifest::ImageIndex(index) => {
                let mut candidates = Vec::with_capacity(index.manifests.len());
                for entry in index.manifests {
                    let digest =
                        oci::Digest::try_from(entry.digest.as_str()).map_err(|_| PackageErrorKind::DigestMissing)?;
                    let identifier =
                        oci::PinnedIdentifier::try_from(top_pinned.as_identifier().clone_with_digest(digest))
                            .map_err(|_| PackageErrorKind::DigestMissing)?;
                    let platform = oci::Platform::try_from(entry.platform).map_err(PackageErrorKind::Internal)?;
                    candidates.push(Candidate {
                        identifier,
                        platform,
                        media_type: entry.media_type,
                        size: entry.size,
                    });
                }
                Ok(InspectResult::Candidates {
                    pinned: top_pinned,
                    candidates,
                })
            }
        }
    }
}

/// Fetches the top-level manifest for `package` without platform selection.
///
/// Mirrors the tag/digest top-id derivation and not-found discrimination of
/// [`PackageManager::resolve`] (tag truly unknown → [`PackageErrorKind::NotFound`];
/// known tag but blob missing offline → [`PackageErrorKind::OfflineManifestMissing`]),
/// but stops before platform selection so callers can inspect an image index
/// as-is.
async fn fetch_top_manifest(
    mgr: &PackageManager,
    package: &oci::Identifier,
) -> Result<(oci::PinnedIdentifier, oci::Manifest), PackageErrorKind> {
    let top_id = if package.digest().is_some() {
        package.clone()
    } else {
        package.clone_with_tag(package.tag_or_latest())
    };
    let (top_digest, top_manifest) = match mgr
        .index()
        .fetch_manifest(&top_id, IndexOperation::Resolve)
        .await
        .map_err(PackageErrorKind::Internal)?
    {
        Some(result) => result,
        None => {
            if let Some(digest) = mgr
                .index()
                .fetch_manifest_digest(&top_id, IndexOperation::Resolve)
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
    let top_pinned = oci::PinnedIdentifier::try_from(top_id.clone_with_digest(top_digest))
        .map_err(|_| PackageErrorKind::DigestMissing)?;
    Ok((top_pinned, top_manifest))
}

#[cfg(test)]
mod spec_tests {
    use tempfile::TempDir;

    use crate::{
        MEDIA_TYPE_PACKAGE_METADATA_V1,
        file_structure::{BlobStore, FileStructure, TagStore},
        oci::index::{ChainMode, Index, LocalConfig, LocalIndex},
        oci::{self, Digest, Identifier},
        package_manager::{PackageManager, error::PackageErrorKind},
    };

    use super::InspectResult;

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";
    const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const HEX_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    const METADATA_JSON: &str = r#"{"type":"bundle","version":1,"env":[{"key":"PATH","type":"path","value":"${installPath}/bin","visibility":"public"}],"dependencies":[],"entrypoints":{}}"#;

    fn tagged_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }
    fn digest(hex: &str) -> Digest {
        Digest::Sha256(hex.to_string())
    }
    fn linux_amd64() -> oci::Platform {
        "linux/amd64".parse().unwrap()
    }

    fn make_manager(dir: &TempDir) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                tag_store: TagStore::new(dir.path().join("tags")),
                blob_store: BlobStore::new(dir.path().join("blobs")),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    fn write_tag_lock(tag_path: &std::path::Path, d: &Digest) {
        std::fs::create_dir_all(tag_path.parent().unwrap()).unwrap();
        let json = format!(r#"{{"version":1,"repository":"{REGISTRY}/{REPO}","tags":{{"{TAG}":"{d}"}}}}"#);
        std::fs::write(tag_path, json).unwrap();
    }

    fn write_blob(dir: &TempDir, d: &Digest, bytes: &str) {
        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let path = blob_store.data(REGISTRY, d);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
    }

    fn image_manifest_json(config_digest: &Digest) -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
            size = METADATA_JSON.len(),
        )
    }

    /// Default mode against a flat image manifest returns metadata only.
    #[tokio::test]
    async fn inspect_default_flat_manifest_returns_metadata() {
        let dir = TempDir::new().unwrap();
        let tag_store = TagStore::new(dir.path().join("tags"));
        write_tag_lock(&tag_store.tags(&tagged_id()), &digest(HEX_A));
        write_blob(&dir, &digest(HEX_A), &image_manifest_json(&digest(HEX_C)));
        write_blob(&dir, &digest(HEX_C), METADATA_JSON);

        let mgr = make_manager(&dir);
        let result = mgr.inspect(&tagged_id(), vec![linux_amd64()], false).await.unwrap();

        match result {
            InspectResult::Manifest { pinned, metadata } => {
                assert_eq!(pinned.digest(), digest(HEX_A));
                assert_eq!(metadata.env().expect("env").into_iter().count(), 1);
            }
            other => panic!("expected Manifest, got {other:?}"),
        }
    }

    /// Default mode against an image index returns the platform candidates,
    /// no metadata loaded.
    #[tokio::test]
    async fn inspect_default_image_index_returns_candidates() {
        let dir = TempDir::new().unwrap();
        let tag_store = TagStore::new(dir.path().join("tags"));
        write_tag_lock(&tag_store.tags(&tagged_id()), &digest(HEX_A));
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child}","size":7,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
            child = digest(HEX_B),
        );
        write_blob(&dir, &digest(HEX_A), &index_json);

        let mgr = make_manager(&dir);
        let result = mgr.inspect(&tagged_id(), vec![], false).await.unwrap();

        match result {
            InspectResult::Candidates { pinned, candidates } => {
                assert_eq!(pinned.digest(), digest(HEX_A), "pinned = index digest");
                assert_eq!(candidates.len(), 1);
                assert_eq!(candidates[0].identifier.digest(), digest(HEX_B));
                assert_eq!(candidates[0].platform, linux_amd64());
                assert_eq!(candidates[0].size, 7);
            }
            other => panic!("expected Candidates, got {other:?}"),
        }
    }

    /// `--resolve` against an image index platform-selects the child and
    /// returns metadata plus a 3-entry chain.
    #[tokio::test]
    async fn inspect_resolve_image_index_returns_chain() {
        let dir = TempDir::new().unwrap();
        let tag_store = TagStore::new(dir.path().join("tags"));
        write_tag_lock(&tag_store.tags(&tagged_id()), &digest(HEX_A));
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child}","size":1,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
            child = digest(HEX_B),
        );
        write_blob(&dir, &digest(HEX_A), &index_json);
        write_blob(&dir, &digest(HEX_B), &image_manifest_json(&digest(HEX_C)));
        write_blob(&dir, &digest(HEX_C), METADATA_JSON);

        let mgr = make_manager(&dir);
        let result = mgr.inspect(&tagged_id(), vec![linux_amd64()], true).await.unwrap();

        match result {
            InspectResult::Resolved { pinned, chain, .. } => {
                assert_eq!(pinned.digest(), digest(HEX_B));
                assert_eq!(chain.chain.len(), 3, "index + child + config");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    /// Unknown tag resolves to `NotFound` in default mode.
    #[tokio::test]
    async fn inspect_unknown_tag_is_not_found() {
        let dir = TempDir::new().unwrap();
        let mgr = make_manager(&dir);
        let err = mgr.inspect(&tagged_id(), vec![], false).await.unwrap_err();
        assert!(
            matches!(err, PackageErrorKind::NotFound),
            "unknown tag must be NotFound, got {err:?}"
        );
    }
}
