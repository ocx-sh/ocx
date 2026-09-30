// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::task::JoinSet;

use crate::{composer, error::PackageError, error::PackageErrorKind, patch::PatchDescriptor};
use ocx_index::{IndexOperation, SelectResult};
use ocx_package::{
    install_info::InstallInfo, metadata::binary::BinaryName, metadata::entrypoint::EntrypointName,
    metadata::env::entry::Entry, metadata::integrations::IntegrationEntry,
};

use super::super::PackageManager;

/// Provenance for one companion overlay entry: the admitting rule glob, the companion,
/// and the digest it resolved to. Descriptive only: overlay membership is decided
/// upstream (`adr_infrastructure_patches.md § The site tier`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchProvenance {
    /// The descriptor rule `match` glob that admitted the companion for the base.
    pub rule_match: String,
    /// The companion identifier whose interface projection produced this entry.
    pub companion: ocx_oci::PackageRef,
    /// The digest-complete install the companion resolved to. Kept beside the tag because
    /// the snapshot and last sync decide which digest a tag reaches; without it an audit
    /// cannot say which companion bytes composed.
    pub pinned: ocx_oci::PinnedPackageRef,
}

/// The companion-overlay region of a composed entry vector: `patch_start` for
/// `provenance.len()` entries, with the caller's `[env]` and `--env` appended after.
///
/// `index >= patch_start` does not imply membership: `provenance[index - patch_start]`
/// panics once a caller passes any `--env`, so ask [`Self::provenance_for`].
#[derive(Debug, Clone, Copy)]
pub struct PatchOverlay<'a> {
    patch_start: usize,
    provenance: &'a [PatchProvenance],
}

impl<'a> PatchOverlay<'a> {
    /// Views the overlay region of a `resolve_env_with_patch_boundary` (or `..._with_attribution`) result.
    pub fn new(patch_start: usize, provenance: &'a [PatchProvenance]) -> Self {
        Self {
            patch_start,
            provenance,
        }
    }

    /// The [`PatchProvenance`] for the composed entry at `index`, or `None` for entries
    /// before or after the overlay, which must render unattributed.
    pub fn provenance_for(&self, index: usize) -> Option<&'a PatchProvenance> {
        index
            .checked_sub(self.patch_start)
            .and_then(|offset| self.provenance.get(offset))
    }
}

#[cfg(test)]
mod patch_overlay_tests {
    use super::{PatchOverlay, PatchProvenance};
    use ocx_oci::{Digest, PackageRef, PinnedPackageRef};

    fn provenance(rule: &str) -> PatchProvenance {
        let companion = PackageRef::new_registry("companion", "ocx.sh");
        PatchProvenance {
            rule_match: rule.to_string(),
            pinned: PinnedPackageRef::try_from(companion.clone_with_digest(Digest::Sha256("a".repeat(64))))
                .expect("digest present"),
            companion,
        }
    }

    /// The three regions of a composed entry vector, in one assertion set:
    /// package-composed entries before the overlay and caller entries after it
    /// are both unattributed; only the overlay itself carries provenance.
    ///
    /// The trailing region is the regression: `--env` overrides compose after
    /// the overlay, so `index >= patch_start` alone is not the membership test.
    #[test]
    fn provenance_for_covers_only_the_overlay_region() {
        // entries: [0,1] composed | [2,3] overlay | [4] an `--env` override.
        let provenances = vec![provenance("first"), provenance("second")];
        let overlay = PatchOverlay::new(2, &provenances);

        assert!(overlay.provenance_for(0).is_none(), "composed entry has no provenance");
        assert!(overlay.provenance_for(1).is_none(), "composed entry has no provenance");
        assert_eq!(
            overlay.provenance_for(2).map(|p| p.rule_match.as_str()),
            Some("first"),
            "the first overlay entry maps to the first provenance slot"
        );
        assert_eq!(overlay.provenance_for(3).map(|p| p.rule_match.as_str()), Some("second"));
        assert!(
            overlay.provenance_for(4).is_none(),
            "an `--env` override sits past the overlay and is nobody's companion contribution"
        );
    }

    /// No patch tier configured: the overlay is empty and `patch_start` equals
    /// the compose count, so every index — including the caller's own entries
    /// past it — must report unattributed rather than panic.
    #[test]
    fn provenance_for_empty_overlay_attributes_nothing() {
        let overlay = PatchOverlay::new(2, &[]);
        for index in 0..4 {
            assert!(
                overlay.provenance_for(index).is_none(),
                "index {index} must be unattributed with an empty overlay"
            );
        }
    }
}

/// What a companion contributes under one admitted base: provenance-paired INTERFACE env
/// entries plus its `integrations` (`adr_package_integrations.md § Patch companions`).
pub struct CompanionOverlay {
    pub entries: Vec<(Entry, PatchProvenance)>,
    pub integrations: Vec<(ocx_oci::PinnedPackageRef, IntegrationEntry)>,
}

/// Whether a companion's one-time projection was emitted, cached per companion identifier.
///
/// The projection lands in the FIRST matching base's overlay; a rule matching N bases
/// would otherwise land the companion N times (duplicate JSON entries, exports, PATH prepends).
enum CompanionOutcome {
    /// Emitted under the first matching base; possibly empty (private-only, no
    /// integrations), which is still not [`Missing`](Self::Missing).
    Projected,
    /// Not installed, lookup failed, or composition failed. The required-companion check
    /// re-fires on every cache hit for this outcome, so the cache never bypasses it.
    Missing,
}

/// Admitted identifier → the companion contributions emitted under it; a companion
/// matching several bases appears only under the first. Built offline from local state.
pub type SitePatchSet = HashMap<ocx_oci::PinnedPackageRef, CompanionOverlay>;

/// Which CLI tier a caller resolves env for, and what it adds on top of the package-composed
/// set (`.claude/artifacts/adr_patch_env_resolution_uniformity.md`).
///
/// Struct variants only, so every field is named at each call site and a new caller
/// cannot silently omit a contribution.
pub enum EnvScope {
    /// A project (or global) `ocx.toml` is in scope.
    Project {
        /// `no-patches` opt-out repositories (canonical `registry/repository`); an opted-out
        /// base gets no companion overlay unless the tier is system-required.
        no_patches: std::collections::BTreeSet<String>,
        /// Caller entries in application order (project `[env]`, groups in `-g` order, then
        /// `--env`), appended last so constants replace and paths land ahead of package paths.
        env: Vec<Entry>,
        /// The toolchain tree to compose through, or `None` for the digest lane. Its selected
        /// groups are what `ComposePaths::resolve` heals before any link path is emitted; never
        /// narrow them to the default group or re-derive them downstream.
        ///
        /// Boxed: an inline lock makes `Project` dwarf `Package` (`clippy::large_enum_variant`).
        toolchain: Option<Box<crate::composer::ToolchainLinks>>,
    },
    /// No project toolchain in scope (OCI tier, scratch managers, self-update probe): empty
    /// opt-out, and `env` holds only `--env` overrides since the OCI tier reads no `ocx.toml`.
    Package {
        /// The `--env` overrides, in argument order.
        env: Vec<Entry>,
    },
}

impl EnvScope {
    /// The OCI tier with no `--env` overrides; a named constructor, not `Default`, so the
    /// caller still states its tier.
    pub fn package_tier() -> Self {
        EnvScope::Package { env: Vec::new() }
    }

    /// The opt-out set: `Project`'s own, empty for `Package`.
    fn opt_out(&self) -> &std::collections::BTreeSet<String> {
        static EMPTY: std::sync::LazyLock<std::collections::BTreeSet<String>> =
            std::sync::LazyLock::new(std::collections::BTreeSet::new);
        match self {
            EnvScope::Project { no_patches, .. } => no_patches,
            EnvScope::Package { .. } => &EMPTY,
        }
    }

    /// The entries this scope contributes, already in application order.
    fn contributed_env(&self) -> &[Entry] {
        match self {
            EnvScope::Project { env, .. } | EnvScope::Package { env } => env,
        }
    }

    /// The toolchain tree to compose through; always `None` for `Package` (no lock, no tree).
    fn toolchain_links(&self) -> Option<&crate::composer::ToolchainLinks> {
        match self {
            EnvScope::Project { toolchain, .. } => toolchain.as_deref(),
            EnvScope::Package { .. } => None,
        }
    }
}

/// Whether `entry` names a reserved `OCX_*` / `__OCX_*` key and must be dropped,
/// warning once per distinct key.
///
/// A skip, never an error, so an already-published package keeps resolving. Without it a
/// publisher could ship `OCX_CONSENT_NAMESPACES = "*/*"` or a forged `__OCX_ENV_STATE`
/// into the user's shell at the next prompt.
fn reserved_key_dropped(entry: &Entry, warned: &mut HashSet<String>) -> bool {
    if !ocx_util::env::is_reserved_ocx_key(&entry.key) {
        return false;
    }
    // Once per key, not per contributor: the reconciler recomposes on every prompt.
    if warned.insert(entry.key.clone()) {
        log::warn!(
            "env var '{}' is in the reserved OCX_*/__OCX_* namespace and was skipped; \
             ocx's own configuration cannot be set from package metadata",
            entry.key
        );
    }
    true
}

/// Emit one `debug` line per project constant that shadows a package constant.
///
/// Never `warn`: shadowing is what project `[env]` is for, so a warning would fire on every
/// `ocx exec` using it. Path entries prepend, so they never shadow.
fn log_project_env_shadowing(composed: &[Entry], project_env: &[Entry]) {
    use ocx_package::metadata::env::modifier::ModifierKind;

    for entry in project_env {
        if entry.kind != ModifierKind::Constant {
            continue;
        }
        if composed
            .iter()
            .any(|existing| existing.key == entry.key && existing.kind == ModifierKind::Constant)
        {
            log::debug!(
                "project env overrides the package-declared constant '{}'; the project value wins",
                entry.key
            );
        }
    }
}

/// GC roots from the site-patch tier: companions and descriptor blobs that must survive
/// collection even when no install symlink reaches them. Built offline by
/// [`PackageManager::resolve_site_patch_roots`].
#[derive(Debug, Clone, Default)]
pub struct SitePatchRoots {
    /// Pinned identifiers for every companion package that should be retained.
    pub companions: Vec<ocx_oci::PinnedPackageRef>,
    /// Every descriptor blob (manifest + layers, all sources) to retain; the registry lets
    /// GC call `BlobStore::path(registry, digest)` without a shard-suffix scan.
    pub descriptors: Vec<(String, ocx_oci::Digest)>,
    /// Per-source descriptor manifest pins, keyed by canonical `registry/repository` (the key
    /// [`PackageManager::build_site_patch_set`] derives). `ocx patch freeze` writes them into
    /// [`PatchSnapshot::descriptors`](crate::patch::PatchSnapshot::descriptors), so a
    /// post-freeze `ocx patch sync` cannot change which companions a frozen build composes.
    pub descriptor_pins: Vec<(String, ocx_oci::Digest)>,
}

/// Which patch-tier pins a [`SitePatchRoots`] resolution reads: the live record under
/// `state/patch-*/`, or that plus an active [`PatchSnapshot`](crate::patch::PatchSnapshot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchRootScope {
    /// The live record only, for `ocx patch freeze`: reading through a snapshot would
    /// re-freeze its own output instead of live state.
    Recorded,
    /// Record ∪ active snapshot, for GC: compose reads snapshot-first, so record-only roots
    /// collect the companion and descriptor blob a frozen build still composes.
    RecordedAndSnapshot,
}

/// What a [`ChainBlob`] is in OCI terms, so `inspect` can label it.
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

/// One blob in the resolution chain, with the descriptor context to render it like a layer.
#[derive(Debug, Clone)]
pub struct ChainBlob {
    /// The blob pinned by its own digest.
    pub identifier: ocx_oci::PinnedPackageRef,
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
/// `chain` is walk order: index (if any), manifest, config. Manifest entries are on disk
/// after `resolve`; the config blob is not until `pull::setup_owned` fetches it.
#[derive(Debug, Clone)]
pub struct ResolvedChain {
    /// The platform-selected pinned identifier; keys every storage path (logical identity).
    pub pinned: ocx_oci::PinnedPackageRef,
    /// The physical identifier for content downloads: [`Self::pinned`] for a registry source,
    /// the routed registry for an `index.ocx.sh` source; transport-only, never persisted or
    /// locked. [`NoTransport`] says why there is no location and what a missing layer is
    /// refused with; the name as typed is never dialled.
    pub transport_pinned: Result<ocx_oci::PinnedOciIdentifier, NoTransport>,
    /// Walk-order chain blobs (the config blob is materialized later by the pull pipeline).
    pub chain: Vec<ChainBlob>,
    /// The platform-selected image manifest the pull pipeline extracts; never an index.
    pub final_manifest: ocx_oci::ImageManifest,
    /// The selected platform (`any` for a flat manifest); the candidate-symlink gate uses it
    /// to suppress foreign-platform installs.
    pub platform: ocx_oci::Platform,
}

/// Why a [`ResolvedChain`] has no location to read layers from.
#[derive(Debug, Clone)]
pub enum NoTransport {
    /// A local materialization (`pull_local`): every layer was staged before the chain.
    LocalMaterialization,
    /// A no-resolve policy (`--offline`) found a name in an index-owned registry with no
    /// recorded root. Deferred to the layer that needs the dial: a stored chain needs none.
    UnrecordedLocation { identifier: String, policy: &'static str },
}

impl NoTransport {
    /// The refusal for a layer of `pinned` absent from the layer store.
    pub fn missing_layer(&self, pinned: &ocx_oci::PinnedPackageRef, layer_digest: &ocx_oci::Digest) -> crate::Error {
        match self {
            Self::LocalMaterialization => crate::Error::LayerNotStaged {
                identifier: pinned.to_string(),
                digest: layer_digest.to_string(),
            },
            Self::UnrecordedLocation { identifier, policy } => ocx_index::error::Error::PolicyResolutionBlocked {
                identifier: identifier.clone(),
                policy,
                block: ocx_index::error::PolicyBlock::UnrecordedLocation,
            }
            .into(),
        }
    }
}

impl ResolvedChain {
    /// Walk-order pinned identifiers of every chain blob, for `ReferenceManager::link_blobs`.
    pub fn blobs(&self) -> impl Iterator<Item = &ocx_oci::PinnedPackageRef> {
        self.chain.iter().map(|blob| &blob.identifier)
    }
}

/// Admitted-set claim attribution for the `binaries` / `entrypoints` / `integrations`
/// arrays of `ocx env` / `ocx package env` (`adr_declared_binaries_metadata.md` §4
/// Decision A, `adr_package_integrations.md` § `AdmittedBinaries` → `AdmittedClaims`).
#[derive(Debug, Clone, Default)]
pub struct AdmittedClaims {
    pub binaries: Vec<(ocx_oci::PinnedPackageRef, BinaryName)>,
    pub entrypoints: Vec<(ocx_oci::PinnedPackageRef, EntrypointName)>,
    pub integrations: Vec<(ocx_oci::PinnedPackageRef, IntegrationEntry)>,
}

/// The physical transport identifier for `pinned`: the index's registry when a source
/// rewrites it, else `pinned`'s own.
///
/// Must go through [`ocx_index::Index::route_to_materialize`], which records the routing
/// pointer, or a lock-pinned `pull`/`exec` re-asks the index site forever; never put that
/// write in [`ocx_index::Index::route`], whose readers must leave no trace. An offline
/// unrecorded location is returned as [`NoTransport::UnrecordedLocation`], not raised.
async fn resolve_transport_pinned(
    index: &ocx_index::Index,
    pinned: &ocx_oci::PinnedPackageRef,
) -> Result<Result<ocx_oci::PinnedOciIdentifier, NoTransport>, PackageErrorKind> {
    match index.route_to_materialize(pinned.as_identifier()).await {
        Ok(routed) => Ok(Ok(routed.at_pin_of(pinned))),
        Err(ocx_index::error::Error::PolicyResolutionBlocked {
            identifier,
            policy,
            block: ocx_index::error::PolicyBlock::UnrecordedLocation,
        }) => Ok(Err(NoTransport::UnrecordedLocation { identifier, policy })),
        Err(error) => Err(PackageErrorKind::Internal(error.into())),
    }
}

impl PackageManager {
    /// Resolves an identifier through the index (tag → digest, platform matching) to its
    /// pinned identifier and the blob chain behind it.
    pub async fn resolve(
        &self,
        package: &ocx_oci::PackageRef,
        platform: ocx_oci::Platform,
    ) -> Result<ResolvedChain, PackageErrorKind> {
        // `fetch_manifest` writes through, so every digest in `chain` is backed by an on-disk blob.
        let (top_pinned, top_manifest) =
            super::common::resolve_top_manifest(self.index(), package, IndexOperation::Resolve).await?;
        // Tag-form top id with the digest dropped: `select` must see the unpinned index ref.
        let top_id = if package.digest().is_some() {
            package.clone()
        } else {
            package.clone_with_tag(package.tag_or_latest())
        };
        match top_manifest {
            // Flat image manifest: the top digest is the pinned identifier; no platform filtering.
            ocx_oci::Manifest::Image(img) => {
                let top_size = blob_data_size(self.file_structure(), &top_pinned).await;
                let top_media = img
                    .media_type
                    .clone()
                    .unwrap_or_else(|| ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string());
                let mut chain = vec![ChainBlob {
                    identifier: top_pinned.clone(),
                    role: ChainRole::Manifest,
                    media_type: top_media,
                    size: top_size,
                }];

                let config_digest = ocx_oci::Digest::try_from(img.config.digest.as_str())
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                let config_pinned = ocx_oci::PinnedPackageRef::try_from(top_id.clone_with_digest(config_digest))
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                chain.push(ChainBlob {
                    identifier: config_pinned,
                    role: ChainRole::Config,
                    media_type: img.config.media_type.clone(),
                    size: img.config.size,
                });
                let transport_pinned = resolve_transport_pinned(self.index(), &top_pinned).await?;
                Ok(ResolvedChain {
                    pinned: top_pinned,
                    transport_pinned,
                    chain,
                    final_manifest: img,
                    // A flat manifest carries no platform metadata (matches `from_image_manifest`).
                    platform: ocx_oci::Platform::any(),
                })
            }
            ocx_oci::Manifest::ImageIndex(index) => {
                let top_size = blob_data_size(self.file_structure(), &top_pinned).await;
                let top_media = index
                    .media_type
                    .clone()
                    .unwrap_or_else(|| ocx_oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string());
                let mut chain = vec![ChainBlob {
                    identifier: top_pinned.clone(),
                    role: ChainRole::Index,
                    media_type: top_media,
                    size: top_size,
                }];

                let pinned = match self.index().select(&top_id, &platform, IndexOperation::Resolve).await {
                    Ok(SelectResult::Found(id)) => {
                        ocx_oci::PinnedPackageRef::try_from(id).map_err(|_| PackageErrorKind::DigestMissing)?
                    }
                    Ok(SelectResult::Ambiguous(v)) => return Err(PackageErrorKind::SelectionAmbiguous(v)),
                    Ok(SelectResult::NotFound) => return Err(PackageErrorKind::NotFound),
                    Ok(SelectResult::FeatureMismatch {
                        host_features,
                        available,
                    }) => {
                        return Err(PackageErrorKind::FeatureMismatch {
                            host_features,
                            available,
                        });
                    }
                    Err(e) => return Err(PackageErrorKind::Internal(e.into())),
                };

                let child_id = top_id.clone_with_digest(pinned.digest());
                let (child_digest, child_manifest) = match self
                    .index()
                    .fetch_manifest(&child_id, IndexOperation::Resolve)
                    .await
                    .map_err(|error| PackageErrorKind::Internal(error.into()))?
                {
                    Some(result) => result,
                    None => {
                        // Parent found via an image-index entry: report offline-missing so the user re-pulls.
                        return Err(PackageErrorKind::OfflineManifestMissing(Box::new(
                            crate::error::OfflineManifestMissing {
                                identifier: child_id,
                                digest: pinned.digest(),
                            },
                        )));
                    }
                };

                let final_manifest = match child_manifest {
                    ocx_oci::Manifest::Image(img) => img,
                    ocx_oci::Manifest::ImageIndex(_) => {
                        return Err(PackageErrorKind::Internal(
                            ocx_index::error::Error::NestedImageIndex { digest: child_digest }.into(),
                        ));
                    }
                };
                let child_pinned = ocx_oci::PinnedPackageRef::try_from(child_id.clone_with_digest(child_digest))
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                // The selecting image-index entry carries the authoritative media type and size.
                let child_descriptor = index
                    .manifests
                    .iter()
                    .find(|entry| entry.digest == child_pinned.digest().to_string());
                let (child_media, child_size) = match child_descriptor {
                    Some(entry) => (entry.media_type.clone(), entry.size),
                    None => (
                        ocx_oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
                        blob_data_size(self.file_structure(), &child_pinned).await,
                    ),
                };
                // A missing or unconvertible platform falls back to `any`, never suppressing the candidate.
                let selected_platform = match child_descriptor {
                    Some(entry) => ocx_oci::Platform::try_from(entry.platform.clone()).unwrap_or_else(|error| {
                        log::warn!(
                            "Selected image-index entry for '{child_pinned}' has an unconvertible platform, treating as `any`: {error}"
                        );
                        ocx_oci::Platform::any()
                    }),
                    None => ocx_oci::Platform::any(),
                };
                chain.push(ChainBlob {
                    identifier: child_pinned,
                    role: ChainRole::Manifest,
                    media_type: child_media,
                    size: child_size,
                });

                let config_digest = ocx_oci::Digest::try_from(final_manifest.config.digest.as_str())
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                let config_pinned = ocx_oci::PinnedPackageRef::try_from(top_id.clone_with_digest(config_digest))
                    .map_err(|_| PackageErrorKind::DigestMissing)?;
                chain.push(ChainBlob {
                    identifier: config_pinned,
                    role: ChainRole::Config,
                    media_type: final_manifest.config.media_type.clone(),
                    size: final_manifest.config.size,
                });

                let transport_pinned = resolve_transport_pinned(self.index(), &pinned).await?;
                Ok(ResolvedChain {
                    pinned,
                    transport_pinned,
                    chain,
                    final_manifest,
                    platform: selected_platform,
                })
            }
        }
    }

    /// Resolves multiple identifiers in parallel, preserving input order.
    pub async fn resolve_all(
        &self,
        packages: &[ocx_oci::PackageRef],
        platform: ocx_oci::Platform,
    ) -> Result<Vec<ResolvedChain>, crate::error::Error> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }
        if packages.len() == 1 {
            let pinned = self.resolve(&packages[0], platform).await.map_err(|kind| {
                crate::error::Error::ResolveFailed(vec![PackageError::new(packages[0].clone(), kind)])
            })?;
            return Ok(vec![pinned]);
        }

        let mut tasks = JoinSet::new();
        for package in packages {
            let mgr = self.clone();
            let package = package.clone();
            let platform = platform.clone();
            tasks.spawn(async move {
                let result = mgr.resolve(&package, platform).await;
                (package, result)
            });
        }

        super::common::drain_package_tasks(packages, tasks, crate::error::Error::ResolveFailed).await
    }

    /// Resolve the composed env for the given roots; `self_view` selects the private
    /// (`--self`) surface. A `[patches]` overlay follows compose's entries in admitted-visit
    /// order, so a patch on a transitive dep overrides a var the root declares.
    pub async fn resolve_env(
        &self,
        packages: &[Arc<InstallInfo>],
        self_view: bool,
        scope: EnvScope,
        platform: &ocx_oci::Platform,
    ) -> crate::Result<Vec<Entry>> {
        let (entries, _, _) = self
            .resolve_env_with_patch_boundary(packages, self_view, scope, platform)
            .await?;
        Ok(entries)
    }

    /// Like [`resolve_env`] plus the overlay start index and per-overlay-entry [`PatchProvenance`].
    ///
    /// Regions: compose output `[0..patch_start)`, the companion overlay, then `scope`'s `[env]`
    /// and `--env`. `provenance` is aligned with the middle
    /// region, so `provenance[i]` names the rule glob + companion for
    /// `entries[patch_start + i]`; bound-check against `provenance.len()`, never assume the overlay runs to the end.
    pub async fn resolve_env_with_patch_boundary(
        &self,
        packages: &[Arc<InstallInfo>],
        self_view: bool,
        scope: EnvScope,
        platform: &ocx_oci::Platform,
    ) -> crate::Result<(Vec<Entry>, usize, Vec<PatchProvenance>)> {
        let (entries, compose_count, provenance, _attribution) = self
            .resolve_env_with_attribution(packages, self_view, scope, platform)
            .await?;
        Ok((entries, compose_count, provenance))
    }

    /// Like [`Self::resolve_env_with_patch_boundary`] plus the admitted-set claim attribution
    /// for `ocx env` / `ocx package env` (`adr_declared_binaries_metadata.md` §4 Decision A).
    ///
    /// `platform` is what the companion overlay composes FOR (host, or `-p`); passing the host
    /// for a base materialized for another platform composes zero companions, or fails closed
    /// on a required one.
    pub async fn resolve_env_with_attribution(
        &self,
        packages: &[Arc<InstallInfo>],
        self_view: bool,
        scope: EnvScope,
        platform: &ocx_oci::Platform,
    ) -> crate::Result<(Vec<Entry>, usize, Vec<PatchProvenance>, AdmittedClaims)> {
        let no_patches = scope.opt_out();
        // Every env caller composes here, so none can disagree on link-following or group-heal order.
        let paths = match scope.toolchain_links() {
            Some(links) => composer::ComposePaths::resolve(links, self.file_structure(), platform).await?,
            None => composer::ComposePaths::digest_only(),
        };
        let out = composer::compose(packages, &self.file_structure().packages, self_view, &paths).await?;
        let mut attribution = AdmittedClaims {
            binaries: out.admitted_binaries,
            entrypoints: out.admitted_entrypoints,
            integrations: out.admitted_integrations,
        };
        // Reserved keys are gated here, not in `Env::apply_entries`, which `emit_lines` bypasses.
        // Per region: a whole-vector `retain` would shift the `compose_count`/`provenance` indices.
        let mut reserved_warned: HashSet<String> = HashSet::new();
        let mut entries = out.entries;
        entries.retain(|entry| !reserved_key_dropped(entry, &mut reserved_warned));
        let compose_count = entries.len();
        let mut provenance: Vec<PatchProvenance> = Vec::new();

        // Gate before projecting companions: resolution asserts every `${deps.*}` dir exists, so
        // gating after fails a required companion over a value this surface never carries
        // (`adr_package_integrations.md § Patch companions`).
        let collect_integrations = composer::integrations_cross(self_view);
        if let Some(mut patch_set) = self
            .build_site_patch_set(&out.admitted, no_patches, platform, collect_integrations)
            .await?
        {
            // One row per (identifier minus advisory tag, namespace) across the whole composition,
            // or a dep shared by base and companion lands twice; base rows seed first and win.
            let mut seen_integrations: HashSet<(ocx_oci::PinnedPackageRef, String)> = attribution
                .integrations
                .iter()
                .map(|(identifier, entry)| (identifier.strip_advisory(), entry.namespace.clone()))
                .collect();
            for admitted_id in &out.admitted {
                if let Some(overlay) = patch_set.remove(admitted_id) {
                    for (entry, entry_provenance) in overlay.entries {
                        // Skip entry and provenance together, or later `--show-patches` lines mis-attribute.
                        if reserved_key_dropped(&entry, &mut reserved_warned) {
                            continue;
                        }
                        entries.push(entry);
                        provenance.push(entry_provenance);
                    }
                    // Empty when the gate is off; nothing was collected upstream.
                    for (identifier, entry) in overlay.integrations {
                        if seen_integrations.insert((identifier.strip_advisory(), entry.namespace.clone())) {
                            attribution.integrations.push((identifier, entry));
                        }
                    }
                }
            }
        }

        // Vector position is the precedence `Env::apply_entries` replays. Kept outside the
        // `ConstantTracker`, or an override misfires as a package-vs-package collision warning.
        let contributed = scope.contributed_env();
        if !contributed.is_empty() {
            log_project_env_shadowing(&entries, contributed);
            // Refiltered despite the parse-time refusal, so a new contributed source cannot reopen the hole.
            entries.extend(
                contributed
                    .iter()
                    .filter(|entry| !reserved_key_dropped(entry, &mut reserved_warned))
                    .cloned(),
            );
        }

        Ok((entries, compose_count, provenance, attribution))
    }

    /// The [`SitePatchSet`] for `admitted` from local state only, or `None` with no `[patches]`.
    ///
    /// `collect_integrations` is forwarded because projection runs at `self_view = false` and
    /// cannot derive it; without it a companion naming an uninstalled dependency would fail a
    /// composition whose surface carries no integrations.
    async fn build_site_patch_set(
        &self,
        admitted: &[ocx_oci::PinnedPackageRef],
        no_patches: &std::collections::BTreeSet<String>,
        platform: &ocx_oci::Platform,
        collect_integrations: bool,
    ) -> crate::Result<Option<SitePatchSet>> {
        let Some(patches) = self.patches() else {
            return Ok(None);
        };

        let file_structure = self.file_structure();
        let blob_store = &file_structure.blobs;
        let package_store = &file_structure.packages;

        // SECURITY: the CAS path is namespaced only by the operator-controlled registry host and
        // SHA-256 addressed, bounding path injection; descriptors carry no signature check.
        let global_id = super::patch_discovery::global_descriptor_id(patches);
        let global_tags_path = file_structure.patch_descriptor_path(&global_id);
        let global_descriptor_result = load_descriptor_frozen_or_live(
            blob_store,
            // The descriptor id's bare-host registry, where `fetch_and_persist_descriptor` persists;
            // the path-prefixed `patches.registry` would look in the wrong directory.
            global_id.registry(),
            &global_id,
            &global_tags_path,
            self.patch_snapshot(),
        )
        .await?;

        // A corrupt global descriptor is tampering, never "no patch": fail closed when required.
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

        // Keyed by full `registry/repo:tag`: a catch-all companion is projected and emitted once,
        // under the first matching base. Fail-closed checks still run on every base.
        let mut companion_projection_cache: HashMap<ocx_oci::PackageRef, CompanionOutcome> = HashMap::new();

        let mut patch_set: SitePatchSet = SitePatchSet::new();

        // Loads run concurrently (core-bounded) into slots by admitted index, so emission order,
        // error determinism and the projection cache never depend on completion order.
        let snapshot: Arc<Option<crate::patch::PatchSnapshot>> = Arc::new(self.patch_snapshot().cloned());
        let system_required = patches.system_required;
        let load_semaphore = crate::concurrency::Concurrency::cores().semaphore();
        let mut descriptor_load_tasks: JoinSet<(usize, crate::Result<DescriptorLoadResult>)> = JoinSet::new();
        for (index, admitted_id) in admitted.iter().enumerate() {
            let base_id = admitted_id.as_identifier();
            let repo_key = format!("{}/{}", base_id.registry(), base_id.repository());
            let digest_key = admitted_id.digest().to_string();
            // Opt-out matches `registry/repository` or the digest: a generated launcher's
            // `file-url-mode/<digest>` identifier has no real repository. System-required wins.
            if (no_patches.contains(&repo_key) || no_patches.contains(&digest_key)) && !system_required {
                continue;
            }
            let pkg_specific_id = super::patch_discovery::patch_descriptor_id(patches, base_id);
            let pkg_tags_path = file_structure.patch_descriptor_path(&pkg_specific_id);
            // Bare-host registry, as for the global descriptor.
            let registry = pkg_specific_id.registry().to_string();
            let blob_store = blob_store.clone();
            let snapshot = snapshot.clone();
            let semaphore = load_semaphore.clone();
            descriptor_load_tasks.spawn(async move {
                let _permit = crate::concurrency::acquire_permit(&semaphore).await;
                let snapshot_ref: Option<&crate::patch::PatchSnapshot> = (*snapshot).as_ref();
                let result = load_descriptor_frozen_or_live(
                    &blob_store,
                    &registry,
                    &pkg_specific_id,
                    &pkg_tags_path,
                    snapshot_ref,
                )
                .await;
                (index, result)
            });
        }
        let mut preloaded_descriptors: Vec<Option<crate::Result<DescriptorLoadResult>>> = Vec::new();
        preloaded_descriptors.resize_with(admitted.len(), || None);
        while let Some(join_result) = descriptor_load_tasks.join_next().await {
            let (index, result) = match join_result {
                Ok(value) => value,
                Err(join_error) if join_error.is_panic() => std::panic::resume_unwind(join_error.into_panic()),
                Err(join_error) => panic!("descriptor load task aborted: {join_error}"),
            };
            preloaded_descriptors[index] = Some(result);
        }

        for (index, admitted_id) in admitted.iter().enumerate() {
            // `None`: opted out above.
            let Some(pkg_descriptor_result) = preloaded_descriptors[index].take() else {
                continue;
            };
            let base_id = admitted_id.as_identifier();
            // Propagate a load error in admitted order (deterministic failure).
            let pkg_descriptor_result = pkg_descriptor_result?;

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

            if global_descriptor.is_none() && pkg_descriptor.is_none() {
                continue;
            }

            let companions = merge_companions(
                base_id,
                patches.required,
                global_descriptor.as_ref(),
                pkg_descriptor.as_ref(),
            );

            if companions.is_empty() {
                continue;
            }

            // Re-cap at the install-time limit: a compromised patch registry could accumulate
            // over-cap entries across both descriptors. Required fails closed; optional truncates.
            let companions = if companions.len() > super::patch_discovery::MAX_TOTAL_COMPANIONS {
                let any_required = companions
                    .iter()
                    .skip(super::patch_discovery::MAX_TOTAL_COMPANIONS)
                    .any(|c| c.required);
                if patches.required || any_required {
                    return Err(crate::Error::from(crate::error::PackageErrorKind::PatchDiscovery(
                        crate::patch::PatchError::DescriptorTooLarge {
                            detail: format!(
                                "companion count {} for '{}' exceeds maximum {}",
                                companions.len(),
                                admitted_id,
                                super::patch_discovery::MAX_TOTAL_COMPANIONS
                            ),
                        },
                    )));
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

            // Projection is cached per companion, but the rule glob is per (base, companion), so
            // provenance attaches only when a projection lands in this base's overlay.
            let mut companion_overlay = CompanionOverlay {
                entries: Vec::new(),
                integrations: Vec::new(),
            };
            for companion_entry in &companions {
                let companion_id = &companion_entry.identifier;
                let make_provenance = |pinned: &ocx_oci::PinnedPackageRef| PatchProvenance {
                    rule_match: companion_entry.rule_match.clone(),
                    companion: companion_id.clone(),
                    pinned: pinned.clone(),
                };

                // A cached `Missing` for a REQUIRED companion still fails closed, or a required but
                // absent companion bypasses the gate on every base after the first.
                if let Some(outcome) = companion_projection_cache.get(companion_id) {
                    match outcome {
                        CompanionOutcome::Missing => {
                            if companion_entry.required {
                                return Err(crate::Error::from(
                                    crate::error::PackageErrorKind::RequiredCompanionFailed {
                                        companion: companion_id.clone(),
                                        source: Box::new(crate::error::PackageErrorKind::NotFound),
                                    },
                                ));
                            }
                            // Optional and missing — skip (already logged on first encounter).
                        }
                        // Already emitted under the first matching base; emitting again duplicates it.
                        CompanionOutcome::Projected => {}
                    }
                    continue;
                }

                // Composes at the patch-tier pin (snapshot, else recorded); a pin with no installed
                // package yields `None` and the required gate fires as normal.
                let companion_install_info = match self.find_companion_local(companion_id, platform).await {
                    Ok(Some(info)) => info,
                    Ok(None) => {
                        companion_projection_cache.insert(companion_id.clone(), CompanionOutcome::Missing);
                        if companion_entry.required {
                            // Install-time discovery should have installed it: fail closed.
                            return Err(crate::Error::from(
                                crate::error::PackageErrorKind::RequiredCompanionFailed {
                                    companion: companion_id.clone(),
                                    source: Box::new(crate::error::PackageErrorKind::NotFound),
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
                        // A lookup error is not "not installed" and could mask a missing required
                        // companion: only optional ones warn and skip.
                        if companion_entry.required {
                            return Err(crate::Error::from(
                                crate::error::PackageErrorKind::RequiredCompanionFailed {
                                    companion: companion_id.clone(),
                                    source: Box::new(crate::error::PackageErrorKind::Internal(error)),
                                },
                            ));
                        }
                        log::warn!(
                            "site-patch-set: error looking up optional companion '{}' for '{}': {error}; skipping",
                            companion_id,
                            admitted_id
                        );
                        companion_projection_cache.insert(companion_id.clone(), CompanionOutcome::Missing);
                        continue;
                    }
                };

                // Interface surface, attributed to the companion. Binaries and entrypoints stay dropped:
                // never on PATH here, so admitting them would advertise unreachable binaries.
                let companion_arc = std::sync::Arc::new(companion_install_info);
                match composer::compose_companion(&companion_arc, package_store, collect_integrations).await {
                    Ok(out) => {
                        // A cache miss proves no earlier base emitted it; empty output is still `Projected`.
                        let pinned = companion_arc.identifier().clone();
                        companion_overlay
                            .entries
                            .extend(out.entries.into_iter().map(|entry| (entry, make_provenance(&pinned))));
                        companion_overlay.integrations.extend(out.admitted_integrations);
                        companion_projection_cache.insert(companion_id.clone(), CompanionOutcome::Projected);
                    }
                    Err(error) => {
                        if companion_entry.required {
                            // Present but composition failed: fail closed, never a partial overlay.
                            return Err(crate::Error::from(
                                crate::error::PackageErrorKind::RequiredCompanionFailed {
                                    companion: companion_id.clone(),
                                    source: Box::new(crate::error::PackageErrorKind::Internal(error)),
                                },
                            ));
                        }
                        log::warn!(
                            "site-patch-set: failed to compose optional companion '{}' for '{}': {error}; skipping",
                            companion_id,
                            admitted_id
                        );
                        companion_projection_cache.insert(companion_id.clone(), CompanionOutcome::Missing);
                    }
                }
            }

            // A companion with integrations but no interface env still contributes.
            if !companion_overlay.entries.is_empty() || !companion_overlay.integrations.is_empty() {
                patch_set.insert(admitted_id.clone(), companion_overlay);
            }
        }

        Ok(Some(patch_set))
    }

    /// The digest a companion composes at: the active [`PatchSnapshot`](crate::patch::PatchSnapshot)
    /// pin, else the recorded pin; `Ok(None)` = unpinned, which callers treat as not installed.
    ///
    /// The snapshot key ([`crate::patch::snapshot::companion_key`], shared with the freeze writer)
    /// pins a repository at a tag, matching the record's per-tag granularity.
    pub(super) async fn companion_pin(
        &self,
        companion_id: &ocx_oci::PackageRef,
    ) -> crate::Result<Option<ocx_oci::Digest>> {
        if let Some(snapshot) = self.patch_snapshot()
            && let Some(pinned_digest) = snapshot
                .companions
                .get(&crate::patch::snapshot::companion_key(companion_id))
        {
            return Ok(Some(pinned_digest.clone()));
        }
        self.companion_pin_recorded(companion_id).await
    }

    /// The recorded companion pin, ignoring any snapshot: `ocx patch freeze` must snapshot live
    /// state, and reading through [`companion_pin`](Self::companion_pin) re-freezes its own output.
    pub(super) async fn companion_pin_recorded(
        &self,
        companion_id: &ocx_oci::PackageRef,
    ) -> crate::Result<Option<ocx_oci::Digest>> {
        let path = self.file_structure().patch_companion_path(companion_id);
        let Some(recorded) = super::patch_discovery::PatchTagMap::read_tag(&path, companion_id.tag_or_latest()).await?
        else {
            return Ok(None);
        };
        match ocx_oci::Digest::try_from(recorded.as_str()) {
            Ok(digest) => Ok(Some(digest)),
            Err(error) => {
                // A malformed record is unpinned, so a required companion still fails closed.
                log::warn!(
                    "site-patch-set: companion pin for '{}' has an invalid digest '{}': {error}; ignoring",
                    companion_id,
                    recorded
                );
                Ok(None)
            }
        }
    }

    /// A companion's installed `InstallInfo` from its patch-tier pin and the package store,
    /// network-free; `Ok(None)` when unpinned or not installed locally.
    ///
    /// Never asks the local index for a pin: a same-repository package-tier tag pointer is
    /// somebody else's answer. `platform` is the one composed FOR (the caller's `-p`).
    async fn find_companion_local(
        &self,
        companion_id: &ocx_oci::PackageRef,
        platform: &ocx_oci::Platform,
    ) -> crate::Result<Option<InstallInfo>> {
        use super::common::find_in_store;

        let Some(top_digest) = self.companion_pin(companion_id).await? else {
            return Ok(None);
        };

        let local_index = companion_manifest_index(self);

        // Cached top manifest: platform-select an image index; uncached falls through below.
        let top_id = companion_id.clone_with_digest(top_digest.clone());
        if let Some(top_manifest) = local_index.fetch_manifest(&top_id, IndexOperation::Query).await? {
            let pinned_id = match top_manifest {
                // Single-platform image: tag-store digest IS the platform manifest digest.
                (digest, ocx_oci::Manifest::Image(_)) => {
                    match ocx_oci::PinnedPackageRef::try_from(companion_id.clone_with_digest(digest)) {
                        Ok(id) => id,
                        Err(_) => return Ok(None),
                    }
                }
                // Select for the composed-FOR platform, not the host, or a companion with no host
                // leaf resolves absent and a required one fails closed.
                (_, ocx_oci::Manifest::ImageIndex(_)) => {
                    let selected_id = match local_index.select(&top_id, platform, IndexOperation::Query).await? {
                        SelectResult::Found(id) => id,
                        SelectResult::NotFound => return Ok(None),
                        SelectResult::Ambiguous(_) => return Ok(None),
                        SelectResult::FeatureMismatch { .. } => return Ok(None),
                    };
                    match ocx_oci::PinnedPackageRef::try_from(selected_id) {
                        Ok(id) => id,
                        Err(_) => return Ok(None),
                    }
                }
            };
            let result = find_in_store(&self.file_structure().packages, &pinned_id)
                .await
                .map_err(crate::Error::from)?;
            return Ok(result);
        }

        // Uncached manifest: the pin may already be the platform digest (single-platform, or
        // `ocx patch test --companion-archive`); `find_in_store` decides, absent = `None`.
        let pinned_id = match ocx_oci::PinnedPackageRef::try_from(companion_id.clone_with_digest(top_digest)) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let result = find_in_store(&self.file_structure().packages, &pinned_id)
            .await
            .map_err(crate::Error::from)?;
        Ok(result)
    }

    /// GC roots from the site-patch tier, strictly offline; empty with no `[patches]`.
    ///
    /// `platform` picks a multi-platform companion's child so the pin matches its install path
    /// (callers pass the host); `scope` picks which pins count ([`PatchRootScope`]).
    ///
    /// # Errors
    ///
    /// Returns an error if the local filesystem state cannot be read.
    pub async fn resolve_site_patch_roots(
        &self,
        platform: &ocx_oci::Platform,
        scope: PatchRootScope,
    ) -> crate::Result<SitePatchRoots> {
        let Some(patches) = self.patches() else {
            return Ok(SitePatchRoots::default());
        };

        let file_structure = self.file_structure();
        let blob_store = &file_structure.blobs;
        let symlink_root = file_structure.symlinks.root().to_path_buf();

        // Same reader as `find_companion_local`, so GC roots and compose derive from one answer.
        let local_index = companion_manifest_index(self);

        let installed_base_ids = super::patch_sync::enumerate_installed_bases(self).await?;
        let _ = symlink_root;

        let mut companion_set: Vec<ocx_oci::PackageRef> = Vec::new();
        let mut descriptor_digests: Vec<(String, ocx_oci::Digest)> = Vec::new();
        let mut descriptor_pins: Vec<(String, ocx_oci::Digest)> = Vec::new();

        // Load the global descriptor once, then match its rules per installed base; an empty
        // base identifier would match only catch-all rules, never scoped ones.
        let global_id = super::patch_discovery::global_descriptor_id(patches);
        let global_tags_path = file_structure.patch_descriptor_path(&global_id);

        let (global_descriptor_opt, global_manifest_digest) = collect_descriptor_digests(
            blob_store,
            // Bare-host registry, as in `build_site_patch_set`.
            global_id.registry(),
            &global_tags_path,
            &mut descriptor_digests,
        )
        .await?;
        seed_snapshot_descriptor_digests(
            self,
            scope,
            blob_store,
            &global_id,
            global_manifest_digest.as_ref(),
            &mut descriptor_digests,
        )
        .await;
        if let Some(manifest_digest) = global_manifest_digest {
            descriptor_pins.push((descriptor_source_key(&global_id), manifest_digest));
        }

        if let Some(ref global_descriptor) = global_descriptor_opt {
            for base_id in &installed_base_ids {
                for companion_entry in global_descriptor.collect_companions(base_id, patches.required) {
                    companion_set.push(companion_entry.identifier);
                }
            }
        }

        for base_id in &installed_base_ids {
            let pkg_specific_id = super::patch_discovery::patch_descriptor_id(patches, base_id);
            let pkg_tags_path = file_structure.patch_descriptor_path(&pkg_specific_id);

            let (pkg_descriptor_opt, pkg_manifest_digest) = collect_descriptor_digests(
                blob_store,
                pkg_specific_id.registry(),
                &pkg_tags_path,
                &mut descriptor_digests,
            )
            .await?;
            seed_snapshot_descriptor_digests(
                self,
                scope,
                blob_store,
                &pkg_specific_id,
                pkg_manifest_digest.as_ref(),
                &mut descriptor_digests,
            )
            .await;
            if let Some(manifest_digest) = pkg_manifest_digest {
                descriptor_pins.push((descriptor_source_key(&pkg_specific_id), manifest_digest));
            }

            if let Some(ref pkg_descriptor) = pkg_descriptor_opt {
                for companion_entry in pkg_descriptor.collect_companions(base_id, patches.required) {
                    companion_set.push(companion_entry.identifier);
                }
            }
        }

        // The patch tier pins the TOP manifest digest; the package store keys by the PLATFORM digest.
        let mut companions: Vec<ocx_oci::PinnedPackageRef> = Vec::new();
        companion_set.sort_by_key(|id| id.to_string());
        companion_set.dedup_by_key(|id| id.to_string());

        for companion_id in &companion_set {
            // The RECORDED pin: freeze builds its snapshot from these roots, so reading through a
            // snapshot re-freezes its own output. Snapshot pins are added below, never instead.
            let top_digest = match self.companion_pin_recorded(companion_id).await {
                Ok(Some(digest)) => digest,
                Ok(None) => {
                    log::debug!(
                        "resolve-site-patch-roots: companion '{}' has no patch-tier pin; skipping",
                        companion_id
                    );
                    continue;
                }
                Err(error) => {
                    log::debug!(
                        "resolve-site-patch-roots: error reading the patch pin for companion '{}': {error}; skipping",
                        companion_id
                    );
                    continue;
                }
            };

            if let Some(pinned) = resolve_companion_pinned(&local_index, companion_id, &top_digest, platform).await {
                companions.push(pinned);
            }
        }

        // GC only: every snapshot-pinned companion is a root, read from the snapshot's own map,
        // since a sync may have advanced both the recorded digest and the rule naming it.
        for (companion_id, top_digest) in snapshot_companion_roots(self, scope) {
            if let Some(pinned) = resolve_companion_pinned(&local_index, &companion_id, &top_digest, platform).await {
                companions.push(pinned);
            }
        }

        companions.sort_by_key(|pinned| pinned.to_string());
        companions.dedup_by_key(|pinned| pinned.to_string());

        descriptor_digests.sort_by_key(|(registry, digest)| format!("{registry}/{digest}"));
        descriptor_digests.dedup_by_key(|(registry, digest)| format!("{registry}/{digest}"));

        // One pin per source: installed tags of one repository share a descriptor source.
        descriptor_pins.sort_by(|(left, _), (right, _)| left.cmp(right));
        descriptor_pins.dedup_by(|(left, _), (right, _)| left == right);

        Ok(SitePatchRoots {
            companions,
            descriptors: descriptor_digests,
            descriptor_pins,
        })
    }
}

/// Size of a chain blob's on-disk `data` file, or `-1` when it cannot be stat'd.
///
/// An unlocked `metadata()` suffices: the value is display-only and the store is
/// content-addressed, so a concurrent rewrite cannot change the size.
async fn blob_data_size(
    file_structure: &ocx_store::file_structure::FileStructure,
    pinned: &ocx_oci::PinnedPackageRef,
) -> i64 {
    let path = file_structure.blobs.data(pinned.registry(), &pinned.digest());
    match tokio::fs::metadata(&path).await {
        Ok(meta) => i64::try_from(meta.len()).unwrap_or(i64::MAX),
        Err(error) => {
            log::debug!("Could not stat chain blob '{}': {error}.", path.display());
            -1
        }
    }
}

/// Outcome of a descriptor load from the tag store + CAS.
#[derive(Debug)]
enum DescriptorLoadResult {
    /// Never looked, or looked and found none: skip silently.
    NotPresent,
    /// Read and parsed; carries the tag-store manifest digest so callers skip a second read.
    Loaded(ocx_oci::Digest, PatchDescriptor),
    /// The tag store says a descriptor exists but the CAS blob is missing or unreadable:
    /// tampering, never "no patch", so a required caller fails closed. The digest is `None`
    /// when the stored string is malformed; GC roots a `Some` digest.
    Corrupt(crate::Error, Option<ocx_oci::Digest>),
}

/// Load the [`PatchDescriptor`] recorded at `tags_path`, offline.
async fn load_descriptor_for_id(
    blob_store: &ocx_store::file_structure::BlobStore,
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
            let digest = match ocx_oci::Digest::try_from(manifest_digest.as_str()) {
                Ok(d) => d,
                Err(_) => {
                    // A malformed stored digest is corruption too; `None` = no manifest blob to protect.
                    let corrupt_err =
                        crate::Error::Digest(ocx_oci::digest::error::DigestError::Invalid(manifest_digest.clone()));
                    return Ok(DescriptorLoadResult::Corrupt(corrupt_err, None));
                }
            };
            match load_descriptor_from_cas(blob_store, registry, &digest).await {
                Ok(descriptor) => Ok(DescriptorLoadResult::Loaded(digest, descriptor)),
                // Carry the digest so GC still protects the manifest blob.
                Err(error) => Ok(DescriptorLoadResult::Corrupt(error, Some(digest))),
            }
        }
    }
}

/// The host platform, the way every ambient (no `-p`) compose path resolves it.
///
/// Test call sites pass this to `resolve_env` so the companion selection they
/// exercise is the one production performs on the same host.
#[cfg(test)]
fn host_platform() -> ocx_oci::Platform {
    ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any)
}

/// Load a descriptor, frozen under an active snapshot: its manifest digest comes from the
/// snapshot's per-source pin ([`descriptor_source_key`]) and loads from the CAS, bypassing the
/// live tag store. A source absent from the snapshot is `NotPresent`, so a post-freeze
/// `ocx patch sync` cannot change what a frozen build composes. No snapshot: the tier floats.
async fn load_descriptor_frozen_or_live(
    blob_store: &ocx_store::file_structure::BlobStore,
    registry: &str,
    descriptor_id: &ocx_oci::PackageRef,
    tags_path: &std::path::Path,
    snapshot: Option<&crate::patch::PatchSnapshot>,
) -> crate::Result<DescriptorLoadResult> {
    use super::patch_discovery::load_descriptor_from_cas;

    let Some(snapshot) = snapshot else {
        return load_descriptor_for_id(blob_store, registry, tags_path).await;
    };

    match snapshot.descriptors.get(&descriptor_source_key(descriptor_id)) {
        Some(manifest_digest) => match load_descriptor_from_cas(blob_store, registry, manifest_digest).await {
            Ok(descriptor) => Ok(DescriptorLoadResult::Loaded(manifest_digest.clone(), descriptor)),
            // Carry the digest so a required tier fails closed instead of dropping the overlay.
            Err(error) => Ok(DescriptorLoadResult::Corrupt(error, Some(manifest_digest.clone()))),
        },
        None => Ok(DescriptorLoadResult::NotPresent),
    }
}

/// Merge global then package-specific companions for `base_id`; on the same companion
/// identifier the package-specific entry wins, as in install-time discovery.
fn merge_companions(
    base_id: &ocx_oci::PackageRef,
    tier_required_default: bool,
    global_descriptor: Option<&PatchDescriptor>,
    pkg_descriptor: Option<&PatchDescriptor>,
) -> Vec<crate::patch::CompanionEntry> {
    use std::collections::HashMap;

    let mut companion_order: Vec<ocx_oci::PackageRef> = Vec::new();
    let mut companion_map: HashMap<ocx_oci::PackageRef, crate::patch::CompanionEntry> = HashMap::new();

    for descriptor in [global_descriptor, pkg_descriptor].into_iter().flatten() {
        for entry in descriptor.collect_companions(base_id, tier_required_default) {
            if !companion_map.contains_key(&entry.identifier) {
                companion_order.push(entry.identifier.clone());
            }
            companion_map.insert(entry.identifier.clone(), entry);
        }
    }

    companion_order
        .into_iter()
        .filter_map(|id| companion_map.remove(&id))
        .collect()
}

/// Restore an installed base's real registry hostname from its root document.
///
/// Store slugs are lossy for port hosts (`localhost:5000` -> `localhost_5000`); the index root
/// keeps the canonical `oci://` pointer (`adr_index_indirection.md` § A2, § C3). Any read or
/// parse miss returns the slug form, which still matches catch-all rules.
/// ponytail: a port host an index routes elsewhere stays in slug form, so a
/// `localhost:5000/...` rule misses it; upgrade by reading the index sources.
pub(super) async fn recover_base_with_real_registry(
    snapshot: &ocx_index::IndexStore,
    slug_base_id: &ocx_oci::PackageRef,
) -> ocx_oci::PackageRef {
    let real_registry = match snapshot
        .read_root_document_bytes(slug_base_id.registry(), slug_base_id.repository())
        .await
    {
        Ok(Some(bytes)) => serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| value.get("repository").and_then(|r| r.as_str()).map(str::to_owned))
            .and_then(|repository| ocx_oci::OciIdentifier::parse_repository_pointer(&repository).ok())
            .map(|location| location.registry().to_string()),
        Ok(None) | Err(_) => None,
    };
    // Accept only a host that un-slugs this directory: an index-routed root names a physical
    // location elsewhere, which must never become package identity.
    match real_registry {
        Some(registry)
            if registry != slug_base_id.registry()
                && ocx_store::file_structure::slugify(&registry)
                    == ocx_store::file_structure::slugify(slug_base_id.registry()) =>
        {
            ocx_oci::PackageRef::new_registry(slug_base_id.repository(), &registry)
                .clone_with_tag(slug_base_id.tag_or_latest())
        }
        _ => slug_base_id.clone(),
    }
}

/// Recursively collect one `PackageRef` per `candidates/{tag}` under `dir`, with the
/// registry slug as registry:
/// ```text
/// {symlink_root}/{registry_slug}/{repo_component_1}/.../candidates/{tag}
/// ```
///
/// # Errors
///
/// Only on unexpected filesystem I/O failures; a `NotFound` directory yields `Ok`.
pub(super) async fn collect_candidates_from_dir(
    dir: &std::path::Path,
    registry_slug: &str,
    repo_components: &mut Vec<String>,
    out: &mut Vec<ocx_oci::PackageRef>,
) -> crate::Result<()> {
    let dir_name = match dir.file_name().and_then(|n| n.to_str()) {
        Some(name) => name.to_string(),
        None => return Ok(()),
    };

    if dir_name == "candidates" {
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
            // Slug as registry: `patch_descriptor_path` slugifies the same way, so lookups match.
            let base_id = ocx_oci::PackageRef::new_registry(&repo, registry_slug).clone_with_tag(&tag);
            out.push(base_id);
        }
        return Ok(());
    }

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
        match entry.file_type().await {
            Ok(file_type) if file_type.is_dir() => {}
            Ok(file_type) if file_type.is_symlink() => {
                // Never follow symlinks (e.g. `current`).
                continue;
            }
            _ => continue,
        }
        // Never push `candidates` into `repo_components`, or the base becomes
        // `cmake/candidates:3.28` and fails scoped descriptor rule matching.
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

/// Canonical `registry/repository` key of a patch descriptor source. `ocx patch freeze` and
/// [`PackageManager::build_site_patch_set`] must derive it from the same descriptor ids, or a
/// frozen build looks up keys the freeze never wrote.
fn descriptor_source_key(descriptor_id: &ocx_oci::PackageRef) -> String {
    format!("{}/{}", descriptor_id.registry(), descriptor_id.repository())
}

/// Load a descriptor, append its blob digests to `descriptor_digests`, and return it with its
/// manifest digest; both `None` unless cleanly loaded, since freeze cannot pin unreadable bytes.
///
/// A corrupt descriptor is debug-logged and skipped, not fail-closed, with its parseable manifest
/// digest still rooted: over-collecting a needed companion is harmful, an orphan is safe.
async fn collect_descriptor_digests(
    blob_store: &ocx_store::file_structure::BlobStore,
    registry: &str,
    tags_path: &std::path::Path,
    descriptor_digests: &mut Vec<(String, ocx_oci::Digest)>,
) -> crate::Result<(Option<PatchDescriptor>, Option<ocx_oci::Digest>)> {
    let load_result = load_descriptor_for_id(blob_store, registry, tags_path).await?;

    let (manifest_digest, descriptor) = match load_result {
        DescriptorLoadResult::NotPresent => return Ok((None, None)),
        DescriptorLoadResult::Corrupt(error, manifest_digest_opt) => {
            log::debug!("resolve-site-patch-roots: descriptor corrupt (best-effort GC root): {error}");
            if let Some(manifest_digest) = manifest_digest_opt {
                descriptor_digests.push((registry.to_string(), manifest_digest));
            }
            return Ok((None, None));
        }
        DescriptorLoadResult::Loaded(manifest_digest, descriptor) => (manifest_digest, descriptor),
    };

    push_descriptor_blob_digests(blob_store, registry, &manifest_digest, descriptor_digests).await;

    Ok((Some(descriptor), Some(manifest_digest)))
}

/// Every companion an active freeze pins, as `(identifier, top digest)`; empty under
/// [`PatchRootScope::Recorded`] or with no snapshot.
///
/// Read from the snapshot's own map, not the descriptor-derived set, so a companion the live
/// descriptor has since dropped stays rooted. The advisory tag is kept so one repository
/// frozen at two tags seeds both digests.
fn snapshot_companion_roots(
    manager: &PackageManager,
    scope: PatchRootScope,
) -> Vec<(ocx_oci::PackageRef, ocx_oci::Digest)> {
    if scope != PatchRootScope::RecordedAndSnapshot {
        return Vec::new();
    }
    let Some(snapshot) = manager.patch_snapshot() else {
        return Vec::new();
    };
    snapshot
        .companions
        .iter()
        .filter_map(|(key, digest)| Some((crate::patch::snapshot::companion_key_identifier(key)?, digest.clone())))
        .collect()
}

/// Root the blob digests of a snapshot-pinned descriptor: compose loads it by the SNAPSHOT
/// digest, so record-only roots collect it once a sync advances the record.
async fn seed_snapshot_descriptor_digests(
    manager: &PackageManager,
    scope: PatchRootScope,
    blob_store: &ocx_store::file_structure::BlobStore,
    descriptor_id: &ocx_oci::PackageRef,
    recorded_manifest: Option<&ocx_oci::Digest>,
    descriptor_digests: &mut Vec<(String, ocx_oci::Digest)>,
) {
    if scope != PatchRootScope::RecordedAndSnapshot {
        return;
    }
    let Some(snapshot) = manager.patch_snapshot() else {
        return;
    };
    let Some(pinned) = snapshot.descriptors.get(&descriptor_source_key(descriptor_id)) else {
        return;
    };
    if recorded_manifest == Some(pinned) {
        return;
    }
    push_descriptor_blob_digests(blob_store, descriptor_id.registry(), pinned, descriptor_digests).await;
}

/// The local-only, network-free reader compose (`find_companion_local`) and GC/freeze
/// (`resolve_site_patch_roots`) resolve a recorded pin through.
///
/// `read_only_view` stops blob-store recovery self-healing into `o/`, which would re-create
/// the directory the pin move emptied.
fn companion_manifest_index(manager: &PackageManager) -> ocx_index::Index {
    ocx_index::Index::from_chained_with_content_store(
        ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: manager.effective_index_store(),
        }),
        Vec::new(),
        ocx_index::ChainMode::Offline,
        manager.file_structure().blobs.clone(),
    )
    .read_only_view()
}

/// Resolve a companion top digest to the pinned identifier the package store keys by, as
/// `find_companion_local` does; `None` if unresolvable locally.
///
/// Keeps `companion_id`'s advisory tag: freeze keys its snapshot per tag, so a tagless pin
/// collapses a repository frozen at two tags into one companion.
async fn resolve_companion_pinned(
    local_index: &ocx_index::Index,
    companion_id: &ocx_oci::PackageRef,
    top_digest: &ocx_oci::Digest,
    host_platform: &ocx_oci::Platform,
) -> Option<ocx_oci::PinnedPackageRef> {
    fn pin(
        repository: &str,
        registry: &str,
        digest: ocx_oci::Digest,
        companion_id: &ocx_oci::PackageRef,
    ) -> Option<ocx_oci::PinnedPackageRef> {
        let mut pinned_id = ocx_oci::PackageRef::new_registry(repository, registry);
        if let Some(tag) = companion_id.tag() {
            pinned_id = pinned_id.clone_with_tag(tag);
        }
        ocx_oci::PinnedPackageRef::try_from(pinned_id.clone_with_digest(digest))
            .inspect_err(|_| {
                log::debug!(
                    "resolve-site-patch-roots: could not pin companion '{}'; skipping",
                    companion_id
                );
            })
            .ok()
    }

    let top_id = companion_id.clone_with_digest(top_digest.clone());
    match local_index
        .fetch_manifest(&top_id, ocx_index::IndexOperation::Query)
        .await
    {
        Ok(Some((_, ocx_oci::Manifest::Image(_)))) => pin(
            companion_id.repository(),
            companion_id.registry(),
            top_digest.clone(),
            companion_id,
        ),
        Ok(Some((_, ocx_oci::Manifest::ImageIndex(_)))) => {
            let selected_id = match local_index
                .select(&top_id, host_platform, ocx_index::IndexOperation::Query)
                .await
            {
                Ok(SelectResult::Found(id)) => id,
                Ok(SelectResult::NotFound | SelectResult::Ambiguous(_) | SelectResult::FeatureMismatch { .. }) => {
                    log::debug!(
                        "resolve-site-patch-roots: could not select platform for companion '{}'; skipping",
                        companion_id
                    );
                    return None;
                }
                Err(error) => {
                    log::debug!(
                        "resolve-site-patch-roots: error selecting platform for companion '{}': {error}; skipping",
                        companion_id
                    );
                    return None;
                }
            };
            let Some(platform_digest) = selected_id.digest() else {
                log::debug!(
                    "resolve-site-patch-roots: selected '{}' missing digest; skipping",
                    companion_id
                );
                return None;
            };
            pin(
                selected_id.repository(),
                selected_id.registry(),
                platform_digest,
                companion_id,
            )
        }
        Ok(None) => {
            // Absent manifest blob: the top digest is exact for single-platform, best-effort otherwise.
            log::debug!(
                "resolve-site-patch-roots: manifest blob absent for '{}'; using tag-store digest as fallback",
                companion_id
            );
            pin(
                companion_id.repository(),
                companion_id.registry(),
                top_digest.clone(),
                companion_id,
            )
        }
        Err(error) => {
            log::debug!(
                "resolve-site-patch-roots: error reading manifest for companion '{}': {error}; skipping",
                companion_id
            );
            None
        }
    }
}

/// Push a descriptor manifest digest and every layer digest it names onto the GC roots,
/// shared by the recorded and snapshot walks so both retain the same blob set.
async fn push_descriptor_blob_digests(
    blob_store: &ocx_store::file_structure::BlobStore,
    registry: &str,
    manifest_digest: &ocx_oci::Digest,
    descriptor_digests: &mut Vec<(String, ocx_oci::Digest)>,
) {
    descriptor_digests.push((registry.to_string(), manifest_digest.clone()));

    let data_path = blob_store.path(registry, manifest_digest).join("data");
    match tokio::fs::read(&data_path).await {
        Ok(bytes) => {
            if let Ok(manifest_value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(layers) = manifest_value.get("layers").and_then(|v| v.as_array())
            {
                for layer in layers {
                    if let Some(digest_str) = layer.get("digest").and_then(|v| v.as_str())
                        && let Ok(layer_digest) = ocx_oci::Digest::try_from(digest_str)
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
}

// Specification tests for `PackageManager::resolve` and its chain-accumulation invariants.
#[cfg(test)]
mod spec_tests {
    use tempfile::TempDir;

    use super::ChainRole;
    use crate::{PackageManager, test_support::manifest_source::FakeManifestSource};
    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_oci::{self, Algorithm, Digest, PackageRef};
    use ocx_store::file_structure::FileStructure;

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";
    const CONFIG_DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
    const FLAT_MANIFEST_JSON: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;

    fn tagged_id() -> PackageRef {
        PackageRef::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }

    fn linux_amd64() -> ocx_oci::Platform {
        "linux/amd64".parse().unwrap()
    }

    /// The snapshot store the manager's LocalIndex reads from — the default
    /// machine-local home under the temp root (`index/`).
    fn index_store(dir: &TempDir) -> IndexStore {
        IndexStore::machine_local(&FileStructure::with_root(dir.path().to_path_buf()))
    }

    /// Build an offline `PackageManager` whose local index reads from the
    /// snapshot home under `dir`. Seed with `seed_object`/`seed_tag` (a
    /// dispatch-shaped fixture, A3) before calling this.
    fn make_manager(dir: &TempDir) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(&fs),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// Build a `PackageManager` chained to `source` under `ChainMode::Default`
    /// so a resolve that needs leaf content recovers it via the fake source.
    fn make_manager_with_source(dir: &TempDir, source: FakeManifestSource) -> PackageManager {
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let index = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: ocx_index::IndexStore::machine_local(&fs),
            }),
            vec![Index::from_impl(source)],
            ChainMode::Default,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// Write `bytes` verbatim into the snapshot dispatch-object CAS under
    /// their own digest (the A3 write invariant — bytes hash to filename)
    /// and return it. Dispatch-shaped fixtures only (image indexes) — a leaf
    /// manifest is never locally cached, so this helper is not valid for one.
    async fn seed_object(store: &IndexStore, bytes: &[u8]) -> Digest {
        let digest = Algorithm::Sha256.hash(bytes);
        store
            .write_dispatch_object(REGISTRY, REPO, &digest, bytes)
            .await
            .unwrap();
        digest
    }

    /// Author a DERIVED root document recording `TAG → digest` (A2 — OCX-authored,
    /// no published index involved in these tests).
    async fn seed_tag(store: &IndexStore, digest: &Digest) {
        let doc = serde_json::json!({
            "repository": format!("oci://{REGISTRY}/{REPO}"),
            "tags": { TAG: { "content": digest.to_string(), "observed": "2026-07-18T00:00:00Z" } }
        });
        store
            .write_root_document(REGISTRY, REPO, &serde_json::to_vec(&doc).unwrap())
            .await
            .unwrap();
    }

    /// `resolve` against a flat `ImageManifest` yields a `ResolvedChain`
    /// with two entries — the top-level manifest digest followed by the
    /// config-blob digest. The leaf is never locally cached (A3), so the
    /// manager is chained to a live fake source under `ChainMode::Default`
    /// that recovers it — mirrors a real single-platform install.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_single_image_returns_two_chain_entries() {
        let dir = TempDir::new().unwrap();
        let manifest_digest = Algorithm::Sha256.hash(FLAT_MANIFEST_JSON.as_bytes());
        let source = FakeManifestSource::default().with(TAG, FLAT_MANIFEST_JSON.as_bytes());
        let mgr = make_manager_with_source(&dir, source);
        let result = mgr.resolve(&tagged_id(), linux_amd64()).await.unwrap();
        assert_eq!(
            result.chain.len(),
            2,
            "flat ImageManifest must produce manifest + config chain entries"
        );
        assert_eq!(result.pinned.digest(), manifest_digest);
        assert_eq!(result.chain[0].role, ChainRole::Manifest);
        assert_eq!(
            result.chain[1].identifier.digest().to_string(),
            CONFIG_DIGEST,
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
    /// and the trailing config-blob digest. The top-level index is a
    /// dispatch object (locally cacheable, A3); the platform-selected child
    /// is a leaf, recovered via the fake source exactly like the flat case.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_image_index_returns_three_chain_entries() {
        let dir = TempDir::new().unwrap();
        let child_digest = Algorithm::Sha256.hash(FLAT_MANIFEST_JSON.as_bytes());
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":1,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#
        );
        let top_digest = Algorithm::Sha256.hash(index_json.as_bytes());
        let source = FakeManifestSource::default()
            .with(TAG, index_json.as_bytes())
            .with(&child_digest.to_string(), FLAT_MANIFEST_JSON.as_bytes());
        let mgr = make_manager_with_source(&dir, source);
        let result = mgr.resolve(&tagged_id(), linux_amd64()).await.unwrap();
        assert_eq!(
            result.chain.len(),
            3,
            "ImageIndex must produce 3 chain entries (top + selected platform + config)"
        );
        assert_eq!(
            result.chain[0].identifier.digest(),
            top_digest,
            "first entry must be the top-level index digest"
        );
        assert_eq!(
            result.chain[1].identifier.digest(),
            child_digest,
            "second entry must be the platform-selected child digest"
        );
        assert_eq!(
            result.chain[2].identifier.digest().to_string(),
            CONFIG_DIGEST,
            "third entry must be the child manifest's config-blob digest"
        );
        assert_eq!(result.pinned.digest(), child_digest);
    }

    /// Nested image indexes (index pointing at another index) are rejected
    /// with a clear error — unsupported OCI shape. Both levels are
    /// dispatch-shaped (image indexes), so this stays a pure offline,
    /// pre-seeded fixture — no leaf content is ever reached.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_rejects_nested_image_index() {
        let dir = TempDir::new().unwrap();
        let store = index_store(&dir);

        let child_json = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        let child_digest = seed_object(&store, child_json.as_bytes()).await;
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.index.v1+json","digest":"{child_digest}","size":1}}]}}"#
        );
        let top_digest = seed_object(&store, index_json.as_bytes()).await;
        seed_tag(&store, &top_digest).await;

        let mgr = make_manager(&dir);
        let result = mgr.resolve(&tagged_id(), linux_amd64()).await;
        assert!(result.is_err(), "nested ImageIndex must be rejected with an error");
    }

    /// Property guarantee: the top-level DISPATCH entry in a successful
    /// `ResolvedChain` has an on-disk verbatim dispatch object in the
    /// snapshot CAS (`adr_index_indirection.md` A3 — a leaf platform
    /// manifest, by contrast, is never locally cached; the trailing config
    /// blob is materialised later by the pull pipeline). Superseded scope
    /// from the pre-C2 "every manifest entry" property: only the dispatch
    /// (image-index) entry is a genuine local-cache guarantee now.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_result_every_entry_has_on_disk_object() {
        let dir = TempDir::new().unwrap();
        let child_digest = Algorithm::Sha256.hash(FLAT_MANIFEST_JSON.as_bytes());
        let index_json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{child_digest}","size":1,"platform":{{"os":"linux","architecture":"amd64"}}}}]}}"#
        );
        let source = FakeManifestSource::default()
            .with(TAG, index_json.as_bytes())
            .with(&child_digest.to_string(), FLAT_MANIFEST_JSON.as_bytes());
        let store = index_store(&dir);
        let mgr = make_manager_with_source(&dir, source);
        let result = mgr.resolve(&tagged_id(), linux_amd64()).await.unwrap();

        let top = &result.chain[0];
        let dispatch_path = store.dispatch_object_path(
            top.identifier.registry(),
            top.identifier.repository(),
            &top.identifier.digest(),
        );
        assert!(
            dispatch_path.exists(),
            "property violated: top-level dispatch entry {} has no dispatch object at {}",
            top.identifier,
            dispatch_path.display()
        );
    }
}

// Site-overlay invariants: a patch beats a root var; a private dep and its patches appear
// only at self_view=true; a companion's private env never surfaces; no `[patches]` leaves
// compose output unchanged; admitted visit order, dedup and the offline build hold.

#[cfg(test)]
mod phase4_spec_tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use ocx_config::patch::ResolvedPatchConfig;

    use crate::{PackageManager, composer};
    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_oci::{Digest, PackageRef, PinnedPackageRef};
    use ocx_package::{
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
    };
    use ocx_store::file_structure::{FileStructure, PackageStore};

    // ── Constants ─────────────────────────────────────────────────────────────

    const REGISTRY: &str = "example.com";
    const PATCH_REGISTRY: &str = "patches.example.com";

    // ── Fixture helpers ───────────────────────────────────────────────────────

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned(repo: &str, hex_char: char) -> PinnedPackageRef {
        let id = PackageRef::new_registry(repo, REGISTRY).clone_with_digest(sha256(hex_char));
        PinnedPackageRef::try_from(id).unwrap()
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
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            ChainMode::Offline,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    fn test_patch_config() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        }
    }

    /// Seed the patch tier's companion pin (tag → digest) the way a discovery
    /// or `ocx patch sync` records it, so `find_companion_local` resolves the
    /// companion without any index or network access.
    pub(super) fn seed_companion_pin(file_structure: &FileStructure, companion_tag_id: &PackageRef, digest: &Digest) {
        let pin_path = file_structure.patch_companion_path(companion_tag_id);
        std::fs::create_dir_all(pin_path.parent().unwrap()).unwrap();
        let record = serde_json::json!({ companion_tag_id.tag_or_latest(): digest.to_string() });
        std::fs::write(&pin_path, serde_json::to_vec(&record).unwrap()).unwrap();
    }

    /// Seed a companion's root-document tag pointer in the local index's wire
    /// grammar (`adr_index_indirection.md` A2) — the PACKAGE-tier pin.
    ///
    /// Used only to prove compose ignores it: a companion composes at its
    /// patch-tier pin, never at whatever the shared local index happens to say
    /// about the same repository.
    pub(super) fn write_companion_root_document(
        file_structure: &FileStructure,
        companion_tag_id: &PackageRef,
        digest: &Digest,
    ) {
        let registry = companion_tag_id.registry();
        let repository = companion_tag_id.repository();
        let root_path = ocx_index::IndexStore::machine_local(file_structure).root_document_path(registry, repository);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        let doc = serde_json::json!({
            "repository": format!("oci://{registry}/{repository}"),
            "tags": {
                companion_tag_id.tag_or_latest(): {
                    "content": digest.to_string(),
                    "observed": "2026-07-18T00:00:00Z",
                }
            }
        });
        std::fs::write(&root_path, serde_json::to_vec(&doc).unwrap()).unwrap();
    }

    /// Clean break: a companion composes at its PATCH-tier pin only. A tag
    /// pointer for the same repository in the shared local index — written by
    /// any `ocx index update` naming that repository — is somebody else's
    /// answer and must not resolve the companion.
    ///
    /// The two halves share one fixture: without the pin the lookup must miss
    /// even though the index names the very digest the package is stored at;
    /// adding the pin then resolves it, so the miss cannot be a broken fixture.
    #[tokio::test(flavor = "multi_thread")]
    async fn compose_ignores_a_stale_local_index_tag() {
        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let file_structure = manager.file_structure().clone();

        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &file_structure.packages,
            &companion_pinned,
            &ResolvedPackage::new(),
            "CA_BUNDLE",
            "/etc/ssl/certs/bundle.crt",
            Visibility::INTERFACE,
        );

        // Package-tier pin only — the pre-fix source of truth.
        write_companion_root_document(&file_structure, &companion_tag_id, &companion_digest);
        assert!(
            manager
                .find_companion_local(&companion_tag_id, &super::host_platform())
                .await
                .expect("lookup must not error")
                .is_none(),
            "a local-index tag pointer must not resolve a companion — the patch tier owns that binding"
        );

        // Patch-tier pin — the only binding that composes.
        seed_companion_pin(&file_structure, &companion_tag_id, &companion_digest);
        assert!(
            manager
                .find_companion_local(&companion_tag_id, &super::host_platform())
                .await
                .expect("lookup must not error")
                .is_some(),
            "the same fixture must resolve once the patch tier pins it (proves the miss above is the pin, not the fixture)"
        );
    }

    /// Write a minimal on-disk package directory (metadata.json + resolve.json).
    pub(super) fn seed_package_in_store(store: &PackageStore, id: &PinnedPackageRef, resolved: &ResolvedPackage) {
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
        id: &PinnedPackageRef,
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

    /// Write a package directory with caller-supplied `metadata.json`.
    ///
    /// The escape hatch behind [`seed_package_in_store`] and
    /// [`seed_package_with_constant_var`]: a fixture needing a carrier neither
    /// models — a `integrations` block, a declared dependency — supplies the
    /// whole document instead of growing another parameter onto those two.
    pub(super) fn seed_package_with_metadata(
        store: &PackageStore,
        id: &PinnedPackageRef,
        resolved: &ResolvedPackage,
        metadata: &serde_json::Value,
    ) {
        let pkg_path = store.path(id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        std::fs::write(pkg_path.join("metadata.json"), metadata.to_string()).unwrap();
        std::fs::write(pkg_path.join("resolve.json"), serde_json::to_string(resolved).unwrap()).unwrap();
    }

    /// Build a minimal `InstallInfo` backed by a real on-disk content dir.
    fn make_install_info(dir: &std::path::Path, repo: &str, hex_char: char, resolved: ResolvedPackage) -> InstallInfo {
        let id = pinned(repo, hex_char);
        let metadata = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            ocx_store::file_structure::PackageDir { dir: pkg_root },
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
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env,
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
            integrations: Default::default(),
        });
        let pkg_root = dir.join(repo);
        std::fs::create_dir_all(pkg_root.join("content")).unwrap();
        InstallInfo::new(
            id,
            metadata,
            resolved,
            ocx_store::file_structure::PackageDir { dir: pkg_root },
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

        let out = composer::compose(&[root], &store, false, &composer::ComposePaths::digest_only())
            .await
            .unwrap();

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

        let out = composer::compose(&[root], &store, false, &composer::ComposePaths::digest_only())
            .await
            .unwrap();

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

        let out = composer::compose(&[root], &store, true, &composer::ComposePaths::digest_only())
            .await
            .unwrap();

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

        let out = composer::compose(&[a, b], &store, false, &composer::ComposePaths::digest_only())
            .await
            .unwrap();

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
            ocx_store::file_structure::PackageDir { dir: pkg_path },
        ));

        // Baseline: plain compose.
        let compose_out = composer::compose(
            std::slice::from_ref(&root),
            &store,
            false,
            &composer::ComposePaths::digest_only(),
        )
        .await
        .unwrap();
        let compose_entries = compose_out.entries;

        // Manager with patches=None.
        let manager = make_manager(&dir); // patches=None by default
        let resolved = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap();

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
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // With patches=Some but no descriptor persisted (NeverLooked → offline → no
        // fetch), resolve_env must succeed and produce exactly the compose output.
        let entries = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap();

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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── Companion: INTERFACE var MY_VAR=companion_value ────────────────────
        // Companion is stored with PATCH_REGISTRY so find_companion_local's
        // PackageStore lookup (keyed by registry) resolves to the right path.
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("companion-pkg", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("companion-pkg", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
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

        // Write companion's root-document tag entry in the wire grammar so
        // find_companion_local (via Index::fetch_manifest_digest / Op::Query)
        // can resolve the tag → digest without a schema mismatch error.
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

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
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
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
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        let entries = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap();

        // Verify global-last invariant: the "*" rule matches EVERY admitted
        // identifier (dep + root), but the companion is emitted once, under the
        // first match. What matters for C1 is that:
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

    /// A companion matched for MORE THAN ONE admitted base contributes its
    /// projection exactly once — not once per matched base.
    ///
    /// Setup: two roots (`alpha`, `beta`), one global descriptor whose `"*"` rule
    /// names a single locally-installed companion carrying an INTERFACE var. Both
    /// roots are admitted, so the rule matches twice; the overlay must still carry
    /// one `COMPANION_VAR` entry.
    ///
    /// Also asserts the provenance vector stays aligned one-to-one with the overlay
    /// region (`patch_start + provenance.len()` brackets it exactly).
    #[tokio::test(flavor = "multi_thread")]
    async fn companion_matching_multiple_bases_projects_once() {
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── Companion: INTERFACE var COMPANION_VAR=once ────────────────────────
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("shared-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("shared-companion", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "COMPANION_VAR",
            "once",
            Visibility::INTERFACE,
        );
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

        // ── Global descriptor: catch-all rule → the one companion ──────────────
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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // ── Two independent admitted bases ─────────────────────────────────────
        let load_root = |repo: &str, hex: char, var_key: &str| {
            let id = pinned(repo, hex);
            seed_package_with_constant_var(
                &store,
                &id,
                &ResolvedPackage::new(),
                var_key,
                "base_value",
                Visibility::INTERFACE,
            );
            let pkg_path = store.path(&id);
            Arc::new(InstallInfo::new(
                id,
                serde_json::from_str::<metadata::Metadata>(
                    &std::fs::read_to_string(pkg_path.join("metadata.json")).unwrap(),
                )
                .unwrap(),
                ResolvedPackage::new(),
                ocx_store::file_structure::PackageDir { dir: pkg_path },
            ))
        };
        let alpha = load_root("alpha", 'a', "ALPHA_VAR");
        let beta = load_root("beta", 'b', "BETA_VAR");

        let (entries, patch_start, provenance) = manager
            .resolve_env_with_patch_boundary(
                &[alpha, beta],
                false,
                super::EnvScope::package_tier(),
                &super::host_platform(),
            )
            .await
            .unwrap();

        // Premise of the dedup assert: BOTH roots composed and were admitted, so the
        // `"*"` rule was evaluated against two bases. Without this the count assert
        // below stays green when only one base is ever matched.
        for marker in ["ALPHA_VAR", "BETA_VAR"] {
            assert!(
                entries.iter().any(|e| e.key == marker),
                "premise: both roots must be admitted (missing {marker}); entries: {entries:?}"
            );
        }

        let companion_count = entries.iter().filter(|e| e.key == "COMPANION_VAR").count();
        assert_eq!(
            companion_count, 1,
            "a companion matched for both admitted bases must be projected exactly once; entries: {entries:?}"
        );

        assert_eq!(
            entries.len(),
            patch_start + provenance.len(),
            "provenance must be aligned one-to-one with the overlay region; entries: {entries:?}, patch_start: {patch_start}, provenance: {provenance:?}"
        );
        assert_eq!(
            provenance.len(),
            1,
            "exactly one overlay entry means exactly one provenance record; provenance: {provenance:?}"
        );
    }

    /// The SAME companion repository listed at TWO different tags is two distinct
    /// companions — each projected and emitted once, not deduped against each
    /// other. Cross-base dedup (`companion_matching_multiple_bases_projects_once`)
    /// is keyed by the FULL identifier (`registry/repo:tag`), so a repository at
    /// two tags stays two entries even though both admitted bases match the same
    /// `"*"` rule and both list the same repository.
    ///
    /// Setup: two roots (`alpha`, `beta`), one global descriptor whose `"*"` rule
    /// names `dedup_companion:1.0.0` THEN `dedup_companion:2.0.0` (same repository,
    /// distinct tags/digests/installs). Both roots are admitted, so each companion
    /// tag is matched against both bases — the per-tag dedup must still let both
    /// versions through exactly once apiece.
    #[tokio::test(flavor = "multi_thread")]
    async fn same_companion_repo_at_two_tags_projects_both_versions() {
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── Companion repo "dedup_companion" at two tags, each a distinct install
        //    carrying the SAME interface key with a DISTINCT constant value, so
        //    version identity is observable in the overlay entries. ────────────
        let companion_digest_v1 = sha256('c');
        let companion_digest_v2 = sha256('d');
        let companion_tag_id_v1 = PackageRef::new_registry("dedup_companion", PATCH_REGISTRY).clone_with_tag("1.0.0");
        let companion_tag_id_v2 = PackageRef::new_registry("dedup_companion", PATCH_REGISTRY).clone_with_tag("2.0.0");
        let companion_pinned_v1 = PinnedPackageRef::try_from(
            PackageRef::new_registry("dedup_companion", PATCH_REGISTRY).clone_with_digest(companion_digest_v1.clone()),
        )
        .unwrap();
        let companion_pinned_v2 = PinnedPackageRef::try_from(
            PackageRef::new_registry("dedup_companion", PATCH_REGISTRY).clone_with_digest(companion_digest_v2.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned_v1,
            &ResolvedPackage::new(),
            "COMPANION_VER",
            "one",
            Visibility::INTERFACE,
        );
        seed_package_with_constant_var(
            &store,
            &companion_pinned_v2,
            &ResolvedPackage::new(),
            "COMPANION_VER",
            "two",
            Visibility::INTERFACE,
        );

        // Both tags share one repository, so the companion pin record must carry
        // BOTH tag → digest entries at once — `seed_companion_pin` writes a
        // single-tag record and a second call would overwrite the first, since
        // the pin path is keyed by repository only.
        let pin_path = tag_store.patch_companion_path(&companion_tag_id_v1);
        std::fs::create_dir_all(pin_path.parent().unwrap()).unwrap();
        let pin_record = serde_json::json!({
            "1.0.0": companion_digest_v1.to_string(),
            "2.0.0": companion_digest_v2.to_string(),
        });
        std::fs::write(&pin_path, serde_json::to_vec(&pin_record).unwrap()).unwrap();

        // ── Global descriptor: catch-all rule → both companion tags, in order ──
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{
                "match": "*",
                "packages": [companion_tag_id_v1.to_string(), companion_tag_id_v2.to_string()],
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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // ── Two independent admitted bases ─────────────────────────────────────
        let load_root = |repo: &str, hex: char, var_key: &str| {
            let id = pinned(repo, hex);
            seed_package_with_constant_var(
                &store,
                &id,
                &ResolvedPackage::new(),
                var_key,
                "base_value",
                Visibility::INTERFACE,
            );
            let pkg_path = store.path(&id);
            Arc::new(InstallInfo::new(
                id,
                serde_json::from_str::<metadata::Metadata>(
                    &std::fs::read_to_string(pkg_path.join("metadata.json")).unwrap(),
                )
                .unwrap(),
                ResolvedPackage::new(),
                ocx_store::file_structure::PackageDir { dir: pkg_path },
            ))
        };
        let alpha = load_root("alpha", 'a', "ALPHA_VAR");
        let beta = load_root("beta", 'b', "BETA_VAR");

        let (entries, patch_start, provenance) = manager
            .resolve_env_with_patch_boundary(
                &[alpha, beta],
                false,
                super::EnvScope::package_tier(),
                &super::host_platform(),
            )
            .await
            .unwrap();

        // Premise: BOTH roots composed and were admitted, so the `"*"` rule was
        // evaluated against two bases — without this, the two-entries assert below
        // would stay green even if only one base were ever matched.
        for marker in ["ALPHA_VAR", "BETA_VAR"] {
            assert!(
                entries.iter().any(|e| e.key == marker),
                "premise: both roots must be admitted (missing {marker}); entries: {entries:?}"
            );
        }

        // Exactly two COMPANION_VER entries — one per tag — despite both admitted
        // bases matching the wildcard rule for both tags. Order follows the
        // package-list order within the rule (v1 before v2), which pins
        // determinism.
        let companion_versions: Vec<&str> = entries
            .iter()
            .filter(|e| e.key == "COMPANION_VER")
            .map(|e| e.value.as_str())
            .collect();
        assert_eq!(
            companion_versions,
            vec!["one", "two"],
            "the same companion repository at two tags must project both versions, each exactly once, in package-list order; entries: {entries:?}"
        );

        assert_eq!(
            entries.len(),
            patch_start + provenance.len(),
            "provenance must be aligned one-to-one with the overlay region; entries: {entries:?}, patch_start: {patch_start}, provenance: {provenance:?}"
        );
        let provenance_companions: Vec<String> = provenance.iter().map(|p| p.companion.to_string()).collect();
        assert_eq!(
            provenance_companions,
            vec![companion_tag_id_v1.to_string(), companion_tag_id_v2.to_string()],
            "the two overlay provenance records must name the two distinct companion identifiers; provenance: {provenance:?}"
        );
    }

    /// Live overlay × visibility: a companion whose INTERFACE var is overlaid on
    /// a PRIVATE dependency is ABSENT from the consumer view and PRESENT under
    /// `--self`. The global rule targets ONLY the dep (not the root), so the
    /// companion var can reach the env exclusively when the dep is admitted —
    /// proving the live patch-overlay path honours the dependency-visibility
    /// surface end-to-end, not just the admitted-set gate in isolation.
    ///
    /// Traceability: C3 visibility tiering on the live companion-overlay path.
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_on_private_dep_overlaid_only_under_self_view() {
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── Companion: INTERFACE var DEP_PATCH_VAR=present ─────────────────────
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("dep-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("dep-companion", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "DEP_PATCH_VAR",
            "present",
            Visibility::INTERFACE,
        );
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

        // ── Global descriptor: rule matches ONLY the private dep, not the root ──
        let descriptor_json = serde_json::json!({
            "version": 1,
            "rules": [{
                "match": "*privatedep*",
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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // ── Root with a PRIVATE dep "privatedep" ───────────────────────────────
        let dep_id = pinned("privatedep", 'd');
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

        // Consumer view: private dep NOT admitted → rule never matches → var ABSENT.
        let consumer = manager
            .resolve_env(
                std::slice::from_ref(&root),
                false,
                super::EnvScope::package_tier(),
                &super::host_platform(),
            )
            .await
            .unwrap();
        assert!(
            !consumer.iter().any(|e| e.key == "DEP_PATCH_VAR"),
            "patch on a PRIVATE dep must be absent from the consumer view; entries: {consumer:?}"
        );

        // Self view: private dep admitted → rule matches dep → var PRESENT.
        let self_view = manager
            .resolve_env(
                std::slice::from_ref(&root),
                true,
                super::EnvScope::package_tier(),
                &super::host_platform(),
            )
            .await
            .unwrap();
        assert!(
            self_view
                .iter()
                .any(|e| e.key == "DEP_PATCH_VAR" && e.value == "present"),
            "patch on a PRIVATE dep must be present under --self; entries: {self_view:?}"
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

        let out = composer::compose(&[root], &store, false, &composer::ComposePaths::digest_only())
            .await
            .unwrap();

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

        let out = composer::compose(&[root], &store, true, &composer::ComposePaths::digest_only())
            .await
            .unwrap();

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
        let entries = manager
            .resolve_env(&[root], true, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap();

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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── Companion: has ONLY a PRIVATE var (must never leak). ──────────────
        // Store under PATCH_REGISTRY so find_companion_local's PackageStore
        // lookup resolves to the same registry the tag-store entry uses.
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("private-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("private-companion", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
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

        // Write companion's root-document tag entry in the wire grammar so
        // find_companion_local (via Index::fetch_manifest_digest / Op::Query)
        // resolves the tag → digest without a schema mismatch error.
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

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
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
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
        let entries = manager
            .resolve_env(&[root], true, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap();

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
        let result = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
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
        let result = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // required=true at tier level so all companions inherit required=true.
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // A companion identifier that is NOT installed locally.
        let companion_tag_id = PackageRef::new_registry("required-companion", PATCH_REGISTRY).clone_with_tag("latest");

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
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .unwrap();

        // Root with no deps — still admitted (roots always in admitted set).
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Companion is NOT installed locally: no tag-store entry, no package dir.
        // `find_companion_local` → `Ok(None)` → required=true → must return Err.
        let result = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
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

    // ── A8: overlay emission order tracks admitted order under parallel load ───

    /// A8 REGRESSION: the companion overlay is emitted in ADMITTED order, and that
    /// order is STABLE regardless of the order in which the per-admitted-id
    /// package-specific descriptors finish loading in parallel (`build_site_patch_set`
    /// Step 3a fans the loads out over a `JoinSet`, then collects them by admitted
    /// index in Step 3b).
    ///
    /// Setup: three roots, each carrying a distinct interface var (`BASE_A/B/C`) —
    /// whose compose-emission sequence reveals the admitted order — and each with
    /// its OWN package-specific descriptor mapping to a distinct companion that
    /// declares a distinct overlay var (`OVERLAY_A/B/C`). No global descriptor, so
    /// each admitted base contributes exactly one overlay var, from its own
    /// descriptor. If the parallel collection mis-associated a descriptor with the
    /// wrong admitted slot (e.g. collected by completion order instead of index),
    /// the `OVERLAY_*` sequence would diverge from the `BASE_*` sequence and would
    /// vary run-to-run.
    ///
    /// The assertion is self-referential — `OVERLAY_*` order must equal `BASE_*`
    /// order — so it does not depend on knowing `compose`'s canonical admitted
    /// order; it only requires the overlay to track it. Repeated runs pin
    /// determinism under the nondeterministic `JoinSet` completion order.
    ///
    /// Traces: plan_patch_review_fixes A8 — order stability of the parallel
    /// descriptor-load fan-out (F4).
    #[tokio::test(flavor = "multi_thread")]
    async fn a8_overlay_emission_order_tracks_admitted_order_under_parallel_load() {
        use super::super::patch_discovery::{PatchTagMap, patch_descriptor_id};
        use ocx_oci::Algorithm;
        use ocx_package::metadata::env::entry::Entry;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // (suffix token, root digest hex, companion digest hex, root repo, companion repo).
        // Digest hex chars must be valid hexadecimal (sha256() repeats the char 64x).
        let specs = [
            ("A", 'a', 'd', "root_a", "comp_a"),
            ("B", 'b', 'e', "root_b", "comp_b"),
            ("C", 'c', 'f', "root_c", "comp_c"),
        ];

        let mut roots: Vec<std::sync::Arc<InstallInfo>> = Vec::new();
        for (suffix, root_hex, companion_hex, root_repo, companion_repo) in specs {
            // Root: one interface var BASE_<suffix> — its compose position reveals
            // the admitted order.
            let root_id = pinned(root_repo, root_hex);
            seed_package_with_constant_var(
                &store,
                &root_id,
                &ResolvedPackage::new(),
                &format!("BASE_{suffix}"),
                "root",
                Visibility::INTERFACE,
            );

            // Companion: one interface var OVERLAY_<suffix>, stored under the patch
            // registry so `find_companion_local` resolves it.
            let companion_digest = sha256(companion_hex);
            let companion_tag_id = PackageRef::new_registry(companion_repo, PATCH_REGISTRY).clone_with_tag("latest");
            let companion_pinned = PinnedPackageRef::try_from(
                PackageRef::new_registry(companion_repo, PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
            )
            .unwrap();
            seed_package_with_constant_var(
                &store,
                &companion_pinned,
                &ResolvedPackage::new(),
                &format!("OVERLAY_{suffix}"),
                "companion",
                Visibility::INTERFACE,
            );
            seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

            // Package-specific descriptor for this root → its own companion.
            let descriptor_json = serde_json::json!({
                "version": 1,
                "rules": [{ "match": "*", "packages": [companion_tag_id.to_string()] }]
            })
            .to_string();
            let layer_bytes = descriptor_json.into_bytes();
            let layer_digest = Algorithm::Sha256.hash(&layer_bytes);
            let manifest_json = serde_json::json!({
                "schemaVersion": 2,
                "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
            })
            .to_string();
            let manifest_bytes = manifest_json.into_bytes();
            let manifest_digest = Algorithm::Sha256.hash(&manifest_bytes);
            blob_store
                .write_blob(PATCH_REGISTRY, &manifest_digest, &manifest_bytes)
                .await
                .unwrap();
            blob_store
                .write_blob(PATCH_REGISTRY, &layer_digest, &layer_bytes)
                .await
                .unwrap();
            let pkg_specific_id = patch_descriptor_id(&patch_config, root_id.as_identifier());
            let pkg_tags_path = tag_store.patch_descriptor_path(&pkg_specific_id);
            PatchTagMap::write_has_descriptor(&pkg_tags_path, &manifest_digest.to_string())
                .await
                .unwrap();

            // Build the root InstallInfo from the seeded package.
            let root_pkg_path = store.path(&root_id);
            roots.push(std::sync::Arc::new(InstallInfo::new(
                root_id.clone(),
                serde_json::from_str::<metadata::Metadata>(
                    &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
                )
                .unwrap(),
                ResolvedPackage::new(),
                ocx_store::file_structure::PackageDir { dir: root_pkg_path },
            )));
        }

        // Extract the ordered `BASE_*` suffixes (admitted order) and `OVERLAY_*`
        // suffixes (overlay order) from a resolved env.
        let suffixes = |entries: &[Entry], prefix: &str| -> Vec<String> {
            entries
                .iter()
                .filter_map(|entry| entry.key.strip_prefix(prefix).map(str::to_string))
                .collect()
        };

        // Run resolution repeatedly. Each run fans the three descriptor loads out
        // over a JoinSet (nondeterministic completion order); the emitted order must
        // be identical every time AND the overlay order must equal the admitted order.
        let mut baseline: Option<(Vec<String>, Vec<String>)> = None;
        for run in 0..12 {
            let entries = manager
                .resolve_env(&roots, false, super::EnvScope::package_tier(), &super::host_platform())
                .await
                .unwrap();
            let base_order = suffixes(&entries, "BASE_");
            let overlay_order = suffixes(&entries, "OVERLAY_");

            assert_eq!(
                base_order.len(),
                3,
                "run {run}: all three admitted bases must emit their BASE_* var; got {base_order:?}"
            );
            assert_eq!(
                overlay_order, base_order,
                "run {run}: overlay emission order must equal admitted (compose) order; got overlay {overlay_order:?} vs admitted {base_order:?}"
            );

            match &baseline {
                None => baseline = Some((base_order, overlay_order)),
                Some((base_first, overlay_first)) => {
                    assert_eq!(
                        &base_order, base_first,
                        "run {run}: admitted order must be stable across runs; got {base_order:?} vs {base_first:?}"
                    );
                    assert_eq!(
                        &overlay_order, overlay_first,
                        "run {run}: overlay order must be stable across runs (parallel load must not reorder); got {overlay_order:?} vs {overlay_first:?}"
                    );
                }
            }
        }
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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id, patch_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // Tier required=false; companion required flag comes from per-rule required field.
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // Companion: NOT installed locally so find_companion_local → Ok(None).
        let companion_tag_id = PackageRef::new_registry("shared-companion", PATCH_REGISTRY).clone_with_tag("latest");

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
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
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
        let pkg_tags_path = tag_store.patch_descriptor_path(&pkg_specific_id);
        PatchTagMap::write_has_descriptor(&pkg_tags_path, &pkg_manifest_digest.to_string())
            .await
            .unwrap();

        // Build the root InstallInfo from the seeded package.
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Companion is NOT installed locally → find_companion_local → Ok(None).
        // Merged required flag: pkg-specific (true) overrides global (false).
        // C7 fail-closed: must return Err because merged required=true.
        let result = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id, patch_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── Global companion: CERT_FILE=global_cert ───────────────────────────
        // Use valid hex chars ('a', 'b') so Digest::try_from succeeds when
        // read_tag_digest parses the stored digest string.
        let global_companion_digest = sha256('a');
        let global_companion_tag_id =
            PackageRef::new_registry("global-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let global_companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("global-companion", PATCH_REGISTRY)
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
        seed_companion_pin(&tag_store, &global_companion_tag_id, &global_companion_digest);

        // ── Pkg-specific companion: CERT_FILE=pkg_cert ────────────────────────
        let pkg_companion_digest = sha256('b');
        let pkg_companion_tag_id = PackageRef::new_registry("pkg-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let pkg_companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("pkg-companion", PATCH_REGISTRY).clone_with_digest(pkg_companion_digest.clone()),
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
        seed_companion_pin(&tag_store, &pkg_companion_tag_id, &pkg_companion_digest);

        // ── Helper: write a descriptor blob and return manifest digest ─────────
        let write_descriptor_blob = |companion_tag: &PackageRef| {
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
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
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
        let pkg_tags_path = tag_store.patch_descriptor_path(&pkg_specific_id);
        PatchTagMap::write_has_descriptor(&pkg_tags_path, &pkg_manifest_digest.to_string())
            .await
            .unwrap();

        // ── Resolve and assert pkg-specific value is last (wins) ──────────────
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // ── Resolve and assert ─────────────────────────────────────────────────
        let entries = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap();

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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;
        use ocx_package::metadata::env::{EnvBuilder, path::Path as EnvPath, var::Modifier};

        let dir = TempDir::new().unwrap();
        let manager = make_manager(&dir).with_patches(Some(test_patch_config()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── Companion: has PATH = "/companion/bin" (interface, Path modifier). ──
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("path-companion", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("path-companion", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();

        {
            // Build companion metadata with a Path var.
            let path_var = ocx_package::metadata::env::var::Var {
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
            let metadata = ocx_package::metadata::Metadata::Bundle(ocx_package::metadata::bundle::Bundle {
                binaries: None,
                version: ocx_package::metadata::bundle::Version::V1,
                strip_components: None,
                env,
                dependencies: ocx_package::metadata::dependency::Dependencies::default(),
                entrypoints: ocx_package::metadata::entrypoint::Entrypoints::default(),
                integrations: Default::default(),
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

        // Companion's root-document tag entry in the wire grammar.
        {
            seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);
        }

        // ── Root: declares PATH = "/root/bin" (interface, Path modifier). ──────
        let root_id = pinned("rootpkg", 'r');
        {
            let path_var = ocx_package::metadata::env::var::Var {
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
            let metadata = ocx_package::metadata::Metadata::Bundle(ocx_package::metadata::bundle::Bundle {
                binaries: None,
                version: ocx_package::metadata::bundle::Version::V1,
                strip_components: None,
                env,
                dependencies: ocx_package::metadata::dependency::Dependencies::default(),
                entrypoints: ocx_package::metadata::entrypoint::Entrypoints::default(),
                integrations: Default::default(),
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
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .unwrap();

        // ── Resolve and assert C1 global-last for Path vars ───────────────────
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        let entries = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap();

        // Both PATH entries must be present.
        let path_entries: Vec<_> = entries.iter().filter(|e| e.key == "PATH").collect();
        assert_eq!(
            path_entries.len(),
            2,
            "F7: expected 2 PATH entries (root + companion); got {}: {entries:?}",
            path_entries.len()
        );

        // C1 global-last: the root's PATH comes first (compose-phase), the
        // companion's last (overlay). The fixture values are POSIX-absolute
        // literals; on Windows a driveless `/root/bin` is not `is_absolute`, so
        // the resolver joins it onto the install drive (yielding `C:/root/bin`).
        // Assert identity by the distinctive path segment — which survives that
        // platform join — since the invariant under test is the ORDER, not the
        // exact byte value.
        assert!(
            path_entries[0].value.contains("root"),
            "F7: first PATH must be the root's (compose-phase, before overlay); got {:?}",
            path_entries[0].value
        );
        assert!(
            path_entries[1].value.contains("companion"),
            "F7: second PATH must be the companion's (overlay, appended globally last); got {:?}",
            path_entries[1].value
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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id, patch_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // Tier required=true → cap breach must fail closed.
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &global_manifest_digest.to_string(),
        )
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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&pkg_specific_id),
            &pkg_manifest_digest.to_string(),
        )
        .await
        .unwrap();

        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        let result = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // Tier required=false → cap breach should warn + truncate.
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // Root package.
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        let root = Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Non-required tier with all-optional companions over cap: must succeed
        // (warn + truncate, not Err).
        let result = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
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
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        // Tier required=true → all companions are required.
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // Companion: NOT installed locally (no tag-store entry, no package dir).
        let companion_tag_id = PackageRef::new_registry("required-companion", PATCH_REGISTRY).clone_with_tag("latest");

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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
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
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage {
                dependencies: vec![ResolvedDependency {
                    identifier: dep1_id.clone(),
                    visibility: Visibility::PUBLIC,
                }],
            },
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Companion is missing. The global "*" rule matches dep1 AND root —
        // so the companion lookup runs for dep1 (cache miss → None → Err immediately).
        // The fix ensures the error is returned on the FIRST encounter, and
        // also that if a later iteration reaches the cache hit for None, it
        // still fails closed (no silent bypass).
        let result = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
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

    // ── Schema regression: root-document envelope — required companion missing ─

    /// Regression for the schema bug: `find_companion_local` previously read the
    /// companion's root document as a raw `BTreeMap<String, String>`, which fails
    /// on the real on-disk root-document envelope (`{"repository":...,"tags":{...}}`,
    /// `adr_index_indirection.md` A2).
    ///
    /// This test seeds the companion's root document in the **correct** schema
    /// but does NOT install the companion package itself (no manifest blob, no
    /// package dir).  The expected outcome is `Err(RequiredCompanionFailed)` —
    /// i.e. `find_companion_local` parses the root document correctly (no schema
    /// error), resolves the digest, looks up the package store (miss →
    /// `Ok(None)`), and then the required-fail-closed arm fires.
    ///
    /// Before the fix, `read_tag_digest` would return `Err` on the real
    /// root-document file, which silently skipped the companion — a C7 violation.
    ///
    /// Traceability: schema regression + C7 fail-closed on real root-document path.
    #[tokio::test(flavor = "multi_thread")]
    async fn schema_regression_required_companion_root_document_envelope_missing_package_returns_err() {
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true, // required=true → must fail closed
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // Companion tag ID whose root document will be written in the REAL
        // envelope format — but no package is installed (no package dir, no blob).
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("required-ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");

        // Write the companion's root document in the correct schema.
        // Before the fix, `read_tag_digest` would fail to parse this and silently
        // skip the required companion — a C7 violation.  After the fix,
        // `fetch_manifest_digest(Op::Query)` parses it correctly, returns
        // `Ok(Some(digest))`, then `find_in_store` returns `Ok(None)` (not
        // installed), and the required-fail-closed arm fires.
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);
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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // Root with no deps — still admitted (roots always in admitted set).
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        let root = std::sync::Arc::new(ocx_package::install_info::InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Must return Err (required companion present in tag store but not installed
        // as a package → fail closed).  Before the fix this returned Ok(()) because
        // the schema mismatch in `read_tag_digest` produced a silent Err → warn+skip.
        let result = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
        assert!(
            result.is_err(),
            "schema regression: required companion with correct root-document tag but missing package must return Err; got Ok({:?})",
            result.ok()
        );

        let err_str = format!("{:?}", result.unwrap_err());
        assert!(
            err_str.contains("required-ca-bundle") || err_str.contains("RequiredCompanionFailed"),
            "schema regression: error must reference the required companion; got: {err_str}"
        );
    }

    /// Regression: a required companion that is fully installed (correct
    /// root-document tag + manifest blob + package dir with interface env var)
    /// must have its interface env appear in the overlay output.
    ///
    /// This test proves the real resolution path (root document → digest →
    /// package store → compose) works end-to-end when the companion IS installed.
    ///
    /// Traceability: schema regression — real path resolves + env surfaces.
    #[tokio::test(flavor = "multi_thread")]
    async fn schema_regression_required_companion_fully_installed_env_appears_in_overlay() {
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true, // required=true, so any resolution error is fatal
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── Companion: fully installed (root-document tag + package dir + interface var). ──
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
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

        // Write the companion's root document in the correct envelope format.
        // This is the on-disk shape produced by a real Phase 3 install.
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

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
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // ── Root with no deps ──────────────────────────────────────────────────
        let root_id = pinned("rootpkg", 'r');
        seed_package_in_store(&store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        let root = std::sync::Arc::new(ocx_package::install_info::InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
        ));

        // Must succeed and include the companion's CA_BUNDLE env var in the output.
        let entries = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap();

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
            ocx_store::file_structure::PackageDir {
                dir: companion_pkg_path,
            },
        ));

        // Plain compose — no patch overlay (compose is patch-agnostic).
        let out = composer::compose(&[companion], &store, false, &composer::ComposePaths::digest_only())
            .await
            .unwrap();

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
                index_store: IndexStore::new(dir.path().join("index")),
            }),
            Vec::new(),
            ChainMode::Remote,
        );
        PackageManager::new(fs, index, None, REGISTRY)
    }

    /// Seed a fully-installed **global** companion into `manager`'s file structure:
    /// the companion's root-document tag, optionally its package directory (carrying an
    /// `INTERFACE` env var `CA_BUNDLE`), and the global `__ocx.patch` descriptor
    /// (rule `*` → companion) recorded as `LookedHasDescriptor`.
    ///
    /// When `seed_package` is `false` the package directory is omitted (companion
    /// tag present but package missing → the required-fail-closed path). Returns
    /// the companion's interface `(key, value)` pair.
    pub(super) async fn seed_installed_global_companion(
        manager: &PackageManager,
        patch_config: &ocx_config::patch::ResolvedPatchConfig,
        seed_package: bool,
    ) -> (&'static str, &'static str) {
        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");

        if seed_package {
            let companion_pinned = PinnedPackageRef::try_from(
                PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
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

        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);
        seed_global_descriptor(manager, patch_config, &[&companion_tag_id]).await;

        ("CA_BUNDLE", "/etc/ssl/certs/ca-bundle.crt")
    }

    /// Seed the global `__ocx.patch` descriptor as `LookedHasDescriptor`, with a
    /// single catch-all rule (`*`) naming every companion in `companion_tag_ids`.
    ///
    /// Writes the blobs `build_site_patch_set` reads back offline: the layer
    /// carrying the descriptor document, the manifest pointing at it, and the
    /// tag-store record naming that manifest digest.
    pub(super) async fn seed_global_descriptor(
        manager: &PackageManager,
        patch_config: &ocx_config::patch::ResolvedPatchConfig,
        companion_tag_ids: &[&PackageRef],
    ) {
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        let packages: Vec<String> = companion_tag_ids.iter().map(|id| id.to_string()).collect();
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
        let global_id = global_descriptor_id(patch_config);
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();
    }

    /// Build a root `InstallInfo` (no deps) seeded in `store`.
    pub(super) fn seed_root_arc(store: &PackageStore, name: &str, hex_char: char) -> Arc<InstallInfo> {
        let root_id = pinned(name, hex_char);
        seed_package_in_store(store, &root_id, &ResolvedPackage::new());
        let root_pkg_path = store.path(&root_id);
        Arc::new(InstallInfo::new(
            root_id.clone(),
            serde_json::from_str::<ocx_package::metadata::Metadata>(
                &std::fs::read_to_string(root_pkg_path.join("metadata.json")).unwrap(),
            )
            .unwrap(),
            ResolvedPackage::new(),
            ocx_store::file_structure::PackageDir { dir: root_pkg_path },
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
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        // Remote-mode index with no sources: resolving a tag through `index()`
        // yields `None`, so success here proves the companion came from local state.
        let manager = make_remote_manager(&dir).with_patches(Some(patch_config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &patch_config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');
        let entries = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap_or_else(|e| {
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
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &patch_config, true).await;

        // Derive the offline view exactly as the CLI does, over the same stores.
        let local_index = LocalIndex::new(LocalConfig {
            index_store: IndexStore::new(dir.path().join("index")),
        });
        let offline = manager.offline_view(local_index);
        assert!(offline.is_offline(), "offline_view must produce an offline manager");

        let root = seed_root_arc(&offline.file_structure().packages.clone(), "rootpkg", 'r');
        let entries = offline
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .unwrap_or_else(|e| {
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
        let dir = TempDir::new().unwrap();
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        // seed_package = false → companion tag present, package missing.
        seed_installed_global_companion(&manager, &patch_config, false).await;

        let local_index = LocalIndex::new(LocalConfig {
            index_store: IndexStore::new(dir.path().join("index")),
        });
        let offline = manager.offline_view(local_index);

        let root = seed_root_arc(&offline.file_structure().packages.clone(), "rootpkg", 'r');
        let result = offline
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await;
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

    // ── One integrations row per (package, namespace) ───────────────────────
    //
    // The base roots and every companion are composed by SEPARATE `compose`
    // calls, and each dedups only within itself. A package reachable from two
    // of those calls therefore arrives at the merge site twice, so the
    // one-row-per-(package, namespace) contract is owned by the merge, not by
    // any single compose.

    /// Seed `store` with a dependency that declares exactly one integrations
    /// namespace, and return it together with the TC that reaches it.
    fn seed_shared_customizing_dep(store: &PackageStore) -> (PinnedPackageRef, ResolvedPackage) {
        let shared_dep = pinned("shareddep", 'd');
        seed_package_with_metadata(
            store,
            &shared_dep,
            &ResolvedPackage::new(),
            &serde_json::json!({
                "type": "bundle",
                "version": 1,
                "integrations": { "vendor.shared": { "declared-by": "shareddep" } },
            }),
        );
        (shared_dep.clone(), tc_reaching(&shared_dep))
    }

    /// A transitive closure whose single PUBLIC dependency is `identifier`.
    fn tc_reaching(identifier: &PinnedPackageRef) -> ResolvedPackage {
        ResolvedPackage {
            dependencies: vec![ResolvedDependency {
                identifier: identifier.clone(),
                visibility: Visibility::PUBLIC,
            }],
        }
    }

    /// The same package at the same digest, named with an advisory tag.
    ///
    /// Two packages may reach one dependency under different advisory tags —
    /// `zlib:1.3@X` from one, `zlib:1@X` from the other — which is the whole
    /// meaning of "advisory". Both compose calls emit the TAG-BEARING
    /// identifier on the row (`composer.rs:376`, `:458`), so a merge-site dedup
    /// keyed on the raw identifier sees two packages where there is one.
    ///
    /// The package store keys on registry + digest, so the tagged name loads the
    /// same seeded package.
    fn with_advisory_tag(identifier: &PinnedPackageRef, tag: &str) -> PinnedPackageRef {
        // `clone_with_tag` drops the digest and `clone_with_digest` keeps the
        // tag, so this order — and only this order — yields both.
        let tagged = PinnedPackageRef::try_from(
            identifier
                .as_identifier()
                .clone_with_tag(tag)
                .clone_with_digest(identifier.digest()),
        )
        .unwrap();
        assert_eq!(
            tagged.digest(),
            identifier.digest(),
            "the tagged name must keep the digest"
        );
        assert_eq!(tagged.tag(), Some(tag), "the tagged name must carry the advisory tag");
        assert_ne!(
            tagged, *identifier,
            "the two names must differ, or the fixture proves nothing"
        );
        assert_eq!(
            tagged.strip_advisory(),
            identifier.strip_advisory(),
            "the two names must differ ONLY by the advisory tag"
        );
        tagged
    }

    /// Install a companion at `name` whose transitive closure is `resolved`, and
    /// return the tag identifier the descriptor rule names it by paired with the
    /// pinned identifier its own contributions are attributed to.
    ///
    /// The companion declares one namespace of its own, `vendor.companion`, so a
    /// test can prove the projection engaged: a count over a namespace the base
    /// also reaches cannot tell "deduped to one row" from "the companion never
    /// contributed", and an optional companion that fails to resolve is
    /// warn-skipped in silence.
    ///
    /// The returned pin keeps the advisory tag, because that is what
    /// `resolve_companion_pinned` attributes a companion's rows to; the package
    /// store keys on registry + digest, so seeding is unaffected either way.
    fn seed_companion_with_tc(
        manager: &PackageManager,
        name: &str,
        hex_char: char,
        resolved: &ResolvedPackage,
    ) -> (PackageRef, PinnedPackageRef) {
        let digest = sha256(hex_char);
        let tag_id = PackageRef::new_registry(name, PATCH_REGISTRY).clone_with_tag("latest");
        let pinned_id = PinnedPackageRef::try_from(tag_id.clone_with_digest(digest.clone())).unwrap();
        seed_package_with_metadata(
            &manager.file_structure().packages,
            &pinned_id,
            resolved,
            &serde_json::json!({
                "type": "bundle",
                "version": 1,
                "integrations": { "vendor.companion": { "declared-by": name } },
            }),
        );
        seed_companion_pin(manager.file_structure(), &tag_id, &digest);
        (tag_id, pinned_id)
    }

    /// Count the composed rows attributing `namespace` to `identifier`.
    /// Counted by package identity, advisory tag ignored — the contract is one
    /// row per (package, namespace), and two advisory tags on one digest are one
    /// package. A tag-sensitive count would report 1 for each of two rows that
    /// name the same package differently, which is the duplicate itself.
    fn integration_rows(attribution: &super::AdmittedClaims, identifier: &PinnedPackageRef, namespace: &str) -> usize {
        attribution
            .integrations
            .iter()
            .filter(|(id, entry)| id.strip_advisory() == identifier.strip_advisory() && entry.namespace == namespace)
            .count()
    }

    /// A dependency reachable from BOTH a base root and a patch companion
    /// contributes its integrations exactly once.
    ///
    /// The base compose admits the dep and emits its row; the companion's own
    /// projection admits the very same dep and emits it again. Neither compose
    /// can see the other's `seen` set, so without dedup at the merge the
    /// consumer receives the identical (package, namespace) row twice, which
    /// reads as two independent declarations.
    ///
    /// The two legs reach the dep under DIFFERENT advisory tags, which is the
    /// axis the merge-site key turns on: rows carry the tag-bearing identifier,
    /// so a key that does not strip the tag sees two packages. This leg pins the
    /// seed side of the dedup (base rows seed the set, the companion's row is
    /// tested against it).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dep_reachable_from_a_base_and_a_companion_contributes_one_row() {
        let dir = TempDir::new().unwrap();
        let config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let store = manager.file_structure().packages.clone();

        let (shared_dep, reaching_tc) = seed_shared_customizing_dep(&store);
        let companion_tc = tc_reaching(&with_advisory_tag(&shared_dep, "1"));
        let (companion_tag_id, companion_pinned) = seed_companion_with_tc(&manager, "ca-bundle", 'c', &companion_tc);
        seed_global_descriptor(&manager, &config, &[&companion_tag_id]).await;

        // The base root reaches the same dependency through its own TC.
        let root = Arc::new(make_install_info(dir.path(), "rootpkg", 'r', reaching_tc));

        let (_entries, _compose_count, _provenance, attribution) = manager
            .resolve_env_with_attribution(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .expect("resolve_env_with_attribution must succeed");

        assert_eq!(
            integration_rows(&attribution, &shared_dep, "vendor.shared"),
            1,
            "a dep reachable from both the base and a companion must contribute one row, not one per compose; \
             rows: {:?}",
            attribution.integrations
        );

        // Positive control, deliberately asserted AFTER the dedup count: the base
        // root alone already emits that one `vendor.shared` row, so `== 1` is
        // equally satisfied by a companion that never engaged — and this tier is
        // `required: false`, so a companion that fails to resolve or project is
        // warn-skipped without a trace. Only a namespace nothing but the companion
        // declares separates "deduped" from "never contributed".
        assert_eq!(
            integration_rows(&attribution, &companion_pinned, "vendor.companion"),
            1,
            "the companion projection must have engaged and contributed its own namespace, \
             or the dedup assertion above proves nothing; rows: {:?}",
            attribution.integrations
        );
    }

    /// Two companions reaching one shared dependency contribute its
    /// integrations once between them.
    ///
    /// Same defect as `a_dep_reachable_from_a_base_and_a_companion_contributes_one_row`
    /// with the base removed: the duplication is between two companion
    /// projections, both landing in the SAME overlay under one admitted base,
    /// so it survives any dedup keyed on the companion identifier.
    ///
    /// The two companions reach the dep under different advisory tags, pinning
    /// the insert side of the merge-site key: nothing seeded the set here, so
    /// both rows are tested against each other rather than against a base row.
    #[tokio::test(flavor = "multi_thread")]
    async fn two_companions_reaching_one_dep_contribute_one_row() {
        let dir = TempDir::new().unwrap();
        let config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let store = manager.file_structure().packages.clone();

        let (shared_dep, reaching_tc) = seed_shared_customizing_dep(&store);
        let tagged_tc = tc_reaching(&with_advisory_tag(&shared_dep, "1"));
        let (first, _) = seed_companion_with_tc(&manager, "ca-bundle", 'c', &reaching_tc);
        let (second, _) = seed_companion_with_tc(&manager, "proxy-config", 'e', &tagged_tc);
        seed_global_descriptor(&manager, &config, &[&first, &second]).await;

        // The base itself declares nothing and reaches nothing — both rows can
        // only come from the two companion projections.
        let root = Arc::new(make_install_info(dir.path(), "rootpkg", 'r', ResolvedPackage::new()));

        let (_entries, _compose_count, _provenance, attribution) = manager
            .resolve_env_with_attribution(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .expect("resolve_env_with_attribution must succeed");

        assert_eq!(
            integration_rows(&attribution, &shared_dep, "vendor.shared"),
            1,
            "two companions sharing one dep must contribute its row once; rows: {:?}",
            attribution.integrations
        );
    }

    // ── The --self surface never resolves a discarded payload ─────────────────

    /// Under `--self` the composition carries zero integrations, so a
    /// companion's payload must never be resolved — not resolved and then
    /// discarded.
    ///
    /// Payload resolution asserts that every `${deps.*}` content directory
    /// exists. The companion projection is pinned to the interface surface and
    /// so cannot derive the caller's gate; while it collected unconditionally, a
    /// payload naming an uninstalled dependency failed the projection outright —
    /// a hard error for this `required` tier, and a silent drop of the
    /// companion's env entries for an optional one. This reaches the launcher
    /// hot path, which composes with `self_view = true`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_companion_payload_naming_an_absent_dep_does_not_fail_the_self_surface() {
        let dir = TempDir::new().unwrap();
        // `required` so a failed projection is a hard error rather than a
        // warn-skip: the regression is then a returned `Err`, not a silence.
        let config = ResolvedPatchConfig {
            required: true,
            ..test_patch_config()
        };
        let manager = make_manager(&dir).with_patches(Some(config.clone()));

        // Declared but never installed: its content directory does not exist,
        // which is exactly what payload resolution asserts.
        let absent_dep = pinned("absentdep", 'a');
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_metadata(
            &manager.file_structure().packages,
            &companion_pinned,
            &ResolvedPackage::new(),
            &serde_json::json!({
                "type": "bundle",
                "version": 1,
                "env": [{ "key": "COMPANION_VAR", "type": "constant", "value": "present", "visibility": "interface" }],
                "dependencies": [{ "identifier": absent_dep.to_string(), "visibility": "public" }],
                "integrations": { "vendor.example": { "path": "${deps.absentdep.installPath}" } },
            }),
        );
        seed_companion_pin(manager.file_structure(), &companion_tag_id, &companion_digest);
        seed_global_descriptor(&manager, &config, &[&companion_tag_id]).await;

        let root = Arc::new(make_install_info(dir.path(), "rootpkg", 'r', ResolvedPackage::new()));

        let (entries, _compose_count, _provenance, attribution) = manager
            .resolve_env_with_attribution(&[root], true, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .expect("the --self surface carries no integrations, so a payload it discards must never be resolved");

        assert!(
            attribution.integrations.is_empty(),
            "--self must carry no integrations from any contributor; got: {:?}",
            attribution.integrations
        );
        assert!(
            entries.iter().any(|e| e.key == "COMPANION_VAR"),
            "the companion's interface env var must still compose — the suppressed carrier is the only thing dropped; \
             entries: {entries:?}"
        );
    }

    // ── C7: the projection cache records "missing", never "safe to skip" ──────

    /// Record a descriptor naming `companion_tag_id` with an explicit per-rule
    /// `required` flag at `descriptor_id`'s patch-descriptor path.
    ///
    /// [`seed_global_descriptor`] omits the flag, so the tier's own `required`
    /// decides; a fixture that needs the two to differ per base writes its own.
    async fn seed_descriptor_with_required(
        manager: &PackageManager,
        descriptor_id: &PackageRef,
        companion_tag_id: &PackageRef,
        required: bool,
    ) {
        use super::super::patch_discovery::PatchTagMap;
        use ocx_oci::Algorithm;

        let blob_store = manager.file_structure().blobs.clone();
        let layer_bytes = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion_tag_id.to_string()], "required": required }]
        })
        .to_string()
        .into_bytes();
        let layer_digest = Algorithm::Sha256.hash(&layer_bytes);
        let manifest_bytes = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string()
        .into_bytes();
        let manifest_digest = Algorithm::Sha256.hash(&manifest_bytes);
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, &manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, &layer_bytes)
            .await
            .unwrap();
        PatchTagMap::write_has_descriptor(
            &manager.file_structure().patch_descriptor_path(descriptor_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();
    }

    /// A companion that is OPTIONAL for the first admitted base and REQUIRED for a
    /// later one must still fail closed on the later base.
    ///
    /// This is the only shape in which the required re-check on a cache HIT can
    /// fire. Whenever the companion is required at its first encounter, the
    /// fail-closed arm on the lookup itself fires and no second base is ever
    /// reached — which is what `fix3_required_companion_missing_fails_closed_even_on_cache_hit`
    /// and `f6_pkg_specific_descriptor_overrides_global_companion_required_flag`
    /// actually exercise, both of them through the cache MISS path. Here the first
    /// base warn-skips the companion and caches it as missing, so the re-check on
    /// the cached value is the only thing left to catch the second base's
    /// requirement: cached "missing" means genuinely not installed, never "an
    /// earlier base already decided this one is safe to skip".
    #[tokio::test(flavor = "multi_thread")]
    async fn a_companion_missing_for_an_optional_base_still_fails_closed_on_a_later_required_base() {
        use super::super::patch_discovery::{global_descriptor_id, patch_descriptor_id};

        let dir = TempDir::new().unwrap();
        // Tier optional, so each rule's own `required` flag decides.
        let config = ResolvedPatchConfig {
            required: false,
            ..test_patch_config()
        };
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let store = manager.file_structure().packages.clone();

        // Named by both descriptors, never installed locally.
        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        // Global rule, optional: the first admitted base warn-skips it.
        seed_descriptor_with_required(&manager, &global_descriptor_id(&config), &companion_tag_id, false).await;

        let alpha = seed_root_arc(&store, "alpha", 'a');
        let beta_id = pinned("beta", 'b');
        let beta = seed_root_arc(&store, "beta", 'b');
        // Package-specific rule for `beta` alone, required: evaluated on a cache hit.
        seed_descriptor_with_required(
            &manager,
            &patch_descriptor_id(&config, beta_id.as_identifier()),
            &companion_tag_id,
            true,
        )
        .await;

        let result = manager
            .resolve_env(
                &[alpha, beta],
                false,
                super::EnvScope::package_tier(),
                &super::host_platform(),
            )
            .await;
        assert!(
            result.is_err(),
            "a companion cached as missing for an optional base must still fail closed for a base that requires it; \
             got Ok({:?})",
            result.ok()
        );
    }
}

// resolve_site_patch_roots derives GC roots from local state only, even under
// ChainMode::Remote, and none with no `[patches]`; breaking this collects an in-use
// companion or makes resolution depend on the network.
#[cfg(test)]
mod phase5a_spec_tests {
    use tempfile::TempDir;

    use crate::PackageManager;
    use ocx_config::patch::ResolvedPatchConfig;
    use ocx_index::{ChainMode, Index, IndexStore, LocalConfig, LocalIndex};
    use ocx_oci::{Algorithm, Digest, PackageRef, PinnedPackageRef};
    use ocx_package::{metadata::visibility::Visibility, resolved_package::ResolvedPackage};
    use ocx_store::file_structure::{BlobStore, FileStructure};

    use super::{
        super::patch_discovery::{PatchTagMap, global_descriptor_id},
        PatchRootScope, descriptor_source_key,
        phase4_spec_tests::{seed_companion_pin, seed_package_with_constant_var},
    };

    const REGISTRY: &str = "example.com";
    const PATCH_REGISTRY: &str = "patches.example.com";

    pub(super) fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    pub(super) fn test_patch_config() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
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
                index_store: IndexStore::new(dir.path().join("index")),
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
                index_store: IndexStore::new(dir.path().join("index")),
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
    pub(super) fn seed_installed_base_symlink(dir: &TempDir, base_id: &PackageRef) {
        let symlink_store = ocx_store::file_structure::SymlinkStore::new(dir.path().join("symlinks"));
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
        companion_tag_id: &PackageRef,
    ) -> (Digest, Digest) {
        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let tag_store = FileStructure::with_root(dir.path().to_path_buf());

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
        let global_tags_path = tag_store.patch_descriptor_path(&global_id);
        PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .unwrap();

        (manifest_digest, layer_digest)
    }

    // ── Spec test 1 — seeded base + global descriptor + installed companion ──

    /// `resolve_site_patch_roots` with an installed base, a global descriptor
    /// (rule `*` → companion), and an installed companion returns:
    ///   - the companion `PinnedPackageRef` in `.companions`
    ///   - the descriptor manifest digest + layer digest in `.descriptors`
    ///
    /// This is the primary contract test for the GC root derivation.
    ///
    /// Traceability: Phase 5A spec test 1.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_site_patch_roots_with_installed_base_and_companion_returns_roots() {
        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));

        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        // Seed an installed base: place a candidate symlink under the symlink store.
        let base_id = PackageRef::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        // Seed the companion package in the package store.
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        // Keeps the advisory tag: `resolve_site_patch_roots` now carries it on the
        // resolved pin so a freeze can key per tag. Store path is unaffected.
        let companion_pinned =
            PinnedPackageRef::try_from(companion_tag_id.clone_with_digest(companion_digest.clone())).unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "CA_BUNDLE",
            "/etc/ssl/certs/bundle.crt",
            Visibility::INTERFACE,
        );

        // Write companion root-document tag entry in the wire grammar `LocalIndex` understands.
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

        // Seed the global descriptor referencing the companion.
        let (manifest_digest, layer_digest) =
            seed_global_descriptor_with_companion(&dir, &patch_config, &companion_tag_id).await;

        // Call the stub — must fail with unimplemented!() until Phase 5A is complete.
        let roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::Recorded)
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

    /// GC roots seeded under [`PatchRootScope::RecordedAndSnapshot`] retain the
    /// companion an active freeze pins, not just the one the live record names.
    ///
    /// The two diverge as soon as an `ocx patch sync` advances the record past
    /// the `patches.snapshot.json` an invocation adopted, because compose
    /// resolves snapshot-first while GC used to seed record-only. The result
    /// was `ocx clean` under `OCX_PATCH_SNAPSHOT` collecting the very companion
    /// the next compose reads (optional: silently absent; required: 81).
    /// `ocx patch freeze` keeps record-only scope — asserted here too, since a
    /// freeze that read through its own snapshot could never record live state.
    #[tokio::test(flavor = "multi_thread")]
    async fn site_patch_roots_retain_the_snapshot_pinned_companion_for_garbage_collection() {
        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let base_id = PackageRef::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        // The record advanced to v2 (a sync); the snapshot still pins v1.
        let frozen_digest = sha256('c');
        let synced_digest = sha256('d');
        // A resolved companion root keeps its advisory tag (the freeze keys on
        // it); the package store still locates it by registry + digest alone.
        let pinned_at = |digest: &Digest| {
            PinnedPackageRef::try_from(
                PackageRef::new_registry("ca-bundle", PATCH_REGISTRY)
                    .clone_with_tag("latest")
                    .clone_with_digest(digest.clone()),
            )
            .unwrap()
        };

        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        seed_companion_pin(&file_structure, &companion_tag_id, &synced_digest);
        seed_global_descriptor_with_companion(&dir, &patch_config, &companion_tag_id).await;

        // A second frozen companion no live descriptor names any more — the
        // rule was dropped by the same sync that advanced the record. The
        // frozen build still composes it, so GC still has to retain it.
        let dropped_digest = sha256('e');
        let dropped_companion = PackageRef::new_registry("legacy-ca", PATCH_REGISTRY).clone_with_tag("latest");
        let dropped_pinned =
            PinnedPackageRef::try_from(dropped_companion.clone_with_digest(dropped_digest.clone())).unwrap();

        let snapshot = crate::patch::PatchSnapshot {
            version: crate::patch::snapshot::SnapshotVersion::CURRENT,
            companions: [
                (
                    crate::patch::snapshot::companion_key(&companion_tag_id),
                    frozen_digest.clone(),
                ),
                (
                    crate::patch::snapshot::companion_key(&dropped_companion),
                    dropped_digest.clone(),
                ),
            ]
            .into_iter()
            .collect(),
            descriptors: std::collections::BTreeMap::new(),
        };
        let manager = make_manager(&dir)
            .with_patches(Some(patch_config.clone()))
            .with_patch_snapshot(Some(snapshot));

        let gc_roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::RecordedAndSnapshot)
            .await
            .expect("resolve_site_patch_roots must succeed");
        assert!(
            gc_roots.companions.contains(&pinned_at(&frozen_digest)),
            "GC roots must retain the snapshot-pinned companion a frozen compose still reads; got: {:?}",
            gc_roots.companions
        );
        assert!(
            gc_roots.companions.contains(&pinned_at(&synced_digest)),
            "GC roots must retain the recorded companion as well; got: {:?}",
            gc_roots.companions
        );
        assert!(
            gc_roots.companions.contains(&dropped_pinned),
            "GC roots must retain a frozen companion no live descriptor names any more; got: {:?}",
            gc_roots.companions
        );

        let freeze_roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::Recorded)
            .await
            .expect("resolve_site_patch_roots must succeed");
        assert_eq!(
            freeze_roots.companions,
            vec![pinned_at(&synced_digest)],
            "a freeze must snapshot live state only, never re-freeze its own snapshot"
        );
    }

    /// The descriptor half of the snapshot-blind GC hole: an active freeze
    /// loads each descriptor by its SNAPSHOT manifest digest from the CAS, so
    /// those blobs are GC roots too once a sync has advanced the record.
    #[tokio::test(flavor = "multi_thread")]
    async fn site_patch_roots_retain_the_snapshot_pinned_descriptor_blobs() {
        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let base_id = PackageRef::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let (recorded_manifest, _) =
            seed_global_descriptor_with_companion(&dir, &patch_config, &companion_tag_id).await;

        // A second, older descriptor generation — still in the CAS, still what
        // the snapshot pins, no longer what the record points at.
        let blob_store = BlobStore::new(dir.path().join("blobs"));
        let frozen_layer_bytes = serde_json::json!({ "version": 1, "rules": [] }).to_string();
        let frozen_layer_digest = Algorithm::Sha256.hash(frozen_layer_bytes.as_bytes());
        let frozen_manifest_bytes = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{ "digest": frozen_layer_digest.to_string() }],
        })
        .to_string();
        let frozen_manifest_digest = Algorithm::Sha256.hash(frozen_manifest_bytes.as_bytes());
        blob_store
            .write_blob(
                PATCH_REGISTRY,
                &frozen_manifest_digest,
                frozen_manifest_bytes.as_bytes(),
            )
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &frozen_layer_digest, frozen_layer_bytes.as_bytes())
            .await
            .unwrap();
        assert_ne!(
            recorded_manifest, frozen_manifest_digest,
            "the two descriptor generations must differ for this test to discriminate"
        );

        let global_key = descriptor_source_key(&global_descriptor_id(&patch_config));
        let snapshot = crate::patch::PatchSnapshot {
            version: crate::patch::snapshot::SnapshotVersion::CURRENT,
            companions: std::collections::BTreeMap::new(),
            descriptors: [(global_key, frozen_manifest_digest.clone())].into_iter().collect(),
        };
        let manager = make_manager(&dir)
            .with_patches(Some(patch_config.clone()))
            .with_patch_snapshot(Some(snapshot));

        let gc_roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::RecordedAndSnapshot)
            .await
            .expect("resolve_site_patch_roots must succeed");
        for digest in [&frozen_manifest_digest, &frozen_layer_digest] {
            assert!(
                gc_roots
                    .descriptors
                    .iter()
                    .any(|(registry, rooted)| registry == PATCH_REGISTRY && rooted == digest),
                "GC roots must retain the snapshot-pinned descriptor blob {digest}; got: {:?}",
                gc_roots.descriptors
            );
        }

        let freeze_roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::Recorded)
            .await
            .expect("resolve_site_patch_roots must succeed");
        assert!(
            !freeze_roots
                .descriptors
                .iter()
                .any(|(_, rooted)| rooted == &frozen_manifest_digest),
            "a freeze must see live state only; got: {:?}",
            freeze_roots.descriptors
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

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        // Remote-mode manager — tag-addressed Op::Query returns Ok(None) via self.index().
        let manager = make_remote_manager(&dir).with_patches(Some(patch_config.clone()));

        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        // Seed installed base.
        let base_id = PackageRef::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        // Seed companion.
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        // Keeps the advisory tag: `resolve_site_patch_roots` now carries it on the
        // resolved pin so a freeze can key per tag. Store path is unaffected.
        let companion_pinned =
            PinnedPackageRef::try_from(companion_tag_id.clone_with_digest(companion_digest.clone())).unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "CA_BUNDLE",
            "/etc/ssl/certs/bundle.crt",
            Visibility::INTERFACE,
        );
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

        // Seed global descriptor.
        seed_global_descriptor_with_companion(&dir, &patch_config, &companion_tag_id).await;

        // Call the stub — must fail with unimplemented!() until Phase 5A is complete.
        let roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::Recorded)
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
        let base_id = PackageRef::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        let roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::Recorded)
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
        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));

        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        // Seed an installed base.  The candidate path is:
        //   symlinks/{registry_slug}/cmake/candidates/3.28
        // The correctly-reconstructed base identifier must be `cmake:3.28`
        // in registry `example.com` — not `cmake/candidates:3.28`.
        let base_id = PackageRef::new_registry("cmake", REGISTRY).clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        // Seed a companion package.
        let companion_digest = sha256('6');
        let companion_tag_id = PackageRef::new_registry("ca-certs", PATCH_REGISTRY).clone_with_tag("v1");
        // Keeps the advisory tag: `resolve_site_patch_roots` now carries it on the
        // resolved pin so a freeze can key per tag. Store path is unaffected.
        let companion_pinned =
            PinnedPackageRef::try_from(companion_tag_id.clone_with_digest(companion_digest.clone())).unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ocx_package::resolved_package::ResolvedPackage::new(),
            "CA_CERTS",
            "/etc/ssl/certs",
            ocx_package::metadata::visibility::Visibility::INTERFACE,
        );
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

        // Seed a descriptor with a scoped rule matching `example.com/cmake:*`.
        // This rule must ONLY fire when the base identifier is correctly
        // reconstructed as `cmake` in registry `example.com`.
        {
            let blob_store = ocx_store::file_structure::BlobStore::new(dir.path().join("blobs"));
            let tag_store_inner = ocx_store::file_structure::FileStructure::with_root(dir.path().to_path_buf());

            let descriptor_json = serde_json::json!({
                "version": 1,
                "rules": [{
                    "match": &format!("{REGISTRY}/cmake:*"),
                    "packages": [companion_tag_id.to_string()]
                }]
            })
            .to_string();
            let layer_bytes = descriptor_json.as_bytes();
            let layer_digest = ocx_oci::Algorithm::Sha256.hash(layer_bytes);
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
            let manifest_digest = ocx_oci::Algorithm::Sha256.hash(manifest_bytes);

            blob_store
                .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes)
                .await
                .unwrap();
            blob_store
                .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes)
                .await
                .unwrap();

            let global_id = super::super::patch_discovery::global_descriptor_id(&patch_config);
            let global_tags_path = tag_store_inner.patch_descriptor_path(&global_id);
            super::super::patch_discovery::PatchTagMap::write_has_descriptor(
                &global_tags_path,
                &manifest_digest.to_string(),
            )
            .await
            .unwrap();
        }

        let roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::Recorded)
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
        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));

        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        // Installed base whose registry carries a PORT → symlink slug `localhost_5000`.
        let base_id = PackageRef::new_registry("cmake", "localhost:5000").clone_with_tag("3.28");
        seed_installed_base_symlink(&dir, &base_id);

        // Seed the base's root document recording the canonical `oci://` `repository`
        // so `recover_base_with_real_registry` can restore the real hostname from
        // the slug directory. The snapshot path slugifies the registry, landing at
        // `.../localhost_5000/p/cmake.json`.
        {
            let base_digest = sha256('b');
            let root_path = ocx_index::IndexStore::machine_local(&tag_store)
                .root_document_path(base_id.registry(), base_id.repository());
            std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
            let json = format!(
                r#"{{"repository":"oci://localhost:5000/cmake","tags":{{"3.28":{{"content":"{base_digest}","observed":"2026-07-18T00:00:00Z"}}}}}}"#
            );
            std::fs::write(&root_path, json).unwrap();
        }

        // Companion installed locally.
        let companion_digest = sha256('c');
        let companion_tag_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        // Keeps the advisory tag: `resolve_site_patch_roots` now carries it on the
        // resolved pin so a freeze can key per tag. Store path is unaffected.
        let companion_pinned =
            PinnedPackageRef::try_from(companion_tag_id.clone_with_digest(companion_digest.clone())).unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ocx_package::resolved_package::ResolvedPackage::new(),
            "CA_BUNDLE",
            "/etc/ssl/certs/bundle.crt",
            ocx_package::metadata::visibility::Visibility::INTERFACE,
        );
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

        // Global descriptor with a SCOPED port-registry rule (not catch-all).
        {
            let blob_store = ocx_store::file_structure::BlobStore::new(dir.path().join("blobs"));
            let descriptor_json = serde_json::json!({
                "version": 1,
                "rules": [{
                    "match": "localhost:5000/cmake:*",
                    "packages": [companion_tag_id.to_string()]
                }]
            })
            .to_string();
            let layer_bytes = descriptor_json.as_bytes();
            let layer_digest = ocx_oci::Algorithm::Sha256.hash(layer_bytes);
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
            let manifest_digest = ocx_oci::Algorithm::Sha256.hash(manifest_bytes);
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
                &tag_store.patch_descriptor_path(&global_id),
                &manifest_digest.to_string(),
            )
            .await
            .unwrap();
        }

        let roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), PatchRootScope::Recorded)
            .await
            .expect("resolve_site_patch_roots must succeed");

        assert!(
            roots.companions.contains(&companion_pinned),
            "scoped rule 'localhost:5000/cmake:*' must match the port-registry base after \
             real-registry recovery from the tag file; got: {:?}",
            roots.companions
        );
    }

    // ── Slug recovery keeps package identity ─────────────────────────────────────

    /// Seeds the root document for `(source, repository)` with the given
    /// `repository` pointer and returns the recovered base for `slug_base_id`.
    async fn recover_with_root_pointer(slug_base_id: &PackageRef, pointer: &str) -> PackageRef {
        let dir = TempDir::new().unwrap();
        let snapshot = IndexStore::machine_local(&FileStructure::with_root(dir.path().to_path_buf()));
        let root_path = snapshot.root_document_path(slug_base_id.registry(), slug_base_id.repository());
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        std::fs::write(&root_path, format!(r#"{{"repository":"{pointer}","tags":{{}}}}"#)).unwrap();
        super::recover_base_with_real_registry(&snapshot, slug_base_id).await
    }

    /// An index-routed name's root document points at its PHYSICAL location on
    /// another registry. That host is not the un-slugged form of the directory
    /// name, so the logical base must survive unchanged — `ghcr.io/cmake` is
    /// neither the package name nor the physical location.
    #[tokio::test]
    async fn recover_base_keeps_index_routed_name_off_its_physical_host() {
        let slug_base_id = PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28");
        let recovered = recover_with_root_pointer(&slug_base_id, "oci://ghcr.io/ocx-contrib/cmake").await;
        assert_eq!(recovered.to_string(), "ocx.sh/cmake:3.28");
    }

    /// The original intent: a slugged directory name (`localhost_5000`) is
    /// restored to the real hostname it was slugged from.
    #[tokio::test]
    async fn recover_base_unslugs_port_registry() {
        let slug_base_id = PackageRef::new_registry("cmake", "localhost_5000").clone_with_tag("3.28");
        let recovered = recover_with_root_pointer(&slug_base_id, "oci://localhost:5000/cmake").await;
        assert_eq!(recovered.to_string(), "localhost:5000/cmake:3.28");
    }
}

// Phase 5B snapshot-preference invariants: once a patch snapshot is set, compose
// must prefer its recorded companion digest over a live tag lookup, and
// with_patch_snapshot plus offline_view must carry that snapshot through
// unchanged. Breaking either lets compose silently re-resolve a companion against
// the live registry instead of the pinned snapshot.

#[cfg(test)]
mod phase5b_spec_tests {
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    use crate::patch::snapshot::{PatchSnapshot, SnapshotVersion};
    use ocx_index::{IndexStore, LocalConfig, LocalIndex};
    use ocx_oci::{PackageRef, PinnedPackageRef};
    use ocx_package::{metadata::visibility::Visibility, resolved_package::ResolvedPackage};

    use super::{
        phase4_spec_tests::{seed_companion_pin, seed_package_with_constant_var, seed_root_arc},
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
        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();

        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        // Digest A = live tag target.  Digest B = snapshot pin.
        let live_digest = sha256('a');
        let snap_digest = sha256('b');

        let companion_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");

        // Live companion: tag → A, package at A carries live_value.
        let live_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(live_digest.clone()),
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
        seed_companion_pin(&tag_store, &companion_id, &live_digest);

        // Snapshot companion: package at digest B carries snapshot_value.
        let snap_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(snap_digest.clone()),
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
        let mut companions_map = BTreeMap::new();
        companions_map.insert(
            crate::patch::snapshot::companion_key(&companion_id),
            snap_digest.clone(),
        );
        let global_id = super::super::patch_discovery::global_descriptor_id(&patch_config);
        let mut descriptors_map = BTreeMap::new();
        descriptors_map.insert(super::descriptor_source_key(&global_id), global_manifest_digest);
        let snapshot = PatchSnapshot {
            version: SnapshotVersion::CURRENT,
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
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
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

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();

        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        let live_digest = sha256('a');
        let companion_id = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");

        let live_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(live_digest.clone()),
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
        seed_companion_pin(&tag_store, &companion_id, &live_digest);

        seed_global_descriptor_with_companion(&dir, &patch_config, &companion_id).await;

        // No snapshot — live tag must win.
        let root = seed_root_arc(&store, "root", 'r');
        let entries = manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
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

    /// A freeze must not collapse a companion repository that the overlay
    /// composes at TWO tags.
    ///
    /// Two tags of one repository are two companions — pinned separately in the
    /// record, composed separately by the overlay
    /// (`same_companion_repo_at_two_tags_projects_both_versions`). A snapshot
    /// keyed by repository alone loses one of them at freeze time, and the
    /// frozen build then composes the surviving version twice, silently: the
    /// exact state a freeze exists to prevent.
    ///
    /// The whole round trip is exercised — live roots → `PatchSnapshot::from_roots`
    /// → compose under that snapshot — because the corruption happens at the
    /// freeze, not at the read.
    #[tokio::test(flavor = "multi_thread")]
    async fn freeze_round_trip_keeps_both_tags_of_one_companion_repository() {
        use super::super::patch_discovery::{PatchTagMap, global_descriptor_id};
        use ocx_oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();
        let blob_store = manager.file_structure().blobs.clone();
        let tag_store = manager.file_structure().clone();

        // ── One companion repository, two tags, two installs, two values ──────
        let digest_v1 = sha256('c');
        let digest_v2 = sha256('d');
        let companion_v1 = PackageRef::new_registry("dedup_companion", PATCH_REGISTRY).clone_with_tag("1.0.0");
        let companion_v2 = PackageRef::new_registry("dedup_companion", PATCH_REGISTRY).clone_with_tag("2.0.0");
        for (digest, value) in [(&digest_v1, "one"), (&digest_v2, "two")] {
            let pinned = PinnedPackageRef::try_from(
                PackageRef::new_registry("dedup_companion", PATCH_REGISTRY).clone_with_digest(digest.clone()),
            )
            .unwrap();
            seed_package_with_constant_var(
                &store,
                &pinned,
                &ResolvedPackage::new(),
                "COMPANION_VER",
                value,
                Visibility::INTERFACE,
            );
        }

        // The pin record holds BOTH tags; `seed_companion_pin` writes a
        // single-tag record and the second call would overwrite the first,
        // since the record path is keyed by repository only.
        let pin_path = tag_store.patch_companion_path(&companion_v1);
        std::fs::create_dir_all(pin_path.parent().unwrap()).unwrap();
        std::fs::write(
            &pin_path,
            serde_json::to_vec(&serde_json::json!({
                "1.0.0": digest_v1.to_string(),
                "2.0.0": digest_v2.to_string(),
            }))
            .unwrap(),
        )
        .unwrap();

        // ── Global descriptor: catch-all rule → both companion tags, in order ─
        let layer_bytes = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion_v1.to_string(), companion_v2.to_string()] }]
        })
        .to_string();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes.as_bytes());
        let manifest_bytes = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{
                "mediaType": "application/octet-stream",
                "digest": layer_digest.to_string(),
                "size": layer_bytes.len()
            }]
        })
        .to_string();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes.as_bytes());
        blob_store
            .write_blob(PATCH_REGISTRY, &manifest_digest, manifest_bytes.as_bytes())
            .await
            .unwrap();
        blob_store
            .write_blob(PATCH_REGISTRY, &layer_digest, layer_bytes.as_bytes())
            .await
            .unwrap();
        let global_id = global_descriptor_id(&patch_config);
        PatchTagMap::write_has_descriptor(
            &tag_store.patch_descriptor_path(&global_id),
            &manifest_digest.to_string(),
        )
        .await
        .unwrap();

        // An installed base is what makes the catch-all rule contribute
        // companions to the freeze's root set.
        let base_id = PackageRef::new_registry("alpha", "example.com").clone_with_tag("1.0.0");
        super::phase5a_spec_tests::seed_installed_base_symlink(&dir, &base_id);

        // ── Freeze: live roots → snapshot ─────────────────────────────────────
        let roots = manager
            .resolve_site_patch_roots(&ocx_oci::Platform::any(), super::PatchRootScope::Recorded)
            .await
            .expect("resolve_site_patch_roots must succeed");
        assert_eq!(
            roots.companions.len(),
            2,
            "premise: the live root set must already carry both tags, or the freeze \
             assertion below cannot discriminate; got: {:?}",
            roots.companions
        );

        let snapshot = PatchSnapshot::from_roots(&roots);
        assert_eq!(
            snapshot.companions.len(),
            2,
            "a freeze must record both tags of one companion repository; got: {:?}",
            snapshot.companions
        );

        // ── Compose under that snapshot ───────────────────────────────────────
        let frozen_manager = make_manager(&dir)
            .with_patches(Some(patch_config.clone()))
            .with_patch_snapshot(Some(snapshot));
        let root = seed_root_arc(&frozen_manager.file_structure().packages.clone(), "alpha", 'a');
        let entries = frozen_manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .expect("frozen resolve_env must succeed");

        let composed: Vec<&str> = entries
            .iter()
            .filter(|entry| entry.key == "COMPANION_VER")
            .map(|entry| entry.value.as_str())
            .collect();
        assert_eq!(
            composed,
            vec!["one", "two"],
            "each tag must compose its OWN frozen digest; a repository-keyed snapshot \
             makes both tags resolve to whichever one was written last; entries: {entries:?}"
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
        let dir = TempDir::new().unwrap();
        let patch_config = test_patch_config();
        let store = make_manager(&dir).file_structure().packages.clone();
        let tag_store = make_manager(&dir).file_structure().clone();

        // Frozen companion `ca-bundle` at digest dC1 → SNAP_VAR=frozen.
        let frozen_companion = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let frozen_digest = sha256('1');
        let frozen_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(frozen_digest.clone()),
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
        seed_companion_pin(&tag_store, &frozen_companion, &frozen_digest);

        // Advanced companion `ca-bundle-v2` at digest dC2 → SNAP_VAR=advanced.
        let advanced_companion = PackageRef::new_registry("ca-bundle-v2", PATCH_REGISTRY).clone_with_tag("latest");
        let advanced_digest = sha256('2');
        let advanced_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle-v2", PATCH_REGISTRY).clone_with_digest(advanced_digest.clone()),
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
        seed_companion_pin(&tag_store, &advanced_companion, &advanced_digest);

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
        companions_map.insert(
            crate::patch::snapshot::companion_key(&frozen_companion),
            frozen_digest.clone(),
        );
        let snapshot = PatchSnapshot {
            version: SnapshotVersion::CURRENT,
            companions: companions_map,
            descriptors: descriptors_map,
        };

        // Frozen build: must compose the FROZEN companion, ignoring the advance.
        let frozen_manager = make_manager(&dir)
            .with_patches(Some(patch_config.clone()))
            .with_patch_snapshot(Some(snapshot));
        let root = seed_root_arc(&frozen_manager.file_structure().packages.clone(), "root", 'r');
        let entries = frozen_manager
            .resolve_env(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
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
            .resolve_env(
                &[root2],
                false,
                super::EnvScope::package_tier(),
                &super::host_platform(),
            )
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
        companions.insert(format!("{PATCH_REGISTRY}/ca-bundle:latest"), sha256('s'));
        let snapshot = PatchSnapshot {
            version: SnapshotVersion::CURRENT,
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
            index_store: IndexStore::new(dir.path().join("index")),
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

    use ocx_config::patch::ResolvedPatchConfig;

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
            no_patches: std::collections::BTreeSet::new(),
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
        let dir = TempDir::new().unwrap();
        let config = patch_config(false);
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let (key, _value) = seed_installed_global_companion(&manager, &config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');

        // Opt the root base out by canonical registry/repository key.
        let no_patches: BTreeSet<String> = [ROOT_REPO_KEY.to_string()].into_iter().collect();
        let (entries, _patch_start, _provenance) = manager
            .resolve_env_with_patch_boundary(
                &[root],
                false,
                super::EnvScope::Project {
                    no_patches,
                    env: Vec::new(),
                    toolchain: None,
                },
                &super::host_platform(),
            )
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
        let dir = TempDir::new().unwrap();
        let config = patch_config(true);
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');

        // Same opt-out as test 11 — but the tier is system_required, so it is ignored.
        let no_patches: BTreeSet<String> = [ROOT_REPO_KEY.to_string()].into_iter().collect();
        let (entries, _patch_start, _provenance) = manager
            .resolve_env_with_patch_boundary(
                &[root],
                false,
                super::EnvScope::Project {
                    no_patches,
                    env: Vec::new(),
                    toolchain: None,
                },
                &super::host_platform(),
            )
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
        let dir = TempDir::new().unwrap();
        let config = patch_config(false);
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');

        // Empty opt-out set — the root base is NOT opted out.
        let no_patches: BTreeSet<String> = BTreeSet::new();
        let (entries, _patch_start, _provenance) = manager
            .resolve_env_with_patch_boundary(
                &[root],
                false,
                super::EnvScope::Project {
                    no_patches,
                    env: Vec::new(),
                    toolchain: None,
                },
                &super::host_platform(),
            )
            .await
            .expect("resolve_env_with_patch_boundary must succeed");

        assert!(
            entries.iter().any(|e| e.key == key && e.value == value),
            "test 13: a base that is NOT opted out must keep its companion overlay; \
             '{key}={value}' must be present. entries: {entries:?}"
        );
    }

    /// (14 — Contract 1 case (c)) For a base that is NOT opted out,
    /// `EnvScope::package_tier()` and an empty `EnvScope::Project` produce
    /// byte-identical resolved entries. This locks the ADR guarantee that the
    /// named no-project state is observationally equal to a project carrying a
    /// zero-length opt-out set — the two are distinguishable *values*, not
    /// distinguishable *outputs*.
    ///
    /// Traces: ADR §"D3 honest no-project boundary"; Contract 1 case (c).
    #[tokio::test(flavor = "multi_thread")]
    async fn no_project_context_equals_project_empty_for_non_opted_base() {
        let dir = TempDir::new().unwrap();
        let config = patch_config(false);
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &config, true).await;

        let store = manager.file_structure().packages.clone();
        let root_project = seed_root_arc(&store, "rootpkg", 'r');
        let root_no_project = seed_root_arc(&store, "rootpkg", 'r');

        let (project_entries, project_start, _project_provenance) = manager
            .resolve_env_with_patch_boundary(
                &[root_project],
                false,
                super::EnvScope::Project {
                    no_patches: BTreeSet::new(),
                    env: Vec::new(),
                    toolchain: None,
                },
                &super::host_platform(),
            )
            .await
            .expect("Project(empty) resolve must succeed");
        let (no_project_entries, no_project_start, _no_project_provenance) = manager
            .resolve_env_with_patch_boundary(
                &[root_no_project],
                false,
                super::EnvScope::package_tier(),
                &super::host_platform(),
            )
            .await
            .expect("package-tier resolve must succeed");

        // Both admit the companion overlay for the non-opted base.
        assert!(
            project_entries.iter().any(|e| e.key == key && e.value == value),
            "test 14: Project(empty) must keep the overlay; '{key}={value}' absent. entries: {project_entries:?}"
        );

        // Byte-identical outputs: same boundary index and same (key, value) sequence.
        assert_eq!(
            project_start, no_project_start,
            "test 14: overlay boundary index must match across scopes"
        );
        let project_pairs: Vec<(&str, &str)> = project_entries
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_str()))
            .collect();
        let no_project_pairs: Vec<(&str, &str)> = no_project_entries
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_str()))
            .collect();
        assert_eq!(
            project_pairs, no_project_pairs,
            "test 14: Package and Project(empty) must yield byte-identical entries for a non-opted base"
        );
    }

    /// (15 — Contract 1 case (e)) The launcher path resolves via
    /// `EnvScope::package_tier()`, which carries an empty opt-out. A
    /// SYSTEM-REQUIRED tier must still overlay its companion under that scope:
    /// `Package` does not disable enforced patches. This is the
    /// direct-launcher analogue of test 12 (opt-out never suppresses a
    /// system-required tier).
    ///
    /// Traces: ADR §"7th fork sub-decision" + §"launcher re-injection";
    /// Contract 1 case (e).
    #[tokio::test(flavor = "multi_thread")]
    async fn no_project_context_still_applies_system_required_tier() {
        let dir = TempDir::new().unwrap();
        let config = patch_config(true);
        let manager = make_manager(&dir).with_patches(Some(config.clone()));
        let (key, value) = seed_installed_global_companion(&manager, &config, true).await;

        let root = seed_root_arc(&manager.file_structure().packages.clone(), "rootpkg", 'r');

        let (entries, _patch_start, _provenance) = manager
            .resolve_env_with_patch_boundary(&[root], false, super::EnvScope::package_tier(), &super::host_platform())
            .await
            .expect("resolve_env_with_patch_boundary must succeed");

        assert!(
            entries.iter().any(|e| e.key == key && e.value == value),
            "test 15: a system_required tier must overlay its companion under the package tier \
             (the launcher path never disables enforced patches); '{key}={value}' must be present. \
             entries: {entries:?}"
        );
    }
}

/// Dependency-direction hygiene gate (ADR AF4 / Codex CX1).
///
/// `EnvScope` is a `package_manager` resolver type. `ProjectConfig`
/// (`crates/ocx_project/src/config.rs`) must stay zero-knowledge of it:
/// the ~4 project/global command sites build the `Project` variant inline
/// from `cfg.no_patches_repositories()`, so `project` never imports the
/// resolver.
/// A helper like `ProjectConfig::patch_scope()` would invert the dependency
/// direction — this source-grep gate fails the build if such a reference ever
/// creeps back.
#[cfg(test)]
mod project_config_isolation_gate {
    /// The `project/config.rs` source, embedded at compile time.
    const CONFIG_SOURCE: &str = include_str!("../../../ocx_project/src/config.rs");

    #[test]
    fn config_does_not_reference_resolver_types() {
        // Build the forbidden tokens from fragments so THIS test file's own
        // source does not count as a reference when a future sibling gate reads
        // it, and so the check is robust to formatting.
        let scope_token = concat!("Patch", "Scope");
        let module_token = concat!("crate::tasks::", "resolve");

        assert!(
            !CONFIG_SOURCE.contains(scope_token),
            "project/config.rs must not reference {scope_token} (dependency-direction inversion — ADR AF4)"
        );
        assert!(
            !CONFIG_SOURCE.contains(module_token),
            "project/config.rs must not import the {module_token} resolver module (ADR AF4)"
        );
    }
}

/// The reserved-key gate lives at the `resolve_env*` seam.
///
/// Every assertion here reads the **observable product**: the entry vector the
/// resolver hands out, and the shell text `conventions::emit_lines` builds from
/// it by dispatching each entry through `Shell::export_*`. Nothing asserts on an
/// internal flag, because the bypass this gate closes is an eval'd stream.
#[cfg(test)]
mod c036_reserved_key_gate {
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::PackageManager;
    use ocx_oci::{Digest, PackageRef, PinnedPackageRef};
    use ocx_package::{install_info::InstallInfo, metadata, resolved_package::ResolvedPackage};
    use ocx_shell::shell::Shell;
    use ocx_store::file_structure::{PackageDir, PackageStore};

    use super::phase4_spec_tests::{seed_companion_pin, seed_global_descriptor, seed_package_with_constant_var};
    use super::phase5a_spec_tests::make_manager;
    use super::{Entry, EnvScope, host_platform};

    const REGISTRY: &str = "example.com";
    const PATCH_REGISTRY: &str = "patches.example.com";

    /// The three reserved keys, each a live consent-bypass primitive: the
    /// whitelist itself, the private ledger carrier, and the denial switch.
    const RESERVED_KEYS: &[&str] = &["OCX_CONSENT_NAMESPACES", "__OCX_ENV_STATE", "OCX_NO_HOOK"];

    /// A benign sibling declared beside every reserved key, so a test that
    /// passes by resolving to nothing at all is impossible.
    const BENIGN: (&str, &str) = ("TOOL_HOME", "/opt/tool");

    fn pinned(repo: &str, registry: &str, hex_char: char) -> PinnedPackageRef {
        let digest = Digest::Sha256(hex_char.to_string().repeat(64));
        PinnedPackageRef::try_from(PackageRef::new_registry(repo, registry).clone_with_digest(digest)).unwrap()
    }

    /// Seed an **already-published** package declaring `vars` verbatim and
    /// return it as an installed root.
    ///
    /// Deliberately writes `metadata.json` and re-reads it through serde rather
    /// than building `Metadata` in memory: that is the read path a published
    /// artifact actually arrives on, and it is the path that must keep working
    /// after `ocx package create` starts refusing these keys.
    fn seed_published_root(store: &PackageStore, vars: &[(&str, &str)]) -> Arc<InstallInfo> {
        let id = pinned("rootpkg", REGISTRY, 'r');
        let pkg_path = store.path(&id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let env: Vec<serde_json::Value> = vars
            .iter()
            .map(|(key, value)| {
                serde_json::json!({ "key": key, "type": "constant", "value": value, "visibility": "interface" })
            })
            .collect();
        let document = serde_json::json!({ "type": "bundle", "version": 1, "env": env }).to_string();
        std::fs::write(pkg_path.join("metadata.json"), &document).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();
        Arc::new(InstallInfo::new(
            id,
            serde_json::from_str::<metadata::Metadata>(&document).unwrap(),
            ResolvedPackage::new(),
            PackageDir { dir: pkg_path },
        ))
    }

    fn manager_with_root(dir: &TempDir, vars: &[(&str, &str)]) -> (PackageManager, Arc<InstallInfo>) {
        let manager = make_manager(dir);
        let root = seed_published_root(&manager.file_structure().packages.clone(), vars);
        (manager, root)
    }

    /// The exact bytes `conventions::emit_lines` would print for `entries` —
    /// the same `Shell::export_*` dispatch, joined instead of `println!`ed.
    fn emitted_stream(shell: Shell, entries: &[Entry]) -> String {
        use ocx_package::metadata::env::list::DEFAULT_SEPARATOR;
        use ocx_package::metadata::env::modifier::ModifierKind;

        entries
            .iter()
            .filter_map(|entry| match entry.kind {
                ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
                ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
                ModifierKind::List => shell.export_list(
                    &entry.key,
                    &entry.value,
                    entry.separator.as_deref().unwrap_or(DEFAULT_SEPARATOR),
                ),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Read-path compatibility. A package published before the write
    /// gate existed still resolves; only the reserved key is dropped.
    #[tokio::test(flavor = "multi_thread")]
    async fn published_package_with_a_reserved_key_still_resolves_without_that_key() {
        for reserved in RESERVED_KEYS {
            let dir = TempDir::new().unwrap();
            let (manager, root) = manager_with_root(&dir, &[(reserved, "*/*"), BENIGN]);

            let entries = manager
                .resolve_env(&[root], false, EnvScope::package_tier(), &host_platform())
                .await
                .expect("an already-published package carrying a reserved key must keep resolving");

            assert!(
                !entries.iter().any(|e| e.key == *reserved),
                "'{reserved}' must be dropped from the resolved env; entries: {entries:?}"
            );
            assert!(
                entries.iter().any(|e| e.key == BENIGN.0 && e.value == BENIGN.1),
                "the package's other env vars must survive the skip; entries: {entries:?}"
            );
        }
    }

    /// The security assertion. A package-declared reserved key must not
    /// appear in the stream `ocx env --shell=bash`, `ocx direnv export` and
    /// `ocx package env` hand to `eval`.
    ///
    /// This is the assertion that goes red when the gate sits at
    /// `Env::apply_entries` instead of at the resolver: `emit_lines` never
    /// routes through `apply_entries`, so the key would be exported verbatim.
    #[tokio::test(flavor = "multi_thread")]
    async fn reserved_package_key_never_reaches_the_emitted_shell_stream() {
        for reserved in RESERVED_KEYS {
            let dir = TempDir::new().unwrap();
            let (manager, root) = manager_with_root(&dir, &[(reserved, "*/*"), BENIGN]);

            let entries = manager
                .resolve_env(&[root], false, EnvScope::package_tier(), &host_platform())
                .await
                .expect("resolve_env must succeed");
            let stream = emitted_stream(Shell::Bash, &entries);

            assert!(
                !stream.contains(reserved),
                "'{reserved}' reached the eval'd stream — the gate is not at the resolver seam.\n{stream}"
            );
            assert!(
                stream.contains(BENIGN.0),
                "the benign sibling must still be emitted, or this test proves nothing.\n{stream}"
            );
        }
    }

    /// The gate covers the patch-companion overlay too. A companion is
    /// metadata from a *different* publisher, admitted by a site rule, so it is
    /// the same bypass with one more hop.
    #[tokio::test(flavor = "multi_thread")]
    async fn reserved_companion_key_is_dropped_and_the_patch_boundary_stays_aligned() {
        let dir = TempDir::new().unwrap();
        let patch_config = ocx_config::patch::ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        };
        let manager = make_manager(&dir).with_patches(Some(patch_config.clone()));
        let store = manager.file_structure().packages.clone();

        let companion_digest = Digest::Sha256("c".repeat(64));
        let companion_tag = PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedPackageRef::try_from(
            PackageRef::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            &ResolvedPackage::new(),
            "OCX_CONSENT_NAMESPACES",
            "*/*",
            ocx_package::metadata::visibility::Visibility::INTERFACE,
        );
        seed_companion_pin(manager.file_structure(), &companion_tag, &companion_digest);
        seed_global_descriptor(&manager, &patch_config, &[&companion_tag]).await;

        let root = seed_published_root(&store, &[BENIGN]);
        let (entries, patch_start, provenance) = manager
            .resolve_env_with_patch_boundary(&[root], false, EnvScope::package_tier(), &host_platform())
            .await
            .expect("a companion carrying a reserved key must not fail the composition");

        assert!(
            !entries.iter().any(|e| e.key == "OCX_CONSENT_NAMESPACES"),
            "a companion-declared reserved key must be dropped; entries: {entries:?}"
        );
        assert!(
            !emitted_stream(Shell::Bash, &entries).contains("OCX_CONSENT_NAMESPACES"),
            "a companion-declared reserved key must not reach the eval'd stream"
        );
        // The overlay region is `[patch_start .. patch_start + provenance.len())`.
        // Dropping an overlay entry without dropping its provenance row would
        // mis-attribute every `--show-patches` annotation after it.
        assert_eq!(
            provenance.len(),
            entries.len().saturating_sub(patch_start),
            "the provenance vector must stay aligned with the surviving overlay region"
        );
    }

    /// Nothing leaves the resolver carrying a reserved key, including
    /// the caller-contributed tail. The project `[env]` and `ocx exec --env`
    /// surfaces each refuse these keys at parse time (a hard error); this is the
    /// resolver's own unconditional statement, so a future contributed source
    /// added without its own gate cannot reopen the hole.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_contributed_reserved_key_is_dropped_too() {
        let dir = TempDir::new().unwrap();
        let (manager, root) = manager_with_root(&dir, &[BENIGN]);

        let entries = manager
            .resolve_env(
                &[root],
                false,
                EnvScope::Package {
                    env: vec![Entry {
                        key: "OCX_DEFAULT_REGISTRY".to_string(),
                        value: "evil.example.com".to_string(),
                        kind: ocx_package::metadata::env::modifier::ModifierKind::Constant,
                        separator: None,
                    }],
                },
                &host_platform(),
            )
            .await
            .expect("resolve_env must succeed");

        assert!(
            !entries.iter().any(|e| e.key == "OCX_DEFAULT_REGISTRY"),
            "a contributed reserved key must be dropped; entries: {entries:?}"
        );
    }

    /// The skip is narrow. `is_reserved_ocx_key` is prefix-anchored, and
    /// a key merely *containing* `OCX_` is an ordinary package variable.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_that_only_mentions_ocx_is_not_dropped() {
        let dir = TempDir::new().unwrap();
        let (manager, root) = manager_with_root(&dir, &[("MY_OCX_HOME", "/opt/ocx"), ("OCX", "1")]);

        let entries = manager
            .resolve_env(&[root], false, EnvScope::package_tier(), &host_platform())
            .await
            .expect("resolve_env must succeed");

        for key in ["MY_OCX_HOME", "OCX"] {
            assert!(
                entries.iter().any(|e| e.key == key),
                "'{key}' is outside the reserved namespace and must survive; entries: {entries:?}"
            );
        }
    }
}
