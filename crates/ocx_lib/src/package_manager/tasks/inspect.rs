// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use tokio::task::JoinSet;

use crate::{
    oci,
    oci::index::IndexOperation,
    package::metadata::ValidMetadata,
    package_manager::{self, error::PackageError, error::PackageErrorKind, tasks::resolve::ResolvedChain},
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
///   image manifest (flat tag or `@digest`): metadata plus the manifest's
///   layer descriptors, no resolution chain.
/// - [`Resolved`](InspectResult::Resolved) — `--resolve`: platform-select
///   through the index, then metadata plus the full resolution chain.
///
/// No install or symlink side effects occur in any variant. Default mode may
/// still populate the local index / blob cache on a tag cache miss — a
/// `Resolve`-class read, not a write the caller asked for. "Read-only" here
/// means "no install, no symlink mutation", not "touches no local cache".
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
        /// The manifest's layer descriptors (digest, media type, size).
        /// Already carried by the fetched manifest — surfaced so a default
        /// inspect shows the package's content without forcing `--resolve`.
        layers: Vec<oci::Descriptor>,
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
    /// No install or symlink side effects occur. Default mode resolves the
    /// tag through the index with `IndexOperation::Resolve`, so a tag cache
    /// miss may populate the local index / blob cache as a side effect of
    /// the read — intended behavior, not a write the caller requested.
    ///
    /// `resolve == false` (default): the manifest at the reference is fetched
    /// **without** platform selection. An image index yields
    /// [`InspectResult::Candidates`] (the available platforms); a single image
    /// manifest yields [`InspectResult::Manifest`] (its declared metadata and
    /// layer descriptors). `-p/--platform` does not apply here.
    ///
    /// `resolve == true`: the identifier is resolved through the index with
    /// platform selection (honoring `platform`), returning
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
        platform: oci::Platform,
        resolve: bool,
    ) -> Result<InspectResult, PackageErrorKind> {
        if resolve {
            let resolved = self.resolve(package, platform).await?;
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
                    layers: img.layers,
                })
            }
            oci::Manifest::ImageIndex(index) => {
                let mut candidates = Vec::with_capacity(index.manifests.len());
                for entry in index.manifests {
                    // A child descriptor whose `digest` string does not parse
                    // is a corrupt image index, not a "missing digest" — carry
                    // the structured `DigestError` so the message names the
                    // bad value (still classifies to DataError/65).
                    let digest = oci::Digest::try_from(entry.digest.as_str())
                        .map_err(|e| PackageErrorKind::Internal(crate::Error::from(e)))?;
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

    /// Inspects multiple packages in parallel, preserving input order.
    ///
    /// Empty input short-circuits to `Ok(vec![])`; a single package takes the
    /// direct path; otherwise each package is inspected on its own task and the
    /// results are drained via
    /// [`drain_package_tasks`](super::common::drain_package_tasks), which
    /// returns successes in input order and batch errors sorted by input index
    /// (deterministic exit code). Mirrors [`find_all`](PackageManager::find_all).
    pub async fn inspect_all(
        &self,
        packages: Vec<oci::Identifier>,
        platform: oci::Platform,
        resolve: bool,
    ) -> Result<Vec<InspectResult>, package_manager::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let _spin = self.progress().spinner(format!("Inspecting '{}'", packages[0]));
            let result = self.inspect(&packages[0], platform, resolve).await.map_err(|kind| {
                package_manager::error::Error::InspectFailed(vec![PackageError::new(packages[0].clone(), kind)])
            })?;
            return Ok(vec![result]);
        }

        let mut tasks = JoinSet::new();
        for package in &packages {
            let mgr = self.clone();
            let package = package.clone();
            let platform = platform.clone();
            tasks.spawn(async move {
                let _spin = mgr.progress().spinner(format!("Inspecting '{package}'"));
                let result = mgr.inspect(&package, platform, resolve).await;
                (package, result)
            });
        }

        super::common::drain_package_tasks(&packages, tasks, package_manager::error::Error::InspectFailed).await
    }
}

/// Fetches the top-level manifest for `package` without platform selection.
///
/// Thin wrapper over [`super::common::resolve_top_manifest`] pinning the
/// default-inspect [`IndexOperation::Resolve`] routing (intended behavior —
/// inspect deliberately uses `Resolve`, not `Query`). The shared helper
/// mirrors the tag/digest top-id derivation and not-found discrimination of
/// [`PackageManager::resolve`] (tag truly unknown → [`PackageErrorKind::NotFound`];
/// known tag but blob missing offline → [`PackageErrorKind::OfflineManifestMissing`]),
/// but stops before platform selection so callers can inspect an image index
/// as-is.
async fn fetch_top_manifest(
    mgr: &PackageManager,
    package: &oci::Identifier,
) -> Result<(oci::PinnedIdentifier, oci::Manifest), PackageErrorKind> {
    super::common::resolve_top_manifest(mgr.index(), package, IndexOperation::Resolve).await
}

#[cfg(test)]
mod spec_tests {
    use tempfile::TempDir;

    use crate::{
        MEDIA_TYPE_PACKAGE_METADATA_V1,
        file_structure::FileStructure,
        oci::index::{ChainMode, Index, IndexImpl, IndexOperation, LocalConfig, LocalIndex},
        oci::{self, Algorithm, Digest, Identifier},
        package_manager::{PackageManager, error::PackageErrorKind},
    };

    use super::InspectResult;

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";
    // A fixed child digest used only as a *reference* inside an image index (the
    // child manifest is not read in default mode), and a fixed layer digest
    // referenced by a manifest descriptor — neither is a stored, verified object.
    const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const HEX_D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

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

    /// A minimal fake source serving a fixed set of `(tag-or-digest) -> bytes`
    /// manifest entries, keyed by tag for a tag-addressed lookup or by digest
    /// string for a digest-addressed one (a platform-selected child), plus a
    /// separate `digest -> bytes` map for opaque config blobs (`fetch_blob`,
    /// a distinct seam from manifest resolution). Used with
    /// `ChainMode::Default` so `PackageManager::inspect` can recover content
    /// through the same AbsentLeaf-recovery path a live registry would — a
    /// leaf platform manifest is never locally cached (A3), so an
    /// offline-only pre-seeded fixture cannot answer a lookup for one.
    #[derive(Clone, Default)]
    struct FakeManifestSource {
        entries: std::collections::HashMap<String, (Vec<u8>, Digest, oci::Manifest)>,
        blobs: std::collections::HashMap<String, Vec<u8>>,
    }

    impl FakeManifestSource {
        fn with(mut self, key: &str, bytes: &[u8]) -> Self {
            let digest = Algorithm::Sha256.hash(bytes);
            let manifest = serde_json::from_slice(bytes).unwrap();
            self.entries.insert(key.to_string(), (bytes.to_vec(), digest, manifest));
            self
        }

        /// Register an opaque blob (e.g. a package-metadata config blob,
        /// which does not parse as an OCI [`oci::Manifest`]) served by
        /// `fetch_blob`, keyed by its own digest string.
        fn with_blob(mut self, digest: &str, bytes: &[u8]) -> Self {
            self.blobs.insert(digest.to_string(), bytes.to_vec());
            self
        }

        fn lookup(&self, identifier: &Identifier) -> Option<(Vec<u8>, Digest, oci::Manifest)> {
            let key = match identifier.digest() {
                Some(digest) => digest.to_string(),
                None => identifier.tag_or_latest().to_string(),
            };
            self.entries.get(&key).cloned()
        }
    }

    #[async_trait::async_trait]
    impl IndexImpl for FakeManifestSource {
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            identifier: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(Digest, oci::Manifest)>> {
            Ok(self.lookup(identifier).map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<Digest>> {
            Ok(self.lookup(identifier).map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
            // Config blobs are fetched by digest via `Index::fetch_blob`
            // (`load_config_metadata`), a separate seam from
            // `fetch_manifest_raw_bytes`.
            Ok(self.blobs.get(&blob_ref.digest().to_string()).cloned())
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &Identifier,
        ) -> crate::Result<Option<(Vec<u8>, Digest, oci::Manifest)>> {
            Ok(self.lookup(identifier))
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Build a `PackageManager` chained to `source` under `ChainMode::Default`.
    fn make_manager(dir: &TempDir, source: FakeManifestSource) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                snapshot_store: fs.index.clone(),
            }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// An offline `PackageManager` with no sources and an empty local index
    /// — for tests asserting a genuine local miss / policy block.
    fn make_offline_manager(dir: &TempDir) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                snapshot_store: fs.index.clone(),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    fn image_manifest_json(config_digest: &Digest) -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{size}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"{layer}","size":4096}}]}}"#,
            size = METADATA_JSON.len(),
            layer = digest(HEX_D),
        )
    }

    /// Default mode against a flat image manifest returns metadata plus the
    /// manifest's layer descriptors — no `--resolve` needed. A leaf platform
    /// manifest is never locally cached (A3); the manager is chained to a
    /// live fake source under `ChainMode::Default` (default-inspect uses
    /// `IndexOperation::Resolve`, so the walk reaches it) that recovers it.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_default_flat_manifest_returns_metadata_and_layers() {
        let dir = TempDir::new().unwrap();
        let config_digest = Algorithm::Sha256.hash(METADATA_JSON.as_bytes());
        let manifest_json = image_manifest_json(&config_digest);
        let manifest_digest = Algorithm::Sha256.hash(manifest_json.as_bytes());
        let source = FakeManifestSource::default()
            .with(TAG, manifest_json.as_bytes())
            .with_blob(&config_digest.to_string(), METADATA_JSON.as_bytes());

        let mgr = make_manager(&dir, source);
        let result = mgr.inspect(&tagged_id(), linux_amd64(), false).await.unwrap();

        match result {
            InspectResult::Manifest {
                pinned,
                metadata,
                layers,
            } => {
                assert_eq!(pinned.digest(), manifest_digest);
                assert_eq!(metadata.env().expect("env").into_iter().count(), 1);
                assert_eq!(layers.len(), 1, "manifest layer surfaced in default mode");
                assert_eq!(layers[0].digest, format!("sha256:{HEX_D}"));
                assert_eq!(layers[0].size, 4096);
            }
            other => panic!("expected Manifest, got {other:?}"),
        }
    }

    /// Default mode against an image index returns the platform candidates,
    /// no metadata loaded. The top-level index is dispatch-shaped (locally
    /// cacheable, A3); the child is only a reference here (not read in
    /// default mode), so a fixed digest is fine and no fake source is needed.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_default_image_index_returns_candidates() {
        let dir = TempDir::new().unwrap();
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child}","size":7,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#,
            child = digest(HEX_B),
        );
        let index_digest = Algorithm::Sha256.hash(index_json.as_bytes());
        let source = FakeManifestSource::default().with(TAG, index_json.as_bytes());

        let mgr = make_manager(&dir, source);
        let result = mgr.inspect(&tagged_id(), oci::Platform::any(), false).await.unwrap();

        match result {
            InspectResult::Candidates { pinned, candidates } => {
                assert_eq!(pinned.digest(), index_digest, "pinned = index digest");
                assert_eq!(candidates.len(), 1);
                assert_eq!(candidates[0].identifier.digest(), digest(HEX_B));
                assert_eq!(candidates[0].platform, linux_amd64());
                assert_eq!(candidates[0].size, 7);
            }
            other => panic!("expected Candidates, got {other:?}"),
        }
    }

    /// `--resolve` against an image index platform-selects the child and
    /// returns metadata plus a 3-entry chain. The top-level index is
    /// dispatch-shaped (locally cacheable); the platform-selected child is a
    /// leaf, recovered via the fake source.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_resolve_image_index_returns_chain() {
        let dir = TempDir::new().unwrap();
        let config_digest = Algorithm::Sha256.hash(METADATA_JSON.as_bytes());
        let child_json = image_manifest_json(&config_digest);
        let child_digest = Algorithm::Sha256.hash(child_json.as_bytes());
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":1,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#
        );
        let source = FakeManifestSource::default()
            .with(TAG, index_json.as_bytes())
            .with(&child_digest.to_string(), child_json.as_bytes())
            .with_blob(&config_digest.to_string(), METADATA_JSON.as_bytes());

        let mgr = make_manager(&dir, source);
        let result = mgr.inspect(&tagged_id(), linux_amd64(), true).await.unwrap();

        match result {
            InspectResult::Resolved { pinned, chain, .. } => {
                assert_eq!(pinned.digest(), child_digest);
                assert_eq!(chain.chain.len(), 3, "index + child + config");
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    /// Unknown (unpinned) tag under the offline manager is a **policy block**
    /// (#155): the local index has no pointer and offline forbids walking the
    /// source, so resolution refuses with `PolicyResolutionBlocked` (exit 81)
    /// rather than a not-found (79). This unifies offline with frozen — under
    /// either no-resolve policy the resolver was forbidden from checking, so
    /// "policy blocked" is the honest answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_unknown_tag_is_policy_blocked_offline() {
        let dir = TempDir::new().unwrap();
        let mgr = make_offline_manager(&dir);
        let err = mgr
            .inspect(&tagged_id(), oci::Platform::any(), false)
            .await
            .unwrap_err();
        match err {
            PackageErrorKind::Internal(crate::Error::OciIndex(
                crate::oci::index::error::Error::PolicyResolutionBlocked { policy, .. },
            )) => assert_eq!(
                policy, "offline",
                "offline manager must label the policy block as offline"
            ),
            other => panic!("unknown tag offline must be a policy block (exit 81), got {other:?}"),
        }
    }

    // ── F4 (Warn gap): exit-65 / malformed inputs ──
    //
    // Design record (`subsystem-cli-commands.md` "package inspect" gotcha):
    // "Exit codes via `classify_error`: NotFound→79, offline manifest/blob
    // miss→81, malformed metadata→65." The `Internal` / `DigestMissing`
    // kinds are what `classify_error` maps to `DataError` (65) at the CLI
    // boundary; these unit tests pin the kind the task layer must surface.

    /// Default mode against a flat image manifest whose config blob holds
    /// structurally-invalid metadata must surface `PackageErrorKind::Internal`
    /// (the metadata-validation-failure path documented for `inspect`).
    /// Mirrors the `common.rs` `load_object_data_rejects_invalid_metadata`
    /// pattern: an env entry references an undeclared dependency, so
    /// `ValidMetadata::try_from` rejects it at the ingress boundary.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_default_malformed_metadata_is_internal() {
        const BAD_METADATA_JSON: &str = r#"{"type":"bundle","version":1,"dependencies":[],"env":[{"key":"FOO","type":"constant","value":"${deps.missing.installPath}/x","visibility":"public"}],"entrypoints":{}}"#;

        let dir = TempDir::new().unwrap();
        let config_digest = Algorithm::Sha256.hash(BAD_METADATA_JSON.as_bytes());
        // The image manifest's config descriptor advertises the structurally
        // invalid metadata blob's length so the media-type/size gate passes
        // and validation is the failing step.
        let manifest_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"{MEDIA_TYPE_PACKAGE_METADATA_V1}","digest":"{config_digest}","size":{size}}},"layers":[]}}"#,
            size = BAD_METADATA_JSON.len(),
        );
        let source = FakeManifestSource::default()
            .with(TAG, manifest_json.as_bytes())
            .with_blob(&config_digest.to_string(), BAD_METADATA_JSON.as_bytes());

        let mgr = make_manager(&dir, source);
        let err = mgr
            .inspect(&tagged_id(), oci::Platform::any(), false)
            .await
            .unwrap_err();

        assert!(
            matches!(err, PackageErrorKind::Internal(_)),
            "malformed metadata must surface Internal (→ DataError/65), got {err:?}"
        );
    }

    /// Default mode against an image index whose child descriptor carries a
    /// structurally-invalid `digest` string must surface
    /// `PackageErrorKind::Internal` wrapping the structured `DigestError`
    /// (so the message names the bad value), not the misleading
    /// `DigestMissing` ("identifier has no digest after resolution"). The
    /// kind still classifies to `DataError`/65.
    #[tokio::test(flavor = "multi_thread")]
    async fn inspect_default_bad_child_digest_is_internal_digest_error() {
        let dir = TempDir::new().unwrap();
        // Child descriptor `digest` is not a valid `algorithm:hex` string.
        let index_json = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"not-a-valid-digest","size":7,"platform":{"os":"linux","architecture":"amd64"}}]}"#;
        let source = FakeManifestSource::default().with(TAG, index_json.as_bytes());

        let mgr = make_manager(&dir, source);
        let err = mgr
            .inspect(&tagged_id(), oci::Platform::any(), false)
            .await
            .unwrap_err();

        assert!(
            matches!(err, PackageErrorKind::Internal(_)),
            "malformed child digest must surface Internal(DigestError), got {err:?}"
        );
        let chain = format!("{err:#}");
        assert!(
            chain.contains("not-a-valid-digest"),
            "error chain must name the bad digest value: {chain}"
        );
    }
}
