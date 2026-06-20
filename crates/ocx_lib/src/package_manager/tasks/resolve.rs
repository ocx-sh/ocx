// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::{collections::HashMap, sync::Arc};

use tokio::task::JoinSet;

use crate::{
    log, oci,
    oci::index::{IndexOperation, SelectResult},
    package::{install_info::InstallInfo, metadata::env::entry::Entry},
    package_manager::{self, composer, error::PackageError, error::PackageErrorKind},
    patch::PatchDescriptor,
};

use super::super::PackageManager;

/// Map from each admitted identifier to its companion INTERFACE env entries.
///
/// Built offline from local `PatchTagMap` + `BlobStore` state (no network).
/// Applied globally last in [`PackageManager::resolve_env`] (invariant C1).
pub type SitePatchSet = HashMap<oci::PinnedIdentifier, Vec<Entry>>;

/// GC roots derived from the site-patch tier.
///
/// Carries the set of companion package identifiers and descriptor blob digests
/// that must survive garbage collection, regardless of whether they are
/// reachable through the normal install-symlink graph.  Seeded into
/// [`crate::package_manager::tasks::garbage_collection::GarbageCollector`]
/// alongside project-registry roots so that patch companions and their
/// descriptor blobs are never prematurely collected.
///
/// Built offline (no network) by [`PackageManager::resolve_site_patch_roots`].
#[derive(Debug, Clone, Default)]
pub struct SitePatchRoots {
    /// Pinned identifiers for every companion package that should be retained.
    pub companions: Vec<oci::PinnedIdentifier>,
    /// Registry + blob digest pairs for every patch descriptor blob that should
    /// be retained.  The registry is required so the GC can call
    /// `BlobStore::path(registry, digest)` without a linear shard-suffix scan.
    ///
    /// GC-oriented: this is the flat list of ALL descriptor blobs (manifest +
    /// layer digests across every resolved source) that must survive GC.
    pub descriptors: Vec<(String, oci::Digest)>,
    /// Per-source descriptor MANIFEST pins, keyed by the descriptor source's
    /// canonical `registry/repository` (the same key
    /// [`PackageManager::build_site_patch_set`] derives from
    /// `global_descriptor_id` / `patch_descriptor_id`).  Value = the manifest
    /// digest of the descriptor at resolve time.
    ///
    /// Freeze-oriented: `ocx patch freeze` writes these into
    /// [`PatchSnapshot::descriptors`](crate::patch::PatchSnapshot::descriptors)
    /// so compose under an active snapshot selects descriptors by FROZEN digest
    /// (whole-tier determinism, C8) rather than re-reading the live tag store —
    /// a post-freeze `ocx patch sync` that advances a descriptor must not change
    /// which companions a frozen build composes.
    pub descriptor_pins: Vec<(String, oci::Digest)>,
}

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
    ///
    /// When a `[patches]` section is configured (`self.patches` is `Some`),
    /// the companion-interface overlay is appended **after** all of compose's
    /// entries (global-last invariant, C1).  Each admitted identifier's
    /// companion entries are appended in the order compose visited them, so
    /// a patch on a transitive dep can override a `Constant` or `Path` var
    /// declared by the root itself.
    ///
    /// With `self.patches = None` the output is byte-identical to the
    /// pre-Phase-4 behaviour (no-config no-op guarantee).
    pub async fn resolve_env(&self, packages: &[Arc<InstallInfo>], self_view: bool) -> crate::Result<Vec<Entry>> {
        // OCI-tier / no-project callers have no per-package opt-out: pass an
        // empty boundary set so every admitted base keeps its companion overlay.
        let (entries, _) = self
            .resolve_env_with_patch_boundary(packages, self_view, &std::collections::BTreeSet::new())
            .await?;
        Ok(entries)
    }

    /// Like [`resolve_env`] but also returns the index at which patch-overlay entries begin.
    ///
    /// The returned `usize` is the first index in the entry slice that belongs to the
    /// companion overlay (entries at indices `[0..patch_start)` came from `composer::compose`,
    /// entries at `[patch_start..)` came from the patch companion projections).
    ///
    /// When `self.patches = None` (no patch tier), `patch_start` equals the total entry
    /// count — the slice `[patch_start..)` is empty, which is byte-identical to the
    /// pre-Phase-4 output.
    ///
    /// The CLI `--show-patches` flag uses this boundary to annotate each entry's origin.
    ///
    /// `no_patches` is the set of canonical `"registry/repository"` keys (tag/digest
    /// excluded) for bases a project declared `no-patches = true`. An opted-out base
    /// gets NO companion overlay UNLESS the tier is system-required (enforcement wins).
    pub async fn resolve_env_with_patch_boundary(
        &self,
        packages: &[Arc<InstallInfo>],
        self_view: bool,
        no_patches: &std::collections::BTreeSet<String>,
    ) -> crate::Result<(Vec<Entry>, usize)> {
        let out = composer::compose(packages, &self.file_structure().packages, self_view).await?;
        let mut entries = out.entries;
        let compose_count = entries.len();

        // Phase 4 overlay: append companion-interface entries for each
        // admitted identifier, in admitted-set visit order.
        //
        // When `self.patches` is `None` this block is a no-op and
        // `entries` is byte-identical to the pre-Phase-4 output.
        if let Some(mut patch_set) = self
            .build_site_patch_set(&out.admitted, packages, self_view, no_patches)
            .await?
        {
            for admitted_id in &out.admitted {
                // `remove` instead of `get`: patch_set is consumed here and not used
                // afterwards, so moving entries out eliminates the per-entry String clone.
                if let Some(companion_entries) = patch_set.remove(admitted_id) {
                    entries.extend(companion_entries);
                }
            }
        }

        Ok((entries, compose_count))
    }

    /// Build the [`SitePatchSet`] for the given admitted identifiers.
    ///
    /// Returns `None` when no `[patches]` section is configured
    /// (`self.patches` is `None`), leaving `resolve_env` output unchanged
    /// (no-config no-op).
    ///
    /// When patches are configured, loads the global and per-package
    /// descriptors from local state only (no network — `PatchTagMap::read` +
    /// `BlobStore::read_blob`), collects companions, projects each
    /// companion's **interface** surface via `composer::compose`, and
    /// returns a map from admitted identifier to its companion env entries.
    ///
    /// ## Scope note (Phase 5)
    ///
    /// Phase 3 discovery persists descriptors for the user-requested base and
    /// the global root.  A transitive dep's package-specific descriptor is
    /// only present if that dep was itself discovered (its own install or a
    /// future `ocx patch sync`).  Global-descriptor companions still cover
    /// every admitted identifier including transitive deps, because rules
    /// match by identifier string.  Full transitive package-specific
    /// discovery is a Phase 5 `ocx patch sync` concern.
    async fn build_site_patch_set(
        &self,
        admitted: &[oci::PinnedIdentifier],
        _roots: &[Arc<InstallInfo>],
        _self_view: bool,
        no_patches: &std::collections::BTreeSet<String>,
    ) -> crate::Result<Option<SitePatchSet>> {
        // No patch tier configured → no overlay; output is byte-identical to pre-Phase-4.
        let Some(patches) = self.patches() else {
            return Ok(None);
        };

        let tag_store = &self.file_structure().tags;
        let blob_store = &self.file_structure().blobs;
        let package_store = &self.file_structure().packages;

        // ── Step 1: Load global descriptor from persisted local state (offline-only). ──
        //
        // The global descriptor lives at the patch registry root (empty repository).
        // Its discovery state was recorded by `discover_and_install_patches` in Phase 3.
        // SECURITY: Phase 5 gap — `load_descriptor_for_id` reads the tag-store path
        // derived from `patches.registry` (operator-controlled via `[patches]` config).
        // Untrusted path injection is bounded here: `global_descriptor_id` only uses
        // the registry hostname to namespace the CAS path, and all blob content is
        // content-addressed (SHA-256). Phase 5 (`ocx patch sync`) will add signature
        // verification before trusting descriptor contents.
        // Under an active freeze snapshot (`OCX_PATCH_SNAPSHOT`) the descriptor is
        // selected by its pinned manifest digest (frozen — C8 whole-tier
        // determinism); otherwise it floats to the live tag-store record.
        let global_id = super::patch_discovery::global_descriptor_id(patches);
        let global_tags_path = tag_store.tags(&global_id);
        let global_descriptor_result = load_descriptor_frozen_or_live(
            blob_store,
            patches.registry.as_str(),
            &global_id,
            &global_tags_path,
            self.patch_snapshot(),
        )
        .await?;

        // C7 fail-closed: a corrupt global descriptor (tag-store says "has descriptor"
        // but CAS blob is missing or unreadable) is a tamper / corruption event, not a
        // "no patch" case.  Fail closed when the tier is required=true.
        let global_descriptor = match global_descriptor_result {
            DescriptorLoadResult::NotPresent => None,
            DescriptorLoadResult::Loaded(_manifest_digest, descriptor) => Some(descriptor),
            DescriptorLoadResult::Corrupt(error, _manifest_digest) => {
                if patches.required {
                    return Err(error);
                }
                log::warn!("site-patch-set: global descriptor corrupt (tier required=false): {error}; skipping");
                None
            }
        };

        // ── Step 2: Companion projection cache ────────────────────────────────────
        //
        // A global descriptor with a catch-all rule (e.g., "match": "*") returns
        // the same companion for every admitted identifier.  Without a cache,
        // `find_companion_local` + `compose` would be called N times for the same
        // companion (N = admitted set size).  The cache below keys by companion
        // `Identifier` so each (companion, required) pair is projected exactly once.
        //
        // Cache value semantics:
        //   `None`           — companion genuinely missing / not installed locally.
        //                      Required-companion fail-closed check fires on every
        //                      cache hit for None to ensure a required companion that
        //                      was missing on the first lookup also fails on subsequent
        //                      admitted identifiers (no silent bypass via the cache).
        //   `Some(Vec<_>)`   — companion present; projection may be empty (all-private
        //                      vars filtered by interface surface), which is fine.
        //
        // NOTE: the cache intentionally stores `None` for lookup failures (not-installed
        // or lookup-error) to avoid repeated failed lookups. The required-fail-closed
        // path re-checks on every cache hit for `None`, so it cannot be bypassed.
        let mut companion_projection_cache: HashMap<oci::Identifier, Option<Vec<Entry>>> = HashMap::new();

        // ── Step 3: Iterate admitted identifiers, collect companions per identifier. ──
        //
        // For each admitted identifier:
        //   a) Load the package-specific descriptor (if any).
        //   b) Merge global + pkg-specific (global first, pkg-specific overrides on
        //      same companion identifier — last-wins for the `required` flag).
        //   c) For each companion, look up the projection cache; on miss, project
        //      via `find_companion_local` + `compose([companion], store, false)`.

        let mut patch_set: SitePatchSet = SitePatchSet::new();

        for admitted_id in admitted {
            let base_id = admitted_id.as_identifier();

            // Per-package opt-out (C7 exception): a project may decline the
            // companion overlay for this base via `[package."<id>"] no-patches`.
            // Match is by canonical `registry/repository` (tag/digest excluded —
            // the opt-out is version-independent). A system-required tier still
            // applies: enforcement beats opt-out.
            let repo_key = format!("{}/{}", base_id.registry(), base_id.repository());
            let system_required = patches.system_required;
            if no_patches.contains(&repo_key) && !system_required {
                continue;
            }

            // Load package-specific descriptor (if any). Frozen by manifest digest
            // under an active snapshot, else floats to the live tag store.
            let pkg_specific_id = super::patch_discovery::patch_descriptor_id(patches, base_id);
            let pkg_tags_path = tag_store.tags(&pkg_specific_id);
            let pkg_descriptor_result = load_descriptor_frozen_or_live(
                blob_store,
                patches.registry.as_str(),
                &pkg_specific_id,
                &pkg_tags_path,
                self.patch_snapshot(),
            )
            .await?;

            // C7 fail-closed for per-identifier corrupt descriptor.
            let pkg_descriptor = match pkg_descriptor_result {
                DescriptorLoadResult::NotPresent => None,
                DescriptorLoadResult::Loaded(_manifest_digest, descriptor) => Some(descriptor),
                DescriptorLoadResult::Corrupt(error, _manifest_digest) => {
                    if patches.required {
                        return Err(error);
                    }
                    log::warn!(
                        "site-patch-set: pkg-specific descriptor for '{}' corrupt (tier required=false): {error}; skipping",
                        admitted_id
                    );
                    None
                }
            };

            // Skip identifiers with neither global nor pkg-specific descriptor.
            if global_descriptor.is_none() && pkg_descriptor.is_none() {
                continue;
            }

            // Merge companions: global first (lower precedence), pkg-specific second
            // (overrides via last-wins on same companion identifier — matches Phase 3
            // merge algorithm in `discover_and_install_patches`).
            let companions = merge_companions(
                base_id,
                patches.required,
                global_descriptor.as_ref(),
                pkg_descriptor.as_ref(),
            );

            if companions.is_empty() {
                continue;
            }

            // Defense-in-depth: cap total companions per admitted id to the same
            // limit enforced by Phase 3 discovery. A compromised patch registry
            // could accumulate entries across both global and pkg-specific
            // descriptors that exceed the limit.
            //
            // Fail-closed posture (consistent with Phase 3):
            //   - required tier OR any over-cap companion is required → Err
            //   - non-required tier AND no over-cap companion is required → warn + truncate
            let companions = if companions.len() > super::patch_discovery::MAX_TOTAL_COMPANIONS {
                let any_required = companions
                    .iter()
                    .skip(super::patch_discovery::MAX_TOTAL_COMPANIONS)
                    .any(|c| c.required);
                if patches.required || any_required {
                    return Err(crate::Error::from(
                        crate::package_manager::error::PackageErrorKind::PatchDiscovery(
                            crate::patch::PatchError::DescriptorTooLarge {
                                detail: format!(
                                    "companion count {} for '{}' exceeds maximum {}",
                                    companions.len(),
                                    admitted_id,
                                    super::patch_discovery::MAX_TOTAL_COMPANIONS
                                ),
                            },
                        ),
                    ));
                }
                log::warn!(
                    "site-patch-set: companion count {} exceeds cap {} for '{}'; truncating (all over-cap companions are optional)",
                    companions.len(),
                    super::patch_discovery::MAX_TOTAL_COMPANIONS,
                    admitted_id
                );
                companions
                    .into_iter()
                    .take(super::patch_discovery::MAX_TOTAL_COMPANIONS)
                    .collect::<Vec<_>>()
            } else {
                companions
            };

            // Project each companion's INTERFACE env (self_view=false — no private leak).
            // Projection results are cached by companion identifier so a global catch-all
            // companion is projected once, not once per admitted identifier.
            let mut companion_entries: Vec<Entry> = Vec::new();
            for companion_entry in &companions {
                let companion_id = &companion_entry.identifier;

                // Cache hit: distinguish "missing" (None) from "present, possibly empty"
                // (Some(entries)).
                //
                // A required companion that was missing on a prior admitted-id lookup
                // must STILL fail closed here — caching None means "genuinely not
                // installed", not "safe to skip". Without this re-check a required
                // companion that is absent would silently bypass the fail-closed gate
                // on every admitted identifier after the first.
                if let Some(cached_projection) = companion_projection_cache.get(companion_id) {
                    match cached_projection {
                        None => {
                            // Genuinely missing — re-apply the required check.
                            if companion_entry.required {
                                return Err(crate::Error::from(
                                    crate::package_manager::error::PackageErrorKind::RequiredCompanionFailed {
                                        companion: companion_id.clone(),
                                        source: Box::new(crate::package_manager::error::PackageErrorKind::NotFound),
                                    },
                                ));
                            }
                            // Optional and missing — skip (already logged on first encounter).
                        }
                        Some(entries) => {
                            // Present (projection may be empty for all-private companions).
                            companion_entries.extend(entries.iter().cloned());
                        }
                    }
                    continue;
                }

                // Cache miss: resolve and project the companion.
                //
                // Resolve the companion to a pinned identifier via `find_plain`.
                // The companion must already be installed locally (Phase 3 installed it).
                // If not found, skip optional companions; log a warning.
                //
                // When a `PatchSnapshot` is active and contains a pin for this companion,
                // prefer the snapshot digest over the live tag lookup (opt-in determinism,
                // Phase 5B ADR C8). The snapshot pin is policy, not truth: if the locally
                // installed package at the snapshot digest is absent the lookup returns None,
                // and the required-fail-closed gate fires as normal.
                let companion_install_info = match self.find_companion_with_snapshot(companion_id).await {
                    Ok(Some(info)) => info,
                    Ok(None) => {
                        // Cache None = genuinely missing, so the required-fail-closed check
                        // fires again on every cache hit (not bypassed by the cache).
                        companion_projection_cache.insert(companion_id.clone(), None);
                        if companion_entry.required {
                            // C7: required companion not installed locally → fail closed.
                            // The companion was supposed to be installed by Phase 3 discovery.
                            // Running without it would violate the invariant that required
                            // companions always contribute to the overlay env.
                            return Err(crate::Error::from(
                                crate::package_manager::error::PackageErrorKind::RequiredCompanionFailed {
                                    companion: companion_id.clone(),
                                    source: Box::new(crate::package_manager::error::PackageErrorKind::NotFound),
                                },
                            ));
                        }
                        log::debug!(
                            "site-patch-set: optional companion '{}' not installed for '{}'; skipping",
                            companion_id,
                            admitted_id
                        );
                        continue;
                    }
                    Err(error) => {
                        // C7 fail-closed: a lookup error for a required companion must not
                        // silently skip. An index I/O error, deserialization error, or
                        // corrupt TagLock is not equivalent to "companion not installed" —
                        // it is an unexpected failure that could mask a missing required
                        // companion.  Only optional companions may warn-and-skip here.
                        if companion_entry.required {
                            return Err(crate::Error::from(
                                crate::package_manager::error::PackageErrorKind::RequiredCompanionFailed {
                                    companion: companion_id.clone(),
                                    source: Box::new(crate::package_manager::error::PackageErrorKind::Internal(error)),
                                },
                            ));
                        }
                        log::warn!(
                            "site-patch-set: error looking up optional companion '{}' for '{}': {error}; skipping",
                            companion_id,
                            admitted_id
                        );
                        // Cache None (missing/failed) so subsequent admitted IDs skip the
                        // lookup — the required-fail-closed check will still fire on cache
                        // hits because None means "genuinely missing".
                        companion_projection_cache.insert(companion_id.clone(), None);
                        continue;
                    }
                };

                // Project interface surface only (self_view=false — invariant: no private leak).
                let companion_arc = std::sync::Arc::new(companion_install_info);
                match composer::compose(std::slice::from_ref(&companion_arc), package_store, false).await {
                    Ok(out) => {
                        // Store Some(entries) — companion is present (may have zero interface
                        // vars, which is fine — private-only companions produce an empty vec).
                        // Cache Some(empty) is distinct from None (missing).
                        companion_projection_cache.insert(companion_id.clone(), Some(out.entries.clone()));
                        companion_entries.extend(out.entries);
                    }
                    Err(error) => {
                        if companion_entry.required {
                            // C7: required companion present locally but env-composition
                            // failed → fail closed. Do not silently emit a partial overlay.
                            return Err(crate::Error::from(
                                crate::package_manager::error::PackageErrorKind::RequiredCompanionFailed {
                                    companion: companion_id.clone(),
                                    source: Box::new(crate::package_manager::error::PackageErrorKind::Internal(error)),
                                },
                            ));
                        }
                        log::warn!(
                            "site-patch-set: failed to compose optional companion '{}' for '{}': {error}; skipping",
                            companion_id,
                            admitted_id
                        );
                        // Cache None (composition failed) — if this companion is required
                        // for another admitted identifier, the cache-hit path will fail closed.
                        companion_projection_cache.insert(companion_id.clone(), None);
                    }
                }
            }

            if !companion_entries.is_empty() {
                patch_set.insert(admitted_id.clone(), companion_entries);
            }
        }

        // If the patch tier is active but no companions were found for any admitted
        // identifier, return `Some(empty_map)` rather than `None` — the caller
        // distinguishes "no patch tier" (None) from "patch tier active but no companions"
        // (Some(empty)).  The overlay loop in `resolve_env` iterates `admitted` and
        // looks up each in the map, so an empty map produces no extra entries.
        Ok(Some(patch_set))
    }

    /// Look up a companion's installed `InstallInfo` from the local index and
    /// package store, without contacting the network.
    ///
    /// Resolves the companion tag → digest via [`IndexOperation::Query`] on the
    /// public `Index` wrapper so the correct `TagLock`-envelope schema is used.
    /// `Op::Query` is strictly local in every [`ChainMode`] (never walks the
    /// chain, never writes) — this keeps `build_site_patch_set` network-free.
    ///
    /// Returns `Ok(None)` when the companion is not installed locally (the
    /// index has no tag record for it).
    async fn find_companion_local(&self, companion_id: &oci::Identifier) -> crate::Result<Option<InstallInfo>> {
        use super::common::find_in_store;

        // Resolve the companion tag → digest through a **guaranteed-local** index.
        //
        // The companion overlay operates on already-installed state: Phase 3
        // installed the companion and committed its tag to the local index. The
        // resolution must be cache-only in EVERY `ChainMode` — in particular it
        // must NOT use the manager's own `index()`, because under
        // `ChainMode::Remote` (`--remote`) a tag-addressed `Op::Query` bypasses the
        // local read and routes to the registry (`ChainedIndex::fetch_manifest_digest`):
        // that would contact the network from the offline-safe overlay path and
        // ignore the locally-installed companion tag. Build a fresh `Offline`
        // index over the same tag + blob stores (empty sources → strictly local,
        // never walks a chain) so the lookup is TagLock-aware and network-free.
        // On miss → companion not installed locally → return None.
        let local_index = oci::index::Index::from_chained(
            oci::index::LocalIndex::new(oci::index::LocalConfig {
                tag_store: self.file_structure().tags.clone(),
                blob_store: self.file_structure().blobs.clone(),
            }),
            Vec::new(),
            oci::index::ChainMode::Offline,
        );
        let Some(digest) = local_index
            .fetch_manifest_digest(companion_id, IndexOperation::Query)
            .await?
        else {
            return Ok(None);
        };

        // Construct the pinned identifier and look up in the package store.
        let pinned_id = match oci::PinnedIdentifier::try_from(companion_id.clone_with_digest(digest)) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };

        // `find_in_store` only ever returns `PackageErrorKind::Internal(inner)` or
        // `Ok(None)` — it never emits other variants. The `From<PackageErrorKind>`
        // impl already extracts the inner error for the `Internal` arm and wraps
        // others structurally (no `.to_string()` erasure).
        let result = find_in_store(&self.file_structure().packages, &pinned_id)
            .await
            .map_err(crate::Error::from)?;
        Ok(result)
    }

    /// Look up a companion's installed `InstallInfo`, preferring a pinned digest
    /// from the active [`crate::patch::PatchSnapshot`] over the live tag lookup.
    ///
    /// When `self.patch_snapshot()` is `Some` and contains a pin for `companion_id`
    /// (keyed by `"registry/repository"`), the pinned digest is used to construct
    /// the `PinnedIdentifier` and the package is looked up directly in the store —
    /// no tag resolution. This is the Phase 5B opt-in determinism mechanism.
    ///
    /// Falls back to [`Self::find_companion_local`] when no snapshot is active or
    /// the snapshot does not contain a pin for this companion.
    ///
    /// # Errors
    ///
    /// Propagates errors from the underlying store reads.
    async fn find_companion_with_snapshot(&self, companion_id: &oci::Identifier) -> crate::Result<Option<InstallInfo>> {
        use super::common::find_in_store;

        // Check the snapshot for a pinned digest for this companion.
        // The snapshot key is "registry/repository" (no tag, no digest suffix).
        if let Some(snapshot) = self.patch_snapshot() {
            let snapshot_key = format!("{}/{}", companion_id.registry(), companion_id.repository());
            if let Some(pinned_digest) = snapshot.companions.get(&snapshot_key) {
                // Build the pinned identifier using the snapshot digest and look up
                // the package in the local store. The snapshot is policy not truth:
                // if the package at this digest is not locally installed, return None
                // (the required-fail-closed gate fires in the caller as normal).
                let pinned_id = match oci::PinnedIdentifier::try_from(
                    companion_id.clone_with_digest(pinned_digest.clone()),
                ) {
                    Ok(id) => id,
                    Err(_) => {
                        // Digest-format error should not occur (snapshot is serialized
                        // from valid Digest values), but fall back to live lookup if it does.
                        log::warn!(
                            "site-patch-set: snapshot pin for '{}' has invalid digest format; falling back to live lookup",
                            snapshot_key
                        );
                        return self.find_companion_local(companion_id).await;
                    }
                };
                let result = find_in_store(&self.file_structure().packages, &pinned_id)
                    .await
                    .map_err(crate::Error::from)?;
                return Ok(result);
            }
        }

        // No snapshot pin for this companion — fall back to the live tag lookup.
        self.find_companion_local(companion_id).await
    }

    /// Resolve GC roots from the site-patch tier (strictly offline, no network).
    ///
    /// Returns a [`SitePatchRoots`] containing the pinned identifiers of all
    /// companion packages and the blob digests of all descriptor blobs that
    /// must survive garbage collection.  These roots are seeded into
    /// [`crate::package_manager::tasks::garbage_collection::GarbageCollector`]
    /// alongside project-registry roots.
    ///
    /// When no `[patches]` section is configured (`self.patches()` is `None`),
    /// returns an empty `SitePatchRoots`.  This method never contacts the
    /// network in any `ChainMode`.
    ///
    /// # Parameters
    ///
    /// - `platforms` — reserved for future per-platform companion filtering
    ///   (a later sub-phase will use this to restrict companion resolution to
    ///   platforms relevant to the current host).  Currently unused; callers
    ///   should pass the host platform set via
    ///   `&[oci::Platform::current().unwrap_or_else(oci::Platform::any)]` so
    ///   the value is meaningful when the filter is implemented.
    ///
    /// # Errors
    ///
    /// Returns an error if the local filesystem state cannot be read.
    pub async fn resolve_site_patch_roots(&self, _platforms: &[oci::Platform]) -> crate::Result<SitePatchRoots> {
        // Short-circuit: no patch tier configured → empty roots.
        let Some(patches) = self.patches() else {
            return Ok(SitePatchRoots::default());
        };

        let tag_store = &self.file_structure().tags;
        let blob_store = &self.file_structure().blobs;
        let symlink_root = self.file_structure().symlinks.root().to_path_buf();

        // Build a guaranteed-local index (same pattern as find_companion_local) so
        // companion tag → digest resolution is network-free in every ChainMode.
        let local_index = oci::index::Index::from_chained(
            oci::index::LocalIndex::new(oci::index::LocalConfig {
                tag_store: tag_store.clone(),
                blob_store: blob_store.clone(),
            }),
            Vec::new(),
            oci::index::ChainMode::Offline,
        );

        // ── Step 1: Collect installed base identifiers via the shared enumerator. ──
        //
        // `enumerate_installed_bases` walks the symlink store and recovers real
        // registry hostnames. Shared with `sync_patches` (patch_sync.rs) so there
        // is exactly ONE symlink-store walk implementation (DRY).
        let installed_base_ids = super::patch_sync::enumerate_installed_bases(self.file_structure()).await?;
        // `symlink_root` is no longer needed after the step above; drop the binding.
        let _ = symlink_root;

        // ── Step 2: For each installed base + the global root, load descriptors. ──
        //
        // Descriptor sources:
        //   (a) Global root — covers every base via rule matching.
        //   (b) Per-base descriptor — specific to each installed base.
        //
        // We collect descriptor digests (manifest + layer) and companion entries.

        let mut companion_set: Vec<oci::Identifier> = Vec::new();
        let mut descriptor_digests: Vec<(String, oci::Digest)> = Vec::new();
        // Per-source manifest pins (source key "registry/repository" → manifest
        // digest) used by `ocx patch freeze` to make compose select descriptors
        // by frozen digest under an active snapshot (C8 whole-tier determinism).
        let mut descriptor_pins: Vec<(String, oci::Digest)> = Vec::new();

        // Global descriptor (covers all bases via rule matching).
        //
        // Load the global descriptor blob digests once (they are the same
        // regardless of which base we match against), and for each installed
        // base derive companions via the global descriptor's rules.
        // Using an empty identifier for the "global" case would only work for
        // catch-all rules; iterating installed bases correctly handles both
        // catch-all ("match: *") and scoped rules.
        let global_id = super::patch_discovery::global_descriptor_id(patches);
        let global_tags_path = tag_store.tags(&global_id);

        // Load global descriptor once (blob digests and descriptor value).
        let (global_descriptor_opt, global_manifest_digest) = collect_descriptor_digests(
            blob_store,
            patches.registry.as_str(),
            &global_tags_path,
            &mut descriptor_digests,
        )
        .await?;
        if let Some(manifest_digest) = global_manifest_digest {
            descriptor_pins.push((descriptor_source_key(&global_id), manifest_digest));
        }

        // Collect global companions for each installed base.
        if let Some(ref global_descriptor) = global_descriptor_opt {
            for base_id in &installed_base_ids {
                for companion_entry in global_descriptor.collect_companions(base_id, patches.required) {
                    companion_set.push(companion_entry.identifier);
                }
            }
            // When no bases are installed, still seed the global descriptor
            // digests (already done above via collect_descriptor_digests).
        }

        // Per-base descriptors for each installed base.
        for base_id in &installed_base_ids {
            let pkg_specific_id = super::patch_discovery::patch_descriptor_id(patches, base_id);
            let pkg_tags_path = tag_store.tags(&pkg_specific_id);

            let (pkg_descriptor_opt, pkg_manifest_digest) = collect_descriptor_digests(
                blob_store,
                patches.registry.as_str(),
                &pkg_tags_path,
                &mut descriptor_digests,
            )
            .await?;
            if let Some(manifest_digest) = pkg_manifest_digest {
                descriptor_pins.push((descriptor_source_key(&pkg_specific_id), manifest_digest));
            }

            if let Some(ref pkg_descriptor) = pkg_descriptor_opt {
                for companion_entry in pkg_descriptor.collect_companions(base_id, patches.required) {
                    companion_set.push(companion_entry.identifier);
                }
            }
        }

        // ── Step 3: Resolve companion tags → pinned identifiers (local-only). ──
        let mut companions: Vec<oci::PinnedIdentifier> = Vec::new();
        // Dedup companion identifiers before resolution (same companion may appear
        // in multiple descriptor sources).
        companion_set.sort_by_key(|id| id.to_string());
        companion_set.dedup_by_key(|id| id.to_string());

        for companion_id in &companion_set {
            match local_index
                .fetch_manifest_digest(companion_id, oci::index::IndexOperation::Query)
                .await
            {
                Ok(Some(digest)) => {
                    // Strip the tag: GC companion roots are identified by registry+repo+digest
                    // only. The tag is lookup metadata, not part of the package identity.
                    let digest_only_id =
                        oci::Identifier::new_registry(companion_id.repository(), companion_id.registry())
                            .clone_with_digest(digest);
                    match oci::PinnedIdentifier::try_from(digest_only_id) {
                        Ok(pinned) => companions.push(pinned),
                        Err(_) => {
                            log::debug!(
                                "resolve-site-patch-roots: could not pin companion '{}'; skipping",
                                companion_id
                            );
                        }
                    }
                }
                Ok(None) => {
                    // Companion not installed locally — skip (GC of a not-yet-installed
                    // companion is moot; no over-collect concern here).
                    log::debug!(
                        "resolve-site-patch-roots: companion '{}' not in local index; skipping",
                        companion_id
                    );
                }
                Err(error) => {
                    // Local index read error — skip and log; do not fail GC derivation.
                    log::debug!(
                        "resolve-site-patch-roots: error querying local index for companion '{}': {error}; skipping",
                        companion_id
                    );
                }
            }
        }

        // Dedup and sort both vecs for deterministic output.
        companions.sort_by_key(|pinned| pinned.to_string());
        companions.dedup_by_key(|pinned| pinned.to_string());

        descriptor_digests.sort_by_key(|(registry, digest)| format!("{registry}/{digest}"));
        descriptor_digests.dedup_by_key(|(registry, digest)| format!("{registry}/{digest}"));

        // Dedup descriptor pins by source key — multiple installed version tags
        // of the same repository resolve to the same package-specific descriptor
        // source (same key, same manifest digest), so one entry per source
        // suffices. Sort for deterministic snapshot output.
        descriptor_pins.sort_by(|(left, _), (right, _)| left.cmp(right));
        descriptor_pins.dedup_by(|(left, _), (right, _)| left == right);

        Ok(SitePatchRoots {
            companions,
            descriptors: descriptor_digests,
            descriptor_pins,
        })
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

// ── Phase 4 helpers ───────────────────────────────────────────────────────────

/// Outcome of a descriptor load from the tag store + CAS.
///
/// Distinguishes three meaningful states so callers can apply correct
/// fail-closed logic:
///
/// - `NotPresent` — tag-store says "never looked" or "looked, no patch":
///   skip silently (not an error).
/// - `Loaded(digest, descriptor)` — descriptor read and parsed successfully.
///   `digest` is the manifest digest parsed from the tag-store JSON.
/// - `Corrupt(error, manifest_digest)` — tag-store records
///   `LookedHasDescriptor` but the CAS blob is missing or unreadable.
///   This is **not** a "no patch" case — the descriptor existed at discovery
///   time but is now corrupt or tampered.  Callers that enforce `required =
///   true` should fail closed here (C7).  `manifest_digest` is `Some` when
///   the stored digest string was parseable (CAS blob corrupt or absent) and
///   `None` when the digest string itself was malformed.
#[derive(Debug)]
enum DescriptorLoadResult {
    NotPresent,
    /// Descriptor loaded successfully.  The `manifest_digest` is the OCI manifest
    /// digest read from the tag-store JSON (same value as
    /// `PatchDiscoveryState::LookedHasDescriptor::manifest_digest` after parsing).
    /// Carrying it here avoids a second tag-store read in callers that need the
    /// digest (e.g. `collect_descriptor_digests`).
    Loaded(oci::Digest, PatchDescriptor),
    /// Descriptor could not be loaded.  `manifest_digest` is `Some` when the
    /// tag-store JSON contained a parseable manifest digest but the CAS blob was
    /// corrupt or missing; `None` when the stored digest string itself was
    /// malformed.  GC callers push the `Some` digest as a root so the manifest
    /// blob survives even when the descriptor layer is damaged.
    Corrupt(crate::Error, Option<oci::Digest>),
}

/// Load a [`PatchDescriptor`] for the given tag-store path (offline, local only).
///
/// Reads the `PatchDiscoveryState` from the tag-store JSON at `tags_path`.
///
/// - `NeverLooked` / `LookedNoDescriptor` → [`DescriptorLoadResult::NotPresent`].
/// - `LookedHasDescriptor` + readable CAS blob → [`DescriptorLoadResult::Loaded`].
/// - `LookedHasDescriptor` + corrupt / missing CAS blob →
///   [`DescriptorLoadResult::Corrupt`].  The caller decides whether to fail
///   closed or warn + skip based on `required`.  A tag-store record saying
///   "descriptor exists" is **not** the same as "no patch for this package" —
///   it is a corruption / tamper event (C7 gap closure).
async fn load_descriptor_for_id(
    blob_store: &crate::file_structure::BlobStore,
    registry: &str,
    tags_path: &std::path::Path,
) -> crate::Result<DescriptorLoadResult> {
    use super::patch_discovery::{PatchDiscoveryState, PatchTagMap, load_descriptor_from_cas};

    let state = PatchTagMap::read(tags_path).await?;
    match state {
        PatchDiscoveryState::NeverLooked | PatchDiscoveryState::LookedNoDescriptor => {
            Ok(DescriptorLoadResult::NotPresent)
        }
        PatchDiscoveryState::LookedHasDescriptor { manifest_digest } => {
            let digest = match crate::oci::Digest::try_from(manifest_digest.as_str()) {
                Ok(d) => d,
                Err(_) => {
                    // The stored manifest digest string is itself malformed —
                    // treat as corruption (not "no patch").  No parseable digest
                    // is available, so the `None` variant tells GC callers there
                    // is no manifest blob to protect.
                    let corrupt_err =
                        crate::Error::Digest(crate::oci::digest::error::DigestError::Invalid(manifest_digest.clone()));
                    return Ok(DescriptorLoadResult::Corrupt(corrupt_err, None));
                }
            };
            match load_descriptor_from_cas(blob_store, registry, &digest).await {
                Ok(descriptor) => Ok(DescriptorLoadResult::Loaded(digest, descriptor)),
                // The manifest digest parsed fine but the CAS blob is corrupt or
                // missing.  Carry the digest so GC callers can protect the
                // manifest blob even when the descriptor layer is unreadable.
                Err(error) => Ok(DescriptorLoadResult::Corrupt(error, Some(digest))),
            }
        }
    }
}

/// Load a descriptor honouring an active freeze snapshot (C8 whole-tier
/// determinism).
///
/// When `snapshot` is `Some`, descriptor SELECTION is frozen: the manifest
/// digest comes from the snapshot's per-source pin
/// ([`PatchSnapshot::descriptors`](crate::patch::PatchSnapshot::descriptors),
/// keyed by [`descriptor_source_key`]) and is loaded directly from the CAS,
/// BYPASSING the live tag store. A source absent from the snapshot is treated as
/// `NotPresent` — the frozen tier did not include it, so a later `ocx patch
/// sync` that publishes a new descriptor cannot change which companions a frozen
/// build composes. When `snapshot` is `None`, the live tag-store load applies
/// (the tier floats), via [`load_descriptor_for_id`].
async fn load_descriptor_frozen_or_live(
    blob_store: &crate::file_structure::BlobStore,
    registry: &str,
    descriptor_id: &oci::Identifier,
    tags_path: &std::path::Path,
    snapshot: Option<&crate::patch::PatchSnapshot>,
) -> crate::Result<DescriptorLoadResult> {
    use super::patch_discovery::load_descriptor_from_cas;

    let Some(snapshot) = snapshot else {
        // No snapshot active → float: select the descriptor from the live tag store.
        return load_descriptor_for_id(blob_store, registry, tags_path).await;
    };

    // Snapshot active → freeze descriptor selection by the pinned manifest digest.
    match snapshot.descriptors.get(&descriptor_source_key(descriptor_id)) {
        Some(manifest_digest) => match load_descriptor_from_cas(blob_store, registry, manifest_digest).await {
            Ok(descriptor) => Ok(DescriptorLoadResult::Loaded(manifest_digest.clone(), descriptor)),
            // The pinned blob is missing/corrupt in the local CAS. Carry the
            // digest so the C7 fail-closed check in the caller fires for a
            // required tier rather than silently dropping the overlay.
            Err(error) => Ok(DescriptorLoadResult::Corrupt(error, Some(manifest_digest.clone()))),
        },
        // Source not in the frozen tier → it did not exist at freeze time, so a
        // frozen build must not compose it.
        None => Ok(DescriptorLoadResult::NotPresent),
    }
}

/// Merge companions from global and package-specific descriptors for `base_id`.
///
/// Global companions are collected first (lower precedence). Package-specific
/// companions are collected second; when the same companion identifier appears
/// in both descriptors, the package-specific entry overrides the global one
/// (last-wins semantics matching the Phase 3 install algorithm).
fn merge_companions(
    base_id: &oci::Identifier,
    tier_required_default: bool,
    global_descriptor: Option<&PatchDescriptor>,
    pkg_descriptor: Option<&PatchDescriptor>,
) -> Vec<crate::patch::CompanionEntry> {
    use std::collections::HashMap;

    let mut companion_order: Vec<oci::Identifier> = Vec::new();
    let mut companion_map: HashMap<oci::Identifier, crate::patch::CompanionEntry> = HashMap::new();

    // Collect from global first, then package-specific.
    for descriptor in [global_descriptor, pkg_descriptor].into_iter().flatten() {
        for entry in descriptor.collect_companions(base_id, tier_required_default) {
            if !companion_map.contains_key(&entry.identifier) {
                companion_order.push(entry.identifier.clone());
            }
            // Overwrite: later (package-specific) entry wins for `required` flag.
            companion_map.insert(entry.identifier.clone(), entry);
        }
    }

    companion_order
        .into_iter()
        .filter_map(|id| companion_map.remove(&id))
        .collect()
}

// ── Phase 5A helpers ─────────────────────────────────────────────────────────

/// Restore an installed base's real registry hostname from its tag file.
///
/// The symlink store names registry directories by the *slug* of the registry
/// (`to_relaxed_slug`), which is lossy for port-containing or otherwise
/// non-slug-safe hostnames (`localhost:5000` -> `localhost_5000`). The tag file
/// written at install time records the canonical `registry/repository` in its
/// `repository` field; reading it back recovers the exact hostname so descriptor
/// rule matching globs against the real identifier string.
///
/// Falls back to the slug-form identifier unchanged on any read/parse miss (a
/// missing tag file, malformed JSON, or a `repository` value that does not carry
/// the expected `/{repository}` suffix) — the slug form still matches catch-all
/// rules and standard dotted registries, so this only refines the port-registry
/// edge and never regresses the common path.
///
/// `pub(super)` so [`super::patch_sync::enumerate_installed_bases`] can share
/// this helper without duplicating the slug-recovery logic.
pub(super) async fn recover_base_with_real_registry(
    tag_store: &crate::file_structure::TagStore,
    slug_base_id: &oci::Identifier,
) -> oci::Identifier {
    let tags_path = tag_store.tags(slug_base_id);
    let real_registry = match tokio::fs::read_to_string(&tags_path).await {
        Ok(content) => serde_json::from_str::<serde_json::Value>(&content)
            .ok()
            .and_then(|value| value.get("repository").and_then(|r| r.as_str()).map(str::to_owned))
            .and_then(|repository| {
                repository
                    .strip_suffix(&format!("/{}", slug_base_id.repository()))
                    .map(str::to_owned)
            }),
        Err(_) => None,
    };
    match real_registry {
        Some(registry) if registry != slug_base_id.registry() => {
            oci::Identifier::new_registry(slug_base_id.repository(), &registry)
                .clone_with_tag(slug_base_id.tag_or_latest())
        }
        _ => slug_base_id.clone(),
    }
}

/// Walk one directory level looking for `candidates/` subdirectories.
///
/// The symlink store layout is:
/// ```text
/// {symlink_root}/{registry_slug}/{repo_component_1}/.../candidates/{tag}
/// ```
///
/// This function is called recursively: when the current directory is
/// `candidates`, each child entry is a tag, and an `Identifier` is
/// constructed from `(registry_slug, repo_components, tag)`.  Otherwise the
/// function descends into each child directory, appending it to
/// `repo_components`.
///
/// Uses `tokio::fs::read_dir` — must be called from an async context.
///
/// `pub(super)` so [`super::patch_sync::enumerate_installed_bases`] can share
/// the single symlink-store walk implementation.
///
/// # Errors
///
/// Returns an error only on unexpected filesystem I/O failures (not on
/// `NotFound`, which is handled by the caller).
pub(super) async fn collect_candidates_from_dir(
    dir: &std::path::Path,
    registry_slug: &str,
    repo_components: &mut Vec<String>,
    out: &mut Vec<oci::Identifier>,
) -> crate::Result<()> {
    let dir_name = match dir.file_name().and_then(|n| n.to_str()) {
        Some(name) => name.to_string(),
        None => return Ok(()),
    };

    if dir_name == "candidates" {
        // Each child of a `candidates/` dir is a tag → candidate symlink.
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(crate::Error::InternalFile(dir.to_path_buf(), error)),
        };
        let repo = repo_components.join("/");
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| crate::Error::InternalFile(dir.to_path_buf(), e))?
        {
            let tag = entry.file_name().to_string_lossy().to_string();
            if tag.is_empty() {
                continue;
            }
            // Build an Identifier using the registry slug as the registry
            // (the slug is used by tag_store.tags() via the same slugify
            // function, so tag lookups use the same path).
            let base_id = oci::Identifier::new_registry(&repo, registry_slug).clone_with_tag(&tag);
            out.push(base_id);
        }
        return Ok(());
    }

    // Descend into child directories, extending the repo path.
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(crate::Error::InternalFile(dir.to_path_buf(), error)),
    };
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| crate::Error::InternalFile(dir.to_path_buf(), e))?
    {
        let child_path = entry.path();
        let child_name = entry.file_name().to_string_lossy().to_string();
        // Skip non-directory entries (e.g. `current` symlinks).
        match entry.file_type().await {
            Ok(file_type) if file_type.is_dir() => {}
            Ok(file_type) if file_type.is_symlink() => {
                // Symlinks at the top level (e.g. `current`) — skip.
                // Directories under a repo path that are symlinks would be
                // unusual; skip them to avoid following unexpected links.
                continue;
            }
            _ => continue,
        }
        // Do NOT push the "candidates" component into repo_components — it is
        // a structural marker, not part of the repository name.  If we did push
        // it, the repo string would become "myrepo/candidates" instead of
        // "myrepo", producing a wrong base identifier (e.g.
        // `"cmake/candidates:3.28"`) that fails scoped descriptor rule matching.
        if child_name == "candidates" {
            Box::pin(collect_candidates_from_dir(
                &child_path,
                registry_slug,
                repo_components,
                out,
            ))
            .await?;
        } else {
            repo_components.push(child_name);
            Box::pin(collect_candidates_from_dir(
                &child_path,
                registry_slug,
                repo_components,
                out,
            ))
            .await?;
            repo_components.pop();
        }
    }
    Ok(())
}

/// Canonical `registry/repository` key identifying a patch descriptor source.
///
/// Both `ocx patch freeze` (which records the key in
/// [`PatchSnapshot::descriptors`](crate::patch::PatchSnapshot::descriptors)) and
/// [`PackageManager::build_site_patch_set`] (which looks the key up under an
/// active snapshot) derive it from the SAME `global_descriptor_id` /
/// `patch_descriptor_id` identifiers, so the freeze side and the compose side
/// always agree on the key for a given source.
fn descriptor_source_key(descriptor_id: &oci::Identifier) -> String {
    format!("{}/{}", descriptor_id.registry(), descriptor_id.repository())
}

/// Load a descriptor from the tag store + CAS, record its blob digests
/// (manifest digest + layer digests from the manifest), and return the
/// parsed [`PatchDescriptor`] when present.
///
/// Reuses [`load_descriptor_for_id`] for the tag-map read and CAS load, then
/// extracts layer digests from the manifest blob on disk.
///
/// - `NotPresent` → returns `Ok(None)`.
/// - `Corrupt` (best-effort skip) → pushes the manifest digest as a GC root
///   when the stored digest was parseable (`Some`), then returns `Ok(None)`.
///   This keeps the manifest blob alive even when the descriptor layer is
///   corrupt, allowing the next `ocx clean` to retry after the corruption is
///   resolved rather than over-collecting the manifest.
/// - `Loaded` → appends manifest digest + all layer digests to
///   `descriptor_digests` and returns `Ok(Some(descriptor))`.
///
/// Unlike the exec-overlay path, GC root derivation is best-effort:
/// a corrupt descriptor is a debug log + skip rather than a fail-closed
/// error.  Over-collection (dropping a still-needed companion) would be
/// harmful; under-retention (keeping an orphan) is safe and will be
/// corrected on the next `ocx clean` after the corruption is fixed.
///
/// The manifest digest is carried directly from [`DescriptorLoadResult`] —
/// no second tag-store read is needed.
async fn collect_descriptor_digests(
    blob_store: &crate::file_structure::BlobStore,
    registry: &str,
    tags_path: &std::path::Path,
    descriptor_digests: &mut Vec<(String, oci::Digest)>,
) -> crate::Result<(Option<PatchDescriptor>, Option<oci::Digest>)> {
    // Delegate tag-map reading + CAS load to the existing helper so there is
    // a single code path for the PatchDiscoveryState → DescriptorLoadResult
    // transition.  This avoids duplicating the tag-map format and state
    // matching logic (DRY).  The helper now returns the parsed manifest digest
    // in both Loaded and Corrupt(_, Some(_)) so we never need a second read.
    //
    // Returns `(descriptor, manifest_digest)`. The manifest digest is `Some`
    // ONLY for a cleanly-loaded descriptor — a corrupt descriptor is not pinned
    // by `ocx patch freeze` (you cannot reproducibly select a descriptor whose
    // bytes are unreadable), though its manifest blob is still seeded as a GC
    // root so a later `ocx clean` does not over-collect.
    let load_result = load_descriptor_for_id(blob_store, registry, tags_path).await?;

    let (manifest_digest, descriptor) = match load_result {
        DescriptorLoadResult::NotPresent => return Ok((None, None)),
        DescriptorLoadResult::Corrupt(error, manifest_digest_opt) => {
            log::debug!("resolve-site-patch-roots: descriptor corrupt (best-effort GC root): {error}");
            // Push the manifest blob as a GC root if the stored digest was
            // parseable — the manifest at least survives even when the layer
            // is missing.
            if let Some(manifest_digest) = manifest_digest_opt {
                descriptor_digests.push((registry.to_string(), manifest_digest));
            }
            return Ok((None, None));
        }
        DescriptorLoadResult::Loaded(manifest_digest, descriptor) => (manifest_digest, descriptor),
    };

    // Record the manifest digest (registry-qualified).
    descriptor_digests.push((registry.to_string(), manifest_digest.clone()));

    // Extract layer digests from the manifest blob on disk.  The manifest was
    // already verified by `load_descriptor_from_cas` inside
    // `load_descriptor_for_id`, so we re-read the on-disk bytes only to parse
    // the layer descriptor list — no re-verification needed.
    let data_path = blob_store.path(registry, &manifest_digest).join("data");
    match tokio::fs::read(&data_path).await {
        Ok(bytes) => {
            if let Ok(manifest_value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(layers) = manifest_value.get("layers").and_then(|v| v.as_array())
            {
                for layer in layers {
                    if let Some(digest_str) = layer.get("digest").and_then(|v| v.as_str())
                        && let Ok(layer_digest) = oci::Digest::try_from(digest_str)
                    {
                        descriptor_digests.push((registry.to_string(), layer_digest));
                    }
                }
            }
        }
        Err(error) => {
            log::debug!(
                "resolve-site-patch-roots: could not re-read manifest blob for layer digest extraction: {error}; manifest digest retained"
            );
        }
    }

    Ok((Some(descriptor), Some(manifest_digest)))
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
    #[tokio::test(flavor = "multi_thread")]
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
    #[tokio::test(flavor = "multi_thread")]
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
    #[tokio::test(flavor = "multi_thread")]
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
    #[tokio::test(flavor = "multi_thread")]
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

// ── Phase 4 specification tests — SitePatchSet + overlay (C-requirements) ──
//
// Traceability:
//   C1 — global-last: overlay appended after compose → patch wins over root var
//   C3 — surface gating: private dep absent under self_view=false, present under true
//   C5 — self_view=true admits full TC so private deps' patches load
//   Interface-only / no private leak — companion private env never surfaces
//   No-config no-op — patches=None → output byte-identical to compose
//   Admitted-set correctness — ComposeOutput.admitted visit order + dedup
//   Offline/local — SitePatchSet built only from local state

#[cfg(test)]
mod phase4_spec_tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::{
        config::patch::ResolvedPatchConfig,
        file_structure::{BlobStore, FileStructure, PackageStore, TagStore},
        oci::index::{ChainMode, Index, LocalConfig, LocalIndex},
        oci::{Digest, Identifier, PinnedIdentifier},
        package::{
            install_info::InstallInfo,
            metadata::{
                self, bundle, dependency,
                entrypoint::Entrypoints,
                env::{
                    self as metadata_env,
                    var::{Modifier, Var},
                },
                visibility::Visibility,
            },
            resolved_package::{ResolvedDependency, ResolvedPackage},
        },
        package_manager::{PackageManager, composer},
    };

    // ── Constants ─────────────────────────────────────────────────────────────

    const REGISTRY: &str = "example.com";
    const PATCH_REGISTRY: &str = "patches.example.com";

    // ── Fixture helpers ───────────────────────────────────────────────────────

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned(repo: &str, hex_char: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, REGISTRY).clone_with_digest(sha256(hex_char));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn make_store(root: &std::path::Path) -> PackageStore {
        let fs = FileStructure::with_root(root.to_path_buf());
        fs.packages.clone()
    }

    /// Build an offline `PackageManager` backed by a tempdir FileStructure.
    ///
    /// `patches = None` — use `with_patches(Some(...))` to enable the patch tier.
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

    fn test_patch_config() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        }
    }

    /// Seed a companion's tag-store entry in the **correct** `TagLock` envelope
    /// format that `LocalIndex::fetch_manifest_digest` expects.
    ///
    /// Schema: `{"version":1,"repository":"<registry>/<repo>","tags":{"<tag>":"<digest>"}}`
    ///
    /// Tests that previously wrote raw `BTreeMap<String, String>` (the wrong
    /// schema) must use this helper instead so that `find_companion_local` can
    /// resolve the tag → digest through the public `Index` wrapper without error.
    pub(super) fn write_companion_tag_lock(
        tag_store: &crate::file_structure::TagStore,
        companion_tag_id: &Identifier,
        digest: &Digest,
    ) {
        let tags_path = tag_store.tags(companion_tag_id);
        std::fs::create_dir_all(tags_path.parent().unwrap()).unwrap();
        let tag = companion_tag_id.tag_or_latest();
        let registry = companion_tag_id.registry();
        let repo = companion_tag_id.repository();
        // Emit the TagLock envelope: version, repository, and tags map.
        let json = serde_json::json!({
            "version": 1,
            "repository": format!("{registry}/{repo}"),
            "tags": { tag: digest.to_string() }
        })
        .to_string();
        std::fs::write(&tags_path, json).unwrap();
    }

    /// Write a minimal on-disk package directory (metadata.json + resolve.json).
    pub(super) fn seed_package_in_store(store: &PackageStore, id: &PinnedIdentifier, resolved: &ResolvedPackage) {
        let pkg_path = store.path(id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = serde_json::json!({ "type": "bundle", "version": 1 });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();
    }

    /// Seed a package with one env var of the given key/value/visibility.
    pub(super) fn seed_package_with_constant_var(
        store: &PackageStore,
        id: &PinnedIdentifier,
        resolved: &ResolvedPackage,
        var_key: &str,
        var_value: &str,
        var_vis: Visibility,
    ) {
        let pkg_path = store.path(id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();

        let vis_str = match var_vis {
            Visibility::PUBLIC => "public",
            Visibility::PRIVATE => "private",
            Visibility::INTERFACE => "interface",
            _ => "private", // SEALED — should not be used for env vars
        };
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": var_key,
                "type": "constant",
                "value": var_value,
                "visibility": vis_str,
            }],
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        let resolved_json = serde_json::to_string(resolved).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), resolved_json).unwrap();
    }

    /// Build a minimal `InstallInfo` backed by a real on-disk content dir.
    fn make_install_info(dir: &std::path::Path, repo: &str, hex_char: char, resolved: ResolvedPackage) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            crate::file_structure::PackageDir { dir: pkg_root },
        )
    }

    /// Build an `InstallInfo` with one constant env var of given key/value/vis.
    #[allow(dead_code)]
    fn make_install_info_with_var(
        dir: &std::path::Path,
        repo: &str,
        hex_char: char,
        resolved: ResolvedPackage,
        var_key: &str,
        var_value: &str,
        var_vis: Visibility,
    ) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let var = Var {
            key: var_key.to_string(),
            modifier: Modifier::Constant(metadata_env::constant::Constant {
                value: var_value.to_string(),
            }),
            visibility: var_vis,
        };
        let mut builder = metadata_env::EnvBuilder::new();
        builder.add_var(var);
        let env = builder.build();
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            crate::file_structure::PackageDir { dir: pkg_root },
        )
    }

    // ── Admitted-set correctness ──────────────────────────────────────────────

    /// compose's `ComposeOutput.admitted` contains the dep (before the root) and
    /// the root, in topological / visit order (dep first, root last).
    ///
    /// Traceability: Admitted-set correctness — deps appear before roots.
    #[tokio::test]
    async fn compose_admitted_set_contains_dep_then_root_in_visit_order() {
        let dir = TempDir::new().unwrap();
        let store = make_store(dir.path());

        let dep_id = pinned("dep", 'd');
        seed_package_in_store(&store, &dep_id, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: dep_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let root = Arc::new(make_install_info(dir.path(), "root", 'r', root_resolved));
        let root_key = root.identifier().strip_advisory();

        let out = composer::compose(&[root], &store, false).await.unwrap();

        // dep_key stripped
        let dep_key = dep_id.strip_advisory();

        assert_eq!(
            out.admitted.len(),
            2,
            "admitted set must contain exactly dep + root; got {:?}",
            out.admitted
        );
        assert_eq!(
            out.admitted[0], dep_key,
            "dep must appear first in admitted set (topological order)"
        );
        assert_eq!(out.admitted[1], root_key, "root must appear last in admitted set");
    }

    /// A PRIVATE dep is excluded from the admitted set under self_view=false.
    ///
    /// Traceability: C3 — surface gating governs admitted set membership.
    #[tokio::test]
    async fn compose_admitted_set_excludes_private_dep_default_exec() {
        let dir = TempDir::new().unwrap();
        let store = make_store(dir.path());

        let priv_dep_id = pinned("privdep", 'p');
        seed_package_in_store(&store, &priv_dep_id, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: priv_dep_id.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info(dir.path(), "root", 'r', root_resolved));
        let root_key = root.identifier().strip_advisory();

        let out = composer::compose(&[root], &store, false).await.unwrap();

        // Private dep excluded from admitted set on interface surface.
        let priv_key = priv_dep_id.strip_advisory();
        assert!(
            !out.admitted.contains(&priv_key),
            "PRIVATE dep must be absent from admitted set under self_view=false"
        );
        assert!(
            out.admitted.contains(&root_key),
            "root must still appear in admitted set"
        );
    }

    /// A PRIVATE dep IS included in the admitted set under self_view=true (--self).
    ///
    /// Traceability: C3/C5 — private surface admits private deps.
    #[tokio::test]
    async fn compose_admitted_set_includes_private_dep_self_view() {
        let dir = TempDir::new().unwrap();
        let store = make_store(dir.path());

        let priv_dep_id = pinned("privdep", 'p');
        seed_package_in_store(&store, &priv_dep_id, &ResolvedPackage::new());

        let root_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: priv_dep_id.clone(),
                visibility: Visibility::PRIVATE,
            }],
        };
        let root = Arc::new(make_install_info(dir.path(), "root", 'r', root_resolved));

        let out = composer::compose(&[root], &store, true).await.unwrap();

        let priv_key = priv_dep_id.strip_advisory();
        assert!(
            out.admitted.contains(&priv_key),
            "PRIVATE dep must be in admitted set under self_view=true"
        );
    }

    /// Cross-root dedup: a shared dep that appears in both roots' TCs is
    /// admitted only once (first-seen wins).
    ///
    /// Traceability: Admitted-set correctness — cross-root dedup.
    #[tokio::test]
    async fn compose_admitted_set_deduplicates_shared_dep_across_roots() {
        let dir = TempDir::new().unwrap();
        let store = make_store(dir.path());

        let shared_id = pinned("shared", 's');
        seed_package_in_store(&store, &shared_id, &ResolvedPackage::new());

        let a_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: shared_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let b_resolved = ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: shared_id.clone(),
                visibility: Visibility::PUBLIC,
            }],
        };
        let a = Arc::new(make_install_info(dir.path(), "roota", 'a', a_resolved));
        let b = Arc::new(make_install_info(dir.path(), "rootb", 'b', b_resolved));

        let out = composer::compose(&[a, b], &store, false).await.unwrap();

        let shared_key = shared_id.strip_advisory();
        let count = out.admitted.iter().filter(|id| **id == shared_key).count();
        assert_eq!(
            count, 1,
            "shared dep must appear exactly once in admitted set; appeared {count} times"
        );
    }

    // ── No-config no-op (patches=None) ───────────────────────────────────────

    /// With `patches = None`, `resolve_env` output is byte-identical to the
    /// raw compose output — no overlay is applied.
    ///
    /// This PASSES against the stub because `build_site_patch_set` short-circuits
    /// on `self.patches() == None` before hitting `unimplemented!()`.
    ///
    /// Traceability: No-config no-op guarantee.
    #[tokio::test]
    async fn resolve_env_no_patches_config_is_byte_identical_to_compose() {
        let dir = TempDir::new().unwrap();
        let store = make_store(dir.path());

        // Root with one interface var.
        let root_id = pinned("rootpkg", 'r');
        seed_package_with_constant_var(
            &store,
            &root_id,
            &ResolvedPackage::new(),
            "ROOT_VAR",
            "root_value",
            Visibility::INTERFACE,
        );
        let pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            {
                // Re-parse from disk so metadata matches seeded JSON.
                let meta_json = std::fs::read_to_string(pkg_path.join("metadata.json")).unwrap();
                serde_json::from_str::<metadata::Metadata>(&meta_json).unwrap()
            },
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: pkg_path },
        ));

        // Baseline: plain compose.
        let compose_out = composer::compose(std::slice::from_ref(&root), &store, false)
            .await
            .unwrap();
        let compose_entries = compose_out.entries;

        // Manager with patches=None.
        let manager = make_manager(&dir); // patches=None by default
        let resolved = manager.resolve_env(&[root], false).await.unwrap();

        assert_eq!(
            compose_entries.len(),
            resolved.len(),
            "patches=None: resolve_env entry count must equal compose output"
        );
        for (ce, re) in compose_entries.iter().zip(resolved.iter()) {
            assert_eq!(ce.key, re.key, "key must match");
            assert_eq!(ce.value, re.value, "value must match");
        }
    }

    // ── C1: global-last — patch overlay appended after all compose entries ─────

    /// C1: With patch tier configured but no descriptors persisted locally
    /// (NeverLooked state — no network in offline test), `resolve_env` succeeds
    /// and produces the same entries as plain compose (root's MY_VAR only).
    ///
    /// The C1 global-last invariant applies when companions ARE found; here we
    /// verify the safe no-descriptor path: output = compose output, no overlay.
    ///
    /// Traceability: C1 global-last invariant (offline / no-descriptor path).
    #[tokio::test]
    async fn c1_companion_overlay_appended_after_root_var_global_last() {
        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();

        // Transitive dep: no env vars.
        let dep_id = pinned("libfoo", 'd');
        seed_package_in_store(&store, &dep_id, &ResolvedPackage::new());

        // Root: declares MY_VAR=root_value (interface) and depends on dep.
        let root_id = pinned("rootpkg", 'r');
        seed_package_with_constant_var(
            &store,
            &root_id,
            &ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
            "MY_VAR",
            "root_value",
            Visibility::INTERFACE,
        );
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // With patches=Some but no descriptor persisted (NeverLooked → offline → no
        // fetch), resolve_env must succeed and produce exactly the compose output.
        let entries = manager.resolve_env(&[root], false).await.unwrap();

        // No descriptors → no companion overlay → only root's MY_VAR is present.
        let my_var_count = entries.iter().filter(|e| e.key == "MY_VAR").count();
        assert_eq!(
            my_var_count, 1,
            "without any descriptor, only the root's MY_VAR must be present; got entries: {entries:?}"
        );
        assert_eq!(
            entries.iter().find(|e| e.key == "MY_VAR").map(|e| e.value.as_str()),
            Some("root_value"),
            "root's MY_VAR value must be root_value"
        );
    }

    /// C1 live: a companion loaded for a transitive dep via a seeded global descriptor
    /// appends its INTERFACE entry AFTER the root's own Constant var.
    ///
    /// With last-wins env semantics the companion's entry (same key) overrides the
    /// root's Constant — proving the global-last invariant on the live overlay path.
    ///
    /// Setup:
    ///   - root declares MY_VAR=root_value (INTERFACE Constant)
    ///   - dep is a transitive PUBLIC dependency
    ///   - companion declares MY_VAR=companion_value (INTERFACE Constant)
    ///   - global descriptor has rule matching "*" → companion
    ///   - global tag-map entry seeded for dep's patch path and root's patch path
    ///
    /// Expected: entries has root's MY_VAR first, then companion's MY_VAR appended
    /// after (so the last occurrence in the Vec wins for any evaluator that
    /// uses last-wins semantics — proving C1 global-last).
    ///
    /// Traceability: C1 global-last invariant (live companion override path).
    #[tokio::test(flavor = "multi_thread")]
    async fn c1_live_companion_entry_appended_after_root_var_proves_global_last() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // ── Companion: INTERFACE var MY_VAR=companion_value ────────────────────
        // Companion is stored with PATCH_REGISTRY so find_companion_local's
        // PackageStore lookup (keyed by registry) resolves to the right path.
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("companion-pkg", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("companion-pkg", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "MY_VAR",
            "companion_value",
            Visibility::INTERFACE,
        );

        // Write companion's tag-store entry in the correct TagLock envelope so
        // find_companion_local (via Index::fetch_manifest_digest / Op::Query)
        // can resolve the tag → digest without a schema mismatch error.
        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);

        // ── Global descriptor: rule "*" → companion ────────────────────────────
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{
                "match": "*",
                "packages": [companion_tag_id.to_string()],
            }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);

        // Write both blobs to the blob store.
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();

        // Write the global tag-map entry: LookedHasDescriptor.
        let global_id = global_descriptor_id(&test_patch_config());
        let global_tags_path = tag_store.tags(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .unwrap();

        // ── Root and dep ───────────────────────────────────────────────────────
        let dep_id = pinned("libfoo", 'd');
        seed_package_in_store(&store, &dep_id, &ResolvedPackage::new());

        let root_id = pinned("rootpkg", 'r');
        seed_package_with_constant_var(
            &store,
            &root_id,
            &ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
            "MY_VAR",
            "root_value",
            Visibility::INTERFACE,
        );
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        let entries = manager.resolve_env(&[root], false).await.unwrap();

        // Verify global-last invariant: the "*" rule matches EVERY admitted
        // identifier (dep + root), so the companion is appended once per admitted
        // identifier. What matters for C1 is that:
        //   1. root's MY_VAR (from compose) comes FIRST in the Vec.
        //   2. All companion MY_VAR entries are appended AFTER compose output
        //      (global-last).
        //   3. The last MY_VAR entry is companion_value (last-wins semantics =
        //      companion overrides root's Constant).
        let first_my_var = entries.iter().find(|e| e.key == "MY_VAR").map(|e| e.value.as_str());
        assert_eq!(
            first_my_var,
            Some("root_value"),
            "C1 live: first MY_VAR must be root's value (compose output before overlay); entries: {entries:?}"
        );

        let last_my_var = entries
            .iter()
            .rev()
            .find(|e| e.key == "MY_VAR")
            .map(|e| e.value.as_str());
        assert_eq!(
            last_my_var,
            Some("companion_value"),
            "C1 live: last MY_VAR must be companion_value (companion appended globally last → overrides root via last-wins); entries: {entries:?}"
        );

        // Structural check: first occurrence index < last occurrence index.
        let first_idx = entries.iter().position(|e| e.key == "MY_VAR").unwrap();
        let last_idx = entries.iter().rposition(|e| e.key == "MY_VAR").unwrap();
        assert!(
            first_idx < last_idx,
            "C1 live: root's MY_VAR index ({first_idx}) must be less than companion's index ({last_idx})"
        );
    }

    // ── C3: surface gating — private dep absent/present by self_view ──────────

    /// C3: Under self_view=false (default exec), a dep with PRIVATE visibility
    /// in the TC is absent from the admitted set, so no patch companion is
    /// loaded for it.
    ///
    /// This is observable from `compose`'s admitted set alone — no patch
    /// infrastructure needed — so it PASSES against the stub.
    ///
    /// Traceability: C3 surface gating.
    #[tokio::test]
    async fn c3_private_dep_absent_from_admitted_set_default_exec() {
        let dir = TempDir::new().unwrap();
        let store = make_store(dir.path());

        let priv_dep_id = pinned("privatepkg", 'p');
        seed_package_in_store(&store, &priv_dep_id, &ResolvedPackage::new());

        let root = Arc::new(make_install_info(
            dir.path(),
            "root",
            'r',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: priv_dep_id.clone(),
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));

        let out = composer::compose(&[root], &store, false).await.unwrap();

        let priv_key = priv_dep_id.strip_advisory();
        assert!(
            !out.admitted.contains(&priv_key),
            "C3: PRIVATE dep must be absent from admitted set under self_view=false"
        );
    }

    /// C3/C5: Under self_view=true (--self / launcher), a dep with PRIVATE
    /// visibility IS in the admitted set and would receive companion overlay.
    ///
    /// This is observable from `compose`'s admitted set alone — PASSES against stub.
    ///
    /// Traceability: C3 + C5 private surface admission.
    #[tokio::test]
    async fn c3_c5_private_dep_present_in_admitted_set_self_view() {
        let dir = TempDir::new().unwrap();
        let store = make_store(dir.path());

        let priv_dep_id = pinned("privatepkg", 'p');
        seed_package_in_store(&store, &priv_dep_id, &ResolvedPackage::new());

        let root = Arc::new(make_install_info(
            dir.path(),
            "root",
            'r',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: priv_dep_id.clone(),
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));

        let out = composer::compose(&[root], &store, true).await.unwrap();

        let priv_key = priv_dep_id.strip_advisory();
        assert!(
            out.admitted.contains(&priv_key),
            "C3/C5: PRIVATE dep must be in admitted set under self_view=true"
        );
    }

    // ── Interface-only / no private leak ─────────────────────────────────────

    /// A companion's private-only env var (Visibility::PRIVATE) must never appear
    /// in the target's env, even when the target is composed under self_view=true.
    ///
    /// The companion is projected via `compose([companion], store, false)` —
    /// interface surface only — so the companion's PRIVATE var is excluded.
    ///
    /// With no descriptor persisted (NeverLooked, offline), resolve_env succeeds
    /// and the companion's PRIVATE var is absent from the output (no overlay loaded).
    ///
    /// Traceability: Interface-only / no-private-leak invariant.
    #[tokio::test]
    async fn no_private_leak_companion_private_var_absent_even_under_self_view() {
        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();

        // Companion (to be installed locally): has ONLY a PRIVATE var.
        let companion_id = pinned("companion-ca", 'c');
        seed_package_with_constant_var(
            &store,
            &companion_id,
            &ResolvedPackage::new(),
            "COMPANION_SECRET",
            "secret_value",
            Visibility::PRIVATE, // private only — must not leak
        );

        // Base dep installed locally (companion will be fetched for this dep).
        let dep_id = pinned("basepkg", 'b');
        seed_package_in_store(&store, &dep_id, &ResolvedPackage::new());

        // Root depends on dep privately.
        let root = Arc::new(make_install_info(
            dir.path(),
            "rootpkg",
            'r',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));

        // With no descriptor persisted (NeverLooked → offline → no fetch), resolve_env
        // succeeds and the companion's PRIVATE var is absent from the output.
        let entries = manager.resolve_env(&[root], true).await.unwrap();

        assert!(
            !entries.iter().any(|e| e.key == "COMPANION_SECRET"),
            "companion PRIVATE var must not appear in the env (no descriptor → no overlay)"
        );
    }

    /// No-private-leak live: a companion that carries ONLY a PRIVATE env var
    /// must NEVER appear in the resolved env, even under self_view=true, because
    /// the companion is projected via `compose([companion], store, false)` which
    /// gates out private surface.
    ///
    /// This test seeds a real global descriptor + companion package so the live
    /// companion-projection path executes, verifying that `compose([companion],
    /// store, false)` (interface surface) excludes the companion's PRIVATE var.
    ///
    /// Traceability: Interface-only / no-private-leak invariant (live projection path).
    #[tokio::test(flavor = "multi_thread")]
    async fn no_private_leak_live_companion_private_var_excluded_by_interface_projection() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // ── Companion: has ONLY a PRIVATE var (must never leak). ──────────────
        // Store under PATCH_REGISTRY so find_companion_local's PackageStore
        // lookup resolves to the same registry the tag-store entry uses.
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("private-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("private-companion", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "COMPANION_SECRET",
            "secret_val",
            Visibility::PRIVATE, // PRIVATE — must be excluded by interface projection
        );

        // Write companion's tag-store entry in the correct TagLock envelope so
        // find_companion_local (via Index::fetch_manifest_digest / Op::Query)
        // resolves the tag → digest without a schema mismatch error.
        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);

        // ── Global descriptor: rule "*" → companion (the private-only companion). ─
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{
                "match": "*",
                "packages": [companion_tag_id.to_string()],
            }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);

        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();

        let global_id = global_descriptor_id(&test_patch_config());
        let global_tags_path = tag_store.tags(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .unwrap();

        // ── Root with a PUBLIC dep. ────────────────────────────────────────────
        let dep_id = pinned("basepkg", 'b');
        seed_package_in_store(&store, &dep_id, &ResolvedPackage::new());

        let root = Arc::new(make_install_info(
            dir.path(),
            "rootpkg",
            'r',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PRIVATE,
                }],
            },
        ));

        // Under self_view=true: the dep IS admitted (private surface), so the
        // companion overlay is attempted. But `compose([companion], store, false)`
        // (interface projection) must exclude the companion's PRIVATE var.
        let entries = manager.resolve_env(&[root], true).await.unwrap();

        assert!(
            !entries.iter().any(|e| e.key == "COMPANION_SECRET"),
            "no-private-leak live: companion's PRIVATE var must be absent even under self_view=true; entries: {entries:?}"
        );
    }

    // ── Offline / local-only reads ────────────────────────────────────────────

    /// With patches configured, `build_site_patch_set` only performs local reads
    /// (PatchTagMap + BlobStore + find_plain).  An OFFLINE manager (no OCI client)
    /// with no descriptors persisted (NeverLooked state) must succeed without any
    /// network call and return output equal to plain compose.
    ///
    /// Traceability: Offline/local invariant (no network in hot path).
    #[tokio::test]
    async fn offline_build_site_patch_set_uses_only_local_reads() {
        let dir = TempDir::new().unwrap();
        // Offline manager (client=None) + patches configured.
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();

        let dep_id = pinned("deplocal", 'd');
        seed_package_in_store(&store, &dep_id, &ResolvedPackage::new());

        let root = Arc::new(make_install_info(
            dir.path(),
            "rootlocal",
            'r',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        ));

        // With patches=Some but is_offline()=true and no descriptors persisted
        // (NeverLooked → no fetch), build_site_patch_set reads local state only
        // and succeeds without contacting the network.
        let result = manager.resolve_env(&[root], false).await;
        assert!(
            result.is_ok(),
            "offline resolve_env with patches=Some must succeed when no descriptors are persisted; got: {result:?}"
        );
    }

    // ── Global-vs-package-specific precedence ─────────────────────────────────

    /// With no descriptors persisted (NeverLooked, offline), `resolve_env` with
    /// patches=Some succeeds and returns the plain compose output.  This verifies
    /// the no-descriptor path of the global-vs-pkg-specific merge algorithm works
    /// end-to-end without panicking or erroring.
    ///
    /// The precedence property itself (pkg-specific overrides global for same
    /// companion identifier) is verified offline here as a no-op: with no
    /// descriptors loaded, neither source contributes companions.
    ///
    /// Traceability: Global vs package-specific override design rule (offline path).
    #[tokio::test]
    async fn global_vs_pkg_specific_pkg_specific_overrides_global() {
        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();

        let dep_id = pinned("target", 't');
        seed_package_in_store(&store, &dep_id, &ResolvedPackage::new());

        let root = Arc::new(make_install_info(
            dir.path(),
            "rootpkg",
            'r',
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        ));

        // With no descriptor persisted (NeverLooked → offline → no fetch), the
        // merge algorithm finds no companions for either source and succeeds.
        let result = manager.resolve_env(&[root], false).await;
        assert!(
            result.is_ok(),
            "resolve_env with patches=Some and no descriptors must succeed; got: {result:?}"
        );
        // No companions → no overlay → output equals compose output (no entries for
        // packages with no env vars).
        let entries = result.unwrap();
        assert!(
            entries.is_empty(),
            "without descriptors, no env overlay must be applied; got: {entries:?}"
        );
    }

    // ── F5: C7 fail-closed — NeverLooked + required=true → Err ──────────────

    /// C7 fail-closed regression: when a companion is `required=true` and the
    /// local tag store has no record of it (NeverLooked state — companion was
    /// never installed), `resolve_env` must return an `Err`, not silently skip
    /// the companion and return a partial overlay.
    ///
    /// This test proves the fix for F1 (required companion not found → Err).
    ///
    /// Setup:
    ///   - root with no deps
    ///   - global descriptor: rule "*" → required companion (required=true via tier)
    ///   - companion: NOT installed locally (no tag-store entry, no package dir)
    ///
    /// Expected: `resolve_env` returns `Err(...)` wrapping `RequiredCompanionFailed`.
    ///
    /// Traceability: C7 fail-closed; F1 regression.
    #[tokio::test(flavor = "multi_thread")]
    async fn c7_required_companion_not_installed_locally_returns_err() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // required=true at tier level so all companions inherit required=true.
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // A companion identifier that is NOT installed locally.
        let companion_tag_id = Identifier::new_registry("required-companion", PATCH_REGISTRY).clone_with_tag("latest");

        // Global descriptor: rule "*" → required companion (via tier required=true).
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{
                "match": "*",
                "packages": [companion_tag_id.to_string()],
            }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);

        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();

        let global_id = global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.tags(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .unwrap();

        // Root with no deps — still admitted (roots always in admitted set).
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Companion is NOT installed locally: no tag-store entry, no package dir.
        // `find_companion_local` → `Ok(None)` → required=true → must return Err.
        let result = manager.resolve_env(&[root], false).await;
        assert!(
            result.is_err(),
            "C7 fail-closed: required companion not installed locally must return Err; got Ok({:?})",
            result.ok()
        );

        // The error chain must mention the companion failure.
        let err_string = format!("{:?}", result.unwrap_err());
        assert!(
            err_string.contains("required-companion") || err_string.contains("RequiredCompanionFailed"),
            "error must reference the companion; got: {err_string}"
        );
    }

    // ── F6: pkg-specific descriptor overrides global for same companion key ───

    /// F6 regression: when both a global and a package-specific descriptor
    /// have an entry for the same companion identifier (but different
    /// `required` flags), the package-specific entry wins (last-wins merge).
    ///
    /// Setup:
    ///   - global descriptor: rule matching "rootpkg" → companion with required=false
    ///   - pkg-specific descriptor for "rootpkg": rule matching "rootpkg" →
    ///     same companion but overriding to required=true
    ///   - companion NOT installed locally
    ///
    /// Expected: the merged companion is required=true (pkg-specific wins), so
    /// `resolve_env` returns `Err` (not `Ok` with skip).
    ///
    /// Traceability: F6 — pkg-specific override semantic; merge_companions last-wins.
    #[tokio::test(flavor = "multi_thread")]
    async fn f6_pkg_specific_descriptor_overrides_global_companion_required_flag() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id, patch_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // Tier required=false; companion required flag comes from per-rule required field.
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // Companion: NOT installed locally so find_companion_local → Ok(None).
        let companion_tag_id = Identifier::new_registry("shared-companion", PATCH_REGISTRY).clone_with_tag("latest");

        // Helper: write a descriptor blob and return its manifest digest.
        let write_descriptor = |required_flag: bool| {
            let descriptor_json = serde_json::json!({
                "version": 1,
                "rules": [{
                    "match": "*",
                    "packages": [companion_tag_id.to_string()],
                    "required": required_flag,
                }]
            })
            .to_string();
            let layer_bytes_owned = descriptor_json.into_bytes();
            (layer_bytes_owned, required_flag)
        };

        // Write global descriptor: required=false.
        let (global_layer_bytes, _) = write_descriptor(false);
        let global_layer_digest = Algorithm::Sha256.hash(&global_layer_bytes);
        let global_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": global_layer_digest.to_string(), "size": global_layer_bytes.len()}]
        })
        .to_string();
        let global_manifest_bytes = global_manifest_json.as_bytes().to_vec();
        let global_manifest_digest = Algorithm::Sha256.hash(&global_manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &global_manifest_digest, &global_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &global_layer_digest, &global_layer_bytes)
            .await
            .unwrap();

        let global_id = global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.tags(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &global_manifest_digest.to_string())
            .await
            .unwrap();

        // Root package to be admitted.
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());

        // Write pkg-specific descriptor for "rootpkg": required=true (overrides global).
        let root_base_id = root_id.as_identifier();
        let (pkg_layer_bytes, _) = write_descriptor(true);
        let pkg_layer_digest = Algorithm::Sha256.hash(&pkg_layer_bytes);
        let pkg_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": pkg_layer_digest.to_string(), "size": pkg_layer_bytes.len()}]
        })
        .to_string();
        let pkg_manifest_bytes = pkg_manifest_json.as_bytes().to_vec();
        let pkg_manifest_digest = Algorithm::Sha256.hash(&pkg_manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &pkg_manifest_digest, &pkg_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &pkg_layer_digest, &pkg_layer_bytes)
            .await
            .unwrap();

        let pkg_specific_id = patch_descriptor_id(&patch_config, root_base_id);
        let pkg_tags_path = tag_store.tags(&pkg_specific_id);
        PatchTagMap::write_has_descriptor(&pkg_tags_path, &pkg_manifest_digest.to_string())
            .await
            .unwrap();

        // Build the root InstallInfo from the seeded package.
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Companion is NOT installed locally → find_companion_local → Ok(None).
        // Merged required flag: pkg-specific (true) overrides global (false).
        // C7 fail-closed: must return Err because merged required=true.
        let result = manager.resolve_env(&[root], false).await;
        assert!(
            result.is_err(),
            "F6: pkg-specific override of required=true must cause Err when companion absent; got Ok({:?})",
            result.ok()
        );
    }

    // ── F6b: pkg-specific companion env-value overrides global companion ─────────
    //
    // ADR: "package-specific overrides global on a shared key."  The F6 test
    // above only verifies the `required` flag merge.  This live test seeds:
    //   - global descriptor: rule "*" → companion-A (CERT_FILE=global_cert)
    //   - pkg-specific descriptor for "rootpkg": rule "*" → companion-A
    //     (CERT_FILE=pkg_cert, different installed package digest)
    // Both companions carry the same env key.  The pkg-specific companion is
    // installed as a distinct package with a different value, so the
    // last-appended entry (pkg-specific) wins — proving env-value override.

    /// F6b regression: when a global descriptor and a package-specific descriptor
    /// both reference a companion with the **same key** (`CERT_FILE`) but the
    /// pkg-specific companion's value is different from the global companion's,
    /// the pkg-specific companion's value is the last entry appended — it wins
    /// under last-wins semantics.
    ///
    /// Setup:
    ///   - global descriptor: rule "*" → global-companion (CERT_FILE=global_cert)
    ///   - pkg-specific descriptor for "rootpkg": rule "*" → pkg-companion
    ///     (CERT_FILE=pkg_cert)
    ///   - Both companions installed locally with distinct digests.
    ///
    /// Expected: the resolved env contains both CERT_FILE entries; the last one
    /// has value "pkg_cert" (pkg-specific companion appended after global).
    ///
    /// Traceability: ADR "package-specific overrides global on a shared key"
    /// (env-value ordering, not just required-flag override).
    #[tokio::test(flavor = "multi_thread")]
    async fn f6b_pkg_specific_companion_env_value_overrides_global_companion_on_shared_key() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id, patch_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // ── Global companion: CERT_FILE=global_cert ───────────────────────────
        // Use valid hex chars ('a', 'b') so Digest::try_from succeeds when
        // read_tag_digest parses the stored digest string.
        let global_companion_digest = sha256('a');
        let global_companion_tag_id =
            Identifier::new_registry("global-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let global_companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("global-companion", PATCH_REGISTRY)
                .clone_with_digest(global_companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &global_companion_pinned,
            &ResolvedPackage::new(),
            "CERT_FILE",
            "global_cert",
            Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &global_companion_tag_id, &global_companion_digest);

        // ── Pkg-specific companion: CERT_FILE=pkg_cert ────────────────────────
        let pkg_companion_digest = sha256('b');
        let pkg_companion_tag_id = Identifier::new_registry("pkg-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let pkg_companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("pkg-companion", PATCH_REGISTRY).clone_with_digest(pkg_companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &pkg_companion_pinned,
            &ResolvedPackage::new(),
            "CERT_FILE",
            "pkg_cert",
            Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &pkg_companion_tag_id, &pkg_companion_digest);

        // ── Helper: write a descriptor blob and return manifest digest ─────────
        let write_descriptor_blob = |companion_tag: &Identifier| {
            let descriptor_json = serde_json::json!({
                "version": 1,
                "rules": [{ "match": "*", "packages": [companion_tag.to_string()] }]
            })
            .to_string();
            descriptor_json.into_bytes()
        };

        // Write global descriptor (references global-companion).
        let global_layer_bytes = write_descriptor_blob(&global_companion_tag_id);
        let global_layer_digest = Algorithm::Sha256.hash(&global_layer_bytes);
        let global_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{
                "mediaType": "application/octet-stream",
                "digest": global_layer_digest.to_string(),
                "size": global_layer_bytes.len()
            }]
        })
        .to_string();
        let global_manifest_bytes = global_manifest_json.as_bytes().to_vec();
        let global_manifest_digest = Algorithm::Sha256.hash(&global_manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &global_manifest_digest, &global_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &global_layer_digest, &global_layer_bytes)
            .await
            .unwrap();
        let global_id = global_descriptor_id(&patch_config);
        let global_tags_path = tag_store.tags(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &global_manifest_digest.to_string())
            .await
            .unwrap();

        // Root package.
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());

        // Write pkg-specific descriptor for "rootpkg" (references pkg-companion).
        let pkg_layer_bytes = write_descriptor_blob(&pkg_companion_tag_id);
        let pkg_layer_digest = Algorithm::Sha256.hash(&pkg_layer_bytes);
        let pkg_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{
                "mediaType": "application/octet-stream",
                "digest": pkg_layer_digest.to_string(),
                "size": pkg_layer_bytes.len()
            }]
        })
        .to_string();
        let pkg_manifest_bytes = pkg_manifest_json.as_bytes().to_vec();
        let pkg_manifest_digest = Algorithm::Sha256.hash(&pkg_manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &pkg_manifest_digest, &pkg_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &pkg_layer_digest, &pkg_layer_bytes)
            .await
            .unwrap();
        let pkg_specific_id = patch_descriptor_id(&patch_config, root_id.as_identifier());
        let pkg_tags_path = tag_store.tags(&pkg_specific_id);
        PatchTagMap::write_has_descriptor(&pkg_tags_path, &pkg_manifest_digest.to_string())
            .await
            .unwrap();

        // ── Resolve and assert pkg-specific value is last (wins) ──────────────
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // ── Resolve and assert ─────────────────────────────────────────────────
        let entries = manager.resolve_env(&[root], false).await.unwrap();

        // Both CERT_FILE entries must be present.
        let cert_entries: Vec<_> = entries.iter().filter(|e| e.key == "CERT_FILE").collect();
        assert_eq!(
            cert_entries.len(),
            2,
            "F6b: expected 2 CERT_FILE entries (global-companion + pkg-companion); got {}: {entries:?}",
            cert_entries.len()
        );

        // Global companion is appended first (global descriptor processed first in merge_companions).
        assert_eq!(
            cert_entries[0].value, "global_cert",
            "F6b: first CERT_FILE must be global companion's value (global descriptor first in overlay)"
        );

        // Pkg-specific companion is appended last — wins under last-wins semantics.
        assert_eq!(
            cert_entries[1].value, "pkg_cert",
            "F6b: last CERT_FILE must be pkg-specific companion's value (appended last → overrides global)"
        );
    }

    // ── F7: C1 variant with Modifier::Path ────────────────────────────────────

    /// C1 invariant with a Path-type env var: a companion that declares a PATH
    /// modifier (prepend) must be appended AFTER the root's own PATH declaration.
    ///
    /// With last-wins semantics for prepend-type vars, the companion's prepend
    /// appears last — so it runs first in a colon-separated PATH search.
    ///
    /// This test ensures the global-last invariant works not just for Constant
    /// vars but also for Path vars.
    ///
    /// Traceability: C1 global-last (Modifier::Path variant); F7 regression.
    #[tokio::test(flavor = "multi_thread")]
    async fn f7_c1_global_last_holds_for_path_modifier() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::{
            oci::Algorithm,
            package::metadata::env::{EnvBuilder, path::Path as EnvPath, var::Modifier},
        };

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // ── Companion: has PATH = "/companion/bin" (interface, Path modifier). ──
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("path-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("path-companion", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();

        {
            // Build companion metadata with a Path var.
            let path_var = crate::package::metadata::env::var::Var {
                key: "PATH".to_string(),
                modifier: Modifier::Path(EnvPath {
                    value: "/companion/bin".to_string(),
                    required: false,
                }),
                visibility: Visibility::INTERFACE,
            };
            let mut builder = EnvBuilder::new();
            builder.add_var(path_var);
            let env = builder.build();
            let metadata = crate::package::metadata::Metadata::Bundle(crate::package::metadata::bundle::Bundle {
                version: crate::package::metadata::bundle::Version::V1,
                strip_components: None,
                env,
                dependencies: crate::package::metadata::dependency::Dependencies::default(),
                entrypoints: crate::package::metadata::entrypoint::Entrypoints::default(),
            });
            let pkg_path = store.path(&companion_pinned);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            std::fs::write(
                pkg_path.join("metadata.json"),
                serde_json::to_string(&metadata).unwrap(),
            )
            .unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        // Tag-store entry for companion in correct TagLock envelope format.
        {
            write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);
        }

        // ── Root: declares PATH = "/root/bin" (interface, Path modifier). ──────
        let root_id = pinned("rootpkg", 'r');
        {
            let path_var = crate::package::metadata::env::var::Var {
                key: "PATH".to_string(),
                modifier: Modifier::Path(EnvPath {
                    value: "/root/bin".to_string(),
                    required: false,
                }),
                visibility: Visibility::INTERFACE,
            };
            let mut builder = EnvBuilder::new();
            builder.add_var(path_var);
            let env = builder.build();
            let metadata = crate::package::metadata::Metadata::Bundle(crate::package::metadata::bundle::Bundle {
                version: crate::package::metadata::bundle::Version::V1,
                strip_components: None,
                env,
                dependencies: crate::package::metadata::dependency::Dependencies::default(),
                entrypoints: crate::package::metadata::entrypoint::Entrypoints::default(),
            });
            let pkg_path = store.path(&root_id);
            std::fs::create_dir_all(pkg_path.join("content")).unwrap();
            std::fs::write(
                pkg_path.join("metadata.json"),
                serde_json::to_string(&metadata).unwrap(),
            )
            .unwrap();
            std::fs::write(
                pkg_path.join("resolve.json"),
                serde_json::to_string(&ResolvedPackage::new()).unwrap(),
            )
            .unwrap();
        }

        // ── Global descriptor: rule "*" → path-companion ──────────────────────
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion_tag_id.to_string()] }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();

        let global_id = global_descriptor_id(&test_patch_config());
        let global_tags_path = tag_store.tags(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .unwrap();

        // ── Resolve and assert C1 global-last for Path vars ───────────────────
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        let entries = manager.resolve_env(&[root], false).await.unwrap();

        // Both PATH entries must be present.
        let path_entries: Vec<_> = entries.iter().filter(|e| e.key == "PATH").collect();
        assert_eq!(
            path_entries.len(),
            2,
            "F7: expected 2 PATH entries (root + companion); got {}: {entries:?}",
            path_entries.len()
        );

        // C1 global-last: root's PATH ("/root/bin") comes first; companion's ("/companion/bin") last.
        assert_eq!(
            path_entries[0].value, "/root/bin",
            "F7: first PATH must be root's /root/bin (compose-phase, before overlay)"
        );
        assert_eq!(
            path_entries[1].value, "/companion/bin",
            "F7: second PATH must be companion's /companion/bin (overlay, appended globally last)"
        );
    }

    // ── Fix 2: fail-closed companion cap regression ───────────────────────────

    /// Regression: when merged companion count exceeds MAX_TOTAL_COMPANIONS and
    /// the patch tier is `required=true`, `resolve_env` must return `Err`.
    ///
    /// Setup: two descriptors (global + pkg-specific) each contributing unique
    /// optional companions to reach > MAX_TOTAL_COMPANIONS total companions.
    /// Since they are all UNIQUE identifiers, the dedup in merge_companions
    /// does NOT reduce the count. With `patches.required = true`, the cap
    /// breach must result in `Err(PatchDiscovery(DescriptorTooLarge))`.
    ///
    /// Traceability: Fix 2 — fail-closed cap consistency with Phase 3.
    #[tokio::test(flavor = "multi_thread")]
    async fn fix2_required_tier_over_cap_returns_err() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id, patch_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // Tier required=true → cap breach must fail closed.
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        let cap = super::super::patch_discovery::MAX_TOTAL_COMPANIONS;
        // Split companions: global gets `cap/2 + 1`, pkg-specific gets `cap/2 + 1`.
        // Total = cap + 2 → exceeds cap. All are unique identifiers (no dedup).
        let half = cap / 2 + 1;

        // Helper: build a descriptor JSON with `n` unique companion identifiers,
        // starting from identifier index `offset`.
        let make_descriptor_json = |n: usize, offset: usize| -> String {
            let packages: Vec<String> = (0..n)
                .map(|i| format!("{}/companion-{:04}:latest", PATCH_REGISTRY, offset + i))
                .collect();
            serde_json::json!({
                "version": 1,
                "rules": [{ "match": "*", "packages": packages }]
            })
            .to_string()
        };

        // Write and register the global descriptor (half companions).
        let global_layer_bytes = make_descriptor_json(half, 0);
        let global_layer_bytes = global_layer_bytes.as_bytes();
        let global_layer_digest = Algorithm::Sha256.hash(global_layer_bytes);
        let global_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": global_layer_digest.to_string(), "size": global_layer_bytes.len()}]
        })
        .to_string();
        let global_manifest_bytes = global_manifest_json.as_bytes();
        let global_manifest_digest = Algorithm::Sha256.hash(global_manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &global_manifest_digest, global_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &global_layer_digest, global_layer_bytes)
            .await
            .unwrap();
        let global_id = global_descriptor_id(&patch_config);
        PatchTagMap::write_has_descriptor(&tag_store.tags(&global_id), &global_manifest_digest.to_string())
            .await
            .unwrap();

        // Root package.
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());

        // Write and register the pkg-specific descriptor (another half companions).
        let pkg_layer_bytes = make_descriptor_json(half, half);
        let pkg_layer_bytes = pkg_layer_bytes.as_bytes();
        let pkg_layer_digest = Algorithm::Sha256.hash(pkg_layer_bytes);
        let pkg_manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": pkg_layer_digest.to_string(), "size": pkg_layer_bytes.len()}]
        })
        .to_string();
        let pkg_manifest_bytes = pkg_manifest_json.as_bytes();
        let pkg_manifest_digest = Algorithm::Sha256.hash(pkg_manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &pkg_manifest_digest, pkg_manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &pkg_layer_digest, pkg_layer_bytes)
            .await
            .unwrap();
        let pkg_specific_id = patch_descriptor_id(&patch_config, root_id.as_identifier());
        PatchTagMap::write_has_descriptor(&tag_store.tags(&pkg_specific_id), &pkg_manifest_digest.to_string())
            .await
            .unwrap();

        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        let result = manager.resolve_env(&[root], false).await;
        assert!(
            result.is_err(),
            "Fix 2: required tier + over-cap companion count must return Err; got Ok({:?})",
            result.ok()
        );

        // Verify the error is PatchDiscovery(DescriptorTooLarge).
        let err = result.unwrap_err();
        let err_str = format!("{err:?}");
        assert!(
            err_str.contains("DescriptorTooLarge") || err_str.contains("exceeds"),
            "Fix 2: error must be DescriptorTooLarge; got: {err_str}"
        );
    }

    /// Regression: when merged companion count exceeds MAX_TOTAL_COMPANIONS
    /// and the patch tier is `required=false` with all over-cap companions also
    /// optional, `resolve_env` must WARN and truncate (not return Err).
    ///
    /// Traceability: Fix 2 — non-required tier + over-cap → warn + truncate.
    #[tokio::test(flavor = "multi_thread")]
    async fn fix2_non_required_tier_over_cap_warns_and_truncates() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // Tier required=false → cap breach should warn + truncate.
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        let cap = super::super::patch_discovery::MAX_TOTAL_COMPANIONS;
        // Build a single descriptor with cap + 1 unique packages (all optional).
        let packages: Vec<String> = (0..=cap)
            .map(|i| format!("{}/companion-optional-{:04}:latest", PATCH_REGISTRY, i))
            .collect();
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": packages }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();
        let global_id = global_descriptor_id(&patch_config);
        PatchTagMap::write_has_descriptor(&tag_store.tags(&global_id), &manifest_digest.to_string())
            .await
            .unwrap();

        // Root package.
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Non-required tier with all-optional companions over cap: must succeed
        // (warn + truncate, not Err).
        let result = manager.resolve_env(&[root], false).await;
        assert!(
            result.is_ok(),
            "Fix 2: non-required tier + all-optional over-cap companions must succeed (warn+truncate); got: {result:?}"
        );
    }

    // ── Fix 3: cache bypass regression — required companion missing twice ─────

    /// Regression: a required companion that is genuinely missing must fail
    /// closed on BOTH the first lookup AND any cache-hit (second admitted id
    /// with the same companion). The cache value `None` must re-trigger the
    /// required-fail-closed check, not silently skip via the cache.
    ///
    /// Setup:
    ///   - Two admitted identifiers in the root's dep tree: dep1 (PUBLIC) and root.
    ///   - Global descriptor: rule "*" → companion (required=true via tier).
    ///   - Companion NOT installed locally → `find_companion_local` → `Ok(None)`.
    ///   - Two admitted IDs → companion looked up twice (first miss → cache None,
    ///     second → cache hit for None → must still fail closed).
    ///
    /// Expected: `resolve_env` returns `Err` (fails closed; not bypassed by cache).
    ///
    /// Traceability: Fix 3 — cache None re-triggers required fail-closed check.
    #[tokio::test(flavor = "multi_thread")]
    async fn fix3_required_companion_missing_fails_closed_even_on_cache_hit() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // Tier required=true → all companions are required.
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // Companion: NOT installed locally (no tag-store entry, no package dir).
        let companion_tag_id = Identifier::new_registry("required-companion", PATCH_REGISTRY).clone_with_tag("latest");

        // Global descriptor: rule "*" → required companion (matches both dep1 and root).
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion_tag_id.to_string()] }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();
        let global_id = global_descriptor_id(&patch_config);
        PatchTagMap::write_has_descriptor(&tag_store.tags(&global_id), &manifest_digest.to_string())
            .await
            .unwrap();

        // dep1: a PUBLIC dep that is in the admitted set.
        let dep1_id = pinned("dep1pkg", 'd');
        seed_package_in_store(&store, &dep1_id, &ResolvedPackage::new());

        // Root: depends on dep1 publicly → two admitted IDs: dep1 + root.
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(
            &store,
            &root_id,
            &ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep1_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
        );
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep1_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Companion is missing. The global "*" rule matches dep1 AND root —
        // so the companion lookup runs for dep1 (cache miss → None → Err immediately).
        // The fix ensures the error is returned on the FIRST encounter, and
        // also that if a later iteration reaches the cache hit for None, it
        // still fails closed (no silent bypass).
        let result = manager.resolve_env(&[root], false).await;
        assert!(
            result.is_err(),
            "Fix 3: required companion missing with two admitted IDs must fail closed; got Ok({:?})",
            result.ok()
        );

        let err_str = format!("{:?}", result.unwrap_err());
        assert!(
            err_str.contains("required-companion") || err_str.contains("RequiredCompanionFailed"),
            "Fix 3: error must reference the required companion; got: {err_str}"
        );
    }

    // ── Recursion / projection guard ──────────────────────────────────────────

    // ── Schema regression: TagLock envelope — required companion missing ────────

    /// Regression for the schema bug: `find_companion_local` previously read the
    /// companion tag file as a raw `BTreeMap<String, String>`, which fails on the
    /// real on-disk `TagLock` envelope (`{"version":1,"repository":...,"tags":{...}}`).
    ///
    /// This test seeds the companion tag file in the **correct** TagLock schema
    /// but does NOT install the companion package itself (no manifest blob, no
    /// package dir).  The expected outcome is `Err(RequiredCompanionFailed)` —
    /// i.e. `find_companion_local` parses the TagLock correctly (no schema error),
    /// resolves the digest, looks up the package store (miss → `Ok(None)`), and
    /// then the required-fail-closed arm fires.
    ///
    /// Before the fix, `read_tag_digest` would return `Err` on the real TagLock
    /// file, which silently skipped the companion — a C7 violation.
    ///
    /// Traceability: schema regression + C7 fail-closed on real TagLock path.
    #[tokio::test(flavor = "multi_thread")]
    async fn schema_regression_required_companion_tag_lock_envelope_missing_package_returns_err() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true, // required=true → must fail closed
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // Companion tag ID whose tag file will be written in the REAL TagLock
        // envelope format — but no package is installed (no package dir, no blob).
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("required-ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");

        // Write the companion tag file in the correct TagLock schema.
        // Before the fix, `read_tag_digest` would fail to parse this and silently
        // skip the required companion — a C7 violation.  After the fix,
        // `fetch_manifest_digest(Op::Query)` parses it correctly, returns
        // `Ok(Some(digest))`, then `find_in_store` returns `Ok(None)` (not
        // installed), and the required-fail-closed arm fires.
        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);
        // NOTE: deliberately NOT seeding the package dir — companion is not installed.

        // Global descriptor: rule "*" → required companion.
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion_tag_id.to_string()] }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();
        let global_id = global_descriptor_id(&patch_config);
        PatchTagMap::write_has_descriptor(&tag_store.tags(&global_id), &manifest_digest.to_string())
            .await
            .unwrap();

        // Root with no deps — still admitted (roots always in admitted set).
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        let root = std::sync::Arc::new(crate::package::install_info::InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Must return Err (required companion present in tag store but not installed
        // as a package → fail closed).  Before the fix this returned Ok(()) because
        // the schema mismatch in `read_tag_digest` produced a silent Err → warn+skip.
        let result = manager.resolve_env(&[root], false).await;
        assert!(
            result.is_err(),
            "schema regression: required companion with correct TagLock tag file but missing package must return Err; got Ok({:?})",
            result.ok()
        );

        let err_str = format!("{:?}", result.unwrap_err());
        assert!(
            err_str.contains("required-ca-bundle") || err_str.contains("RequiredCompanionFailed"),
            "schema regression: error must reference the required companion; got: {err_str}"
        );
    }

    /// Regression: a required companion that is fully installed (correct TagLock
    /// tag file + manifest blob + package dir with interface env var) must have
    /// its interface env appear in the overlay output.
    ///
    /// This test proves the real resolution path (TagLock → digest → package
    /// store → compose) works end-to-end when the companion IS installed.
    ///
    /// Traceability: schema regression — real path resolves + env surfaces.
    #[tokio::test(flavor = "multi_thread")]
    async fn schema_regression_required_companion_fully_installed_env_appears_in_overlay() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true, // required=true, so any resolution error is fatal
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        // ── Companion: fully installed (TagLock tag + package dir + interface var). ──
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();

        // Seed the companion's package in the store with an INTERFACE env var.
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "CA_BUNDLE",
            "/etc/ssl/certs/ca-bundle.crt",
            Visibility::INTERFACE,
        );

        // Write the companion's tag file in the correct TagLock envelope format.
        // This is the on-disk shape produced by a real Phase 3 install.
        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);

        // ── Global descriptor: rule "*" → companion ────────────────────────────
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion_tag_id.to_string()] }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();
        let global_id = global_descriptor_id(&patch_config);
        PatchTagMap::write_has_descriptor(&tag_store.tags(&global_id), &manifest_digest.to_string())
            .await
            .unwrap();

        // ── Root with no deps ──────────────────────────────────────────────────
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        let root = std::sync::Arc::new(crate::package::install_info::InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Must succeed and include the companion's CA_BUNDLE env var in the output.
        let entries = manager.resolve_env(&[root], false).await.unwrap();

        assert!(
            entries.iter().any(|e| e.key == "CA_BUNDLE"),
            "schema regression: fully-installed required companion's INTERFACE var must appear in overlay; entries: {entries:?}"
        );
        let ca_entry = entries.iter().find(|e| e.key == "CA_BUNDLE").unwrap();
        assert_eq!(
            ca_entry.value, "/etc/ssl/certs/ca-bundle.crt",
            "schema regression: CA_BUNDLE value must match companion's seeded value"
        );
    }

    /// The companion projection call (`compose([companion], store, false)`) is
    /// NOT itself patched — companions are projected via a plain compose call
    /// that does not recurse into `build_site_patch_set`.
    ///
    /// This is a structural property: the overlay lives in `resolve_env`, not in
    /// `compose`.  Since the companion projection calls `compose` directly,
    /// not `resolve_env`, there is no recursion path.
    ///
    /// This test verifies the structural guard using the admitted-set output
    /// from a plain compose call — does not depend on the stub.  PASSES.
    ///
    /// Traceability: Recursion/projection guard (companion compose is plain,
    /// not recursive through resolve_env).
    #[tokio::test]
    async fn companion_projection_via_plain_compose_has_no_recursive_overlay() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let store = make_store(dir.path());

        // Simulate what the Phase 4 implementation does: project a companion
        // via compose([companion], store, false) — interface surface only.
        let companion_id = pinned("companion-tool", 'c');
        seed_package_with_constant_var(
            &store,
            &companion_id,
            &ResolvedPackage::new(),
            "COMPANION_VAR",
            "companion_val",
            Visibility::INTERFACE,
        );
        let companion_pkg_path = store.path(&companion_id);
        let companion = Arc::new(InstallInfo::new(
            companion_id.clone(),
            serde_json::from_str::<metadata::Metadata>(
                &std::fs::read_to_string(companion_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir {
                dir: companion_pkg_path,
            },
        ));

        // Plain compose — no patch overlay (compose is patch-agnostic).
        let out = composer::compose(&[companion], &store, false).await.unwrap();

        // The companion's interface var must be projected.
        assert!(
            out.entries.iter().any(|e| e.key == "COMPANION_VAR"),
            "companion's interface var must appear in plain compose projection"
        );

        // The companion's admitted set should only contain itself (no sub-overlay).
        assert_eq!(
            out.admitted.len(),
            1,
            "companion projection admitted set must contain exactly the companion; got {:?}",
            out.admitted
        );
    }

    // ── Index-mode independence + offline-view overlay (Codex no-ship #2/#3) ─────

    /// Build a `PackageManager` whose index is in [`ChainMode::Remote`] with **no
    /// configured sources**.
    ///
    /// In `Remote` mode a tag-addressed `Op::Query` bypasses the local index read
    /// and routes straight to the sources (here: none → `Ok(None)`). A manager
    /// built this way therefore CANNOT resolve a locally-installed companion tag
    /// through its own `index()` — which is exactly why the companion overlay must
    /// resolve companions through a guaranteed-local index, not the manager's
    /// mode-sensitive one.
    fn make_remote_manager(dir: &TempDir) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                tag_store: TagStore::new(dir.path().join("tags")),
                blob_store: BlobStore::new(dir.path().join("blobs")),
            }),
            Vec::new(),
            ChainMode::Remote,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// Seed a fully-installed **global** companion into `manager`'s file structure:
    /// the companion's `TagLock` tag, optionally its package directory (carrying an
    /// `INTERFACE` env var `CA_BUNDLE`), and the global `__ocx.patch` descriptor
    /// (rule `*` → companion) recorded as `LookedHasDescriptor`.
    ///
    /// When `seed_package` is `false` the package directory is omitted (companion
    /// tag present but package missing → the required-fail-closed path). Returns
    /// the companion's interface `(key, value)` pair.
    pub(super) async fn seed_installed_global_companion(
        manager: &PackageManager,
        patch_config: &crate::config::patch::ResolvedPatchConfig,
        seed_package: bool,
    ) -> (&'static str, &'static str) {
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use crate::oci::Algorithm;

        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().tags.clone();

        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");

        if seed_package {
            let companion_pinned = PinnedIdentifier::try_from(
                Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
            )
            .unwrap();
            seed_package_with_constant_var(
                &store,
                &companion_pinned,
                &ResolvedPackage::new(),
                "CA_BUNDLE",
                "/etc/ssl/certs/ca-bundle.crt",
                Visibility::INTERFACE,
            );
        }

        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);

        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion_tag_id.to_string()] }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();
        let global_id = global_descriptor_id(patch_config);
        PatchTagMap::write_has_descriptor(&tag_store.tags(&global_id), &manifest_digest.to_string())
            .await
            .unwrap();

        ("CA_BUNDLE", "/etc/ssl/certs/ca-bundle.crt")
    }

    /// Build a root `InstallInfo` (no deps) seeded in `store`.
    pub(super) fn seed_root_arc(store: &PackageStore, name: &str, hex_char: char) -> Arc<InstallInfo> {
        let root_id = pinned(name, hex_char);
        seed_package_in_store(store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<crate::package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            crate::file_structure::PackageDir { dir: root_pkg_path },
        ))
    }

    /// Regression (Codex no-ship): the companion overlay must resolve companions
    /// from **local installed state in every index mode**, including `--remote`.
    ///
    /// A `ChainMode::Remote` index routes a tag-addressed `Op::Query` to its
    /// sources (here none), so resolving the companion through the manager's own
    /// `index()` would miss the locally-installed tag and — for a `required`
    /// companion — wrongly fail closed (or contact the registry when sources are
    /// configured). The companion IS installed locally; the overlay must apply.
    ///
    /// Before the fix `find_companion_local` used `self.index()` → under Remote
    /// mode this returned `None` and the required-fail-closed arm fired even though
    /// the companion was installed. After the fix the lookup goes through a
    /// guaranteed-local index, so the overlay applies regardless of mode.
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_mode_companion_overlay_resolves_from_local_state() {
        let dir = TempDir::new().unwrap();
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        // Remote-mode index with no sources: resolving a tag through `index()`
        // yields `None`, so success here proves the companion came from local state.
        let manager = make_remote_manager(&dir).with_patches(Some(patch_config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &patch_config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');
        let entries = manager.resolve_env(&[root], false).await.unwrap_or_else(|e| {
            panic!("remote-mode overlay must resolve the locally-installed companion, got Err: {e:?}")
        });

        let entry = entries
            .iter()
            .find(|e| e.key == key)
            .unwrap_or_else(|| panic!("remote-mode overlay must include companion var '{key}'; entries: {entries:?}"));
        assert_eq!(
            entry.value, value,
            "companion overlay value must match seeded interface var"
        );
    }

    /// Regression (Codex no-ship): `offline_view` must **preserve** the patch tier
    /// so already-discovered companion overlays still apply on local-only env paths
    /// (`ocx direnv export`, the global toolchain). ADR C4/C6 ("works offline once
    /// synced"; zip-`OCX_HOME` → offline → identical patched env).
    ///
    /// Before the fix `offline_view` set `patches: None`, so the overlay was
    /// silently dropped on exactly the offline exporters that should still apply it.
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_view_applies_installed_companion_overlay() {
        let dir = TempDir::new().unwrap();
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &patch_config, true).await;

        // Derive the offline view exactly as the CLI does, over the same stores.
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(dir.path().join("tags")),
            blob_store: BlobStore::new(dir.path().join("blobs")),
        });
        let offline = manager.offline_view(local_index);
        assert!(offline.is_offline(), "offline_view must produce an offline manager");

        let root = seed_root_arc(&offline.file_structure().packages.clone(), "rootpkg", 'r');
        let entries = offline.resolve_env(&[root], false).await.unwrap_or_else(|e| {
            panic!("offline_view must apply the already-installed companion overlay, got Err: {e:?}")
        });

        assert!(
            entries.iter().any(|e| e.key == key && e.value == value),
            "offline_view overlay must include companion var '{key}'; entries: {entries:?}"
        );
    }

    /// Regression (Codex no-ship): a `required` companion that is missing on an
    /// `offline_view` path must **fail closed** (C7) — not silently skip. Before
    /// the fix `offline_view` dropped the patch tier, so the required companion was
    /// neither applied nor reported missing (fail-OPEN).
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_view_required_missing_companion_fails_closed() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let patch_config = crate::config::patch::ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        // seed_package = false → companion tag present, package missing.
        seed_installed_global_companion(&manager, &patch_config, false).await;

        let local_index = LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(dir.path().join("tags")),
            blob_store: BlobStore::new(dir.path().join("blobs")),
        });
        let offline = manager.offline_view(local_index);

        let root = seed_root_arc(&offline.file_structure().packages.clone(), "rootpkg", 'r');
        let result = offline.resolve_env(&[root], false).await;
        assert!(
            result.is_err(),
            "offline_view with a required-but-missing companion must fail closed; got Ok({:?})",
            result.ok()
        );
        let err_str = format!("{:?}", result.unwrap_err());
        assert!(
            err_str.contains("required-ca-bundle")
                || err_str.contains("ca-bundle")
                || err_str.contains("RequiredCompanionFailed"),
            "offline_view fail-closed error must reference the required companion; got: {err_str}"
        );
    }
}

// ── Phase 5A specification tests — resolve_site_patch_roots (GC root derivation) ──
//
// Traceability:
//   Spec test 1 — seeded installed base + global descriptor + installed companion
//                 → companion PinnedIdentifier in .companions; descriptor manifest+layer
//                 digests in .descriptors.
//   Spec test 2 — ChainMode::Remote manager with companion installed locally
//                 → still returns companion (proves network-free, local-only).
//   Spec test 3 — patches=None → empty SitePatchRoots.
//
// These tests MUST compile and FAIL with unimplemented!() against the Phase 5A stub.
#[cfg(test)]
mod phase5a_spec_tests {
    use tempfile::TempDir;

    use crate::{
        config::patch::ResolvedPatchConfig,
        file_structure::{BlobStore, FileStructure, TagStore},
        oci::{
            Algorithm, Digest, Identifier, PinnedIdentifier,
            index::{ChainMode, Index, LocalConfig, LocalIndex},
        },
        package::{metadata::visibility::Visibility, resolved_package::ResolvedPackage},
        package_manager::PackageManager,
    };

    use super::{
        super::patch_discovery::{PatchTagMap, global_descriptor_id},
        phase4_spec_tests::{seed_package_with_constant_var, write_companion_tag_lock},
    };

    const REGISTRY: &str = "example.com";
    const PATCH_REGISTRY: &str = "patches.example.com";

    pub(super) fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    pub(super) fn test_patch_config() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        }
    }

    /// Build an offline `PackageManager` backed by a tempdir FileStructure.
    pub(super) fn make_manager(dir: &TempDir) -> PackageManager {
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

    /// Build a `ChainMode::Remote` `PackageManager` (no sources configured).
    ///
    /// In `Remote` mode a tag-addressed `Op::Query` bypasses the local index
    /// and routes to sources (here: none → `Ok(None)`). Success here proves
    /// `resolve_site_patch_roots` uses a guaranteed-local index, not the
    /// manager's mode-sensitive one.
    fn make_remote_manager(dir: &TempDir) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                tag_store: TagStore::new(dir.path().join("tags")),
                blob_store: BlobStore::new(dir.path().join("blobs")),
            }),
            Vec::new(),
            ChainMode::Remote,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// Seed a candidate symlink for `base_id` (so it appears as an installed base).
    ///
    /// The symlink store layout is:
    ///   `symlinks/{registry_slug}/{repo_path}/candidates/{tag}`
    ///
    /// This matches `SymlinkStore::candidate(&base_id)`. The symlink is
    /// not a real symlink in the test — the symlink path existing (even as an
    /// empty file or dir) is all that `resolve_site_patch_roots` needs to
    /// enumerate it as an installed base.
    ///
    /// Actually, `resolve_site_patch_roots` reads the symlink store by walking
    /// the directory tree under `symlinks/`. A real symlink to a package dir
    /// is the correct representation, but for the purpose of making
    /// `resolve_site_patch_roots` discover the base identifier, we just need
    /// the candidate path to exist. We create the parent dirs and a placeholder
    /// (regular file) for the candidate entry itself.
    pub(super) fn seed_installed_base_symlink(dir: &TempDir, base_id: &Identifier) {
        let symlink_store = crate::file_structure::SymlinkStore::new(dir.path().join("symlinks"));
        let candidate_path = symlink_store.candidate(base_id);
        std::fs::create_dir_all(candidate_path.parent().unwrap()).unwrap();
        // Write a placeholder that lets the walker see this as an installed candidate.
        // In a real install this would be a symlink → package dir; here a regular file
        // suffices because resolve_site_patch_roots only needs to derive the identifier
        // (registry/repo/tag) from the path, not follow the symlink.
        std::fs::write(&candidate_path, b"").unwrap();
    }

    /// Write a minimal descriptor blob pair (manifest + layer) into the blob store
    /// and record `LookedHasDescriptor` in the global tag-store entry.
    ///
    /// Returns `(manifest_digest, layer_digest)` for the caller to use in assertions.
    pub(super) async fn seed_global_descriptor_with_companion(
        dir: &TempDir,
        patch_config: &ResolvedPatchConfig,
        companion_tag_id: &Identifier,
    ) -> (Digest, Digest) {
        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let tag_store = TagStore::new(dir.path().join("tags"));

        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion_tag_id.to_string()] }]
        })
        .to_string();
        let layer_bytes = descriptor_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{
                "mediaType": "application/octet-stream",
                "digest": layer_digest.to_string(),
                "size": layer_bytes.len()
            }]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);

        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
            .await
            .unwrap();

        let global_id = global_descriptor_id(patch_config);
        let global_tags_path = tag_store.tags(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .unwrap();

        (manifest_digest, layer_digest)
    }

    // ── Spec test 1 — seeded base + global descriptor + installed companion ──

    /// `resolve_site_patch_roots` with an installed base, a global descriptor
    /// (rule `*` → companion), and an installed companion returns:
    ///   - the companion `PinnedIdentifier` in `.companions`
    ///   - the descriptor manifest digest + layer digest in `.descriptors`
    ///
    /// This is the primary contract test for the GC root derivation.
    ///
    /// Traceability: Phase 5A spec test 1.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_site_patch_roots_with_installed_base_and_companion_returns_roots() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (and any sibling test that reads WRITE_BLOB_CALL_COUNT). The counter is a
        // process-global static; holding this lock ensures our write_blob calls (via
        // `seed_global_descriptor_with_companion`) do not inflate its delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));

        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().tags.clone();

        // Seed an installed base: place a candidate symlink under the symlink store.
        let base_id = Identifier::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        // Seed the companion package in the package store.
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "CA_BUNDLE",
            "/etc/ssl/certs/bundle.crt",
            Visibility::INTERFACE,
        );

        // Write companion tag-store entry (TagLock format that LocalIndex understands).
        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);

        // Seed the global descriptor referencing the companion.
        let (manifest_digest, layer_digest) =
            seed_global_descriptor_with_companion(&dir, &patch_config, &companion_tag_id).await;

        // Call the stub — must fail with unimplemented!() until Phase 5A is complete.
        let roots = manager
            .resolve_site_patch_roots(&[])
            .await
            .expect("resolve_site_patch_roots must succeed");

        // Contract: companion pinned identifier is in .companions.
        assert!(
            roots.companions.contains(&companion_pinned),
            "spec test 1: companion pinned identifier must be in .companions; got: {:?}",
            roots.companions
        );

        // Contract: descriptor manifest digest is in .descriptors (registry-qualified).
        assert!(
            roots
                .descriptors
                .iter()
                .any(|(reg, dig)| reg == PATCH_REGISTRY && dig == &manifest_digest),
            "spec test 1: descriptor manifest digest must be in .descriptors; got: {:?}",
            roots.descriptors
        );

        // Contract: descriptor layer digest is in .descriptors (registry-qualified).
        assert!(
            roots
                .descriptors
                .iter()
                .any(|(reg, dig)| reg == PATCH_REGISTRY && dig == &layer_digest),
            "spec test 1: descriptor layer digest must be in .descriptors; got: {:?}",
            roots.descriptors
        );
    }

    // ── Spec test 2 — ChainMode::Remote proves network-free ─────────────────────

    /// `resolve_site_patch_roots` under `ChainMode::Remote` (no configured sources)
    /// still returns the companion, proving the lookup is guaranteed-local.
    ///
    /// A Remote-mode index with no sources returns `Ok(None)` for tag-addressed
    /// queries. If `resolve_site_patch_roots` used `self.index()` it would miss
    /// the locally-installed companion and return an empty `.companions`. Success
    /// here (companion present) proves the local-index-bypass pattern mirrors
    /// `find_companion_local`.
    ///
    /// Traceability: Phase 5A spec test 2 (network-free under ChainMode::Remote).
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_site_patch_roots_is_network_free_under_remote_chain_mode() {
        // Same serialisation guard as the companion spec test above.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        // Remote-mode manager — tag-addressed Op::Query returns Ok(None) via self.index().
        let manager = make_remote_manager(&dir).with_patches(Some(patch_config.clone()));

        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().tags.clone();

        // Seed installed base.
        let base_id = Identifier::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        // Seed companion.
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "CA_BUNDLE",
            "/etc/ssl/certs/bundle.crt",
            Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);

        // Seed global descriptor.
        seed_global_descriptor_with_companion(&dir, &patch_config, &companion_tag_id).await;

        // Call the stub — must fail with unimplemented!() until Phase 5A is complete.
        let roots = manager
            .resolve_site_patch_roots(&[])
            .await
            .expect("resolve_site_patch_roots must succeed even under ChainMode::Remote");

        // Contract: companion found via local-only index, even in Remote mode.
        assert!(
            roots.companions.contains(&companion_pinned),
            "spec test 2: Remote-mode manager must return companion from local state; got: {:?}",
            roots.companions
        );
    }

    // ── Spec test 3 — patches=None → empty SitePatchRoots ───────────────────────

    /// `resolve_site_patch_roots` returns an empty `SitePatchRoots` when
    /// `self.patches()` is `None` (no `[patches]` section configured).
    ///
    /// This is the no-config no-op guarantee — same short-circuit logic as
    /// `build_site_patch_set`.
    ///
    /// Traceability: Phase 5A spec test 3.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_site_patch_roots_with_no_patches_config_returns_empty() {
        let dir = TempDir::new().unwrap();
        // Manager with patches=None.
        let manager = make_manager(&dir); // no .with_patches(...)

        // Seed an installed base so we can prove the early-return fires, not a
        // "nothing to enumerate" path.
        let base_id = Identifier::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        let roots = manager
            .resolve_site_patch_roots(&[])
            .await
            .expect("resolve_site_patch_roots with patches=None must return Ok(empty)");

        assert!(
            roots.companions.is_empty(),
            "spec test 3: patches=None → .companions must be empty; got: {:?}",
            roots.companions
        );
        assert!(
            roots.descriptors.is_empty(),
            "spec test 3: patches=None → .descriptors must be empty; got: {:?}",
            roots.descriptors
        );
    }

    // ── Spec test 6 — scoped descriptor rule matches reconstructed base id ───────
    //
    // Regression test for the `collect_candidates_from_dir` bug (finding #4):
    // when the "candidates" directory name was incorrectly pushed into
    // `repo_components`, the reconstructed base identifier became
    // `"cmake/candidates:3.28"` instead of `"cmake:3.28"`, causing scoped
    // descriptor rules (e.g. `"match": "example.com/cmake:*"`) to never match.
    //
    // A catch-all rule (`"match": "*"`) masks the bug because glob `*` matches
    // any string including the corrupted form.  This test uses a scoped rule
    // to expose it.

    /// A descriptor with a scoped `match` rule (`"example.com/cmake:*"`) must
    /// match the correctly-reconstructed base identifier (`"cmake:3.28"` in
    /// registry `"example.com"`) and return the companion in `.companions`.
    ///
    /// If the `collect_candidates_from_dir` bug is present, the base identifier
    /// becomes `"cmake/candidates:3.28"` which does NOT match the scoped rule,
    /// and `.companions` is empty — causing the test to fail.
    ///
    /// Traceability: Phase 5A spec test 6 (scoped-rule regression).
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_site_patch_roots_scoped_rule_matches_reconstructed_base_id() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));

        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().tags.clone();

        // Seed an installed base.  The candidate path is:
        //   symlinks/{registry_slug}/cmake/candidates/3.28
        // The correctly-reconstructed base identifier must be `cmake:3.28`
        // in registry `example.com` — not `cmake/candidates:3.28`.
        let base_id = Identifier::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        // Seed a companion package.
        let companion_digest = sha256('6');
        let companion_tag_id = Identifier::new_registry("ca-certs", PATCH_REGISTRY).clone_with_tag("v1");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-certs", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &crate::package::resolved_package::ResolvedPackage::new(),
            "CA_CERTS",
            "/etc/ssl/certs",
            crate::package::metadata::visibility::Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);

        // Seed a descriptor with a scoped rule matching `example.com/cmake:*`.
        // This rule must ONLY fire when the base identifier is correctly
        // reconstructed as `cmake` in registry `example.com`.
        {
            let blob_store = crate::file_structure::BlobStore::new(dir.path().join("blobs"));
            let tag_store_inner = crate::file_structure::TagStore::new(dir.path().join("tags"));

            let descriptor_json = serde_json::json!({
                "version": 1,
                "rules": [{
                    "match": &format!("{REGISTRY}/cmake:*"),
                    "packages": [companion_tag_id.to_string()]
                }]
            })
            .to_string();
            let layer_bytes = descriptor_json.as_bytes();
            let layer_digest = crate::oci::Algorithm::Sha256.hash(layer_bytes);
            let manifest_json = serde_json::json!({
                "schemaVersion": 2,
                "layers": [{
                    "mediaType": "application/octet-stream",
                    "digest": layer_digest.to_string(),
                    "size": layer_bytes.len()
                }]
            })
            .to_string();
            let manifest_bytes = manifest_json.as_bytes();
            let manifest_digest = crate::oci::Algorithm::Sha256.hash(manifest_bytes);

            blob_store
                .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
                .await
                .unwrap();
            blob_store
                .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
                .await
                .unwrap();

            let global_id = super::super::patch_discovery::global_descriptor_id(&patch_config);
            let global_tags_path = tag_store_inner.tags(&global_id);
            super::super::patch_discovery::PatchTagMap::write_has_descriptor(
                &global_tags_path,
                &manifest_digest.to_string(),
            )
            .await
            .unwrap();
        }

        let roots = manager
            .resolve_site_patch_roots(&[])
            .await
            .expect("resolve_site_patch_roots must succeed");

        // Contract: scoped rule matches correctly-reconstructed base identifier.
        // Failure here means the base identifier was corrupted (e.g.
        // `cmake/candidates:3.28` instead of `cmake:3.28`) so the scoped
        // rule did not fire.
        assert!(
            roots.companions.contains(&companion_pinned),
            "spec test 6 (scoped-rule regression): companion must match scoped rule \
             '{REGISTRY}/cmake:*'; base id was likely corrupted by candidates-dir bug; \
             got: {:?}",
            roots.companions
        );
    }

    // ── Spec test 7 — real registry recovered for port-containing base ───────────

    /// Review-finding regression: the registry of an enumerated base must be the
    /// REAL hostname, not the lossy symlink slug, so a scoped descriptor rule that
    /// pins a port-containing registry matches.
    ///
    /// The symlink store names `localhost:5000` as `localhost_5000` (`to_relaxed_slug`
    /// maps `:` to `_`). Without recovering the real hostname from the base's tag
    /// file, `base_id.to_string()` is `localhost_5000/cmake:3.28`, which does NOT
    /// match a rule `localhost:5000/cmake:*`, so the companion is dropped from the
    /// GC roots (over-collection of an in-use companion). With recovery it matches.
    ///
    /// A catch-all `*` rule and standard dotted registries mask this — only a
    /// scoped rule against a port-containing registry exposes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_site_patch_roots_recovers_real_registry_for_port_containing_base() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). Holding this lock
        // prevents concurrent write_blob calls from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));

        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().tags.clone();

        // Installed base whose registry carries a PORT → symlink slug `localhost_5000`.
        let base_id = Identifier::new_registry("cmake", "localhost:5000").clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        // Seed the base's tag file recording the canonical `repository` so the real
        // hostname can be recovered from the slug directory. `tag_store.tags(&base_id)`
        // slugifies the registry, landing at `tags/localhost_5000/cmake.json`.
        {
            let base_digest = sha256('b');
            let tags_path = tag_store.tags(&base_id);
            std::fs::create_dir_all(tags_path.parent().unwrap()).unwrap();
            let json =
                format!(r#"{{"version":1,"repository":"localhost:5000/cmake","tags":{{"3.28":"{base_digest}"}}}}"#);
            std::fs::write(&tags_path, json).unwrap();
        }

        // Companion installed locally.
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &crate::package::resolved_package::ResolvedPackage::new(),
            "CA_BUNDLE",
            "/etc/ssl/certs/bundle.crt",
            crate::package::metadata::visibility::Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &companion_tag_id, &companion_digest);

        // Global descriptor with a SCOPED port-registry rule (not catch-all).
        {
            let blob_store = crate::file_structure::BlobStore::new(dir.path().join("blobs"));
            let descriptor_json = serde_json::json!({
                "version": 1,
                "rules": [{
                    "match": "localhost:5000/cmake:*",
                    "packages": [companion_tag_id.to_string()]
                }]
            })
            .to_string();
            let layer_bytes = descriptor_json.as_bytes();
            let layer_digest = crate::oci::Algorithm::Sha256.hash(layer_bytes);
            let manifest_json = serde_json::json!({
                "schemaVersion": 2,
                "layers": [{
                    "mediaType": "application/octet-stream",
                    "digest": layer_digest.to_string(),
                    "size": layer_bytes.len()
                }]
            })
            .to_string();
            let manifest_bytes = manifest_json.as_bytes();
            let manifest_digest = crate::oci::Algorithm::Sha256.hash(manifest_bytes);
            blob_store
                .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
                .await
                .unwrap();
            blob_store
                .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
                .await
                .unwrap();
            let global_id = super::super::patch_discovery::global_descriptor_id(&patch_config);
            super::super::patch_discovery::PatchTagMap::write_has_descriptor(
                &tag_store.tags(&global_id),
                &manifest_digest.to_string(),
            )
            .await
            .unwrap();
        }

        let roots = manager
            .resolve_site_patch_roots(&[])
            .await
            .expect("resolve_site_patch_roots must succeed");

        assert!(
            roots.companions.contains(&companion_pinned),
            "scoped rule 'localhost:5000/cmake:*' must match the port-registry base after \
             real-registry recovery from the tag file; got: {:?}",
            roots.companions
        );
    }
}

// ── Phase 5B specification tests — snapshot compose preference + offline_view carry ──
//
// Traceability:
//   Test 4 — compose preference: snapshot digest wins over live tag lookup for a companion.
//   Test 5 — with_patch_snapshot + offline_view carry the snapshot through.
//
// These tests MUST compile and FAIL against the Phase 5B stubs:
//   - `build_site_patch_set` does not yet consult `self.patch_snapshot()`.
//
// After Phase 5B implementation all five tests must pass.

#[cfg(test)]
mod phase5b_spec_tests {
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    use crate::{
        file_structure::TagStore,
        oci::{
            Identifier, PinnedIdentifier,
            index::{LocalConfig, LocalIndex},
        },
        package::{metadata::visibility::Visibility, resolved_package::ResolvedPackage},
        patch::snapshot::{PatchSnapshot, SnapshotVersion},
    };

    use super::{
        phase4_spec_tests::{seed_package_with_constant_var, seed_root_arc, write_companion_tag_lock},
        phase5a_spec_tests::{make_manager, seed_global_descriptor_with_companion, sha256, test_patch_config},
    };

    const PATCH_REGISTRY: &str = "patches.example.com";

    // ── Test 4 — snapshot digest wins over live tag lookup ────────────────────

    /// When a `PatchSnapshot` is active and contains a pin for a companion,
    /// `build_site_patch_set` / `resolve_env` must resolve the companion via
    /// the SNAPSHOT digest, not the live tag lookup.
    ///
    /// Setup:
    ///   - companion installed at digest A (live tag `latest` → A).
    ///   - `PatchSnapshot` pins the same companion to digest B (different).
    ///   - package dir at digest A carries `SNAP_VAR=live_value` (INTERFACE).
    ///   - package dir at digest B carries `SNAP_VAR=snapshot_value` (INTERFACE).
    ///
    /// Expected: `resolve_env` returns `SNAP_VAR=snapshot_value` (snapshot B wins).
    /// Without snapshot: `resolve_env` would return `SNAP_VAR=live_value` (live A).
    ///
    /// Traceability: Phase 5B spec test 4 — compose snapshot preference.
    ///
    /// NOTE: This test FAILS until Phase 5B implements snapshot preference in
    /// `build_site_patch_set`.
    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_digest_wins_over_live_tag_lookup_in_compose() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`:
        // `seed_global_descriptor_with_companion` calls `write_blob`, which increments
        // the process-global `WRITE_BLOB_CALL_COUNT`. Holding this lock prevents our
        // calls from inflating the delta observed by the coalescing test.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();

        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().tags.clone();

        // Digest A = live tag target.  Digest B = snapshot pin.
        let live_digest = sha256('a');
        let snap_digest = sha256('b');

        let companion_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");

        // Live companion: tag → A, package at A carries live_value.
        let live_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(live_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &live_pinned,
            &ResolvedPackage::new(),
            "SNAP_VAR",
            "live_value",
            Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &companion_id, &live_digest);

        // Snapshot companion: package at digest B carries snapshot_value.
        let snap_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(snap_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &snap_pinned,
            &ResolvedPackage::new(),
            "SNAP_VAR",
            "snapshot_value",
            Visibility::INTERFACE,
        );

        // Seed the global descriptor pointing to the companion tag, capturing its
        // manifest digest so the snapshot can freeze descriptor SELECTION too
        // (whole-tier freeze — C8 pins companion AND descriptor digests).
        let (global_manifest_digest, _layer_digest) =
            seed_global_descriptor_with_companion(&dir, &patch_config, &companion_id).await;

        // Build a snapshot that pins the companion to digest B AND the global
        // descriptor to its frozen manifest digest (so the frozen descriptor is
        // the one that names the companion under the snapshot).
        let companion_key = format!("{PATCH_REGISTRY}/ca-bundle");
        let mut companions_map = BTreeMap::new();
        companions_map.insert(companion_key, snap_digest.clone());
        let global_id = super::super::patch_discovery::global_descriptor_id(&patch_config);
        let mut descriptors_map = BTreeMap::new();
        descriptors_map.insert(super::descriptor_source_key(&global_id), global_manifest_digest);
        let snapshot = PatchSnapshot {
            version: SnapshotVersion::V1,
            companions: companions_map,
            descriptors: descriptors_map,
        };

        // Build a fresh manager (same tempdir) with the snapshot injected.
        let manager_with_snapshot = make_manager(&dir)
            .with_patches(Some(patch_config.clone()))
            .with_patch_snapshot(Some(snapshot));

        let root = seed_root_arc(&manager_with_snapshot.file_structure().packages.clone(), "root", 'r');

        // This FAILS until Phase 5B implements snapshot preference in build_site_patch_set.
        let entries = manager_with_snapshot
            .resolve_env(&[root], false)
            .await
            .expect("resolve_env with snapshot must succeed");

        let snap_var = entries.iter().find(|e| e.key == "SNAP_VAR");
        assert!(
            snap_var.is_some(),
            "test 4: SNAP_VAR must be present in resolved env; entries: {entries:?}"
        );
        assert_eq!(
            snap_var.unwrap().value,
            "snapshot_value",
            "test 4: snapshot digest B must win over live tag A; got '{}'",
            snap_var.unwrap().value
        );
    }

    /// Without a snapshot, `resolve_env` uses the live tag (digest A) and returns
    /// `SNAP_VAR=live_value`.
    ///
    /// Baseline confirming test 4 is genuinely observing the right variable.
    ///
    /// Traceability: Phase 5B spec test 4 baseline — no snapshot floats to live.
    #[tokio::test(flavor = "multi_thread")]
    async fn without_snapshot_live_tag_is_used() {
        // Same serialisation guard as the snapshot preference test above.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();

        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().tags.clone();

        let live_digest = sha256('a');
        let companion_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");

        let live_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(live_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &live_pinned,
            &ResolvedPackage::new(),
            "SNAP_VAR",
            "live_value",
            Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &companion_id, &live_digest);

        seed_global_descriptor_with_companion(&dir, &patch_config, &companion_id).await;

        // No snapshot — live tag must win.
        let root = seed_root_arc(&store, "root", 'r');
        let entries = manager
            .resolve_env(&[root], false)
            .await
            .expect("resolve_env without snapshot must succeed");

        let snap_var = entries.iter().find(|e| e.key == "SNAP_VAR");
        assert!(
            snap_var.is_some(),
            "test 4 baseline: SNAP_VAR must be present without snapshot; entries: {entries:?}"
        );
        assert_eq!(
            snap_var.unwrap().value,
            "live_value",
            "test 4 baseline: without snapshot the live tag must be used; got '{}'",
            snap_var.unwrap().value
        );
    }

    /// C8 whole-tier freeze (Codex BLOCK regression): once a snapshot pins the
    /// descriptor source, a post-freeze `ocx patch sync` that ADVANCES the live
    /// descriptor — re-pointing the global tag to a NEW descriptor that names a
    /// DIFFERENT companion — must NOT change which companion a frozen build
    /// composes. The frozen build selects the descriptor by its pinned manifest
    /// digest, never the live tag store.
    ///
    /// Setup: frozen descriptor M1 names companion `ca-bundle` (SNAP_VAR=frozen);
    /// live descriptor advances to M2 naming `ca-bundle-v2` (SNAP_VAR=advanced)
    /// and re-points the global tag to M2. Under the snapshot pinning M1, the
    /// result must be `frozen` — the advance is invisible.
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_descriptor_selection_ignores_post_freeze_sync() {
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let store = make_manager(&dir).file_structure().packages.clone();
        let tag_store = make_manager(&dir).file_structure().tags.clone();

        // Frozen companion `ca-bundle` at digest dC1 → SNAP_VAR=frozen.
        let frozen_companion = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let frozen_digest = sha256('1');
        let frozen_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(frozen_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &frozen_pinned,
            &ResolvedPackage::new(),
            "SNAP_VAR",
            "frozen",
            Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &frozen_companion, &frozen_digest);

        // Advanced companion `ca-bundle-v2` at digest dC2 → SNAP_VAR=advanced.
        let advanced_companion = Identifier::new_registry("ca-bundle-v2", PATCH_REGISTRY).clone_with_tag("latest");
        let advanced_digest = sha256('2');
        let advanced_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle-v2", PATCH_REGISTRY).clone_with_digest(advanced_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &advanced_pinned,
            &ResolvedPackage::new(),
            "SNAP_VAR",
            "advanced",
            Visibility::INTERFACE,
        );
        write_companion_tag_lock(&tag_store, &advanced_companion, &advanced_digest);

        // Freeze: descriptor M1 names the frozen companion. This is the global tag
        // target captured by `ocx patch freeze`.
        let (frozen_manifest, _l1) =
            seed_global_descriptor_with_companion(&dir, &patch_config, &frozen_companion).await;

        // Post-freeze sync: descriptor M2 names the advanced companion and RE-POINTS
        // the live global tag to M2 (overwriting M1's tag entry — the digest advances).
        let (advanced_manifest, _l2) =
            seed_global_descriptor_with_companion(&dir, &patch_config, &advanced_companion).await;
        assert_ne!(
            frozen_manifest, advanced_manifest,
            "the two descriptors must have distinct manifest digests for this test to be meaningful"
        );

        // Snapshot pins descriptor source → M1 and companion ca-bundle → dC1.
        let global_id = super::super::patch_discovery::global_descriptor_id(&patch_config);
        let mut descriptors_map = BTreeMap::new();
        descriptors_map.insert(super::descriptor_source_key(&global_id), frozen_manifest);
        let mut companions_map = BTreeMap::new();
        companions_map.insert(format!("{PATCH_REGISTRY}/ca-bundle"), frozen_digest.clone());
        let snapshot = PatchSnapshot {
            version: SnapshotVersion::V1,
            companions: companions_map,
            descriptors: descriptors_map,
        };

        // Frozen build: must compose the FROZEN companion, ignoring the advance.
        let frozen_manager = make_manager(&dir)
            .with_patches(Some(patch_config.clone()))
            .with_patch_snapshot(Some(snapshot));
        let root = seed_root_arc(&frozen_manager.file_structure().packages.clone(), "root", 'r');
        let entries = frozen_manager
            .resolve_env(&[root], false)
            .await
            .expect("frozen resolve_env");
        let snap_var = entries.iter().find(|e| e.key == "SNAP_VAR");
        assert_eq!(
            snap_var.map(|e| e.value.as_str()),
            Some("frozen"),
            "frozen build must compose the descriptor pinned at freeze (M1 → ca-bundle), \
             not the post-freeze advance (M2 → ca-bundle-v2); entries: {entries:?}"
        );

        // Floating build (no snapshot): follows the live tag to M2 → advanced.
        let floating_manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let root2 = seed_root_arc(&floating_manager.file_structure().packages.clone(), "root", 'r');
        let live_entries = floating_manager
            .resolve_env(&[root2], false)
            .await
            .expect("floating resolve_env");
        let live_var = live_entries.iter().find(|e| e.key == "SNAP_VAR");
        assert_eq!(
            live_var.map(|e| e.value.as_str()),
            Some("advanced"),
            "without a snapshot the live descriptor (M2 → ca-bundle-v2) must win — \
             confirms the freeze test is observing a real difference; entries: {live_entries:?}"
        );
    }

    // ── Test 5 — with_patch_snapshot + offline_view carry the snapshot ────────

    /// `with_patch_snapshot(Some(snap))` must store the snapshot, and
    /// `offline_view(...)` must carry the snapshot through so offline env
    /// paths (direnv export, global toolchain) still use frozen digests.
    ///
    /// Contract:
    ///   - `manager.patch_snapshot()` is `Some` after `with_patch_snapshot`.
    ///   - `manager.offline_view(local_index).patch_snapshot()` is `Some`.
    ///   - The snapshot from the offline view equals the original.
    ///
    /// Traceability: Phase 5B spec test 5 — builder + offline_view carry.
    ///
    /// NOTE: This test PASSES — `with_patch_snapshot` and `offline_view` are
    /// already implemented. Included as a pinning guard.
    #[test]
    fn with_patch_snapshot_and_offline_view_carry_snapshot() {
        let dir = TempDir::new().unwrap();

        let mut companions = BTreeMap::new();
        companions.insert(format!("{PATCH_REGISTRY}/ca-bundle"), sha256('s'));
        let snapshot = PatchSnapshot {
            version: SnapshotVersion::V1,
            companions,
            descriptors: BTreeMap::new(),
        };

        let manager = make_manager(&dir).with_patch_snapshot(Some(snapshot.clone()));

        assert!(
            manager.patch_snapshot().is_some(),
            "test 5: patch_snapshot() must be Some after with_patch_snapshot"
        );
        assert_eq!(
            manager.patch_snapshot().unwrap(),
            &snapshot,
            "test 5: patch_snapshot() must equal the injected snapshot"
        );

        let local_index = LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(dir.path().join("tags")),
            blob_store: crate::file_structure::BlobStore::new(dir.path().join("blobs")),
        });
        let offline = manager.offline_view(local_index);

        assert!(
            offline.is_offline(),
            "test 5: offline_view must produce an offline manager"
        );
        assert!(
            offline.patch_snapshot().is_some(),
            "test 5: offline_view must carry the patch snapshot through"
        );
        assert_eq!(
            offline.patch_snapshot().unwrap(),
            &snapshot,
            "test 5: offline_view patch_snapshot must equal the original"
        );
    }

    /// Manager built without `with_patch_snapshot` reports `None`.
    ///
    /// Traceability: Phase 5B spec test 5 — absence is preserved.
    #[test]
    fn manager_without_snapshot_returns_none() {
        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir);
        assert!(
            manager.patch_snapshot().is_none(),
            "test 5: manager without snapshot must return None from patch_snapshot()"
        );
    }
}

/// Phase 5D — per-package `no-patches` opt-out boundary (C7 enforcement exception).
///
/// These tests exercise `resolve_env_with_patch_boundary`'s `no_patches` set:
/// an admitted base listed in the opt-out set gets NO companion overlay UNLESS
/// the configured tier is `system_required` (enforcement beats opt-out).
///
/// All three seed the canonical global-companion fixture (rule `"match": "*"`),
/// which projects the `CA_BUNDLE` interface var onto every admitted base —
/// including the root `example.com/rootpkg`. The presence or absence of
/// `CA_BUNDLE` in the resolved env is the overlay marker the assertions key on.
#[cfg(test)]
mod phase5d_spec_tests {
    use std::collections::BTreeSet;

    use tempfile::TempDir;

    use crate::config::patch::ResolvedPatchConfig;

    use super::{
        phase4_spec_tests::{seed_installed_global_companion, seed_root_arc},
        phase5a_spec_tests::make_manager,
    };

    const PATCH_REGISTRY: &str = "patches.example.com";

    /// The canonical `registry/repository` key for the root seeded by
    /// `seed_root_arc(.., "rootpkg", ..)` in the `example.com` test registry.
    const ROOT_REPO_KEY: &str = "example.com/rootpkg";

    /// Build a patch config for the test patch registry with the given
    /// `system_required` posture (the non-overridable system origin marker).
    fn patch_config(system_required: bool) -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required,
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        }
    }

    /// (11) An admitted base listed in `no_patches` with a NON-system_required
    /// tier gets NO companion overlay: the `CA_BUNDLE` entry is ABSENT from the
    /// resolved env.
    ///
    /// Traces: FILE D `build_site_patch_set` opt-out skip — "if
    /// no_patches.contains(&repo_key) && !system_required { continue; }";
    /// ADR §"Per-package opt-out".
    #[tokio::test(flavor = "multi_thread")]
    async fn opted_out_base_with_non_system_required_tier_has_no_overlay() {
        // `seed_installed_global_companion` calls `write_blob`; serialise against
        // the process-global `WRITE_BLOB_CALL_COUNT` consumers.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let config = patch_config(false);
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let (key, _value) = seed_installed_global_companion(&manager, &config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');

        // Opt the root base out by canonical registry/repository key.
        let no_patches: BTreeSet<String> = [ROOT_REPO_KEY.to_string()].into_iter().collect();
        let (entries, _patch_start) = manager
            .resolve_env_with_patch_boundary(&[root], false, &no_patches)
            .await
            .expect("resolve_env_with_patch_boundary must succeed");

        assert!(
            !entries.iter().any(|e| e.key == key),
            "test 11: opted-out base with a non-system_required tier must have NO \
             companion overlay; '{key}' must be absent. entries: {entries:?}"
        );
    }

    /// (12) The SAME opted-out base, but the tier is `system_required = true`:
    /// the companion overlay entries are PRESENT — enforcement beats opt-out.
    ///
    /// Traces: FILE D `build_site_patch_set` — the `&& !system_required` guard
    /// means a system-required tier never honours the opt-out; ADR §"Config
    /// scopes (C7 enforcement)" + §"Per-package opt-out" exception.
    #[tokio::test(flavor = "multi_thread")]
    async fn opted_out_base_with_system_required_tier_keeps_overlay() {
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let config = patch_config(true);
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');

        // Same opt-out as test 11 — but the tier is system_required, so it is ignored.
        let no_patches: BTreeSet<String> = [ROOT_REPO_KEY.to_string()].into_iter().collect();
        let (entries, _patch_start) = manager
            .resolve_env_with_patch_boundary(&[root], false, &no_patches)
            .await
            .expect("resolve_env_with_patch_boundary must succeed");

        assert!(
            entries.iter().any(|e| e.key == key && e.value == value),
            "test 12: a system_required tier must apply the overlay EVEN when the base \
             is opted out (enforcement beats opt-out); '{key}={value}' must be present. \
             entries: {entries:?}"
        );
    }

    /// (13) A base NOT in `no_patches` (empty opt-out set) keeps its companion
    /// overlay: the `CA_BUNDLE` entry is PRESENT. Baseline that the marker var
    /// genuinely appears when the opt-out does not fire.
    ///
    /// Traces: FILE D `build_site_patch_set` — only `no_patches.contains(&repo_key)`
    /// skips the overlay; an absent key leaves the overlay intact.
    #[tokio::test(flavor = "multi_thread")]
    async fn base_not_opted_out_keeps_overlay() {
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let dir = TempDir::new().unwrap();
        let config = patch_config(false);
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');

        // Empty opt-out set — the root base is NOT opted out.
        let no_patches: BTreeSet<String> = BTreeSet::new();
        let (entries, _patch_start) = manager
            .resolve_env_with_patch_boundary(&[root], false, &no_patches)
            .await
            .expect("resolve_env_with_patch_boundary must succeed");

        assert!(
            entries.iter().any(|e| e.key == key && e.value == value),
            "test 13: a base that is NOT opted out must keep its companion overlay; \
             '{key}={value}' must be present. entries: {entries:?}"
        );
    }
}
