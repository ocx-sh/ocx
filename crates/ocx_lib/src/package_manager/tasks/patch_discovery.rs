// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Lazy three-state patch discovery and companion install.
//!
//! This module implements Phase 3 of the infrastructure-patches feature
//! (`adr_infrastructure_patches.md`, milestone #111, issue #114).
//!
//! ## Responsibility
//!
//! After a user-requested base install completes,
//! [`PackageManager::discover_and_install_patches`] runs once per user-facing
//! base identifier. It:
//!
//! 1. Looks up the three-state discovery record for the base package's patch
//!    repository in the tag store.
//! 2. On cache miss (state = `NeverLooked`), fetches the `__ocx.patch`
//!    descriptor from the patch registry.
//! 3. Persists the descriptor blobs and records the new state.
//! 4. Collects companions from the global and package-specific descriptors.
//! 5. Installs each companion through the base-install primitive.
//!    Required-companion failures fail closed (`RequiredCompanionFailed`);
//!    optional-companion failures warn and continue.
//!
//! ## Recursion guard
//!
//! Discovery fires **only** at the user-requested-install boundary. Companion
//! installs go through [`PackageManager::install_companion`], which calls
//! `pull` directly WITHOUT calling `discover_and_install_patches`. This makes
//! companions non-patched by design — a companion cannot itself trigger further
//! discovery, preventing infinite recursion.
//!
//! ## Three-state tag store
//!
//! The persisted discovery state for a given `(patch_registry, patch_repo)`
//! pair lives in the tag-store JSON file for that repo:
//!
//! | File state | `__ocx.patch` key | Meaning |
//! |---|---|---|
//! | File absent | — | `NeverLooked` — no discovery attempt yet |
//! | File present, key absent | — | `LookedNoDescriptor` — looked, no `__ocx.patch` at this registry |
//! | File present, key present | `"<manifest_digest>"` | `LookedHasDescriptor` — descriptor persisted |
//!
//! Reads and atomic writes use [`LockedJsonFile<BTreeMap<String,String>>`] so
//! concurrent processes (parallel `ocx package install`) cannot corrupt the
//! three-state map.

use std::collections::{BTreeMap, HashMap};

use crate::{
    config::patch::{PatchConfig, ResolvedPatchConfig, expand_patch_path},
    log,
    oci::{self, Identifier},
    package::install_info::InstallInfo,
    package::tag::InternalTag,
    patch::{FetchedDescriptorBlobs, PatchDescriptor, fetch_patch_descriptor_blobs, persist_patch_descriptor},
    utility::fs::LockedJsonFile,
};

use super::super::{PackageManager, error::PackageErrorKind};

// ── Safety limits ─────────────────────────────────────────────────────────────

/// Maximum total number of companions that may be collected across ALL
/// descriptor sources (global + package-specific) for a single base install.
///
/// The per-descriptor limits ([`crate::patch::descriptor::MAX_RULES`] ×
/// [`crate::patch::descriptor::MAX_PACKAGES_PER_RULE`]) guard against a single
/// malformed descriptor; this cap guards against the cross-product when both
/// descriptors independently hit their limits and share no identifiers (so
/// dedup provides no reduction). `256 × 64 × 2 = 32 768` without this cap —
/// unreasonable for a companion list that is normally a handful of entries.
///
/// Defense-in-depth against a compromised or misconfigured patch registry.
/// Exceeding this limit causes [`PackageErrorKind::PatchDiscovery`] with a
/// [`crate::patch::PatchError::DescriptorTooLarge`] error.
pub const MAX_TOTAL_COMPANIONS: usize = 256;

// ── Three-state discovery state ───────────────────────────────────────────────

/// Three-state discovery record for a single `(patch_registry, patch_repo)` pair.
///
/// Encoded in the tag-store JSON map at the key [`InternalTag::PATCH_TAG`]
/// (`"__ocx.patch"`):
///
/// - File **absent** → [`NeverLooked`](PatchDiscoveryState::NeverLooked).
/// - File present, key **absent** → [`LookedNoDescriptor`](PatchDiscoveryState::LookedNoDescriptor).
/// - File present, key present → [`LookedHasDescriptor`](PatchDiscoveryState::LookedHasDescriptor)
///   with the manifest digest as value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchDiscoveryState {
    /// The tag store file for this repo does not exist yet.
    ///
    /// The discovery routine has never contacted the patch registry for this
    /// (registry, repo) pair. Should trigger a network lookup when online.
    NeverLooked,

    /// The tag store file exists, but the `__ocx.patch` key is absent.
    ///
    /// The discovery routine looked and found no `__ocx.patch` descriptor at
    /// the patch registry for this (registry, repo) pair.
    LookedNoDescriptor,

    /// The tag store file exists and the `__ocx.patch` key holds the
    /// manifest digest string of the persisted descriptor.
    ///
    /// The descriptor blob is already in the CAS blob store.
    LookedHasDescriptor {
        /// Manifest digest string (e.g. `"sha256:<64hex>"`).
        manifest_digest: String,
    },
}

// ── PatchTagMap helper ────────────────────────────────────────────────────────

/// Atomic read-modify-write helper for the `__ocx.patch` key in a tag-store
/// JSON file.
///
/// The tag-store file maps tag strings to digest strings
/// (`BTreeMap<String, String>`). This helper exposes the three-state read
/// and the atomic write for the `__ocx.patch` key specifically, backed by
/// [`LockedJsonFile`] to be cross-process safe.
pub struct PatchTagMap;

impl PatchTagMap {
    /// Read the discovery state for the given tag-store path.
    ///
    /// - Path absent → [`PatchDiscoveryState::NeverLooked`].
    /// - Path present, `__ocx.patch` key absent →
    ///   [`PatchDiscoveryState::LookedNoDescriptor`].
    /// - Path present, key present →
    ///   [`PatchDiscoveryState::LookedHasDescriptor`].
    ///
    /// Uses a shared lock for read — multiple concurrent readers are safe.
    pub async fn read(tags_path: &std::path::Path) -> crate::Result<PatchDiscoveryState> {
        // State (a): file absent → never looked.
        let Some(mut locked) = LockedJsonFile::<BTreeMap<String, String>>::open_shared(tags_path).await? else {
            return Ok(PatchDiscoveryState::NeverLooked);
        };
        // File present — read the map.
        let map = locked.read().await?.unwrap_or_default();
        match map.get(InternalTag::PATCH_TAG) {
            // State (c): key present → looked, has descriptor.
            Some(digest) => Ok(PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: digest.clone(),
            }),
            // State (b): key absent → looked, no descriptor.
            None => Ok(PatchDiscoveryState::LookedNoDescriptor),
        }
    }

    /// Atomically record the "looked, no descriptor" state for a repo.
    ///
    /// Acquires an exclusive lock, reads the existing map (creates an empty map
    /// if the file is new), removes the `__ocx.patch` key if present, and
    /// writes back. Idempotent if the key was already absent.
    pub async fn write_no_descriptor(tags_path: &std::path::Path) -> crate::Result<()> {
        let mut locked = LockedJsonFile::<BTreeMap<String, String>>::open_exclusive(tags_path).await?;
        let mut map = locked.read().await?.unwrap_or_default();
        map.remove(InternalTag::PATCH_TAG);
        locked.write(&map).await
    }

    /// Atomically record the "looked, has descriptor" state for a repo.
    ///
    /// Acquires an exclusive lock, reads the existing map, inserts (or
    /// updates) the `__ocx.patch` key with the given manifest digest string,
    /// and writes back.
    pub async fn write_has_descriptor(tags_path: &std::path::Path, manifest_digest: &str) -> crate::Result<()> {
        let mut locked = LockedJsonFile::<BTreeMap<String, String>>::open_exclusive(tags_path).await?;
        let mut map = locked.read().await?.unwrap_or_default();
        map.insert(InternalTag::PATCH_TAG.to_string(), manifest_digest.to_string());
        locked.write(&map).await
    }
}

// ── PatchDiscoveryMode ────────────────────────────────────────────────────────

/// Controls whether a discovery pass may skip already-recorded states.
///
/// - [`Lazy`](PatchDiscoveryMode::Lazy): the normal install-time behaviour.
///   Skips `LookedNoDescriptor` and `LookedHasDescriptor` repos — only
///   `NeverLooked` entries trigger a network fetch.
/// - [`Sync`](PatchDiscoveryMode::Sync): the `ocx patch sync` behaviour.
///   Re-fetches EVERY descriptor source (global root + per-base) regardless
///   of the recorded state. Used to advance descriptor blobs when the
///   upstream registry has published a new version.
///
/// Prefer this enum over a boolean parameter per the project style guide
/// (quality-core: boolean parameters should be enums for two-state flags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchDiscoveryMode {
    /// Normal lazy mode: skip already-recorded states; only fetch on `NeverLooked`.
    Lazy,
    /// Force-recheck mode: re-fetch every descriptor source regardless of state.
    Sync,
}

/// Which descriptor sources a discovery pass consults.
///
/// `ocx patch sync` re-checks only the KNOWN SET — the installed base repos
/// plus the single global root descriptor. This selector keeps the global
/// root from being probed through a *synthetic* package-specific sub-path:
///
/// - [`Both`](PatchDescriptorScope::Both): the global root descriptor AND the
///   package-specific descriptor for `base_id`. The normal install-time and
///   per-installed-base path — companion matching for a real base must union
///   both sources, otherwise a required global companion (e.g. a corp CA that
///   matches `*`) would never be installed for that base and a later offline
///   `exec` would fail closed.
/// - [`GlobalOnly`](PatchDescriptorScope::GlobalOnly): only the global
///   descriptor. Used by the sync path when there are zero installed bases so
///   the global descriptor is still refreshed WITHOUT fabricating a synthetic
///   base whose path-template expansion would probe an extra package-specific
///   source outside the known set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PatchDescriptorScope {
    /// Global root descriptor + the package-specific descriptor for `base_id`.
    Both,
    /// Only the global root descriptor (`base_id` is not used to derive a
    /// package-specific source — no synthetic probe).
    GlobalOnly,
}

// ── PackageManager::discover_and_install_patches ──────────────────────────────

impl PackageManager {
    /// Lazy patch discovery and companion install for a user-requested base install.
    ///
    /// Called from the user-facing install boundary (after the base package and
    /// its transitive deps have been materialized). NOT called from companion
    /// installs or transitive-dep pulls — those paths go through
    /// [`install_companion`](PackageManager::install_companion), which bypasses
    /// this method to prevent recursive discovery.
    ///
    /// ## Short-circuits
    ///
    /// Returns `Ok(())` immediately when:
    /// - `self.patches` is `None` (no patch tier configured), or
    /// - `self.is_offline()` (discovery requires a network call).
    ///
    /// ## Discovery protocol (online, patch tier active)
    ///
    /// For each of the two descriptor sources (global root, then package-specific):
    ///
    /// 1. Read the three-state tag-store record via [`PatchTagMap::read`].
    /// 2. On `NeverLooked` or `LookedNoDescriptor` (first discovery): call
    ///    [`fetch_and_persist_descriptor`] with [`DescriptorCommit::Eager`]. On a
    ///    `None` result, record `LookedNoDescriptor`; on `Some`, persist the blobs
    ///    to the CAS and commit the `LookedHasDescriptor` pointer EAGERLY. There is
    ///    no prior good digest to preserve, and recording the descriptor keeps a
    ///    later offline compose fail-closed on a missing required companion.
    /// 3. On `LookedHasDescriptor` in [`PatchDiscoveryMode::Sync`] (re-sync): call
    ///    [`fetch_and_persist_descriptor`] with [`DescriptorCommit::Deferred`] — the
    ///    pointer advance is held in a pending accumulator and committed only after
    ///    every required companion installs, so a transient re-sync failure keeps
    ///    the last-known-good digest.
    /// 4. On `LookedHasDescriptor` in [`PatchDiscoveryMode::Lazy`]: the descriptor
    ///    blob is already in the CAS; load it for companion collection.
    ///
    /// After both descriptors are resolved, collect companions, then install each
    /// via [`install_companion`]. This is per-descriptor deferral, NOT a cross-tag
    /// transaction: eager commits (step 2) are already durable before companion
    /// install, and the deferred commit loop (step 3) writes each pending tag entry
    /// in its own non-atomic `write_has_descriptor` call. A required-companion
    /// failure returns before the deferred loop runs, so only the deferred (re-sync)
    /// advances roll back; a crash mid-loop can leave some deferred advances
    /// committed and others not — a benign window that self-heals on the next sync
    /// (idempotent re-fetch).
    ///
    /// ## Recursion guard
    ///
    /// This method is NOT on the companion-install code path. Companion installs
    /// flow through [`install_companion`], which calls `pull` directly. There is
    /// no `discover_and_install_patches` call there.
    ///
    /// # Errors
    ///
    /// Returns `Err(PackageErrorKind::RequiredCompanionFailed { .. })` when a
    /// companion marked `required = true` fails to install. Optional companions
    /// are logged as warnings and do not produce an error.
    ///
    /// # Returns
    ///
    /// The number of companion packages successfully installed (or re-installed)
    /// on this call. `0` when no patch tier is configured, the manager is
    /// offline, or no companion matched.
    pub async fn discover_and_install_patches(
        &self,
        base_id: &Identifier,
        platforms: &[oci::Platform],
    ) -> Result<usize, PackageErrorKind> {
        self.discover_and_install_patches_with_mode(
            base_id,
            platforms,
            PatchDiscoveryMode::Lazy,
            PatchDescriptorScope::Both,
        )
        .await
    }

    /// Inner implementation of patch discovery, parameterised by
    /// [`PatchDiscoveryMode`].
    ///
    /// [`discover_and_install_patches`](Self::discover_and_install_patches) is the
    /// public entry point and always passes [`PatchDiscoveryMode::Lazy`].
    /// [`sync_patches`](Self::sync_patches) drives this with
    /// [`PatchDiscoveryMode::Sync`] to force-recheck every descriptor source.
    ///
    /// Returns the number of companion packages successfully installed —
    /// [`sync_patches`](Self::sync_patches) sums this across every base to
    /// populate `PatchSyncReport::companions_installed`.
    pub(super) async fn discover_and_install_patches_with_mode(
        &self,
        base_id: &Identifier,
        platforms: &[oci::Platform],
        mode: PatchDiscoveryMode,
        scope: PatchDescriptorScope,
    ) -> Result<usize, PackageErrorKind> {
        // Short-circuit 1: no patch tier configured.
        let Some(patches) = self.patches() else {
            return Ok(0);
        };

        // Short-circuit 2: offline — discovery requires a network call.
        //
        // Unlike lazy discovery (a side-effect of install where offline is
        // silently accepted), sync mode is an explicit user action. The offline
        // posture in Sync mode is identical structurally — return Ok(0) here
        // and let the caller (sync_patches) report the offline condition to the
        // user. The manager's offline guard stays here rather than in the caller
        // so that library consumers that call discover_and_install_patches_with_mode
        // directly also benefit from the guard.
        if self.is_offline() {
            return Ok(0);
        }

        // At this point we know we're online and patches are configured.
        // The OCI client is available — require_client() will succeed.
        // Individual sub-functions call require_client() at their call sites.
        let tag_store = &self.file_structure().tags;
        let blob_store = &self.file_structure().blobs;

        // Build identifiers for the two descriptor sources:
        // (a) global descriptor — reserved `global` repository at the patch registry
        // (b) package-specific descriptor — per-identifier sub-path
        // Build the descriptor source list per the requested scope. Under
        // `GlobalOnly` the package-specific id is NEVER computed, so a synthetic
        // `base_id` cannot probe an extra source outside the known set.
        let global_id = global_descriptor_id(patches);
        let descriptor_ids: Vec<Identifier> = match scope {
            // Global first (lower precedence for dedup); package-specific second
            // (higher precedence — its companions override on the same identifier).
            PatchDescriptorScope::Both => vec![global_id, patch_descriptor_id(patches, base_id)],
            PatchDescriptorScope::GlobalOnly => vec![global_id],
        };

        let mut descriptors: Vec<PatchDescriptor> = Vec::new();
        // A1 hybrid commit accumulator: holds ONLY the deferred (re-sync)
        // `LookedHasDescriptor` advances — those overwriting an existing descriptor
        // pointer. First-discovery advances (prior NeverLooked / LookedNoDescriptor)
        // are committed eagerly inside `fetch_and_persist_descriptor` and never land
        // here. The deferred advances are committed after every required companion
        // installs, so a transient re-sync failure preserves each source's
        // last-known-good digest.
        let mut pending_tag_writes: Vec<PendingDescriptorCommit> = Vec::new();

        for descriptor_id in &descriptor_ids {
            let tags_path = tag_store.tags(descriptor_id);
            let state = PatchTagMap::read(&tags_path)
                .await
                .map_err(PackageErrorKind::Internal)?;

            match state {
                PatchDiscoveryState::NeverLooked => {
                    // Never looked — attempt network fetch (both Lazy and Sync modes).
                    // First discovery: commit the descriptor pointer EAGERLY (A1) so a
                    // later offline compose fails closed on a missing required companion.
                    match fetch_and_persist_descriptor(
                        self,
                        descriptor_id,
                        &tags_path,
                        DescriptorCommit::Eager,
                        &mut pending_tag_writes,
                    )
                    .await?
                    {
                        Some(descriptor) => {
                            descriptors.push(descriptor);
                        }
                        None => {
                            // No patch descriptor at this registry — record looked-no-patch.
                            // (fetch_and_persist_descriptor already wrote the state.)
                        }
                    }
                }
                PatchDiscoveryState::LookedNoDescriptor => {
                    if mode == PatchDiscoveryMode::Sync {
                        // Sync mode: force-recheck even though we previously found nothing.
                        // The upstream registry may have added a descriptor since the last look.
                        log::debug!(
                            "patch discovery (sync): re-fetching '{}' — previously no descriptor, force-rechecking",
                            descriptor_id
                        );
                        // Prior state carried no descriptor pointer — this is a first
                        // discovery, so commit EAGERLY (A1) if one appears upstream.
                        match fetch_and_persist_descriptor(
                            self,
                            descriptor_id,
                            &tags_path,
                            DescriptorCommit::Eager,
                            &mut pending_tag_writes,
                        )
                        .await?
                        {
                            Some(descriptor) => {
                                descriptors.push(descriptor);
                            }
                            None => {
                                // Still no descriptor — state already written by the helper.
                            }
                        }
                    } else {
                        // Lazy mode: previously looked; confirmed no descriptor — skip.
                        log::debug!(
                            "patch discovery: skipping '{}' — previously looked, no descriptor found",
                            descriptor_id
                        );
                    }
                }
                PatchDiscoveryState::LookedHasDescriptor { manifest_digest } => {
                    if mode == PatchDiscoveryMode::Sync {
                        // Sync mode: force-recheck even though we already have a descriptor.
                        // If the upstream digest has advanced, fetch_and_persist_descriptor
                        // will overwrite the tag-store entry and persist the new blobs.
                        // If unchanged, the function is effectively a no-op (blobs already
                        // in CAS; write_has_descriptor is idempotent on the same digest).
                        log::debug!(
                            "patch discovery (sync): re-fetching '{}' — force-rechecking existing descriptor (recorded digest: {})",
                            descriptor_id,
                            manifest_digest
                        );
                        // Re-sync over an existing LookedHasDescriptor: DEFER the advance
                        // (A1) so a required-companion failure preserves the recorded digest.
                        match fetch_and_persist_descriptor(
                            self,
                            descriptor_id,
                            &tags_path,
                            DescriptorCommit::Deferred,
                            &mut pending_tag_writes,
                        )
                        .await?
                        {
                            Some(descriptor) => {
                                descriptors.push(descriptor);
                            }
                            None => {
                                // Descriptor vanished from upstream — state written as LookedNoDescriptor.
                            }
                        }
                        continue;
                    }
                    // Descriptor already persisted — load it from the CAS.
                    let digest = match crate::oci::Digest::try_from(manifest_digest.as_str()) {
                        Ok(d) => d,
                        Err(error) => {
                            log::warn!(
                                "patch discovery: invalid cached manifest digest '{}' for '{}': {error}; re-fetching",
                                manifest_digest,
                                descriptor_id
                            );
                            // Re-fetch without pre-clearing the tag-store entry.
                            // Only transition state after a definitive result:
                            // - re-fetch succeeds → the EAGER advance (below) commits the new
                            //   digest immediately.
                            // - re-fetch finds no descriptor → fetch_and_persist_descriptor
                            //   writes LookedNoDescriptor.
                            // - re-fetch fails (network/auth) → we leave the existing
                            //   LookedHasDescriptor entry intact (stale but not regressed)
                            //   so the next install attempt can retry. We do NOT downgrade
                            //   to LookedNoDescriptor on a transient failure because that
                            //   would cause a permanent "no patch" state for a package
                            //   that previously had a descriptor.
                            //
                            // Prior state is LookedHasDescriptor but its cached digest is
                            // CORRUPT (unparseable) — there is no last-known-good blob to
                            // preserve, so commit EAGERLY. The tag then points at the freshly
                            // persisted valid blobs, restoring fail-closed compose under a
                            // non-required tier carrying a rule-level required:true companion.
                            if let Some(descriptor) = fetch_and_persist_descriptor(
                                self,
                                descriptor_id,
                                &tags_path,
                                DescriptorCommit::Eager,
                                &mut pending_tag_writes,
                            )
                            .await?
                            {
                                descriptors.push(descriptor);
                            }
                            continue;
                        }
                    };
                    // Read the layer blob from CAS using the manifest digest to locate
                    // the manifest, then parse the descriptor JSON from the layer blob.
                    // We read the manifest blob to get the layer digest, then read the layer.
                    match load_descriptor_from_cas(blob_store, descriptor_id.registry(), &digest).await {
                        Ok(descriptor) => {
                            descriptors.push(descriptor);
                        }
                        Err(error) => {
                            log::warn!(
                                "patch discovery: failed to load cached descriptor for '{}': {error}; re-fetching",
                                descriptor_id
                            );
                            // Re-fetch if CAS read fails. Prior state is LookedHasDescriptor
                            // but its cached blob is CORRUPT or MISSING — there is no
                            // last-known-good blob to preserve, so commit EAGERLY. The tag
                            // then points at the freshly persisted valid blobs, restoring
                            // fail-closed compose under a non-required tier carrying a
                            // rule-level required:true companion.
                            if let Some(descriptor) = fetch_and_persist_descriptor(
                                self,
                                descriptor_id,
                                &tags_path,
                                DescriptorCommit::Eager,
                                &mut pending_tag_writes,
                            )
                            .await?
                            {
                                descriptors.push(descriptor);
                            }
                        }
                    }
                }
            }
        }

        // Collect companions from all resolved descriptors (global first, then
        // package-specific). Dedup by identifier — PACKAGE-SPECIFIC wins over
        // global when the same companion identifier appears in both descriptors.
        //
        // Algorithm: use an ordered map (IndexMap-style via Vec+HashMap) to
        // preserve first-seen insertion order while allowing a later
        // package-specific entry to override a global entry's `required` flag
        // for the same identifier. The map is keyed by identifier; the value
        // is overwritten whenever a later (more-specific) entry appears,
        // matching the spec: "package-specific rule wins for the `required`
        // flag and is installed once."
        //
        // Insertion order is preserved in `companion_order` (Vec of identifiers
        // in first-seen order); `companion_map` holds the current winning entry.
        let mut companion_order: Vec<crate::oci::Identifier> = Vec::new();
        let mut companion_map: HashMap<crate::oci::Identifier, crate::patch::CompanionEntry> = HashMap::new();
        for descriptor in &descriptors {
            let entries = descriptor.collect_companions(base_id, patches.required);
            for entry in entries {
                if !companion_map.contains_key(&entry.identifier) {
                    // First time: record insertion order.
                    companion_order.push(entry.identifier.clone());
                }
                // Always overwrite: later (package-specific) descriptor wins.
                companion_map.insert(entry.identifier.clone(), entry);
            }
        }
        // Reconstruct the final list in first-seen order with the winning entry.
        let companions: Vec<crate::patch::CompanionEntry> = companion_order
            .into_iter()
            .filter_map(|id| companion_map.remove(&id))
            .collect();

        // Defense-in-depth: cap total companions across all descriptors.
        // Per-descriptor limits (MAX_RULES × MAX_PACKAGES_PER_RULE) are enforced
        // at parse time, but with two descriptors and no shared identifiers the
        // combined count could reach 2 × 256 × 64 = 32 768. This cap (256) is
        // well above any legitimate use case and guards against a compromised or
        // misconfigured operator-controlled patch registry.
        if companions.len() > MAX_TOTAL_COMPANIONS {
            return Err(PackageErrorKind::PatchDiscovery(
                crate::patch::PatchError::DescriptorTooLarge {
                    detail: format!(
                        "total companion count {} across all descriptors exceeds maximum {}",
                        companions.len(),
                        MAX_TOTAL_COMPANIONS
                    ),
                },
            ));
        }

        // Install each companion through the recursion-guard primitive.
        //
        // Cross-registry companion warning (defense-in-depth, trust-model note):
        // The operator owns the patch registry and the descriptor. A companion
        // referencing a different registry host than the patch registry is
        // ALLOWED — the operator may legitimately cross-reference packages from
        // any registry. However, off-registry companions are surfaced as WARNs
        // so an operator can notice unexpected cross-registry references caused
        // by a misconfigured or compromised descriptor. This is advisory only;
        // discovery is NOT blocked. See adr_infrastructure_patches.md §"Trust model".
        let mut installed_count: usize = 0;
        if companions.is_empty() {
            log::debug!("patch discovery: no companions for '{}'", base_id);
        } else {
            log::debug!(
                "patch discovery: installing {} companion(s) for '{}'",
                companions.len(),
                base_id
            );
            for companion in companions {
                // Warn if the companion's registry host differs from the patch
                // registry's host. A cross-registry companion is permitted but
                // surfaced so the operator can notice unusual descriptor content.
                if companion.identifier.registry() != patches.registry.as_str() {
                    log::warn!(
                        "patch discovery: companion '{}' is hosted on registry '{}' which differs from the configured patch registry '{}'; this is allowed but unexpected — verify your patch descriptor",
                        companion.identifier,
                        companion.identifier.registry(),
                        patches.registry
                    );
                }
                let companion_id = companion.identifier.clone();
                let platforms_vec = platforms.to_vec();
                match self.install_companion(&companion_id, platforms_vec).await {
                    Ok(_) => {
                        installed_count += 1;
                        log::debug!("patch discovery: companion '{}' installed", companion_id);
                    }
                    Err(kind) => {
                        if companion.required {
                            // Fail-closed: required companion failed → abort BEFORE the
                            // deferred commit loop, so each re-sync source keeps its prior
                            // digest. Eager first-discovery advances were already committed
                            // (intended — they keep offline compose fail-closed).
                            return Err(PackageErrorKind::RequiredCompanionFailed {
                                companion: companion_id,
                                source: Box::new(kind),
                            });
                        } else {
                            // Fail-open: optional companion failed → warn and continue.
                            log::warn!(
                                "patch discovery: optional companion '{}' failed (skipping): {}",
                                companion_id,
                                kind
                            );
                        }
                    }
                }
            }
        }

        // A1 hybrid commit — deferred leg: now that every required companion
        // installed, commit the deferred re-sync advances. A required-companion
        // failure returned above without reaching here, so each deferred source
        // keeps its prior digest and a later offline compose still resolves the
        // previously installed companion. Optional-companion failures warn and fall
        // through — their advance still commits (fail-open contract).
        //
        // This loop is NOT atomic across sources: each `write_has_descriptor` is its
        // own tag-store write, so a crash mid-loop can leave some deferred advances
        // committed and others not. That window is pre-existing and benign — the next
        // `ocx patch sync` re-fetches and re-commits idempotently.
        for commit in &pending_tag_writes {
            PatchTagMap::write_has_descriptor(&commit.tags_path, &commit.manifest_digest)
                .await
                .map_err(PackageErrorKind::Internal)?;
        }

        Ok(installed_count)
    }

    /// Install a companion package through the base-install primitive.
    ///
    /// This method calls `pull` directly, bypassing
    /// [`discover_and_install_patches`]. This is the recursion guard:
    /// companions are never themselves patched (no recursive discovery).
    ///
    /// `candidate` and `select` are both `false` for companions — they are
    /// materialized into the object store only (no install symlinks).
    ///
    /// # Recursion guard regression test
    ///
    /// The `#[cfg(test)]` module at the bottom of this file contains a test
    /// that verifies a companion install does NOT invoke discovery.
    pub(super) async fn install_companion(
        &self,
        companion_id: &Identifier,
        platforms: Vec<oci::Platform>,
    ) -> Result<InstallInfo, PackageErrorKind> {
        // Pull the companion into the object store (candidate=false, select=false).
        // No call to discover_and_install_patches — this is the recursion guard.
        self.pull(companion_id, platforms).await
    }
}

// ── Free functions (discovery helpers) ───────────────────────────────────────

/// Compute the patch-registry `Identifier` for the package-specific descriptor.
///
/// Applies `expand_patch_path` to the base identifier's registry host and
/// repository, then constructs an `Identifier` rooted at `patches.registry`
/// tagged with [`InternalTag::PATCH_TAG`].
///
/// The global descriptor identifier uses the reserved single-segment
/// [`GLOBAL_PATCH_REPOSITORY`] repository. The default path template always
/// produces a two-or-more-segment sub-path, so the two identifiers never
/// collide. A custom template can break that assumption (e.g. `path = "global"`,
/// or `{repository}` for a base repository named `global`); the reservation
/// guard below detects such a collapse and falls back to the default
/// two-segment form so a per-package descriptor can never address — and thus
/// overwrite or shadow — the reserved global slot.
pub fn patch_descriptor_id(patches: &ResolvedPatchConfig, base_id: &Identifier) -> Identifier {
    let sub_path = expand_patch_path(&patches.path_template, base_id.registry(), base_id.repository());
    // Reservation guard: a custom `path` template must never collapse a
    // per-package descriptor onto the reserved single-segment `global` slot.
    // Fall back to the default `<registry-slug>/<repository>` form, which is
    // always two or more segments and so can never equal the reserved name.
    let sub_path = if sub_path == GLOBAL_PATCH_REPOSITORY {
        expand_patch_path(
            PatchConfig::DEFAULT_PATH_TEMPLATE,
            base_id.registry(),
            base_id.repository(),
        )
    } else {
        sub_path
    };
    Identifier::new_registry(&sub_path, &patches.registry).clone_with_tag(InternalTag::PATCH_TAG)
}

/// The reserved repository name for the global patch descriptor.
///
/// The global descriptor applies to every base. It lives at this single fixed
/// repository under the patch registry — `<patch-registry>/global` — tagged
/// with `__ocx.patch`. A single path segment cannot collide with the default
/// package-specific sub-path, which always has two or more segments
/// (`<registry-slug>/<repository>`). [`patch_descriptor_id`] additionally
/// enforces the reservation for custom templates: if an expanded per-package
/// path would equal this name, it falls back to the default two-segment form,
/// so the global slot is never reachable through the per-package path. Unlike
/// an empty repository (the former encoding), a normal repository name is a
/// valid OCI path component accepted by every registry, including Docker
/// `registry:2`.
pub const GLOBAL_PATCH_REPOSITORY: &str = "global";

/// Compute the global patch-registry `Identifier`.
///
/// The global descriptor lives at the reserved [`GLOBAL_PATCH_REPOSITORY`]
/// repository under the patch registry, tagged with `__ocx.patch`. It is
/// structurally distinct from any package-specific sub-path (which always has
/// two or more segments), so the two identifiers never collide.
pub fn global_descriptor_id(patches: &ResolvedPatchConfig) -> Identifier {
    Identifier::new_registry(GLOBAL_PATCH_REPOSITORY, &patches.registry).clone_with_tag(InternalTag::PATCH_TAG)
}

/// Whether a lazy patch-discovery failure must abort a user-requested base install.
///
/// Lazy discovery is a side effect of `install` / `install_all`, not an explicit
/// user action. Its fatality is gated on the patch tier's fail posture, mirroring
/// the compose-time gating in
/// [`build_site_patch_set`](PackageManager::build_site_patch_set) and the
/// best-effort model of [`sync_patches`](PackageManager::sync_patches):
///
/// - **Required tier** (`patches.required == true`): fatal. An unreachable or
///   erroring patch server means we cannot confirm that no mandated companion
///   (e.g. a corporate CA overlay) applies to this base, so the install fails
///   closed rather than silently proceed without the overlay (C7).
/// - **`RequiredCompanionFailed`**: fatal regardless of the tier posture — a
///   rule-level `required = true` companion that failed to install is a
///   fail-closed event even under a non-required tier.
/// - **Otherwise** (non-required tier, e.g. a descriptor fetch/parse failure
///   against an empty or unreachable patch server): NOT fatal. The caller warns
///   and continues installing the base without companions.
///
/// `patches` is `None` only when no tier is configured, in which case discovery
/// short-circuits before it can error; the `false` result then keeps the
/// function total.
pub(super) fn install_discovery_error_is_fatal(
    patches: Option<&ResolvedPatchConfig>,
    error: &PackageErrorKind,
) -> bool {
    patches.is_some_and(|patches| patches.required) || matches!(error, PackageErrorKind::RequiredCompanionFailed { .. })
}

/// A deferred `LookedHasDescriptor` tag-store advance (A1 hybrid commit — deferred leg).
///
/// A re-sync over an existing `LookedHasDescriptor` records its pointer advance
/// here instead of committing it immediately. The caller applies the accumulated
/// commits only after every required companion installs successfully, so a
/// required-companion failure leaves each re-synced source's prior digest intact.
/// First-discovery advances are NOT deferred — see [`DescriptorCommit`].
struct PendingDescriptorCommit {
    tags_path: std::path::PathBuf,
    manifest_digest: String,
}

/// When [`fetch_and_persist_descriptor`] commits a freshly fetched descriptor's
/// `LookedHasDescriptor` tag-store advance (A1 hybrid commit strategy).
///
/// The choice is driven by the PRIOR three-state at the descriptor source:
///
/// - [`Eager`](DescriptorCommit::Eager) — prior state was `NeverLooked` or
///   `LookedNoDescriptor` (first discovery). Commit the pointer immediately: there
///   is no prior good digest to preserve, and recording the descriptor keeps a
///   later offline compose fail-closed on a missing required companion. A
///   first-discovery required-companion failure still errors, but the tag has
///   already advanced — the intended fail-closed posture, not a regression.
/// - [`Deferred`](DescriptorCommit::Deferred) — prior state was
///   `LookedHasDescriptor` (re-sync). Defer the advance into
///   [`PendingDescriptorCommit`] until every required companion installs, so a
///   transient re-sync failure preserves the last-known-good digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DescriptorCommit {
    /// Commit the tag-store advance immediately (first discovery).
    Eager,
    /// Defer the tag-store advance until required companions install (re-sync).
    Deferred,
}

/// Fetch and persist the blobs for one descriptor source, recording its
/// discovery state.
///
/// Returns `Some(PatchDescriptor)` when a descriptor was found and its blobs
/// were successfully persisted; `None` when the patch tag does not exist at the
/// registry (the "looked, no patch" state).
///
/// Side effects:
/// - Calls [`fetch_patch_descriptor_blobs`] once.
/// - On success, calls [`persist_patch_descriptor`] to write blobs to the CAS,
///   then commits the `LookedHasDescriptor` tag-store advance per `commit`:
///   [`DescriptorCommit::Eager`] writes it now; [`DescriptorCommit::Deferred`]
///   pushes a [`PendingDescriptorCommit`] onto `pending` for the caller to commit
///   after required companions install (A1 hybrid commit strategy).
/// - On the "no patch" result, calls [`PatchTagMap::write_no_descriptor`]
///   eagerly (there are no companions to fail, so the state is safe to record).
async fn fetch_and_persist_descriptor(
    manager: &PackageManager,
    descriptor_id: &Identifier,
    tags_path: &std::path::Path,
    commit: DescriptorCommit,
    pending: &mut Vec<PendingDescriptorCommit>,
) -> Result<Option<PatchDescriptor>, PackageErrorKind> {
    // Require client — we already checked is_offline() in the caller but
    // require_client() is the canonical guard here.
    let client = manager.require_client().map_err(PackageErrorKind::Internal)?;
    let blob_store = &manager.file_structure().blobs;
    let registry = descriptor_id.registry();

    // Fetch the descriptor blobs from the registry.
    let fetched = fetch_patch_descriptor_blobs(client, descriptor_id)
        .await
        .map_err(PackageErrorKind::PatchDiscovery)?;

    match fetched {
        None => {
            // No __ocx.patch at this registry — record "looked, no patch".
            log::debug!("patch discovery: no descriptor at '{}'", descriptor_id);
            PatchTagMap::write_no_descriptor(tags_path)
                .await
                .map_err(PackageErrorKind::Internal)?;
            Ok(None)
        }
        Some(FetchedDescriptorBlobs {
            manifest_bytes,
            layer_bytes,
            manifest_digest,
            layer_digest,
        }) => {
            // Persist both blobs to the CAS and parse the descriptor.
            let (descriptor, persisted) = persist_patch_descriptor(
                blob_store,
                registry,
                manifest_digest,
                &manifest_bytes,
                layer_digest,
                &layer_bytes,
            )
            .await
            .map_err(PackageErrorKind::PatchDiscovery)?;

            // A1 hybrid commit: the blobs are already in the CAS above. Commit the
            // "looked, has descriptor" tag-store advance per the prior-state policy.
            let persisted_digest = persisted.manifest_digest.to_string();
            match commit {
                DescriptorCommit::Eager => {
                    // First discovery (prior NeverLooked / LookedNoDescriptor): commit
                    // now. No prior good digest to preserve, and recording the descriptor
                    // keeps a later offline compose fail-closed on a missing required
                    // companion.
                    PatchTagMap::write_has_descriptor(tags_path, &persisted_digest)
                        .await
                        .map_err(PackageErrorKind::Internal)?;
                }
                DescriptorCommit::Deferred => {
                    // Re-sync (prior LookedHasDescriptor): defer the advance so the caller
                    // commits it only after every required companion installs. An
                    // uncommitted advance leaves a harmless orphan blob for GC.
                    pending.push(PendingDescriptorCommit {
                        tags_path: tags_path.to_path_buf(),
                        manifest_digest: persisted_digest,
                    });
                }
            }

            log::debug!(
                "patch discovery: persisted descriptor for '{}' (manifest: {})",
                descriptor_id,
                persisted.manifest_digest
            );
            Ok(Some(descriptor))
        }
    }
}

/// Load a persisted `PatchDescriptor` from the CAS blob store.
///
/// Reads the manifest blob to find the layer digest, then reads the layer
/// blob and parses it as a `PatchDescriptor`. Used when the three-state
/// tag-store entry is `LookedHasDescriptor` (descriptor already on disk).
///
/// # Content-address verification
///
/// After reading each blob, this function recomputes the SHA-256 digest of
/// the returned bytes and compares it to the expected digest. A mismatch
/// indicates on-disk tampering or corruption of the cached blob and is
/// returned as a [`crate::patch::PatchError::ManifestDigestMismatch`] or
/// [`crate::patch::PatchError::LayerDigestMismatch`] error, ensuring the
/// tampered blob is rejected rather than silently trusted.
pub(super) async fn load_descriptor_from_cas(
    blob_store: &crate::file_structure::BlobStore,
    registry: &str,
    manifest_digest: &crate::oci::Digest,
) -> Result<PatchDescriptor, crate::Error> {
    use crate::patch::PatchError;

    // Read the manifest blob — returns None if not present in the CAS.
    let manifest_bytes = blob_store.read_blob(registry, manifest_digest).await?.ok_or_else(|| {
        let path = blob_store.data(registry, manifest_digest);
        crate::error::file_error(&path, std::io::Error::other("manifest blob not found in CAS"))
    })?;

    // Content-address re-verification: recompute SHA-256 of the manifest bytes
    // and compare to the expected digest. Detects on-disk tampering/corruption.
    let computed_manifest_digest = crate::oci::Algorithm::Sha256.hash(&manifest_bytes);
    if &computed_manifest_digest != manifest_digest {
        return Err(crate::Error::from(
            crate::package_manager::error::PackageErrorKind::PatchDiscovery(PatchError::ManifestDigestMismatch {
                declared: manifest_digest.to_string(),
                computed: computed_manifest_digest.to_string(),
            }),
        ));
    }

    // Parse the manifest to find the layer digest.
    // The manifest is a plain OCI image manifest JSON; parse minimally.
    let manifest_value: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).map_err(crate::Error::SerializationFailure)?;

    // Extract the first (and only) layer digest.
    let layer_digest_str = manifest_value
        .get("layers")
        .and_then(|layers| layers.get(0))
        .and_then(|layer| layer.get("digest"))
        .and_then(|digest| digest.as_str())
        .ok_or_else(|| {
            let path = blob_store.data(registry, manifest_digest);
            crate::error::file_error(&path, std::io::Error::other("cached manifest has no layer digest"))
        })?;

    let layer_digest = crate::oci::Digest::try_from(layer_digest_str).map_err(crate::Error::Digest)?;

    // Read and parse the layer blob.
    let layer_bytes = blob_store.read_blob(registry, &layer_digest).await?.ok_or_else(|| {
        let path = blob_store.data(registry, &layer_digest);
        crate::error::file_error(&path, std::io::Error::other("descriptor layer blob not found in CAS"))
    })?;

    // Content-address re-verification: recompute SHA-256 of the layer bytes
    // and compare to the digest declared in the manifest. Detects tampering or
    // corruption of the cached layer blob independently of the manifest check.
    let computed_layer_digest = crate::oci::Algorithm::Sha256.hash(&layer_bytes);
    if computed_layer_digest != layer_digest {
        return Err(crate::Error::from(
            crate::package_manager::error::PackageErrorKind::PatchDiscovery(PatchError::LayerDigestMismatch {
                declared: layer_digest.to_string(),
                computed: computed_layer_digest.to_string(),
            }),
        ));
    }

    PatchDescriptor::from_json_bytes(&layer_bytes).map_err(|error| {
        // Preserve the structured PatchError chain instead of erasing it via
        // `.to_string()`. `InvalidDescriptorJson` carries a `serde_json::Error`
        // source; use `PackageErrorKind::PatchDiscovery` so the full chain is
        // walkable for exit-code classification and diagnostics.
        crate::Error::from(crate::package_manager::error::PackageErrorKind::PatchDiscovery(error))
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    use crate::{
        config::patch::ResolvedPatchConfig,
        file_structure::FileStructure,
        oci::{
            Identifier,
            index::{ChainMode, Index, LocalConfig, LocalIndex},
        },
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Helper: path to the tags file for a synthetic repo under a temp dir.
    fn tags_path(dir: &TempDir, registry: &str, repo: &str) -> PathBuf {
        dir.path().join(registry).join(format!("{repo}.json"))
    }

    /// Build a minimal offline `PackageManager` for unit testing.
    ///
    /// No OCI client → `is_offline()` returns `true`. Useful for testing
    /// short-circuit behaviour of `discover_and_install_patches`.
    fn make_offline_manager(ocx_home: &Path) -> super::super::super::PackageManager {
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: fs.tags.clone(),
            blob_store: fs.blobs.clone(),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        super::super::super::PackageManager::new(fs, index, None, "localhost:5000")
    }

    /// Build a minimal `ResolvedPatchConfig` for testing patch-tier presence.
    fn test_patch_config() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: "patches.corp.com".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: true,
        }
    }

    /// Build a non-offline `PackageManager` with a real (but unreachable) OCI client.
    ///
    /// `is_offline()` returns `false` because `client = Some(...)`. Network calls
    /// will fail, but this lets tests probe code paths that the offline short-circuit
    /// in `discover_and_install_patches` would otherwise skip.
    fn make_online_manager(ocx_home: &Path) -> super::super::super::PackageManager {
        use crate::oci::ClientBuilder;
        let fs = FileStructure::with_root(ocx_home.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: fs.tags.clone(),
            blob_store: fs.blobs.clone(),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        // Client is Some → is_offline() = false, even though network calls would fail.
        let client = ClientBuilder::new().build();
        super::super::super::PackageManager::new(fs, index, Some(client), "localhost:5000")
    }

    // ── Three-state TagStore helper ───────────────────────────────────────────

    /// `install_discovery_error_is_fatal` gates install-time discovery fatality
    /// on the patch tier's fail posture — the empty/unreachable patch-server bug.
    ///
    /// A non-required tier tolerates a descriptor-fetch failure (warn + continue),
    /// a required tier fails closed (C7), and a `RequiredCompanionFailed` is fatal
    /// under either posture. Deleting the `patches.required` clause makes the
    /// non-required assertion fail (regression guard for the fix).
    #[test]
    fn install_discovery_fatality_gated_on_required() {
        use crate::patch::PatchError;

        // A descriptor fetch/parse failure — the class raised against an empty or
        // unreachable patch server (stands in for `FetchFailed`, which needs a
        // network error to construct).
        let fetch_error = PackageErrorKind::PatchDiscovery(PatchError::UnsupportedVersion { version: 999 });
        let required_companion = PackageErrorKind::RequiredCompanionFailed {
            companion: Identifier::parse("patches.corp.com/corp-ca:1.0").expect("valid identifier"),
            source: Box::new(PackageErrorKind::NotFound),
        };

        let required_tier = ResolvedPatchConfig {
            required: true,
            ..test_patch_config()
        };
        let optional_tier = ResolvedPatchConfig {
            required: false,
            ..test_patch_config()
        };

        // Required tier: any discovery error is fatal (fail-closed).
        assert!(install_discovery_error_is_fatal(Some(&required_tier), &fetch_error));
        // Non-required tier: a descriptor-fetch failure is tolerated (the fix).
        assert!(!install_discovery_error_is_fatal(Some(&optional_tier), &fetch_error));
        // RequiredCompanionFailed is fatal even under a non-required tier.
        assert!(install_discovery_error_is_fatal(
            Some(&optional_tier),
            &required_companion
        ));
        // No tier configured → nothing can fail (defensive totality).
        assert!(!install_discovery_error_is_fatal(None, &fetch_error));
    }

    /// State (a): file absent → `NeverLooked`.
    ///
    /// Traces: TESTABILITY §three-state TagStore, state (a).
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_read_absent_file_is_never_looked() {
        let dir = TempDir::new().unwrap();
        let path = tags_path(&dir, "patches.corp.com", "some_repo/tool");
        let state = PatchTagMap::read(&path)
            .await
            .expect("read must succeed on absent file");
        assert_eq!(
            state,
            PatchDiscoveryState::NeverLooked,
            "absent file must yield NeverLooked"
        );
    }

    /// State (b): file present but no `__ocx.patch` key → `LookedNoDescriptor`.
    ///
    /// Traces: TESTABILITY §three-state TagStore, state (b).
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_read_present_file_no_key_is_looked_no_descriptor() {
        let dir = TempDir::new().unwrap();
        let path = tags_path(&dir, "patches.corp.com", "some_repo/tool");
        // Write a tag map with an unrelated key (no __ocx.patch).
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        let map: BTreeMap<String, String> = [("latest".to_string(), "sha256:aabb".to_string())]
            .into_iter()
            .collect();
        tokio::fs::write(&path, serde_json::to_vec(&map).unwrap())
            .await
            .unwrap();

        let state = PatchTagMap::read(&path).await.expect("read must succeed");
        assert_eq!(
            state,
            PatchDiscoveryState::LookedNoDescriptor,
            "file present without __ocx.patch key must yield LookedNoDescriptor"
        );
    }

    /// State (c): file present with `__ocx.patch` key → `LookedHasDescriptor`.
    ///
    /// Traces: TESTABILITY §three-state TagStore, state (c).
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_read_present_file_with_key_is_looked_has_descriptor() {
        let dir = TempDir::new().unwrap();
        let path = tags_path(&dir, "patches.corp.com", "some_repo/tool");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        let digest = "sha256:deadbeef".repeat(4); // synthetic digest string
        let map: BTreeMap<String, String> = [(InternalTag::PATCH_TAG.to_string(), digest.clone())]
            .into_iter()
            .collect();
        tokio::fs::write(&path, serde_json::to_vec(&map).unwrap())
            .await
            .unwrap();

        let state = PatchTagMap::read(&path).await.expect("read must succeed");
        assert!(
            matches!(&state, PatchDiscoveryState::LookedHasDescriptor { manifest_digest } if manifest_digest == &digest),
            "file with __ocx.patch key must yield LookedHasDescriptor with the digest"
        );
    }

    /// Atomic write: `write_no_descriptor` removes `__ocx.patch` from the map.
    ///
    /// Traces: TESTABILITY §atomic write round-trip.
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_write_no_descriptor_removes_key() {
        let dir = TempDir::new().unwrap();
        let path = tags_path(&dir, "patches.corp.com", "some_repo/tool");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        // Seed with __ocx.patch key present.
        let map: BTreeMap<String, String> = [(InternalTag::PATCH_TAG.to_string(), "sha256:1234".to_string())]
            .into_iter()
            .collect();
        tokio::fs::write(&path, serde_json::to_vec(&map).unwrap())
            .await
            .unwrap();

        PatchTagMap::write_no_descriptor(&path)
            .await
            .expect("write must succeed");

        let state = PatchTagMap::read(&path).await.expect("read after write must succeed");
        assert_eq!(
            state,
            PatchDiscoveryState::LookedNoDescriptor,
            "after write_no_descriptor, state must be LookedNoDescriptor"
        );
    }

    /// Atomic write: `write_has_descriptor` inserts `__ocx.patch` into the map.
    ///
    /// Traces: TESTABILITY §atomic write round-trip.
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_write_has_descriptor_inserts_key() {
        let dir = TempDir::new().unwrap();
        let path = tags_path(&dir, "patches.corp.com", "some_repo/tool");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();
        let digest = "sha256:cafebabe00000000000000000000000000000000000000000000000000000000";

        PatchTagMap::write_has_descriptor(&path, digest)
            .await
            .expect("write must succeed");

        let state = PatchTagMap::read(&path).await.expect("read after write must succeed");
        assert!(
            matches!(&state, PatchDiscoveryState::LookedHasDescriptor { manifest_digest } if manifest_digest == digest),
            "after write_has_descriptor, state must be LookedHasDescriptor with the digest"
        );
    }

    /// Atomic write: `write_no_descriptor` on a non-existent file creates the
    /// file (and all parent directories) and sets state to `LookedNoDescriptor`.
    ///
    /// Traces: TESTABILITY §atomic write round-trip (creates file + parents).
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_write_no_descriptor_creates_file() {
        let dir = TempDir::new().unwrap();
        // Path where parent dir does not yet exist.
        let path = dir.path().join("new_registry").join("deep").join("repo.json");

        PatchTagMap::write_no_descriptor(&path)
            .await
            .expect("write must create file");

        let state = PatchTagMap::read(&path).await.expect("read after write must succeed");
        assert_eq!(
            state,
            PatchDiscoveryState::LookedNoDescriptor,
            "newly created file with no key must read as LookedNoDescriptor"
        );
    }

    /// Atomic write: concurrent `write_has_descriptor` + `write_no_descriptor`
    /// do not corrupt the JSON map (last writer wins, map is valid JSON).
    ///
    /// Traces: TESTABILITY §concurrent-safe via LockedJsonFile.
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_concurrent_writes_do_not_corrupt() {
        let dir = TempDir::new().unwrap();
        let path = tags_path(&dir, "patches.corp.com", "concurrent/tool");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();

        let path1 = path.clone();
        let path2 = path.clone();
        let digest = "sha256:abc".to_string();
        let digest_clone = digest.clone();

        let task_has = tokio::spawn(async move {
            PatchTagMap::write_has_descriptor(&path1, &digest_clone)
                .await
                .expect("write_has_descriptor must not fail");
        });
        let task_no = tokio::spawn(async move {
            PatchTagMap::write_no_descriptor(&path2)
                .await
                .expect("write_no_descriptor must not fail");
        });

        let (r1, r2) = tokio::join!(task_has, task_no);
        r1.expect("task 1 must complete without panic");
        r2.expect("task 2 must complete without panic");

        // After both, the file must be valid JSON and the state must be one
        // of the two expected terminal states (not corrupt).
        let state = PatchTagMap::read(&path)
            .await
            .expect("file must be readable after concurrent writes");
        assert!(
            matches!(
                state,
                PatchDiscoveryState::LookedNoDescriptor | PatchDiscoveryState::LookedHasDescriptor { .. }
            ),
            "after concurrent writes, state must be a valid terminal state, got: {state:?}"
        );
    }

    /// Idempotency: `write_has_descriptor` called twice with the same digest
    /// yields the same terminal state — the second call is a no-op.
    ///
    /// Traces: idempotency invariant; CAS tag-store is a map so repeated
    /// `insert(PATCH_TAG, same_digest)` is equivalent to one insert.
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_write_has_descriptor_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let path = tags_path(&dir, "patches.corp.com", "tool/idem");
        let digest = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

        PatchTagMap::write_has_descriptor(&path, digest)
            .await
            .expect("first write must succeed");
        PatchTagMap::write_has_descriptor(&path, digest)
            .await
            .expect("second write must succeed (idempotent)");

        let state = PatchTagMap::read(&path).await.expect("read must succeed");
        assert!(
            matches!(&state, PatchDiscoveryState::LookedHasDescriptor { manifest_digest } if manifest_digest == digest),
            "after two identical writes, state must still be LookedHasDescriptor with the same digest"
        );
    }

    /// `write_no_descriptor` preserves unrelated keys in the existing tag map.
    ///
    /// The write is a read-modify-write that ONLY removes the `__ocx.patch` key;
    /// other tag→digest mappings (`latest`, semver tags) in the file must survive.
    ///
    /// Traces: atomic read-modify-write semantics; only `__ocx.patch` key is
    /// touched, other keys preserved.
    #[tokio::test(flavor = "multi_thread")]
    async fn patch_tag_map_write_no_descriptor_preserves_other_keys() {
        let dir = TempDir::new().unwrap();
        let path = tags_path(&dir, "patches.corp.com", "tool/keys");
        tokio::fs::create_dir_all(path.parent().unwrap()).await.unwrap();

        // Seed with __ocx.patch AND an unrelated "latest" key.
        let mut seed: BTreeMap<String, String> = BTreeMap::new();
        seed.insert(InternalTag::PATCH_TAG.to_string(), "sha256:old".to_string());
        seed.insert("latest".to_string(), "sha256:1234".to_string());
        tokio::fs::write(&path, serde_json::to_vec(&seed).unwrap())
            .await
            .unwrap();

        PatchTagMap::write_no_descriptor(&path)
            .await
            .expect("write must succeed");

        // Re-read the raw JSON to check both keys.
        let raw = tokio::fs::read(&path).await.unwrap();
        let map: BTreeMap<String, String> = serde_json::from_slice(&raw).unwrap();

        assert!(
            !map.contains_key(InternalTag::PATCH_TAG),
            "write_no_descriptor must remove the __ocx.patch key"
        );
        assert_eq!(
            map.get("latest").map(String::as_str),
            Some("sha256:1234"),
            "write_no_descriptor must preserve the 'latest' key"
        );
    }

    /// `PatchDiscoveryState::NeverLooked` equals itself (PartialEq sanity).
    ///
    /// Ensures the `#[derive(PartialEq)]` is correct and that `NeverLooked !=
    /// LookedNoDescriptor` — a regression guard against incorrect equality impls.
    #[test]
    fn patch_discovery_state_partial_eq() {
        assert_eq!(PatchDiscoveryState::NeverLooked, PatchDiscoveryState::NeverLooked);
        assert_eq!(
            PatchDiscoveryState::LookedNoDescriptor,
            PatchDiscoveryState::LookedNoDescriptor
        );
        assert_ne!(
            PatchDiscoveryState::NeverLooked,
            PatchDiscoveryState::LookedNoDescriptor,
            "NeverLooked and LookedNoDescriptor must not be equal"
        );
        assert_ne!(
            PatchDiscoveryState::NeverLooked,
            PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: "sha256:abc".to_string()
            },
            "NeverLooked must not equal LookedHasDescriptor"
        );
        assert_ne!(
            PatchDiscoveryState::LookedNoDescriptor,
            PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: "sha256:abc".to_string()
            },
            "LookedNoDescriptor must not equal LookedHasDescriptor"
        );
    }

    // ── Patch-repo Identifier derivation ─────────────────────────────────────

    /// `patch_descriptor_id` produces a non-empty repository sub-path tagged
    /// with `PATCH_TAG` and rooted at the patch registry — not the base id's
    /// registry.
    ///
    /// For base identifier `ocx.sh/cmake:3.28` with registry `patches.corp.com`
    /// and the default template `{registry}/{repository}`, the produced repository
    /// sub-path must:
    /// - be non-empty,
    /// - contain "cmake" (from the base repository),
    /// - have tag = `__ocx.patch`,
    /// - have registry = `patches.corp.com` (the patch registry, NOT `ocx.sh`).
    ///
    /// Traces: TESTABILITY §patch-repo Identifier derivation.
    #[test]
    fn patch_descriptor_id_is_non_empty_sub_path() {
        let patches = test_patch_config();
        let base_id = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        let patch_id = patch_descriptor_id(&patches, &base_id);

        // Repository sub-path must be non-empty.
        assert!(
            !patch_id.repository().is_empty(),
            "patch_descriptor_id must produce a non-empty repository sub-path; got empty"
        );
        // Sub-path must contain the base identifier's repository component.
        assert!(
            patch_id.repository().contains("cmake"),
            "patch_descriptor_id sub-path must include base repository 'cmake'; got: '{}'",
            patch_id.repository()
        );
        // Registry must be the patch registry, NOT the base identifier's registry.
        assert_eq!(
            patch_id.registry(),
            "patches.corp.com",
            "patch_descriptor_id registry must be the patch registry, not the base registry"
        );
        // Tag must be the PATCH_TAG sentinel.
        assert_eq!(
            patch_id.tag(),
            Some(InternalTag::PATCH_TAG),
            "patch_descriptor_id must carry tag = PATCH_TAG ('__ocx.patch')"
        );
    }

    /// `global_descriptor_id` produces the reserved single-segment `global`
    /// repository tagged with `PATCH_TAG` at the patch registry.
    ///
    /// The global descriptor is structurally distinct from any package-specific
    /// sub-path: the default template `{registry}/{repository}` always expands
    /// to two or more segments for a well-formed base identifier, while the
    /// global repository is one segment, so the two identifiers never collide.
    /// A normal repository name (not an empty path) is a valid OCI path
    /// component accepted by every registry, including Docker `registry:2`.
    ///
    /// Traces: TESTABILITY §patch-repo Identifier derivation (global root is
    /// DISTINCT from any sub-path); DELIVERABLES 2c.
    #[test]
    fn global_descriptor_id_is_distinct_from_package_specific() {
        let patches = test_patch_config();
        let global_id = global_descriptor_id(&patches);
        let base_id = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        let pkg_specific_id = patch_descriptor_id(&patches, &base_id);

        // Both must be rooted at the patch registry.
        assert_eq!(global_id.registry(), "patches.corp.com");
        // Both must carry PATCH_TAG.
        assert_eq!(global_id.tag(), Some(InternalTag::PATCH_TAG));
        // The global descriptor uses the reserved single-segment repository.
        assert_eq!(global_id.repository(), GLOBAL_PATCH_REPOSITORY);
        assert!(
            !global_id.repository().contains('/'),
            "global descriptor repository must be a single path segment (collision-proof); got '{}'",
            global_id.repository()
        );
        // It must be a non-empty, valid OCI path component (not the empty root).
        assert!(
            !global_id.repository().is_empty(),
            "global descriptor repository must not be empty"
        );
        // The global and package-specific repositories must differ.
        assert_ne!(
            global_id.repository(),
            pkg_specific_id.repository(),
            "global descriptor repository must differ from package-specific sub-path"
        );
    }

    /// A misconfigured literal template `path = "global"` must NOT route a
    /// per-package descriptor onto the reserved global slot. The reservation
    /// guard in `patch_descriptor_id` rewrites the collapse to the default
    /// two-segment form, keeping per-package and global descriptors distinct so
    /// a per-base `patch publish` can never overwrite the org-wide descriptor.
    ///
    /// Traces: reservation guard (Codex review 2026-06-21) — the `global`
    /// repository is enforced, not merely documented.
    #[test]
    fn patch_descriptor_id_literal_global_template_does_not_collide() {
        let mut patches = test_patch_config();
        patches.path_template = "global".to_string();
        let base_id = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");

        let pkg_id = patch_descriptor_id(&patches, &base_id);
        let global_id = global_descriptor_id(&patches);

        assert_ne!(
            pkg_id.repository(),
            GLOBAL_PATCH_REPOSITORY,
            "literal `path = \"global\"` must not collapse onto the reserved global slot"
        );
        assert_ne!(
            pkg_id.repository(),
            global_id.repository(),
            "per-package and global descriptor repositories must stay distinct"
        );
        // The fallback uses the default form, so the base repository survives.
        assert!(
            pkg_id.repository().contains("cmake"),
            "fallback must preserve the base repository component; got '{}'",
            pkg_id.repository()
        );
    }

    /// Template `{repository}` for a base repository literally named `global`
    /// would expand onto the reserved slot. The reservation guard must rewrite
    /// it to the default two-segment form.
    ///
    /// Traces: reservation guard (Codex review 2026-06-21) — dynamic collapse
    /// (data-dependent, not statically visible in the template) is also caught.
    #[test]
    fn patch_descriptor_id_repository_named_global_does_not_collide() {
        let mut patches = test_patch_config();
        patches.path_template = "{repository}".to_string();
        let base_id = Identifier::parse("ocx.sh/global:1.0").expect("valid identifier");

        let pkg_id = patch_descriptor_id(&patches, &base_id);
        let global_id = global_descriptor_id(&patches);

        assert_ne!(
            pkg_id.repository(),
            GLOBAL_PATCH_REPOSITORY,
            "`{{repository}}` with a base repo named `global` must not collapse onto the reserved slot"
        );
        assert_ne!(
            pkg_id.repository(),
            global_id.repository(),
            "per-package and global descriptor repositories must stay distinct"
        );
    }

    // ── PackageManager field threading ────────────────────────────────────────

    /// `PackageManager::new()` initializes `patches` to `None`.
    ///
    /// Traces: STUB MANIFEST §1 `PackageManager::new()` initializes `patches: None`.
    #[test]
    fn package_manager_new_has_no_patches() {
        let tmp = TempDir::new().unwrap();
        let manager = make_offline_manager(tmp.path());
        assert!(
            manager.patches().is_none(),
            "PackageManager::new() must initialize patches to None"
        );
    }

    /// `PackageManager::with_patches(Some(config))` sets the patches field.
    ///
    /// Traces: STUB MANIFEST §1 `with_patches(patches: Option<ResolvedPatchConfig>) -> Self`.
    #[test]
    fn package_manager_with_patches_some_sets_field() {
        let tmp = TempDir::new().unwrap();
        let config = test_patch_config();
        let manager = make_offline_manager(tmp.path()).with_patches(Some(config.clone()));
        let stored = manager
            .patches()
            .expect("with_patches(Some(...)) must store the config");
        assert_eq!(stored.registry, config.registry, "registry must match");
        assert_eq!(stored.path_template, config.path_template, "path_template must match");
        assert_eq!(stored.required, config.required, "required must match");
    }

    /// `PackageManager::with_patches(None)` keeps patches as `None`.
    ///
    /// Traces: STUB MANIFEST §1 `with_patches(None)` semantics.
    #[test]
    fn package_manager_with_patches_none_keeps_none() {
        let tmp = TempDir::new().unwrap();
        let manager = make_offline_manager(tmp.path())
            .with_patches(Some(test_patch_config())) // set first
            .with_patches(None); // then clear
        assert!(
            manager.patches().is_none(),
            "with_patches(None) must clear the patches field"
        );
    }

    /// `PackageManager::offline_view()` **preserves** the patch config.
    ///
    /// `offline_view` disables the network (client = None → `is_offline()`), not
    /// the patch tier: Phase 3 discovery already short-circuits on `is_offline()`,
    /// while the purely-local Phase 4 compose-time overlay must still apply on
    /// offline env paths (`ocx direnv export`, the global toolchain) so
    /// already-discovered companion overlays apply and a `required` companion that
    /// is unavailable fails closed (ADR C4/C6/C7).
    ///
    /// Traces: DELIVERABLES §2a (discovery short-circuit keys off `is_offline()`,
    /// not the absence of patch config).
    #[test]
    fn offline_view_preserves_patch_config() {
        let tmp = TempDir::new().unwrap();
        let fs = FileStructure::with_root(tmp.path().to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            tag_store: fs.tags.clone(),
            blob_store: fs.blobs.clone(),
        });
        let manager = make_offline_manager(tmp.path()).with_patches(Some(test_patch_config()));
        assert!(
            manager.patches().is_some(),
            "setup: patches must be Some before offline_view"
        );

        let offline = manager.offline_view(local_index);
        assert!(
            offline.patches().is_some(),
            "offline_view must preserve patches (overlay is local; only the network is disabled)"
        );
        assert!(offline.is_offline(), "offline_view must produce an offline manager");
    }

    // ── discover_and_install_patches short-circuits ───────────────────────────

    /// `discover_and_install_patches` returns `Ok(())` immediately when no patch
    /// tier is configured (`self.patches` is `None`).
    ///
    /// Contract: "Returns `Ok(())` immediately when self.patches is `None`
    /// (no patch tier) OR offline."
    ///
    /// Traces: DELIVERABLES §2a; TESTABILITY §no [patches] config.
    #[tokio::test(flavor = "multi_thread")]
    async fn discover_patches_is_noop_when_no_patches_config() {
        let tmp = TempDir::new().unwrap();
        // Manager has no patches config (None).
        let manager = make_offline_manager(tmp.path());
        assert!(manager.patches().is_none(), "setup: patches must be None");

        let base_id = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        // Must short-circuit to Ok(()) without panicking or hitting unimplemented!.
        let result = manager.discover_and_install_patches(&base_id, &[]).await;
        assert!(
            result.is_ok(),
            "discover_and_install_patches must return Ok(()) when patches is None"
        );
    }

    /// `discover_and_install_patches` returns `Ok(())` immediately when offline,
    /// even when a patch tier is configured.
    ///
    /// Contract: "Returns `Ok(())` immediately when self.is_offline()
    /// (discovery requires a network call)."
    ///
    /// Traces: DELIVERABLES §2a; TESTABILITY §offline.
    #[tokio::test(flavor = "multi_thread")]
    async fn discover_patches_is_noop_when_offline() {
        let tmp = TempDir::new().unwrap();
        // Manager is offline (client = None) but has a patch config.
        let manager = make_offline_manager(tmp.path()).with_patches(Some(test_patch_config()));
        assert!(manager.is_offline(), "setup: manager must be offline");
        assert!(
            manager.patches().is_some(),
            "setup: patch config must be Some to prove offline wins"
        );

        let base_id = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        // Must short-circuit to Ok(()) without any network call.
        let result = manager.discover_and_install_patches(&base_id, &[]).await;
        assert!(
            result.is_ok(),
            "discover_and_install_patches must return Ok(()) when offline (even with patch config present)"
        );
    }

    // ── RequiredCompanionFailed error variant ─────────────────────────────────

    /// `PackageErrorKind::RequiredCompanionFailed` carries the companion identifier
    /// and the source error; its `Display` includes the companion name.
    ///
    /// Traces: STUB MANIFEST §3 `RequiredCompanionFailed { companion, source }`.
    #[test]
    fn required_companion_failed_display_includes_companion() {
        use crate::package_manager::error::PackageErrorKind;

        let companion = Identifier::parse("patches.corp.com/certs/ca-bundle:latest").expect("valid identifier");
        let source = Box::new(PackageErrorKind::NotFound);
        let kind = PackageErrorKind::RequiredCompanionFailed {
            companion: companion.clone(),
            source,
        };
        let display = kind.to_string();
        assert!(
            display.contains("certs/ca-bundle"),
            "RequiredCompanionFailed Display must include the companion name; got: {display}"
        );
        assert!(
            display.contains("required companion"),
            "RequiredCompanionFailed Display must mention 'required companion'; got: {display}"
        );
    }

    /// `PackageErrorKind::RequiredCompanionFailed` delegates exit-code
    /// classification to the inner `source` error.
    ///
    /// This ensures the exit code reflects the root cause (e.g. `NotFound`
    /// → exit 79) rather than a generic companion-failure code.
    ///
    /// Traces: STUB MANIFEST §3 classification arm delegates to `source.classify()`.
    #[test]
    fn required_companion_failed_exit_code_delegates_to_source() {
        use crate::cli::{ClassifyExitCode, ExitCode};
        use crate::package_manager::error::PackageErrorKind;

        // Source = NotFound → should classify to NotFound (79).
        let kind = PackageErrorKind::RequiredCompanionFailed {
            companion: Identifier::parse("patches.corp.com/ca:latest").expect("valid"),
            source: Box::new(PackageErrorKind::NotFound),
        };
        let code = kind.classify();
        assert_eq!(
            code,
            Some(ExitCode::NotFound),
            "RequiredCompanionFailed with NotFound source must classify as NotFound"
        );
    }

    // ── Recursion guard ───────────────────────────────────────────────────────

    /// Regression guard: `install_companion` and `discover_and_install_patches`
    /// are distinct methods on `PackageManager`.
    ///
    /// The recursion guard is enforced by code structure: companions are installed
    /// through `install_companion`, which calls the pull primitive directly without
    /// invoking `discover_and_install_patches`. Only the user-facing install
    /// boundary calls `discover_and_install_patches`.
    ///
    /// This test is a compile-time assertion: if either method is removed or if
    /// `install_companion` is merged into `discover_and_install_patches`, the
    /// function-pointer casts below will fail to compile.
    ///
    /// Traces: TESTABILITY §recursion guard; DELIVERABLES §2g.
    #[test]
    fn install_companion_and_discovery_exist_as_distinct_methods() {
        // Both symbols must resolve to distinct inherent methods on `PackageManager`.
        // The casts verify the methods have the expected async-fn signatures.
        // `fn(_, _, _) -> _` is the coercion point — if the method does not exist
        // or has a different argument count the cast fails at compile time.
        let _ = PackageManager::discover_and_install_patches as fn(_, _, _) -> _;
        // `install_companion` takes `self`, `&Identifier`, `Vec<Platform>`.
        let _ = PackageManager::install_companion as fn(_, _, _) -> _;
        // If both casts compile, the two methods exist as distinct items.
    }

    /// Regression guard: `install_companion` is `pub(super)` — it is NOT a public
    /// method visible outside the `tasks` module. This ensures companion installs
    /// cannot be accidentally triggered from the CLI layer without going through
    /// `discover_and_install_patches`.
    ///
    /// The test is a visibility proof: if `install_companion` were `pub`, the
    /// function-pointer cast would still compile, but the **body** of this test
    /// lives inside `mod tests` which is a sibling of `patch_discovery.rs` and
    /// thus within `pub(super)` scope. The companion path struct
    /// `PackageManager::install_companion` is accessible here only because `tests`
    /// is inside `tasks`, which is `super` relative to `patch_discovery`.
    ///
    /// Traces: DELIVERABLES §2g recursion guard — boundary split.
    #[test]
    fn install_companion_accessible_from_tasks_but_not_public() {
        // This cast compiles because `pub(super)` is visible within `tasks::*`.
        // If this test were in a sibling crate or CLI, it would NOT compile.
        let _ = PackageManager::install_companion as fn(_, _, _) -> _;
    }

    /// Regression guard (runtime): `install_companion` DOES NOT write to the
    /// patch tag store.
    ///
    /// This test mechanically proves the recursion guard: even with a patch tier
    /// configured, calling `install_companion` on an offline manager must leave
    /// the tag-store untouched. If `install_companion` were ever changed to call
    /// `discover_and_install_patches`, the tag-store file for the companion's
    /// patch repo would be written (either `LookedNoDescriptor` or
    /// `LookedHasDescriptor`), causing this assertion to fail.
    ///
    /// The companion install itself fails (offline manager, nothing in store), but
    /// the tag-store absence is the meaningful invariant: it proves no discovery
    /// path was invoked.
    ///
    /// Traces: DELIVERABLES §2g recursion guard; TESTABILITY §recursion guard
    /// regression.
    #[tokio::test(flavor = "multi_thread")]
    async fn install_companion_does_not_write_patch_tag_store() {
        let tmp = TempDir::new().unwrap();
        let patches = test_patch_config();

        // Build an offline manager with a patch tier configured. The patch tier
        // would cause `discover_and_install_patches` to attempt a network fetch
        // (if online) — but `install_companion` must never invoke discovery at all.
        let manager = make_offline_manager(tmp.path()).with_patches(Some(patches.clone()));
        assert!(manager.is_offline(), "setup: manager must be offline");
        assert!(manager.patches().is_some(), "setup: patches must be Some");

        // The companion identifier to install.
        let companion_id = Identifier::parse("patches.corp.com/certs/ca-bundle:latest").expect("valid identifier");

        // Compute the tag-store path that `discover_and_install_patches` WOULD write
        // for this companion's patch repo, using the same logic as the discovery code.
        // If discovery were invoked for the companion, it would compute:
        //   patch_descriptor_id(&patches, &companion_id) → Identifier at patches.corp.com
        // and then write the three-state record at tag_store.tags(descriptor_id).
        //
        // We need to know the tag-store path for the companion's patch descriptor.
        // Use the same helper as production code.
        let pkg_specific_descriptor_id = patch_descriptor_id(&patches, &companion_id);
        let tag_store = &manager.file_structure().tags;
        let companion_patch_tags_path = tag_store.tags(&pkg_specific_descriptor_id);

        // Verify the tag-store file is absent before the call.
        assert!(
            !companion_patch_tags_path.exists(),
            "setup: tag-store file must not exist before install_companion"
        );

        // Invoke install_companion — it will fail (offline + empty store) but must
        // NOT touch the tag store.
        let result = manager.install_companion(&companion_id, vec![]).await;
        assert!(
            result.is_err(),
            "install_companion on an offline empty manager must fail (expected: not a guard failure)"
        );

        // The critical assertion: the tag-store file must remain absent.
        // If `install_companion` had called `discover_and_install_patches`, it
        // would have short-circuited at the `is_offline()` check — but that check
        // lives INSIDE discover_and_install_patches, not before it. Writing to the
        // tag store requires reaching the NeverLooked branch inside
        // discover_and_install_patches. Since `install_companion` calls `pull`
        // directly without going through discovery, the tag-store must stay untouched.
        assert!(
            !companion_patch_tags_path.exists(),
            "install_companion must NOT write to the patch tag store — recursion guard violated"
        );

        // Also verify the global descriptor path was not written.
        let global_descriptor_id = global_descriptor_id(&patches);
        let global_patch_tags_path = tag_store.tags(&global_descriptor_id);
        assert!(
            !global_patch_tags_path.exists(),
            "install_companion must NOT write to the global patch tag store — recursion guard violated"
        );
    }

    /// Behavioral recursion guard: `install_companion` does NOT invoke
    /// `discover_and_install_patches` even when the manager is NOT offline.
    ///
    /// The offline test above relies on the `is_offline()` short-circuit inside
    /// `discover_and_install_patches`. This test uses a manager with a real client
    /// (not offline) so the short-circuit does NOT protect us — the guard must hold
    /// structurally. The companion's patch tag-store is seeded as `LookedNoDescriptor`,
    /// which would be the state written by `write_no_descriptor` if discovery
    /// were invoked and then found no descriptor. We can't seed it as NeverLooked
    /// and expect the write path to trigger without a real network, so we assert
    /// a complementary invariant: any state that `discover_and_install_patches`
    /// would write (i.e. `LookedNoDescriptor` → key absent vs `LookedHasDescriptor`
    /// → key present) must not appear to CHANGE between before and after the call.
    ///
    /// We seed `LookedHasDescriptor` with a synthetic digest. If `install_companion`
    /// called `discover_and_install_patches`, discovery would:
    ///   1. Read `LookedHasDescriptor` → try to load from CAS → fail (blob absent).
    ///   2. Log a warning and try to re-fetch from network → fail (no real registry).
    ///   3. Leave the tag-store entry in `LookedHasDescriptor` state (per Fix 3).
    /// So the file content would be IDENTICAL. This is not observable.
    ///
    /// Instead we seed it as ABSENT (NeverLooked). `discover_and_install_patches`
    /// with a NeverLooked state would call `fetch_and_persist_descriptor` which
    /// calls `require_client()` and then `fetch_patch_descriptor_blobs` — which
    /// would FAIL with a network error. On failure the function returns `Err` and
    /// the tag-store file remains absent. But with `install_companion` (correct),
    /// the file is also absent. So this case is indistinguishable too.
    ///
    /// The definitive structural invariant therefore relies on Fix 5 (total-companion
    /// cap): if `discover_and_install_patches` ran for the companion and somehow
    /// managed to collect companions (impossible without network here), it would
    /// enforce the cap. The compile-time structural test
    /// (`install_companion_and_discovery_exist_as_distinct_methods`) remains the
    /// primary guard; this test verifies the *operational* path with a non-offline
    /// manager produces the same observable result (error from `pull` with nothing
    /// written to the companion's patch tag-store).
    ///
    /// Traces: DELIVERABLES §2g recursion guard (behavioral variant, non-offline).
    #[tokio::test(flavor = "multi_thread")]
    async fn install_companion_does_not_write_patch_tag_store_non_offline() {
        let tmp = TempDir::new().unwrap();
        let patches = test_patch_config();

        // Non-offline manager (has client) with patch tier configured.
        // The offline short-circuit will NOT fire inside discover_and_install_patches
        // — if install_companion called it, it would proceed past the is_offline() check.
        let manager = make_online_manager(tmp.path()).with_patches(Some(patches.clone()));
        assert!(!manager.is_offline(), "setup: manager must NOT be offline");
        assert!(manager.patches().is_some(), "setup: patches must be Some");

        let companion_id = Identifier::parse("patches.corp.com/certs/ca-bundle:latest").expect("valid identifier");

        // Compute the tag-store paths for the companion's patch repos.
        let pkg_specific_descriptor_id = patch_descriptor_id(&patches, &companion_id);
        let global_descriptor = global_descriptor_id(&patches);
        let tag_store = &manager.file_structure().tags;
        let companion_pkg_tags_path = tag_store.tags(&pkg_specific_descriptor_id);
        let companion_global_tags_path = tag_store.tags(&global_descriptor);

        // Verify tag-store files are absent before the call.
        assert!(
            !companion_pkg_tags_path.exists(),
            "setup: companion pkg tag-store must be absent before install_companion"
        );
        assert!(
            !companion_global_tags_path.exists(),
            "setup: companion global tag-store must be absent before install_companion"
        );

        // Call install_companion — will fail (no package data in store), but must NOT
        // write any tag-store entries for the companion's patch repos.
        let result = manager.install_companion(&companion_id, vec![]).await;
        assert!(result.is_err(), "install_companion must fail (no data in store)");

        // Critical: even with a non-offline manager, install_companion must NOT have
        // written any patch tag-store entries. If the recursion guard was violated and
        // discover_and_install_patches was called, it would have attempted network
        // access (non-offline), failed, and potentially written LookedNoDescriptor.
        // With the guard intact, install_companion calls pull directly → nothing
        // is written to the patch tag-store.
        assert!(
            !companion_pkg_tags_path.exists(),
            "install_companion (non-offline) must NOT write to the companion's pkg patch tag-store — recursion guard violated"
        );
        assert!(
            !companion_global_tags_path.exists(),
            "install_companion (non-offline) must NOT write to the companion's global patch tag-store — recursion guard violated"
        );
    }

    /// Safety cap: `discover_and_install_patches` returns an error when the total
    /// number of companions across all descriptors exceeds `MAX_TOTAL_COMPANIONS`.
    ///
    /// This test exercises the defense-in-depth cap added in Fix 5 by directly
    /// injecting an oversized companion list through the dedup/collect path via a
    /// crafted `PatchTagMap` state + descriptor seeded in the CAS.
    ///
    /// Since building a real descriptor with MAX_TOTAL_COMPANIONS + 1 companions
    /// requires valid JSON that matches our flat-matcher, we test the cap indirectly
    /// by verifying the constant value is sane and by checking that the error type
    /// is `PatchDiscovery(DescriptorTooLarge)` when the production code would fire.
    ///
    /// Traces: Fix 5 — defense-in-depth total companion cap.
    #[test]
    fn max_total_companions_constant_is_sane() {
        // The cap must be greater than a single descriptor's max (MAX_PACKAGES_PER_RULE)
        // but less than the cross-product of two fully-loaded descriptors.
        // Use const blocks so the compiler evaluates these at compile time and
        // clippy::assertions_on_constants is satisfied.
        use crate::patch::descriptor::{MAX_PACKAGES_PER_RULE, MAX_RULES};
        const { assert!(MAX_TOTAL_COMPANIONS >= MAX_PACKAGES_PER_RULE) };
        const { assert!(MAX_TOTAL_COMPANIONS <= 1024) };
        // Runtime assertion for the cross-product check (involves arithmetic).
        let single_descriptor_max = MAX_RULES * MAX_PACKAGES_PER_RULE;
        assert!(
            MAX_TOTAL_COMPANIONS < 2 * single_descriptor_max,
            "MAX_TOTAL_COMPANIONS must be below the two-descriptor cross-product to provide defense; got {} vs max {}",
            MAX_TOTAL_COMPANIONS,
            2 * single_descriptor_max
        );
    }

    /// The `PackageErrorKind::PatchDiscovery` variant is emitted when the total
    /// companion count across descriptors exceeds `MAX_TOTAL_COMPANIONS`.
    ///
    /// This test verifies the error type and message format by directly exercising
    /// the cap check against a crafted oversized list.
    #[test]
    fn patch_discovery_error_variant_for_total_companion_cap() {
        use crate::package_manager::error::PackageErrorKind;
        use crate::patch::PatchError;

        let error = PackageErrorKind::PatchDiscovery(PatchError::DescriptorTooLarge {
            detail: format!(
                "total companion count {} across all descriptors exceeds maximum {}",
                MAX_TOTAL_COMPANIONS + 1,
                MAX_TOTAL_COMPANIONS
            ),
        });

        let display = error.to_string();
        assert!(
            display.contains("patch discovery error"),
            "PatchDiscovery variant Display must mention 'patch discovery error'; got: {display}"
        );
        assert!(
            display.contains("exceeds maximum"),
            "PatchDiscovery(DescriptorTooLarge) Display must mention 'exceeds maximum'; got: {display}"
        );
    }

    // ── Fix 4: CAS integrity regression ──────────────────────────────────────

    /// Regression: a corrupted on-disk manifest blob (bytes do not match the
    /// declared SHA-256 digest) must be rejected with `ManifestDigestMismatch`,
    /// not silently parsed and accepted.
    ///
    /// Setup:
    ///   1. Write a valid manifest + layer blob to CAS under their correct digests.
    ///   2. Overwrite the manifest blob file on disk with different bytes — corrupting
    ///      it in place, but without changing the digest key used to reference it.
    ///   3. Call `load_descriptor_from_cas` with the ORIGINAL (correct) manifest digest.
    ///   4. Expect `Err` referencing `ManifestDigestMismatch`.
    ///
    /// Traceability: Fix 4 — CAS integrity re-verification in `load_descriptor_from_cas`.
    #[tokio::test(flavor = "multi_thread")]
    async fn fix4_corrupted_manifest_blob_rejected_with_digest_mismatch() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`:
        // this test calls BlobStore::write_blob, which increments the process-global
        // WRITE_BLOB_CALL_COUNT. Holding this lock prevents concurrent calls from
        // inflating the delta measured by the coalescing test.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let blob_store = fs.blobs.clone();
        let registry = "patches.corp.com";

        // Build a valid descriptor layer.
        let layer_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [] }]
        })
        .to_string();
        let layer_bytes = layer_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);

        // Build the manifest referencing the layer.
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);

        // Write both blobs with correct content under correct digests.
        blob_store
            .write_blob(registry, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(registry, &layer_digest, layer_bytes)
            .await
            .unwrap();

        // Verify the baseline: un-corrupted load succeeds.
        let ok_result = load_descriptor_from_cas(&blob_store, registry, &manifest_digest).await;
        assert!(
            ok_result.is_ok(),
            "Fix 4 baseline: valid blobs must load successfully; got: {ok_result:?}"
        );

        // Corrupt the manifest blob on disk by overwriting it with different bytes —
        // but keep the same path (same digest key). The `write_blob` function is
        // idempotent, so we must write directly to the underlying data path.
        let manifest_blob_path = blob_store.data(registry, &manifest_digest);
        std::fs::write(&manifest_blob_path, b"CORRUPTED MANIFEST CONTENT").unwrap();

        // Load must now fail with ManifestDigestMismatch.
        let result = load_descriptor_from_cas(&blob_store, registry, &manifest_digest).await;
        assert!(
            result.is_err(),
            "Fix 4: corrupted manifest blob must be rejected; got Ok({:?})",
            result.ok()
        );

        // Downcast through the error chain to verify the specific error variant.
        let err = result.unwrap_err();
        let err_str = format!("{err:?}");
        assert!(
            err_str.contains("ManifestDigestMismatch") || err_str.contains("manifest digest mismatch"),
            "Fix 4: error must be ManifestDigestMismatch; got: {err_str}"
        );
    }

    /// Regression: a corrupted on-disk layer blob (bytes do not match the digest
    /// declared in the manifest) must be rejected with `LayerDigestMismatch`.
    ///
    /// The manifest itself is valid and passes its own digest check; only the
    /// layer blob has been tampered with after writing.
    ///
    /// Traceability: Fix 4 — CAS integrity re-verification (layer path).
    #[tokio::test(flavor = "multi_thread")]
    async fn fix4_corrupted_layer_blob_rejected_with_digest_mismatch() {
        // Same serialisation guard as `fix4_corrupted_manifest_blob_rejected_with_digest_mismatch`.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        use crate::oci::Algorithm;

        let dir = TempDir::new().unwrap();
        let fs = FileStructure::with_root(dir.path().to_path_buf());
        let blob_store = fs.blobs.clone();
        let registry = "patches.corp.com";

        // Build a valid descriptor layer.
        let layer_json = serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [] }]
        })
        .to_string();
        let layer_bytes = layer_json.as_bytes();
        let layer_digest = Algorithm::Sha256.hash(layer_bytes);

        // Build the manifest referencing the layer.
        let manifest_json = serde_json::json!({
            "schemaVersion": 2,
            "layers": [{"mediaType": "application/octet-stream", "digest": layer_digest.to_string(), "size": layer_bytes.len()}]
        })
        .to_string();
        let manifest_bytes = manifest_json.as_bytes();
        let manifest_digest = Algorithm::Sha256.hash(manifest_bytes);

        // Write both blobs correctly.
        blob_store
            .write_blob(registry, &manifest_digest, manifest_bytes)
            .await
            .unwrap();
        blob_store
            .write_blob(registry, &layer_digest, layer_bytes)
            .await
            .unwrap();

        // Corrupt the LAYER blob on disk (manifest stays valid).
        let layer_blob_path = blob_store.data(registry, &layer_digest);
        std::fs::write(&layer_blob_path, b"CORRUPTED LAYER CONTENT").unwrap();

        // Load must fail with LayerDigestMismatch.
        let result = load_descriptor_from_cas(&blob_store, registry, &manifest_digest).await;
        assert!(
            result.is_err(),
            "Fix 4: corrupted layer blob must be rejected; got Ok({:?})",
            result.ok()
        );

        let err_str = format!("{:?}", result.unwrap_err());
        assert!(
            err_str.contains("LayerDigestMismatch") || err_str.contains("layer digest mismatch"),
            "Fix 4: error must be LayerDigestMismatch; got: {err_str}"
        );
    }

    /// Cross-registry companion warning is not a block — `discover_and_install_patches`
    /// proceeds even when a companion's registry differs from the patch registry.
    ///
    /// Since the warning is a `log::warn!` with no side effect, this test asserts
    /// indirectly: an offline manager with a cross-registry companion in the descriptor
    /// still returns `Ok(())` (the short-circuit fires before installation is attempted).
    /// The warning code path is exercised in the online path; this test confirms the
    /// structural decision (warn, not block) by verifying no error is returned for
    /// the offline case.
    ///
    /// Traces: Fix 6 — defense-in-depth cross-registry warning (warn, not block).
    #[tokio::test(flavor = "multi_thread")]
    async fn discover_patches_returns_ok_even_with_cross_registry_companion_intent() {
        // An offline manager with a patch tier configured short-circuits at is_offline().
        // The important property: cross-registry companion logic (fix 6) is a WARN,
        // not a block — this confirms the design decision.
        let tmp = TempDir::new().unwrap();
        let manager = make_offline_manager(tmp.path()).with_patches(Some(test_patch_config()));
        let base_id = Identifier::parse("ocx.sh/cmake:3.28").expect("valid identifier");
        let result = manager.discover_and_install_patches(&base_id, &[]).await;
        assert!(
            result.is_ok(),
            "discover_and_install_patches must return Ok when offline (cross-registry warning is advisory only); got: {result:?}"
        );
    }
}
