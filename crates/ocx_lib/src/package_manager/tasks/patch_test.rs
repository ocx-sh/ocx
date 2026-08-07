// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx patch test` — compose a local (unpublished) patch descriptor onto a base.
//!
//! This module implements the lib half of Phase 6A's maintainer `ocx patch test`
//! command (`adr_infrastructure_patches.md`, milestone #111, issue #117). The CLI
//! verb (`crates/ocx_cli/src/command/patch.rs::run_patch_test`) provisions a
//! scratch [`crate::file_structure::FileStructure`] (a tempdir, like
//! `package_test.rs`) and then calls [`PackageManager::seed_and_compose_patch_test`]
//! to seed the authored descriptor into that scratch store and compose the
//! resulting companion overlay onto the base — **without publishing**.
//!
//! ## Why a local-state seam, not a compose parameter
//!
//! `resolve_env` / `build_site_patch_set` read patch overlay state exclusively
//! from the persisted tag store + CAS (`PatchTagMap` + `BlobStore`). To preview a
//! descriptor that has never been published, `ocx patch test` **seeds local
//! state** — it persists the descriptor blobs into the scratch CAS and writes a
//! `LookedHasDescriptor` patch tag for the global root — then calls the unchanged
//! `resolve_env`. The C7/C8 hot path keeps a single source of truth (persisted
//! state); the maintainer preview is just a pre-seeded scratch store. This avoids
//! threading an `Option<descriptor>` override through the compose hot path.
//!
//! ## Responsibility split
//!
//! - **CLI** owns the scratch `FileStructure`, descriptor file I/O, and companion
//!   archive materialization (`pull_local` / registry pull).
//! - **This lib method** owns the deterministic seed-and-compose: persist the
//!   descriptor blobs, record the global patch tag, run `resolve_env` over
//!   the base, and return the matched companions + composed entries.

use std::sync::Arc;

use crate::{
    config::patch::ResolvedPatchConfig,
    oci::{self, Identifier},
    package::install_info::InstallInfo,
    package::metadata::env::entry::Entry,
    patch::PatchDescriptor,
};

use super::super::{PackageManager, error::PackageErrorKind};
use super::resolve::{PatchOverlay, PatchProvenance};

// ── Public result type ─────────────────────────────────────────────────────────

/// Outcome of a [`PackageManager::seed_and_compose_patch_test`] preview.
///
/// Carries the companion identifiers the descriptor matched for the base and the
/// composed environment entries produced by overlaying those companions onto the
/// base's interface surface. The CLI renders this into a `PatchTestReport`.
#[derive(Debug, Clone)]
pub struct PatchTestComposition {
    /// Companion identifiers matched for the base under the descriptor's rules,
    /// in descriptor rule order.
    pub matched_companions: Vec<Identifier>,
    /// Composed environment entries: the base's interface surface followed by the
    /// companion overlay (global-last), exactly as `resolve_env` produces them.
    pub entries: Vec<Entry>,
    /// The index in `entries` at which the companion overlay begins. Entries at
    /// `[0..patch_start)` are the base's native interface surface; entries at
    /// `[patch_start..)` are the companion overlay carrying [`PatchProvenance`].
    pub patch_start: usize,
    /// Per-overlay-entry provenance, aligned one-to-one with the overlay region.
    /// Names the rule glob + companion that produced each overlay entry so
    /// `ocx patch test` can trace it.
    ///
    /// Shorter than `entries[patch_start..]` whenever the caller contributed
    /// `--env` overrides — those compose after the overlay and are nobody's
    /// companion contribution. Read it through [`Self::overlay`], never by
    /// indexing.
    pub provenance: Vec<PatchProvenance>,
}

impl PatchTestComposition {
    /// The companion-overlay region of [`Self::entries`], for attributing a
    /// composed entry to the rule + companion that produced it.
    ///
    /// The one supported way to read [`Self::provenance`]: it bound-checks, so
    /// an `--env` override sitting past the overlay reports as unattributed
    /// instead of panicking on an out-of-range index.
    pub fn overlay(&self) -> PatchOverlay<'_> {
        PatchOverlay::new(self.patch_start, &self.provenance)
    }
}

// ── PackageManager::seed_and_compose_patch_test ────────────────────────────────

impl PackageManager {
    /// Seed `descriptor_bytes` as a global patch descriptor in this
    /// manager's (scratch) store and compose the resulting companion overlay
    /// onto `base`.
    ///
    /// Steps (to be implemented):
    ///
    /// 1. Persist the descriptor blobs (synthesized single-layer manifest +
    ///    descriptor layer) into the CAS via
    ///    [`crate::patch::persist_patch_descriptor`].
    /// 2. Write the global `LookedHasDescriptor` patch tag (so any matching
    ///    rule applies) via [`super::patch_discovery::PatchTagMap::write_has_descriptor`].
    /// 3. Run [`PackageManager::resolve_env`] over `base` (`self_view = false`) so
    ///    the seeded descriptor's companions are composed. A required companion
    ///    that cannot be resolved surfaces as
    ///    [`PackageErrorKind::RequiredCompanionFailed`] (C7 fail-closed).
    /// 4. Return the matched companions + composed entries.
    ///
    /// The caller must have already materialized any companion packages the
    /// descriptor names for `base` into this manager's store (the maintainer
    /// supplies them via `--companion-archive` or a registry pull).
    ///
    /// `patches` is the already-resolved patch tier — the global identifier
    /// and the `required` posture both come from it. The CLI resolves and
    /// validates the tier before calling, so this method cannot reach a
    /// no-tier state (DIP: depend on the resolved value, not a config lookup).
    ///
    /// `platform` is the platform the preview composes for — the CLI's `-p`,
    /// already used to materialize the base and the companions. It selects the
    /// leaf of a multi-platform companion's image index; the host platform
    /// would resolve a companion that ships no host leaf as absent, failing a
    /// required one closed.
    ///
    /// # Errors
    ///
    /// - [`PackageErrorKind::PatchDiscovery`] — the descriptor bytes are not a
    ///   valid patch descriptor.
    /// - [`PackageErrorKind::RequiredCompanionFailed`] — a `required` companion
    ///   named for `base` is not resolvable in the scratch store.
    /// - [`PackageErrorKind::Internal`] — a CAS/tag write or compose failure.
    pub async fn seed_and_compose_patch_test(
        &self,
        base: &Arc<InstallInfo>,
        descriptor_bytes: &[u8],
        patches: &ResolvedPatchConfig,
        env_overrides: Vec<Entry>,
        platform: &oci::Platform,
    ) -> Result<PatchTestComposition, PackageErrorKind> {
        // Validate the descriptor up front so a malformed file fails before any
        // store mutation.
        let descriptor =
            PatchDescriptor::from_json_bytes(descriptor_bytes).map_err(PackageErrorKind::PatchDiscovery)?;

        // ── Step 1: Persist the descriptor blobs into the scratch CAS. ──
        //
        // Synthesize the minimal single-layer manifest shape that
        // `load_descriptor_frozen_or_live` / `load_descriptor_from_cas` expect,
        // then write both blobs under the descriptor id's BARE-HOST registry —
        // the exact key the compose-time read (`build_site_patch_set`) uses.
        // The configured `patches.registry` may carry a path prefix
        // (`registry.corp.example/ocx-patches`), which `global_descriptor_id`
        // folds in front of the repository; keying the CAS by the raw string
        // would write to a directory nothing ever reads back.
        let global_id = super::patch_discovery::global_descriptor_id(patches);
        let (manifest_bytes, manifest_digest, layer_digest) = synthesize_descriptor_manifest(descriptor_bytes);
        let blob_store = &self.file_structure().blobs;
        crate::patch::persist_patch_descriptor(
            blob_store,
            global_id.registry(),
            manifest_digest.clone(),
            &manifest_bytes,
            layer_digest,
            descriptor_bytes,
        )
        .await
        .map_err(PackageErrorKind::PatchDiscovery)?;

        // ── Step 2: Record the global LookedHasDescriptor patch tag. ──
        //
        // Seeding at the global root means any matching rule applies to the base,
        // mirroring how Phase 3 discovery records a found descriptor. The
        // unchanged `build_site_patch_set` reads this tag + the CAS blob to
        // reconstruct the descriptor — no compose-time descriptor override.
        let global_tags_path = self.file_structure().patch_descriptor_path(&global_id);
        super::patch_discovery::PatchTagMap::write_has_descriptor(&global_tags_path, &manifest_digest.to_string())
            .await
            .map_err(PackageErrorKind::Internal)?;

        // ── Step 2b: Prove the seed is readable back, through the loader compose
        // will use. ──
        //
        // `build_site_patch_set` downgrades a `Corrupt` descriptor read to a
        // warning whenever the tier is `required = false`, so a seed that landed
        // where nothing reads it composes zero companions and exits 0 — the
        // maintainer cannot tell "the descriptor matches nothing" from "the
        // preview never loaded the descriptor at all". Reading it back here makes
        // a broken seed fail loudly, regardless of the tier's fail posture.
        super::patch_discovery::load_descriptor_from_cas(blob_store, global_id.registry(), &manifest_digest)
            .await
            .map_err(PackageErrorKind::Internal)?;

        // ── Step 3: Compose the overlay onto the base via the unchanged hot path. ──
        //
        // `resolve_env_with_patch_boundary` (self_view=false) composes the base's
        // interface surface and overlays the seeded global descriptor's companions,
        // returning the overlay boundary + per-entry provenance so the CLI report
        // can trace each overlay var to its rule + companion. A `required` companion
        // that cannot be resolved in the scratch store surfaces as
        // `RequiredCompanionFailed` (C7 fail-closed) — propagated unchanged.
        // `--env` overrides ride the scope's own arm: they compose after the
        // companion overlay, so a maintainer can pin a variable while previewing
        // a descriptor. They carry no `PatchProvenance` — they are not a
        // companion's contribution — so the report below attributes only what
        // the descriptor actually matched.
        let (entries, patch_start, provenance) = self
            .resolve_env_with_patch_boundary(
                std::slice::from_ref(base),
                false,
                crate::package_manager::EnvScope::Package { env: env_overrides },
                platform,
            )
            .await
            .map_err(unwrap_resolve_env_error)?;

        // ── Step 4: Report which companions the descriptor matched for the base. ──
        //
        // Re-run the pure matcher over the base so the report names the matched
        // companions independently of whether each was present in the store.
        let base_id = base.identifier().as_identifier();
        let matched_companions: Vec<Identifier> = descriptor
            .collect_companions(base_id, patches.required)
            .into_iter()
            .map(|entry| entry.identifier)
            .collect();

        Ok(PatchTestComposition {
            matched_companions,
            entries,
            patch_start,
            provenance,
        })
    }

    /// Materialize a local companion archive into this (scratch) store for
    /// `ocx patch test --companion-archive`, then pin its tag → digest in the
    /// scratch store's patch state so companion resolution
    /// ([`find_companion_local`]) finds it WITHOUT a registry round-trip — the
    /// contract `--companion-archive` promises.
    ///
    /// The pin lands in this manager's own `FileStructure` (the scratch root),
    /// which is where compose reads it from — deliberately NOT the local index
    /// home, which `ocx patch test` shares with the rest of the invocation and
    /// must not grow with an unpublished package.
    ///
    /// Returns the companion's canonical `registry/repository` key so the caller
    /// can skip the registry pull for it (an UNPUBLISHED local companion would
    /// otherwise fail the pull and abort the preview before compose runs).
    ///
    /// [`find_companion_local`]: super::resolve
    ///
    /// # Errors
    ///
    /// Propagates [`PackageErrorKind`] from `pull_local` materialization or the
    /// pin write.
    pub async fn materialize_test_companion(
        &self,
        info: crate::package::info::Info,
        layers: &[crate::publisher::LayerRef],
    ) -> Result<String, PackageErrorKind> {
        let companion_tag_id = info.identifier.clone();
        let installed = self.pull_local(info, layers, None).await?;
        let digest = installed.identifier().digest();
        // `pull_local` materializes the platform manifest directly, so the pin
        // names it — compose's manifest-absent fallback then finds the package
        // at that digest without a platform-selection hop.
        super::patch_discovery::PatchTagMap::write_tag(
            &self.file_structure().patch_companion_path(&companion_tag_id),
            companion_tag_id.tag_or_latest(),
            &digest.to_string(),
        )
        .await
        .map_err(PackageErrorKind::Internal)?;
        Ok(format!(
            "{}/{}",
            companion_tag_id.registry(),
            companion_tag_id.repository()
        ))
    }
}

// ── Free helpers ───────────────────────────────────────────────────────────────

/// Synthesize the minimal single-layer OCI image manifest for a local
/// (unpublished) descriptor.
///
/// Returns `(manifest_bytes, manifest_digest, layer_digest)`. The manifest is a
/// single-layer image manifest whose only layer is the descriptor blob — the
/// shape [`crate::patch::persist_patch_descriptor`] and the CAS loader
/// ([`super::patch_discovery::load_descriptor_from_cas`]) expect.
fn synthesize_descriptor_manifest(descriptor_bytes: &[u8]) -> (Vec<u8>, oci::Digest, oci::Digest) {
    let layer_digest = oci::Algorithm::Sha256.hash(descriptor_bytes);
    let manifest_json = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "artifactType": crate::patch::PATCH_MANIFEST_ARTIFACT_TYPE,
        "config": {
            "mediaType": "application/vnd.oci.empty.v1+json",
            "digest": "sha256:44136fa355ba77b9ad7b35f2cca4bb730ad02e2e8dc7f2af7a1b3e7c0ef5c6a7",
            "size": 2
        },
        "layers": [{
            "mediaType": crate::patch::PATCH_DESCRIPTOR_LAYER_MEDIA_TYPE,
            "digest": layer_digest.to_string(),
            "size": descriptor_bytes.len()
        }]
    })
    .to_string();
    let manifest_bytes = manifest_json.into_bytes();
    let manifest_digest = oci::Algorithm::Sha256.hash(&manifest_bytes);
    (manifest_bytes, manifest_digest, layer_digest)
}

/// Unwrap the patch-relevant `PackageErrorKind` from a `resolve_env` failure.
///
/// `resolve_env` returns `crate::Error`; the C7 fail-closed path wraps a
/// `PackageErrorKind::RequiredCompanionFailed` in a single-entry `ResolveFailed`
/// batch (`From<PackageErrorKind> for crate::Error`). Surface that inner kind
/// directly so a maintainer sees `RequiredCompanionFailed` rather than the
/// opaque outer `Error`. Any other `Error` is carried as `Internal`.
fn unwrap_resolve_env_error(error: crate::Error) -> PackageErrorKind {
    use crate::package_manager::error::Error as BatchError;

    if let crate::Error::PackageManager(BatchError::ResolveFailed(package_errors)) = error {
        // Single-entry batch from the `From<PackageErrorKind>` wrap path. Take the
        // first entry's kind so the maintainer sees the structured fail-closed kind.
        if let Some(first) = package_errors.into_iter().next() {
            return first.kind;
        }
        // Empty batch is not produced by the wrap path, but stay total: re-wrap as
        // an internal error carrying an empty batch so nothing is silently dropped.
        return PackageErrorKind::Internal(crate::Error::PackageManager(BatchError::ResolveFailed(Vec::new())));
    }
    PackageErrorKind::Internal(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    use crate::{
        config::patch::ResolvedPatchConfig,
        file_structure::{FileStructure, PackageDir, PackageStore},
        oci::{
            Digest, Identifier, PinnedIdentifier,
            index::{ChainMode, Index, LocalConfig, LocalIndex},
        },
        package::{
            install_info::InstallInfo,
            metadata::{
                self, bundle, dependency, entrypoint::Entrypoints, env as metadata_env, visibility::Visibility,
            },
            resolved_package::ResolvedPackage,
        },
    };

    const PATCH_REGISTRY: &str = "patches.corp.com";

    /// Two `--companion-archive` materializations for different tags of the same
    /// repo in one invocation must BOTH resolve: the second pin merges into the
    /// companion record rather than clobbering the first tag, and the local
    /// index is left untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn companion_archive_pins_each_tag_without_touching_the_local_index() {
        use super::super::patch_discovery::PatchTagMap;

        let dir = TempDir::new().unwrap();
        let file_structure = FileStructure::with_root(dir.path().to_path_buf());
        let id_a = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("1.0");
        let id_b = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("2.0");
        let digest_a = Digest::Sha256("a".repeat(64));
        let digest_b = Digest::Sha256("b".repeat(64));
        let pin_path = file_structure.patch_companion_path(&id_a);

        PatchTagMap::write_tag(&pin_path, id_a.tag_or_latest(), &digest_a.to_string())
            .await
            .unwrap();
        PatchTagMap::write_tag(
            &file_structure.patch_companion_path(&id_b),
            id_b.tag_or_latest(),
            &digest_b.to_string(),
        )
        .await
        .unwrap();

        assert_eq!(
            PatchTagMap::read_tag(&pin_path, "1.0").await.unwrap(),
            Some(digest_a.to_string()),
            "the first archive's tag must survive the second pin write"
        );
        assert_eq!(
            PatchTagMap::read_tag(&pin_path, "2.0").await.unwrap(),
            Some(digest_b.to_string()),
            "the second archive's tag must be present"
        );
        assert!(
            !file_structure.index.root().exists(),
            "pinning a companion must not write anything into the local index home"
        );
    }

    // ── Seed helpers (DAMP — self-contained per quality-core test guidance) ───────

    /// Build an offline scratch `PackageManager` with the `[patches]` tier set.
    ///
    /// Mirrors the scratch store the CLI `run_patch_test` builds: an offline
    /// manager over a tempdir-backed `FileStructure`, so required-companion
    /// resolution fails closed when the companion is absent (no network rescue).
    fn make_scratch_manager(root: &Path, patches: ResolvedPatchConfig) -> PackageManager {
        let file_structure = FileStructure::with_root(root.to_path_buf());
        let local_index = LocalIndex::new(LocalConfig {
            index_store: file_structure.index.clone(),
        });
        let index = Index::from_chained(local_index, vec![], ChainMode::Offline);
        PackageManager::new(file_structure, index, None, PATCH_REGISTRY).with_patches(Some(patches))
    }

    /// Tier config: `required` controls the fail posture for matched companions.
    fn patch_config(required: bool) -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: PATCH_REGISTRY.to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required,
        }
    }

    /// The host platform — what the CLI resolves when `-p` is absent.
    fn host_platform() -> oci::Platform {
        oci::Platform::current().unwrap_or_else(oci::Platform::any)
    }

    /// A 64-hex digest seeded from a single fill character.
    fn sha256(fill: char) -> Digest {
        Digest::Sha256(fill.to_string().repeat(64))
    }

    /// A pinned identifier rooted at the patch registry.
    fn pinned(repo: &str, fill: char) -> PinnedIdentifier {
        PinnedIdentifier::try_from(Identifier::new_registry(repo, PATCH_REGISTRY).clone_with_digest(sha256(fill)))
            .unwrap()
    }

    /// Write a minimal on-disk package directory carrying one constant env var.
    fn seed_package_with_constant_var(
        store: &PackageStore,
        id: &PinnedIdentifier,
        var_key: &str,
        var_value: &str,
        visibility: Visibility,
    ) {
        let pkg_path = store.path(id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let visibility_str = match visibility {
            Visibility::PUBLIC => "public",
            Visibility::INTERFACE => "interface",
            _ => "private",
        };
        let meta = serde_json::json!({
            "type": "bundle",
            "version": 1,
            "env": [{
                "key": var_key,
                "type": "constant",
                "value": var_value,
                "visibility": visibility_str,
            }],
        });
        std::fs::write(pkg_path.join("metadata.json"), meta.to_string()).unwrap();
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();
    }

    /// Seed the patch tier's companion pin the way `materialize_test_companion`
    /// records it, so companion resolution finds the scratch-store package.
    fn seed_companion_pin(file_structure: &FileStructure, companion_tag_id: &Identifier, digest: &Digest) {
        let pin_path = file_structure.patch_companion_path(companion_tag_id);
        std::fs::create_dir_all(pin_path.parent().unwrap()).unwrap();
        let record = serde_json::json!({ companion_tag_id.tag_or_latest(): digest.to_string() });
        std::fs::write(&pin_path, serde_json::to_vec(&record).unwrap()).unwrap();
    }

    /// Build a base `InstallInfo` (no deps, no env) backed by an on-disk content dir.
    fn make_base(root: &Path, store: &PackageStore, repo: &str, fill: char) -> Arc<InstallInfo> {
        let id = pinned(repo, fill);
        let pkg_path = store.path(&id);
        std::fs::create_dir_all(pkg_path.join("content")).unwrap();
        let meta = metadata::Metadata::Bundle(bundle::Bundle {
            binaries: None,
            version: bundle::Version::V1,
            strip_components: None,
            env: metadata_env::Env::default(),
            dependencies: dependency::Dependencies::default(),
            entrypoints: Entrypoints::default(),
        });
        std::fs::write(
            pkg_path.join("resolve.json"),
            serde_json::to_string(&ResolvedPackage::new()).unwrap(),
        )
        .unwrap();
        let _ = root;
        Arc::new(InstallInfo::new(
            id,
            meta,
            ResolvedPackage::new(),
            PackageDir { dir: pkg_path },
        ))
    }

    /// A single-rule global descriptor (`match: "*"`) naming `companion`.
    fn catch_all_descriptor(companion: &Identifier) -> Vec<u8> {
        serde_json::json!({
            "version": 1,
            "rules": [{ "match": "*", "packages": [companion.to_string()] }]
        })
        .to_string()
        .into_bytes()
    }

    // ── Target 4: seeded descriptor + present companion → companion var present ───

    /// Traces: SPECIFY target 4 — a seeded local descriptor whose matched
    /// companion is present in the scratch store yields the companion's INTERFACE
    /// var in the composed `seed_and_compose_patch_test` output.
    ///
    /// Setup mirrors the CLI `run_patch_test` seed path: the companion package is
    /// installed (tag-lock + package dir with an INTERFACE var), the descriptor
    /// names it under a catch-all rule, and the base has no env of its own. The
    /// composition must surface the companion's var.
    ///
    /// FAILS against the stub body (which returns empty `entries`).
    #[tokio::test(flavor = "multi_thread")]
    async fn seeded_descriptor_with_present_companion_yields_companion_interface_var() {
        let dir = TempDir::new().unwrap();
        let patches = patch_config(true);
        let manager = make_scratch_manager(dir.path(), patches.clone());
        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        // Companion: fully installed with an INTERFACE var.
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            "SSL_CERT_FILE",
            "/etc/ssl/certs/ca-bundle.crt",
            Visibility::INTERFACE,
        );
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

        // Base: no deps, no env of its own.
        let base = make_base(dir.path(), &store, "cmake", 'r');

        let descriptor_bytes = catch_all_descriptor(&companion_tag_id);
        let composition = manager
            .seed_and_compose_patch_test(&base, &descriptor_bytes, &patches, Vec::new(), &host_platform())
            .await
            .expect("seed-and-compose must succeed when the required companion is present");

        // The companion matched the catch-all rule for the base.
        assert!(
            composition
                .matched_companions
                .iter()
                .any(|c| c.repository() == "ca-bundle"),
            "matched_companions must include the ca-bundle companion; got: {:?}",
            composition.matched_companions
        );

        // The composed env must carry the companion's INTERFACE var.
        let entry = composition.entries.iter().find(|e| e.key == "SSL_CERT_FILE");
        assert!(
            entry.is_some(),
            "composed env must contain the companion's SSL_CERT_FILE var; entries: {:?}",
            composition.entries
        );
        assert_eq!(
            entry.map(|e| e.value.as_str()),
            Some("/etc/ssl/certs/ca-bundle.crt"),
            "companion var value must be the descriptor companion's value"
        );

        // Provenance: the overlay entry is traceable to its rule glob AND its
        // companion. `SSL_CERT_FILE` is an overlay entry (index >= patch_start),
        // and its aligned provenance names the catch-all rule + ca-bundle companion.
        let overlay_index = composition
            .entries
            .iter()
            .position(|e| e.key == "SSL_CERT_FILE")
            .expect("SSL_CERT_FILE must be present");
        assert!(
            overlay_index >= composition.patch_start,
            "the companion var must live in the overlay slice (index {overlay_index} >= patch_start {})",
            composition.patch_start
        );
        assert_eq!(
            composition.entries.len() - composition.patch_start,
            composition.provenance.len(),
            "provenance must be aligned one-to-one with the overlay slice"
        );
        let prov = &composition.provenance[overlay_index - composition.patch_start];
        assert_eq!(prov.rule_match, "*", "overlay entry traces to the catch-all rule glob");
        assert_eq!(
            prov.companion.repository(),
            "ca-bundle",
            "overlay entry traces to the ca-bundle companion; got {}",
            prov.companion
        );
    }

    // ── Target 4b: path-prefixed patch registry (github issue #286 / B1) ──────────

    /// A path-prefixed patch registry: a host authority followed by a `/`-separated
    /// path component, e.g. `registry.corp.example/ocx-patches`. Pins the CAS key
    /// both sides of the seed must agree on — the BARE HOST from
    /// [`super::patch_discovery::global_descriptor_id`]`(patches).registry()`, via
    /// `patch_registry_identifier`'s host/prefix split. With the bare-host
    /// [`PATCH_REGISTRY`] constant used elsewhere in this module the raw config
    /// string and the bare host coincide, so only a path-prefixed registry can
    /// tell a correct key from the raw one.
    const PATCH_REGISTRY_PATH_PREFIXED: &str = "patches.corp.com/ocx-patches";

    /// Traces: regression for github issue #286 — a maintainer's `[patches]`
    /// registry carrying a path prefix (the common shape for a corporate OCI
    /// registry, e.g. `registry.corp.example/ocx-patches`) made `ocx patch test`
    /// fail with "manifest blob not found in CAS" even though the seeded
    /// descriptor was just persisted by the very same call. Same setup as
    /// [`seeded_descriptor_with_present_companion_yields_companion_interface_var`]
    /// above, but `patches.registry` carries a path prefix.
    ///
    /// Guards the write/read key agreement: seeding once keyed the blobs by the
    /// raw config string while compose read them back under the bare host — two
    /// different CAS directories, so the (required) global descriptor read back
    /// as `Corrupt` and the call failed closed instead of composing.
    #[tokio::test(flavor = "multi_thread")]
    async fn seeded_descriptor_with_path_prefixed_registry_and_present_companion_yields_companion_interface_var() {
        let dir = TempDir::new().unwrap();
        let patches = ResolvedPatchConfig {
            registry: PATCH_REGISTRY_PATH_PREFIXED.to_string(),
            ..patch_config(true)
        };
        let manager = make_scratch_manager(dir.path(), patches.clone());
        let store = manager.file_structure().packages.clone();
        let tag_store = manager.file_structure().clone();

        // Companion: fully installed with an INTERFACE var. Its OWN registry is
        // independent of the (path-prefixed) patch registry — only the
        // descriptor's own storage location is affected by the prefix.
        let companion_digest = sha256('c');
        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let companion_pinned = PinnedIdentifier::try_from(
            Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_digest(companion_digest.clone()),
        )
        .unwrap();
        seed_package_with_constant_var(
            &store,
            &companion_pinned,
            "SSL_CERT_FILE",
            "/etc/ssl/certs/ca-bundle.crt",
            Visibility::INTERFACE,
        );
        seed_companion_pin(&tag_store, &companion_tag_id, &companion_digest);

        // Base: no deps, no env of its own.
        let base = make_base(dir.path(), &store, "cmake", 'r');

        let descriptor_bytes = catch_all_descriptor(&companion_tag_id);
        let composition = manager
            .seed_and_compose_patch_test(&base, &descriptor_bytes, &patches, Vec::new(), &host_platform())
            .await
            .expect("seed-and-compose must succeed with a path-prefixed patch registry");

        // The companion matched the catch-all rule for the base.
        assert!(
            composition
                .matched_companions
                .iter()
                .any(|c| c.repository() == "ca-bundle"),
            "matched_companions must include the ca-bundle companion; got: {:?}",
            composition.matched_companions
        );

        // The composed env must carry the companion's INTERFACE var.
        let entry = composition.entries.iter().find(|e| e.key == "SSL_CERT_FILE");
        assert!(
            entry.is_some(),
            "composed env must contain the companion's SSL_CERT_FILE var under a \
             path-prefixed patch registry; entries: {:?}",
            composition.entries
        );
        assert_eq!(
            entry.map(|e| e.value.as_str()),
            Some("/etc/ssl/certs/ca-bundle.crt"),
            "companion var value must be the descriptor companion's value"
        );
    }

    /// The seed is read back before compose (Step 2b), and the loader that
    /// guard relies on genuinely discriminates: it loads the freshly seeded
    /// descriptor, and errors once the persisted manifest blob is removed.
    ///
    /// `required = false` is the posture that hides an unreadable seed —
    /// `build_site_patch_set` downgrades a corrupt descriptor to a warning
    /// there, so without the guard a broken seed exits 0 with no companions.
    #[tokio::test(flavor = "multi_thread")]
    async fn seeded_descriptor_read_back_discriminates_a_missing_manifest_blob() {
        use super::super::patch_discovery::{global_descriptor_id, load_descriptor_from_cas};

        let dir = TempDir::new().unwrap();
        let patches = patch_config(false);
        let manager = make_scratch_manager(dir.path(), patches.clone());
        let store = manager.file_structure().packages.clone();
        let base = make_base(dir.path(), &store, "cmake", 'r');

        // The companion is deliberately absent: with an optional tier that is a
        // successful, zero-companion compose — exactly the case the guard has to
        // tell apart from an unreadable seed.
        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let descriptor_bytes = catch_all_descriptor(&companion_tag_id);
        manager
            .seed_and_compose_patch_test(&base, &descriptor_bytes, &patches, Vec::new(), &host_platform())
            .await
            .expect("an optional companion that is absent must still seed and compose");

        let global_id = global_descriptor_id(&patches);
        let (_, manifest_digest, _) = synthesize_descriptor_manifest(&descriptor_bytes);
        let blob_store = &manager.file_structure().blobs;

        load_descriptor_from_cas(blob_store, global_id.registry(), &manifest_digest)
            .await
            .expect("the seeded descriptor must read back from the CAS");

        std::fs::remove_file(blob_store.data(global_id.registry(), &manifest_digest))
            .expect("the persisted manifest blob must exist before removal");
        assert!(
            load_descriptor_from_cas(blob_store, global_id.registry(), &manifest_digest)
                .await
                .is_err(),
            "the read-back must fail once the manifest blob is gone — otherwise the guard proves nothing"
        );
    }

    /// A seed that cannot be read back fails the call, even under an optional
    /// tier — the posture where compose warn-skips a corrupt descriptor and
    /// would otherwise return a zero-companion `Ok`.
    ///
    /// Poisons the CAS before seeding: `BlobStore::write_blob` short-circuits
    /// on an existing non-empty file, so the seed's own write lands on the
    /// garbage and `persist_patch_descriptor` never re-reads it. Only Step 2b's
    /// read-back notices, so this test reds exactly on that guard.
    #[tokio::test(flavor = "multi_thread")]
    async fn unreadable_seed_fails_the_call_under_an_optional_tier() {
        use super::super::patch_discovery::global_descriptor_id;

        let dir = TempDir::new().unwrap();
        let patches = patch_config(false);
        let manager = make_scratch_manager(dir.path(), patches.clone());
        let store = manager.file_structure().packages.clone();
        let base = make_base(dir.path(), &store, "cmake", 'r');

        let companion_tag_id = Identifier::new_registry("ca-bundle", PATCH_REGISTRY).clone_with_tag("latest");
        let descriptor_bytes = catch_all_descriptor(&companion_tag_id);

        let global_id = global_descriptor_id(&patches);
        let (_, manifest_digest, _) = synthesize_descriptor_manifest(&descriptor_bytes);
        let poisoned = manager
            .file_structure()
            .blobs
            .data(global_id.registry(), &manifest_digest);
        std::fs::create_dir_all(poisoned.parent().unwrap()).unwrap();
        std::fs::write(&poisoned, b"not the manifest these bytes are addressed by").unwrap();

        let result = manager
            .seed_and_compose_patch_test(&base, &descriptor_bytes, &patches, Vec::new(), &host_platform())
            .await;

        assert!(
            result.is_err(),
            "an unreadable seed must fail loudly, not compose zero companions and exit 0; got: {result:?}"
        );
    }

    // ── Target 5: required companion absent → fail-closed error ───────────────────

    /// Traces: SPECIFY target 5 — when the descriptor names a `required` companion
    /// (tier `required = true`) that is NOT resolvable in the scratch store,
    /// `seed_and_compose_patch_test` fails closed rather than emitting a partial
    /// overlay.
    ///
    /// The companion is never installed (no tag-lock, no package dir), so the
    /// seeded global descriptor's required companion cannot resolve and the
    /// composition must surface `RequiredCompanionFailed` (C7).
    ///
    /// FAILS against the stub body (which returns `Ok` with empty `entries`).
    #[tokio::test(flavor = "multi_thread")]
    async fn required_companion_absent_fails_closed() {
        let dir = TempDir::new().unwrap();
        let patches = patch_config(true);
        let manager = make_scratch_manager(dir.path(), patches.clone());
        let store = manager.file_structure().packages.clone();

        // Companion identifier the descriptor names — deliberately NOT installed.
        let companion_tag_id = Identifier::new_registry("license-server", PATCH_REGISTRY).clone_with_tag("latest");

        // Base present; companion absent.
        let base = make_base(dir.path(), &store, "java", 'r');

        let descriptor_bytes = catch_all_descriptor(&companion_tag_id);
        let result = manager
            .seed_and_compose_patch_test(&base, &descriptor_bytes, &patches, Vec::new(), &host_platform())
            .await;

        assert!(
            matches!(result, Err(PackageErrorKind::RequiredCompanionFailed { .. })),
            "an unresolvable required companion must fail closed with RequiredCompanionFailed; got: {result:?}"
        );
    }
}
