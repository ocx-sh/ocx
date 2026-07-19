// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::time::Duration;

use crate::{
    archive, file_structure, oci,
    package::{self, install_info::InstallInfo, metadata},
    package_manager::error::PackageErrorKind,
    publisher, utility,
};

use super::super::PackageManager;
use super::pull::{SetupGroups, setup_owned};
use super::resolve::{ChainBlob, ChainRole, ResolvedChain};

/// Singleflight coordinator for blob writes within a single `pull_local` operation.
///
/// Owns a `singleflight::Group<Digest, ()>` scoped to one pull operation, coalescing
/// concurrent same-digest fan-out from the resolver `JoinSet`. Created per
/// [`PackageManager::pull_local`] call so entries are freed when the pull completes.
///
/// Index-layer callers (`write_manifest_blob`, `local_index::stage_blob_bytes`) call
/// `BlobStore::write_blob` directly without a coordinator — they are sequential callers
/// and content-addressed safety covers them.
pub(crate) struct PullCoordinator {
    write_group: utility::singleflight::Group<oci::Digest, ()>,
}

impl PullCoordinator {
    pub(crate) fn new() -> Self {
        Self {
            write_group: utility::singleflight::Group::new(
                /* max_entries */ 1024,
                /* timeout */ Duration::from_secs(60),
            ),
        }
    }

    /// Wrap `BlobStore::write_blob` with per-pull-operation dedup.
    ///
    /// The first caller for a given `digest` becomes the leader and performs
    /// the actual write. Concurrent callers with the same digest become waiters
    /// and receive the result when the leader completes. This prevents redundant
    /// concurrent downloads of the same digest within a single pull operation.
    pub(crate) async fn stage_blob_bytes(
        &self,
        store: &crate::file_structure::BlobStore,
        registry: &str,
        digest: &oci::Digest,
        bytes: &[u8],
    ) -> crate::Result<()> {
        use utility::singleflight::Acquisition;

        match self.write_group.try_acquire(digest.clone()).await {
            Ok(Acquisition::Resolved(())) => Ok(()),
            Ok(Acquisition::Leader(handle)) => {
                match store.write_blob(registry, digest, bytes).await {
                    Ok(()) => {
                        handle.complete(());
                        Ok(())
                    }
                    Err(e) => {
                        // Broadcast a shared error to waiters; the leader
                        // surfaces the original typed error to its caller.
                        let _shared = handle.fail(std::io::Error::other(format!("{e}")));
                        Err(e)
                    }
                }
            }
            Err(sf_err) => Err(crate::Error::Singleflight(sf_err)),
        }
    }
}

/// Maximum size (in bytes) for a file-layer archive that `pull_local` will load into memory.
///
/// 8 GiB is chosen to resist accidental OOM on CI runners (typical limit: 16 GiB RAM) while
/// still being larger than any realistic single-package archive. A file-layer that exceeds this
/// limit is almost certainly either a mistake or a malformed input. CWE-400/789.
const MAX_FILE_LAYER_BYTES: u64 = 8 * 1024 * 1024 * 1024;

impl PackageManager {
    /// Materialize a package built from a local [`Info`](package::info::Info) and layer
    /// descriptors without going through the registry.
    ///
    /// Reuses the install pipeline (dep resolution, blob extraction, entrypoint generation,
    /// atomic move) and threads `dest_override` through the two destination computation
    /// sites in `pull.rs` so the root package lands at the caller-supplied path rather
    /// than the content-addressed object store.
    ///
    /// # Parameters
    ///
    /// * `info` — package identifier + metadata + platform.
    /// * `layers` — [`LayerRef`](publisher::LayerRef)s in declaration order. `File` layers
    ///   are read, sha256-hashed, and staged into the regular `BlobStore`. `Digest` layers
    ///   are pulled from the registry on demand by the existing layer-fetch path. In offline
    ///   mode, missing digest blobs error with `OfflineMode`.
    /// * `dest_override` — when `Some(path)`, the package is moved to `path` instead of
    ///   `$OCX_HOME/packages/{registry}/.../{digest}/`. The launcher bake-in path is computed
    ///   from the same override. `path` must be empty (or absent) and reside on the same
    ///   filesystem as `$OCX_HOME/layers/`.
    ///
    /// # Singleflight bypass
    ///
    /// Unlike [`pull`](PackageManager::pull), this method constructs a fresh `SetupGroups`
    /// per call and calls `setup_owned` directly, bypassing the `setup_impl` dedup gate.
    /// This ensures two concurrent invocations of the same content with different
    /// `dest_override` paths each get their own materialization. Layer-level singleflight
    /// (within the fresh group) still deduplicates within a single invocation's transitive
    /// layer pulls.
    ///
    /// # Side effects
    ///
    /// Dependencies are auto-installed into the regular object store under
    /// `$OCX_HOME/packages/`. Only the root package honors the override.
    ///
    /// # Errors
    ///
    /// Returns [`PackageErrorKind::Internal`] wrapping [`crate::Error::OfflineMode`] when offline
    /// mode is active and a digest layer must be fetched from the registry. Returns
    /// [`PackageErrorKind::Internal`] for I/O failures, plus all other error kinds raised by the
    /// install pipeline.
    pub async fn pull_local(
        &self,
        info: package::info::Info,
        layers: &[publisher::LayerRef],
        dest_override: Option<&std::path::Path>,
    ) -> Result<InstallInfo, PackageErrorKind> {
        let fs = self.file_structure();
        let registry = info.identifier.registry().to_string();

        // Create a per-pull coordinator that coalesces concurrent same-digest
        // blob writes within this operation via singleflight dedup.
        let coordinator = PullCoordinator::new();

        // Step 1: Resolve layer descriptors locally.
        // File layers → hash + write to blobs/ + extract to layers/{digest}/content/
        // Digest layers → pull from registry on demand (or error offline).
        let layer_descriptors = stage_layers(self, layers, &info.identifier, &registry, &coordinator).await?;

        // Step 2: Synthesize the OCI image manifest from the info + layer descriptors.
        // Shared with `Publisher::push_package_image` so push and test agree byte-for-byte.
        let parts = crate::oci::manifest_builder::build_package_manifest(&info.metadata, layer_descriptors)
            .map_err(|e| PackageErrorKind::Internal(e.into()))?;

        // Step 3: Stage the manifest blob to blobs/ so refs/blobs/ links resolve.
        stage_blob_bytes(
            fs,
            &registry,
            &parts.manifest_bytes,
            &parts.manifest_digest,
            &coordinator,
        )
        .await?;

        // Step 4: Synthesize a PinnedIdentifier keyed by the manifest digest.
        let pinned = {
            let id_with_digest = info.identifier.clone_with_digest(parts.manifest_digest.clone());
            oci::PinnedIdentifier::try_from(id_with_digest).map_err(|e| PackageErrorKind::Internal(e.into()))?
        };

        // Step 5: Validate metadata (same gate as setup_owned applies to registry-fetched
        // metadata). Do it here so the error surfaces before we acquire a temp dir.
        let validated_metadata: metadata::Metadata = metadata::ValidMetadata::try_from(info.metadata)
            .map_err(PackageErrorKind::Internal)?
            .into();

        // Step 6: Synthesize a ResolvedChain from the staged manifest.
        // All-pub fields — struct literal per plan §2.2 step 4.
        let manifest_media = parts
            .manifest
            .media_type
            .clone()
            .unwrap_or_else(|| oci::OCI_IMAGE_MEDIA_TYPE.to_string());
        let chain = ResolvedChain {
            pinned: pinned.clone(),
            chain: vec![ChainBlob {
                identifier: pinned.clone(),
                role: ChainRole::Manifest,
                media_type: manifest_media,
                size: i64::try_from(parts.manifest_bytes.len()).unwrap_or(i64::MAX),
            }],
            final_manifest: parts.manifest,
            platform: info.platform.clone(),
        };

        // Step 7: Hand off to setup_owned with a fresh SetupGroups (singleflight bypass).
        // Concurrent calls with different dest_override paths each get their own
        // materialization — the fresh group prevents cross-contamination.
        setup_owned(
            self,
            &pinned,
            chain,
            info.platform.clone(),
            SetupGroups::new(),
            dest_override,
            Some(validated_metadata),
        )
        .await
    }
}

/// Stage layer refs into the local `blobs/` + `layers/` stores.
///
/// Returns OCI descriptors in the same order as `layers`, ready for
/// [`build_image_manifest`](crate::oci::manifest_builder::build_image_manifest).
///
/// File layers are read, hashed, their raw bytes written to `blobs/` (so
/// `refs/blobs/` links resolve after install), and the archive extracted into
/// `layers/{registry}/{digest}/content/` so the assembly step finds the content
/// via the fast-path.
///
/// Digest layers are expected to already have their content present in
/// `layers/{registry}/{digest}/content/`. If absent: error with
/// `PackageErrorKind::OfflineMode` (offline) or pull via client (online).
async fn stage_layers(
    mgr: &PackageManager,
    layers: &[publisher::LayerRef],
    base_identifier: &oci::Identifier,
    registry: &str,
    coordinator: &PullCoordinator,
) -> Result<Vec<oci::Descriptor>, PackageErrorKind> {
    use crate::MEDIA_TYPE_TAR_GZ;

    let fs = mgr.file_structure();
    let mut descriptors = Vec::with_capacity(layers.len());

    for layer_ref in layers {
        match layer_ref {
            publisher::LayerRef::File { path, layout } => {
                validate_file_layer(path).await?;

                // Read + hash the archive in one pass.
                let (bytes, digest) = oci::Algorithm::Sha256
                    .hash_file_read(path)
                    .await
                    .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(path.clone(), e)))?;

                let size = i64::try_from(bytes.len()).map_err(|_| {
                    PackageErrorKind::Internal(
                        crate::oci::client::error::ClientError::InvalidManifest(format!(
                            "layer blob at '{}' is larger than i64::MAX bytes",
                            path.display()
                        ))
                        .into(),
                    )
                })?;

                // Infer media type from extension.
                let media_type = crate::media_type_from_path(path).unwrap_or(MEDIA_TYPE_TAR_GZ);

                // Stage raw bytes to blobs/ so refs/blobs/ links are valid.
                stage_blob_bytes(fs, registry, &bytes, &digest, coordinator).await?;

                // Explicitly release the allocation before extraction so peak RSS is
                // approximately max(archive_size) rather than 2× (Perf W3).
                drop(bytes);

                // Extract archive into layers/{registry}/{digest}/content/
                // if not already present (idempotent).
                let layer_content = fs.layers.content(registry, &digest);
                if !crate::utility::fs::path_exists_lossy(&layer_content).await {
                    let layer_path = fs.layers.path(registry, &digest);
                    let temp_extract = layer_path.with_extension("_extract_tmp");
                    extract_archive_to_temp(path, &temp_extract, &digest).await?;
                    super::layer_staging::finalize_layer_dir(fs, registry, &digest, &temp_extract).await?;
                }

                descriptors.push(oci::Descriptor {
                    media_type: media_type.to_string(),
                    digest: digest.to_string(),
                    size,
                    urls: None,
                    artifact_type: None,
                    // Emits keys only when the publisher set layout (BC2); the
                    // default spec yields `None`, keeping the manifest identical.
                    annotations: layout.to_annotations(),
                });
            }

            publisher::LayerRef::Digest {
                digest,
                media_type,
                layout,
            } => {
                // Check if the layer content is already present locally.
                let layer_content = fs.layers.content(registry, digest);
                if crate::utility::fs::path_exists_lossy(&layer_content).await {
                    let size = resolve_digest_size(mgr, fs, base_identifier, registry, digest).await?;
                    descriptors.push(oci::Descriptor {
                        media_type: media_type.as_media_type().to_string(),
                        digest: digest.to_string(),
                        size,
                        urls: None,
                        artifact_type: None,
                        annotations: layout.to_annotations(),
                    });
                } else if mgr.is_offline() {
                    return Err(PackageErrorKind::Internal(crate::Error::OfflineMode));
                } else {
                    // Online: pull blob from registry into a temp dir, then atomic-rename.
                    let layer_path = fs.layers.path(registry, digest);
                    let temp_layer = layer_path.with_extension("_tmp");
                    let blob_size =
                        pull_digest_layer_to_temp(mgr, base_identifier, digest, media_type, &temp_layer).await?;
                    super::layer_staging::finalize_layer_dir(fs, registry, digest, &temp_layer).await?;

                    descriptors.push(oci::Descriptor {
                        media_type: media_type.as_media_type().to_string(),
                        digest: digest.to_string(),
                        size: blob_size,
                        urls: None,
                        artifact_type: None,
                        annotations: layout.to_annotations(),
                    });
                }
            }
        }
    }

    Ok(descriptors)
}

/// Validate a file-layer source path before reading it.
///
/// Rejects non-regular files (directories, symlinks, FIFOs, sockets, devices) and
/// archives exceeding [`MAX_FILE_LAYER_BYTES`]. CWE-400/789 defense: bounding the
/// in-memory hash buffer and refusing unbounded streams (FIFOs report `len() == 0`
/// but stream forever).
///
/// Uses `symlink_metadata`, which does NOT follow symlinks. A symlink to a regular
/// file is therefore rejected here, eliminating the TOCTOU class where an adversary
/// swaps the symlink target between the `is_file()` check and the subsequent
/// `hash_file_read` / `extract_with_options` opens.
async fn validate_file_layer(path: &std::path::Path) -> Result<(), PackageErrorKind> {
    let file_meta = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(path.to_path_buf(), e)))?;
    if !file_meta.is_file() {
        return Err(PackageErrorKind::Internal(crate::Error::InternalFile(
            path.to_path_buf(),
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "file layer at '{}' must be a regular file (not a directory, symlink, FIFO, socket, or device)",
                    path.display(),
                ),
            ),
        )));
    }
    let file_size = file_meta.len();
    if file_size > MAX_FILE_LAYER_BYTES {
        return Err(PackageErrorKind::Internal(
            crate::oci::client::error::ClientError::InvalidManifest(format!(
                "file layer at '{}' is {} bytes, exceeding the {}-byte limit (MAX_FILE_LAYER_BYTES); \
                 use a smaller archive or a digest reference to an already-staged layer",
                path.display(),
                file_size,
                MAX_FILE_LAYER_BYTES,
            ))
            .into(),
        ));
    }
    Ok(())
}

/// Resolve the byte size of a cached digest layer's blob.
///
/// Order of strategies:
/// 1. **Local blob fast path** — if `blobs/{registry}/{digest}/data` exists, stat it.
/// 2. **Offline error** — if no local blob and `mgr.is_offline()`, return
///    [`crate::Error::OfflineMode`]. Required for manifest parity with `package push`:
///    the OCI descriptor's `size` field cannot default to 0.
/// 3. **HEAD fallback** — online with no local blob, capture content-length via
///    [`oci::Client::head_blob`].
async fn resolve_digest_size(
    mgr: &PackageManager,
    fs: &file_structure::FileStructure,
    base_identifier: &oci::Identifier,
    registry: &str,
    digest: &oci::Digest,
) -> Result<i64, PackageErrorKind> {
    let blob_data = fs.blobs.data(registry, digest);
    if blob_data.exists() {
        let meta = tokio::fs::metadata(&blob_data)
            .await
            .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(blob_data.clone(), e)))?;
        let blob_len = meta.len();
        return i64::try_from(blob_len).map_err(|_| {
            PackageErrorKind::Internal(
                crate::oci::client::error::ClientError::InvalidManifest(format!(
                    "blob size {blob_len} for digest {digest} exceeds i64::MAX"
                ))
                .into(),
            )
        });
    }
    if mgr.is_offline() {
        return Err(PackageErrorKind::Internal(crate::Error::OfflineMode));
    }
    let client = mgr.require_client().map_err(PackageErrorKind::Internal)?;
    let size_u64 = client
        .head_blob(base_identifier, digest)
        .await
        .map_err(PackageErrorKind::Internal)?;
    i64::try_from(size_u64).map_err(|_| {
        PackageErrorKind::Internal(
            crate::oci::client::error::ClientError::InvalidManifest(format!(
                "blob size {size_u64} for digest {digest} exceeds i64::MAX"
            ))
            .into(),
        )
    })
}

/// HEAD the digest blob (parity with `package push`), pull it via the registry client
/// into `temp_layer/`, write the CAS-recovery digest file. Returns the blob's
/// content-length for the synthesized OCI descriptor. Stops *before* the atomic rename
/// into the layer store.
///
/// Calls `head_blob` first so the descriptor's `size` field has byte-for-byte parity
/// with the manifest produced by `package push` (see `client.rs:602`). The synthesized
/// pinned identifier is rooted at the real package repo because OCI layer blobs live
/// in the same repo as the referencing manifest.
async fn pull_digest_layer_to_temp(
    mgr: &PackageManager,
    base_identifier: &oci::Identifier,
    digest: &oci::Digest,
    media_type: &publisher::ArchiveMediaType,
    temp_layer: &std::path::Path,
) -> Result<i64, PackageErrorKind> {
    let client = mgr.require_client().map_err(PackageErrorKind::Internal)?;
    let size_u64 = client
        .head_blob(base_identifier, digest)
        .await
        .map_err(PackageErrorKind::Internal)?;
    let blob_size = i64::try_from(size_u64).map_err(|_| {
        PackageErrorKind::Internal(
            crate::oci::client::error::ClientError::InvalidManifest(format!(
                "blob size {size_u64} for digest {digest} exceeds i64::MAX"
            ))
            .into(),
        )
    })?;

    let layer_desc = oci::Descriptor {
        media_type: media_type.as_media_type().to_string(),
        digest: digest.to_string(),
        size: blob_size,
        urls: None,
        artifact_type: None,
        annotations: None,
    };

    let synth_pinned = oci::PinnedIdentifier::try_from(base_identifier.clone_with_digest(digest.clone()))
        .map_err(|e| PackageErrorKind::Internal(e.into()))?;

    client.pull_layer(&synth_pinned, &layer_desc, temp_layer).await?;

    file_structure::write_digest_file(&temp_layer.join(file_structure::DIGEST_FILENAME), digest)
        .await
        .map_err(PackageErrorKind::Internal)?;

    Ok(blob_size)
}

/// Mkdir `temp_extract/content/`, extract the archive at `src` into it, write the
/// CAS-recovery digest file at `temp_extract/{DIGEST_FILENAME}`. Stops *before* the
/// atomic rename into the layer store — caller owns that step.
///
/// The archive extracts verbatim (`strip_components: 0`) into the shared
/// content-addressed layer store, exactly like the registry pull path. The
/// package-wide strip is applied once, later, at assemble time — extracting
/// with a strip here would both corrupt the shared store on blob reuse and
/// double-strip, because the same assemble step re-applies it.
async fn extract_archive_to_temp(
    src: &std::path::Path,
    temp_extract: &std::path::Path,
    digest: &oci::Digest,
) -> Result<(), PackageErrorKind> {
    let content_in_temp = temp_extract.join("content");
    tokio::fs::create_dir_all(&content_in_temp)
        .await
        .map_err(|e| PackageErrorKind::Internal(crate::Error::InternalFile(content_in_temp.clone(), e)))?;

    let extract_options = archive::ExtractOptions {
        strip_components: 0,
        algorithm: None,
    };
    archive::Archive::extract_with_options(src, &content_in_temp, Some(extract_options))
        .await
        .map_err(PackageErrorKind::Internal)?;

    file_structure::write_digest_file(&temp_extract.join(file_structure::DIGEST_FILENAME), digest)
        .await
        .map_err(PackageErrorKind::Internal)?;

    Ok(())
}

/// Write `bytes` into `blobs/{registry}/{digest}/data` via tempfile + atomic rename.
///
/// Content-addressed: if the blob data file already exists the write is skipped
/// (identity guaranteed — same digest ⟹ same bytes). The `coordinator` coalesces
/// concurrent same-digest writes within a single pull operation so the underlying
/// `BlobStore::write_blob` is called at most once per unique digest.
async fn stage_blob_bytes(
    fs: &file_structure::FileStructure,
    registry: &str,
    bytes: &[u8],
    digest: &oci::Digest,
    coordinator: &PullCoordinator,
) -> Result<(), PackageErrorKind> {
    // Fast-path: blob is content-addressed, so if the data file exists the
    // bytes are identical. Skip before entering the singleflight group.
    if fs.blobs.data(registry, digest).exists() {
        return Ok(());
    }

    coordinator
        .stage_blob_bytes(&fs.blobs, registry, digest, bytes)
        .await
        .map_err(PackageErrorKind::Internal)
}

#[cfg(test)]
mod tests {
    use crate::{
        file_structure::{BlobStore, FileStructure, TagStore},
        oci::{
            self,
            index::{ChainMode, Index, LocalConfig, LocalIndex},
        },
        package::{
            info::Info,
            metadata::{
                Metadata,
                bundle::{Bundle, Version},
                dependency::Dependencies,
                entrypoint::{Entrypoint, Entrypoints},
                env::Env,
            },
        },
        package_manager::PackageManager,
    };

    // Consumed only by tests gated `#[cfg(unix)]` / `#[cfg(not(target_os = "windows"))]`
    // — on Windows neither test compiles, so guard the imports to match.
    #[cfg(not(target_os = "windows"))]
    use super::MAX_FILE_LAYER_BYTES;
    #[cfg(not(target_os = "windows"))]
    use crate::package_manager::error::PackageErrorKind;

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Build an isolated offline `PackageManager` backed by a temp directory.
    ///
    /// Returns the `TempDir` guard (kept alive for the test), the manager, and
    /// the root path so tests can probe the file system.
    fn setup_offline_manager() -> (tempfile::TempDir, PackageManager) {
        let dir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                tag_store: TagStore::new(dir.path().join("tags")),
                blob_store: BlobStore::new(dir.path().join("blobs")),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        // None client = offline mode — no registry calls.
        let mgr = PackageManager::new(fs, index, None, "example.com");
        (dir, mgr)
    }

    /// Build a minimal [`Info`] fixture for testing.
    ///
    /// Uses a deterministic tag identifier (no digest). `pull_local` will
    /// compute and assign a digest internally after manifest assembly.
    fn fixture_info(dir_name: &str) -> Info {
        let identifier =
            oci::Identifier::new_registry(format!("test/{dir_name}"), "example.com").clone_with_tag("1.0.0");
        let metadata = Metadata::Bundle(Bundle {
            binaries: None,
            version: Version::V1,
            strip_components: None,
            env: Env::default(),
            dependencies: Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        let platform = oci::Platform::Specific {
            os: oci::OperatingSystem::Linux,
            arch: oci::Architecture::Amd64,
            variant: None,
            os_features: Vec::new(),
        };
        Info {
            identifier,
            metadata,
            platform,
        }
    }

    /// Build a minimal [`Info`] fixture with one entrypoint so launcher
    /// generation is exercised.
    fn fixture_info_with_entrypoint(dir_name: &str) -> Info {
        let mut info = fixture_info(dir_name);
        let name = crate::package::metadata::entrypoint::EntrypointName::try_from("hello").unwrap();
        let Metadata::Bundle(ref mut b) = info.metadata;
        b.entrypoints = Entrypoints::new(std::collections::BTreeMap::from([(name, Entrypoint::default())]));
        info
    }

    // ── dest_override_threads_to_move ─────────────────────────────────────────
    //
    // When `dest_override = Some(path)`, `pull_local` must move the assembled
    // package to `path`, NOT to `fs.packages.path(pinned)`.
    //
    // At Phase 3 (Specify), this test panics with `unimplemented!()` — that is
    // the expected failing state. After Phase 4 implementation it must pass.
    #[tokio::test(flavor = "multi_thread")]
    async fn dest_override_threads_to_move() {
        let (root_dir, mgr) = setup_offline_manager();
        let info = fixture_info("mytool");

        // Destination override: a fresh directory inside the temp root.
        let dest = root_dir.path().join("override-dest");
        std::fs::create_dir_all(&dest).unwrap();

        let result = mgr.pull_local(info, &[], Some(&dest)).await;
        let install_info = result.expect("pull_local with dest_override must succeed");

        // The package root reported by InstallInfo must equal the override path,
        // NOT any path under $OCX_HOME/packages/.
        let pkg_root = install_info.dir().root();
        assert_eq!(
            pkg_root, dest,
            "InstallInfo.dir().root() must equal dest_override, not the object store path"
        );

        // The object store path for this package must NOT be created.
        let object_store_path = mgr.file_structure().packages.path(install_info.identifier());
        assert!(
            !object_store_path.exists(),
            "package must NOT be materialized in $OCX_HOME/packages/ when dest_override is set; \
             found: {}",
            object_store_path.display()
        );
    }

    // ── extract_archive_to_temp_writes_verbatim_tree (U11) ────────────────────
    //
    // U11 (B1 · D12): the local materialization path extracts a VERBATIM layer
    // tree into the shared content-addressed layer store — the package-wide
    // strip is NOT applied here (it moved to assemble time). A bundle whose tar
    // carries a leading top-level directory keeps that directory in `content/`.
    // This is what prevents the double-strip a partial fix would create on the
    // local path (extract-strip + the same assemble-strip re-applied).
    #[tokio::test]
    async fn extract_archive_to_temp_writes_verbatim_tree() {
        // Build a .tar.gz whose single entry carries a leading directory; tar's
        // unpack creates the parent dirs, materializing `topdir/bin/tool`.
        let archive_bytes = {
            let mut buf = Vec::new();
            let encoder = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let content = b"tool bytes\n";
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "topdir/bin/tool", &content[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
            buf
        };

        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("bundle.tar.gz");
        std::fs::write(&archive_path, &archive_bytes).unwrap();

        let temp_extract = dir.path().join("temp-extract");
        std::fs::create_dir_all(&temp_extract).unwrap();

        // The digest is only written to the CAS-recovery marker; any valid
        // sha256 works — extraction never consults it.
        let digest = oci::Digest::Sha256("a".repeat(64));

        super::extract_archive_to_temp(&archive_path, &temp_extract, &digest)
            .await
            .expect("extract_archive_to_temp must succeed");

        let content = temp_extract.join("content");
        // Verbatim: the leading directory is preserved (no strip on the local path).
        assert!(
            content.join("topdir/bin/tool").is_file(),
            "local extraction must be verbatim — topdir/bin/tool must exist under content/"
        );
        // No strip / no double-strip: the top dir must NOT be removed at extraction.
        assert!(
            !content.join("bin/tool").exists(),
            "local extraction must NOT strip the leading component into the shared layer store"
        );
        assert_eq!(
            std::fs::read(content.join("topdir/bin/tool")).unwrap(),
            b"tool bytes\n",
            "extracted file contents must be intact"
        );
    }

    // ── launcher_baked_with_override_root ─────────────────────────────────────
    //
    // When `dest_override = Some(path)`, the generated launcher script must
    // contain `path` as the baked-in package root — NOT the object store path.
    //
    // The launcher uses the pkg root to resolve `${installPath}/bin` and similar
    // template expansions at runtime. If the wrong root is baked in, every binary
    // invocation finds wrong paths.
    //
    // Requires at least one entrypoint so launcher generation is triggered.
    #[tokio::test(flavor = "multi_thread")]
    async fn launcher_baked_with_override_root() {
        let (root_dir, mgr) = setup_offline_manager();
        let info = fixture_info_with_entrypoint("launcher-tool");

        let dest = root_dir.path().join("launcher-dest");
        std::fs::create_dir_all(&dest).unwrap();

        let result = mgr.pull_local(info, &[], Some(&dest)).await;
        let _install_info = result.expect("pull_local with dest_override must succeed");

        // Launchers are written to `{dest}/entrypoints/`. Scan any launcher
        // script and assert its body contains the override root path.
        let entrypoints_dir = dest.join("entrypoints");
        assert!(
            entrypoints_dir.exists(),
            "entrypoints dir must exist when entrypoints are declared"
        );
        let launcher_contains_override =
            std::fs::read_dir(&entrypoints_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .any(|entry| {
                    let contents = std::fs::read_to_string(entry.path()).unwrap_or_default();
                    contents.contains(dest.to_str().unwrap())
                });
        assert!(
            launcher_contains_override,
            "at least one launcher script must contain the override dest path '{}'",
            dest.display()
        );
    }

    // ── concurrent_same_content_distinct_dests ────────────────────────────────
    //
    // Two concurrent `pull_local` calls with identical `info` but different
    // `dest_override` paths must BOTH succeed and each materialize independently.
    //
    // This is the regression test for review finding B2: the singleflight key in
    // `setup_impl` is `pinned.strip_advisory()` (content-addressed). If `pull_local`
    // used `setup_impl`, the second call would be deduplicated and land in the same
    // destination as the first. By bypassing `setup_impl` and calling `setup_owned`
    // with a fresh `SetupGroups`, each call gets its own materialization.
    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_same_content_distinct_dests() {
        let (root_dir, mgr) = setup_offline_manager();
        let info_a = fixture_info("concurrent-tool");
        let info_b = fixture_info("concurrent-tool"); // same identifier

        let dest_a = root_dir.path().join("dest-a");
        let dest_b = root_dir.path().join("dest-b");
        std::fs::create_dir_all(&dest_a).unwrap();
        std::fs::create_dir_all(&dest_b).unwrap();

        // Run both calls concurrently — they must not interfere.
        let (result_a, result_b) = tokio::join!(
            mgr.pull_local(info_a, &[], Some(&dest_a)),
            mgr.pull_local(info_b, &[], Some(&dest_b)),
        );

        let info_a = result_a.expect("first concurrent pull_local must succeed");
        let info_b = result_b.expect("second concurrent pull_local must succeed");

        // Each call must report its own destination.
        assert_eq!(info_a.dir().root(), dest_a, "first call must land in dest-a");
        assert_eq!(info_b.dir().root(), dest_b, "second call must land in dest-b");

        // Both destinations must be populated (content dir exists).
        assert!(
            dest_a.join("content").exists() || dest_a.exists(),
            "dest_a must be materialized"
        );
        assert!(
            dest_b.join("content").exists() || dest_b.exists(),
            "dest_b must be materialized"
        );
    }

    // ── digest_layer_descriptor_size_matches_head_blob ────────────────────────
    //
    // Regression test for Warn #3 (parity bug): when a `LayerRef::Digest` layer
    // must be pulled from the registry (content not yet locally present), the
    // synthesized OCI descriptor must carry the real blob size reported by
    // `head_blob`, not a hardcoded 0.
    //
    // A descriptor with `size: 0` produces manifest bytes that differ from what
    // `package push` would produce for the same layer — breaking the plan §1
    // "byte-for-byte parity" promise.
    //
    // This test uses a `StubTransport` with the real `archive.tar.xz` fixture
    // to exercise the full download + extraction path, then confirms that
    // `head_blob` was called and the resulting manifest has a non-zero layer size.
    #[tokio::test(flavor = "multi_thread")]
    async fn digest_layer_descriptor_size_matches_head_blob() {
        use crate::{
            oci::{
                Algorithm, Client,
                client::test_transport::{StubTransport, StubTransportData},
            },
            publisher::ArchiveMediaType,
        };

        // ── Build a fake blob: the real tar.xz fixture bytes ─────────────────
        let archive_path = crate::test::data::archive_xz();
        let archive_bytes = std::fs::read(&archive_path).expect("test fixture archive.tar.xz must exist");
        let expected_size = i64::try_from(archive_bytes.len()).expect("fixture size fits i64");
        let digest = Algorithm::Sha256.hash(&archive_bytes);
        let digest_str = digest.to_string();

        // ── Set up stub transport with the archive bytes ──────────────────────
        let stub_data = StubTransportData::new();
        stub_data.write().blobs.insert(digest_str.clone(), archive_bytes);

        let client = Client::with_transport(Box::new(StubTransport::new(stub_data.clone())));

        // ── Build an online PackageManager (has a client) ─────────────────────
        let dir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                tag_store: TagStore::new(dir.path().join("tags")),
                blob_store: BlobStore::new(dir.path().join("blobs")),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        let mgr = PackageManager::new(fs.clone(), index, Some(client), "example.com");

        let info = fixture_info("digest-parity-tool");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        // LayerRef::Digest pointing at the fixture archive.
        let layers = [crate::publisher::LayerRef::Digest {
            digest: digest.clone(),
            media_type: ArchiveMediaType::TarXz,
            layout: oci::LayerLayoutSpec::default(),
        }];

        let result = mgr.pull_local(info, &layers, Some(&dest)).await;
        assert!(
            result.is_ok(),
            "pull_local with a valid digest layer must succeed; err: {:?}",
            result.err()
        );

        // ── Verify head_blob was called for the digest ────────────────────────
        let calls = stub_data.read().calls.clone();
        let head_blob_called = calls.iter().any(|c| c == &format!("head_blob:{digest_str}"));
        assert!(
            head_blob_called,
            "head_blob must be called for digest-layer pull to capture real size; calls: {calls:?}"
        );

        // ── Verify the manifest stored in the blob store has non-zero layer size
        //
        // The manifest bytes were staged by stage_blob_bytes. Walk the blobs
        // directory to find the manifest blob (it is the only JSON blob with a
        // "layers" key), parse it, and check the layer sizes.
        let blobs_root = dir.path().join("blobs");
        let manifest_has_nonzero_size =
            find_manifest_layer_sizes(&blobs_root).expect("at least one manifest blob must exist after pull_local");
        assert!(
            manifest_has_nonzero_size.iter().all(|&s| s == expected_size),
            "all digest layers in synthesized manifest must have size = {expected_size} (from head_blob); \
             got sizes: {manifest_has_nonzero_size:?}"
        );
    }

    /// Recursively walk `dir` and collect all layer sizes from every OCI
    /// image manifest JSON blob (files named `"data"` containing a `"layers"`
    /// array with `"size"` fields).
    ///
    /// Returns `None` if no manifest blobs were found.
    fn find_manifest_layer_sizes(dir: &std::path::Path) -> Option<Vec<i64>> {
        fn walk(dir: &std::path::Path, out: &mut Vec<i64>) {
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.file_name().and_then(|n| n.to_str()) == Some("data") {
                    let bytes = std::fs::read(&path).unwrap_or_default();
                    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&bytes)
                        && let Some(layers) = json.get("layers").and_then(|l| l.as_array())
                    {
                        for layer in layers {
                            if let Some(size) = layer.get("size").and_then(|s| s.as_i64()) {
                                out.push(size);
                            }
                        }
                    }
                }
            }
        }
        let mut sizes = Vec::new();
        walk(dir, &mut sizes);
        if sizes.is_empty() { None } else { Some(sizes) }
    }

    // ── file_layer_exceeding_size_cap_errors ─────────────────────────────────
    //
    // CWE-400/789: a file-layer archive whose on-disk size exceeds
    // `MAX_FILE_LAYER_BYTES` must be rejected BEFORE `hash_file_read` allocates
    // a matching `Vec<u8>`.
    //
    // The test creates a sparse file of exactly `MAX_FILE_LAYER_BYTES + 1` bytes.
    // Sparse files on Linux/macOS occupy no real disk blocks; the test completes
    // instantly without consuming gigabytes of RAM.
    //
    // On Windows `File::set_len` may pre-allocate real space depending on
    // filesystem type; we skip the test there to avoid storage pressure.
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(not(target_os = "windows"))]
    async fn file_layer_exceeding_size_cap_errors() {
        use std::fs::OpenOptions;
        use std::io::Seek;

        let (root_dir, mgr) = setup_offline_manager();
        let info = fixture_info("oversized-tool");

        // Create a sparse file one byte beyond the cap.
        let archive_path = root_dir.path().join("oversized.tar.xz");
        {
            let mut f = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&archive_path)
                .unwrap();
            // `set_len` on a new file creates a sparse file — no real allocation.
            f.set_len(MAX_FILE_LAYER_BYTES + 1).unwrap();
            // Move the file pointer so the OS flush completes correctly.
            f.seek(std::io::SeekFrom::End(0)).unwrap();
        }

        let layers = [crate::publisher::LayerRef::File {
            path: archive_path,
            layout: oci::LayerLayoutSpec::default(),
        }];
        let result = mgr.pull_local(info, &layers, None).await;

        assert!(
            result.is_err(),
            "pull_local must reject a file layer exceeding MAX_FILE_LAYER_BYTES before allocation"
        );
        // The error must be Internal (not a different kind).
        match result.unwrap_err() {
            PackageErrorKind::Internal(_) => {}
            other => panic!("expected Internal error for oversized file layer, got: {other:?}"),
        }
    }

    // ── cached_digest_layer_with_absent_blob_uses_head_blob ───────────────────
    //
    // When a digest layer's content directory is already present locally but
    // the raw blob data file is absent, `stage_layers` must call `head_blob`
    // to capture the real size rather than defaulting to 0.
    //
    // A size of 0 in the synthesized manifest breaks byte-for-byte parity with
    // the manifest produced by `package push` for the same layer.
    #[tokio::test(flavor = "multi_thread")]
    async fn cached_digest_layer_with_absent_blob_uses_head_blob() {
        use crate::{
            oci::{
                Algorithm, Client,
                client::test_transport::{StubTransport, StubTransportData},
            },
            publisher::ArchiveMediaType,
        };

        // Build a fake archive blob.
        let archive_path = crate::test::data::archive_xz();
        let archive_bytes = std::fs::read(&archive_path).expect("archive fixture must exist");
        let expected_size = i64::try_from(archive_bytes.len()).unwrap();
        let digest = Algorithm::Sha256.hash(&archive_bytes);
        let digest_str = digest.to_string();

        // Set up stub transport.
        let stub_data = StubTransportData::new();
        stub_data
            .write()
            .blobs
            .insert(digest_str.clone(), archive_bytes.clone());
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data.clone())));

        // Build online PackageManager.
        let dir = tempfile::tempdir().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                tag_store: TagStore::new(dir.path().join("tags")),
                blob_store: BlobStore::new(dir.path().join("blobs")),
            }),
            Vec::new(),
            crate::oci::index::ChainMode::Offline,
        );
        let mgr = PackageManager::new(fs.clone(), index, Some(client), "example.com");

        // Pre-create the layer content directory so the fast-path fires.
        // Do NOT write the blob data file — this is the "absent blob" scenario.
        let registry = "example.com";
        let layer_content = fs.layers.content(registry, &digest);
        tokio::fs::create_dir_all(&layer_content).await.unwrap();

        let info = fixture_info("cached-blob-size-tool");
        let dest = dir.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let layers = [crate::publisher::LayerRef::Digest {
            digest: digest.clone(),
            media_type: ArchiveMediaType::TarXz,
            layout: oci::LayerLayoutSpec::default(),
        }];

        let result = mgr.pull_local(info, &layers, Some(&dest)).await;
        assert!(
            result.is_ok(),
            "pull_local with cached layer but absent blob must succeed via head_blob; err: {:?}",
            result.err()
        );

        // Verify head_blob was called for the digest.
        let calls = stub_data.read().calls.clone();
        let head_blob_called = calls.iter().any(|c| c == &format!("head_blob:{digest_str}"));
        assert!(
            head_blob_called,
            "head_blob must be called when layer content is cached but blob data file is absent; \
             calls: {calls:?}"
        );

        // Verify manifest has non-zero layer size.
        let blobs_root = dir.path().join("blobs");
        if let Some(sizes) = find_manifest_layer_sizes(&blobs_root) {
            assert!(
                sizes.iter().all(|&s| s == expected_size),
                "manifest layer sizes must equal expected_size={expected_size} (from head_blob); \
                 got: {sizes:?}"
            );
        }
    }

    // ── non_regular_file_layer_rejected ──────────────────────────────────────
    //
    // CWE-400/789 FIFO bypass: a named socket passed as a file layer reports
    // `len() == 0` via `Metadata::len()`, which would bypass the size cap and
    // allow `hash_file_read` to enter an unbounded read loop accumulating into
    // `Vec<u8>` without bound.
    //
    // The fix adds an `is_file()` guard before the size check.  This test
    // verifies the guard by passing a Unix domain socket as a layer and
    // asserting a clean `Internal` error rather than a hang.
    //
    // Sockets are chosen because `std::os::unix::net::UnixListener::bind`
    // creates one without extra dependencies (unlike FIFOs which require
    // `nix::unistd::mkfifo` or `mkfifo(1)`).
    #[tokio::test(flavor = "multi_thread")]
    #[cfg(unix)]
    async fn non_regular_file_layer_rejected() {
        let (root_dir, mgr) = setup_offline_manager();
        let info = fixture_info("non-regular-tool");

        // Create a Unix domain socket at a path inside the temp root.
        // A socket is a non-regular file whose `Metadata::len()` is 0 on Linux
        // and macOS — exactly the file type that bypasses a size-only guard.
        let socket_path = root_dir.path().join("fake.tar.gz");
        let _listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .expect("must be able to bind a Unix socket in temp dir");

        let layers = [crate::publisher::LayerRef::File {
            path: socket_path,
            layout: oci::LayerLayoutSpec::default(),
        }];
        let result = mgr.pull_local(info, &layers, None).await;

        assert!(
            result.is_err(),
            "pull_local must reject a non-regular file layer (socket) immediately"
        );
        // The error must be Internal(Error::InternalFile(...)) so that classify_error
        // maps it to ExitCode::IoError (74), not DataError (65).
        match result.unwrap_err() {
            PackageErrorKind::Internal(crate::Error::InternalFile(_, ref io_err)) => {
                assert_eq!(
                    io_err.kind(),
                    std::io::ErrorKind::InvalidInput,
                    "non-regular file rejection must carry io::ErrorKind::InvalidInput"
                );
            }
            other => panic!("expected Internal(InternalFile) error for non-regular file layer, got: {other:?}"),
        }
    }

    // ── pull_coordinator_coalesces_concurrent_same_digest_writers ────────────
    //
    // PullCoordinator.stage_blob_bytes must coalesce concurrent calls for the
    // same digest: the first call (leader) performs the actual write; concurrent
    // callers (waiters) receive Ok(()) WITHOUT re-executing the write.
    //
    // Asserts the coalescing property via the `BlobStore::WRITE_BLOB_CALL_COUNT`
    // test-only AtomicUsize: after two concurrent calls resolve, the counter
    // must equal exactly 1. Without this assertion, content-addressing alone
    // would make "both calls return Ok" a passing condition even when no dedup
    // happens — masking a regression that would otherwise cost a duplicate
    // download per concurrent caller in `ocx_mirror`.
    //
    // Uses a `tokio::sync::Barrier` to force both async tasks to reach
    // `stage_blob_bytes` before either advances, ensuring the leader/waiter
    // split is exercised deterministically rather than relying on scheduler
    // luck.
    #[tokio::test(flavor = "multi_thread")]
    async fn pull_coordinator_coalesces_concurrent_same_digest_writers() {
        use crate::{file_structure::BlobStore, oci::Algorithm};
        use std::sync::Arc;
        use std::sync::atomic::Ordering;
        use tokio::sync::Barrier;

        // Serialise against sibling tests that also call `BlobStore::write_blob`
        // (the Windows-cfg `write_blob_retries_*` tests). The counter is a
        // process-global static; cargo runs tests in parallel within a single
        // binary by default, so a sibling write_blob caller would inflate the
        // observed delta. The lock is the single sanctioned serializer.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        // Capture the call-count BASELINE before any work, then assert on the
        // DELTA at the end. Under the test lock, no sibling write_blob caller
        // can interpose.
        let baseline = crate::file_structure::WRITE_BLOB_CALL_COUNT.load(Ordering::SeqCst);

        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path().join("blobs"));
        let coordinator = Arc::new(super::PullCoordinator::new());

        let bytes = b"hello coordinator";
        let digest = Algorithm::Sha256.hash(bytes);
        let registry = "example.com";

        let barrier = Arc::new(Barrier::new(2));

        // Launch two concurrent calls for the same digest, gated by a barrier
        // so both reach stage_blob_bytes before either can complete.
        let store_a = store.clone();
        let store_b = store.clone();
        let coordinator_a = coordinator.clone();
        let coordinator_b = coordinator.clone();
        let digest_a = digest.clone();
        let digest_b = digest.clone();
        let bytes_a = bytes.to_vec();
        let bytes_b = bytes.to_vec();
        let barrier_a = barrier.clone();
        let barrier_b = barrier.clone();

        let (result_a, result_b) = tokio::join!(
            async move {
                barrier_a.wait().await;
                coordinator_a
                    .stage_blob_bytes(&store_a, registry, &digest_a, &bytes_a)
                    .await
            },
            async move {
                barrier_b.wait().await;
                coordinator_b
                    .stage_blob_bytes(&store_b, registry, &digest_b, &bytes_b)
                    .await
            },
        );

        assert!(result_a.is_ok(), "first concurrent writer must succeed: {result_a:?}");
        assert!(result_b.is_ok(), "second concurrent writer must succeed: {result_b:?}");

        // The target data file must exist after both calls.
        let data_path = store.data(registry, &digest);
        assert!(
            data_path.exists(),
            "blob data file must exist after concurrent writes: {data_path:?}"
        );
        let written = std::fs::read(&data_path).unwrap();
        assert_eq!(written, bytes, "blob data must equal the written bytes");

        // Singleflight coalescing: exactly ONE invocation of BlobStore::write_blob
        // across both concurrent callers. A delta > 1 means dedup failed and
        // duplicate work was performed.
        let delta = crate::file_structure::WRITE_BLOB_CALL_COUNT
            .load(Ordering::SeqCst)
            .saturating_sub(baseline);
        assert_eq!(
            delta, 1,
            "PullCoordinator must coalesce: expected exactly 1 NEW call to BlobStore::write_blob across both \
             concurrent same-digest writers; got delta={delta}. A regression that removes singleflight or calls \
             write_blob directly would surface here as delta >= 2."
        );
    }

    // ── pull_coordinator_returns_singleflight_error_on_leader_failure ────────
    //
    // When the leader call to BlobStore::write_blob fails, PullCoordinator must
    // propagate the error to the caller.
    //
    // We simulate the failure by pointing the BlobStore at a path that cannot be
    // created (a file path used as a directory).
    #[tokio::test(flavor = "multi_thread")]
    async fn pull_coordinator_returns_singleflight_error_on_leader_failure() {
        use crate::{file_structure::BlobStore, oci::Algorithm};

        let dir = tempfile::tempdir().unwrap();
        // Create a file where the blobs directory would be — this blocks
        // directory creation and forces write_blob to fail.
        let blobs_path = dir.path().join("blobs");
        std::fs::write(&blobs_path, b"not a directory").unwrap();

        let store = BlobStore::new(blobs_path);
        let coordinator = super::PullCoordinator::new();

        let bytes = b"this write must fail";
        let digest = Algorithm::Sha256.hash(bytes);
        let registry = "example.com";

        let result = coordinator.stage_blob_bytes(&store, registry, &digest, bytes).await;

        assert!(
            result.is_err(),
            "stage_blob_bytes must propagate leader failure; got Ok"
        );
    }
}
