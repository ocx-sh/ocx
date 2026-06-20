// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Patch-tier snapshot — opt-in determinism for the site-patch tier.
//!
//! A [`PatchSnapshot`] is written by `ocx patch freeze` and read at
//! compose-time to prefer pinned digests over live tag lookups.  It is the
//! patch-tier equivalent of `ocx.lock` for the project toolchain tier.
//!
//! ## File location
//!
//! The snapshot lives at [`PATCH_SNAPSHOT_FILE`] (`patches.snapshot.json`)
//! as a sibling of `ocx.lock` in the project root (or `$OCX_HOME` under
//! `--global`).  The path is derived the same way as the lock file: by
//! joining the resolved project directory with [`PATCH_SNAPSHOT_FILE`].
//!
//! ## Key scheme
//!
//! - **`companions` map** — key = `registry/repository` (the pinned
//!   identifier without the `@sha256:…` suffix), value = the pinned digest.
//!   The key is the Display output of
//!   [`oci::Identifier`](crate::oci::Identifier) without the digest
//!   component, so round-trips reliably.
//! - **`descriptors` map** — key = the descriptor SOURCE's canonical
//!   `registry/repository` (the global root and each package-specific source,
//!   from
//!   [`SitePatchRoots::descriptor_pins`](crate::package_manager::tasks::resolve::SitePatchRoots::descriptor_pins)),
//!   value = the descriptor's manifest digest at freeze time.  This map drives
//!   descriptor SELECTION at compose time (C8 whole-tier determinism): under an
//!   active snapshot the overlay loads each descriptor by its pinned manifest
//!   digest from the CAS instead of re-reading the live tag store, so a
//!   post-freeze `ocx patch sync` that publishes a new descriptor cannot change
//!   which companions a frozen build composes.  A source absent from this map
//!   did not exist at freeze time and is not composed by a frozen build.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::oci;
use crate::package_manager::SitePatchRoots;

/// File name of the patch snapshot, sibling to `ocx.lock`.
pub const PATCH_SNAPSHOT_FILE: &str = "patches.snapshot.json";

/// On-disk version tag for the patch snapshot format.
///
/// `serde_repr` rejects unknown integer values on deserialise automatically —
/// no manual version check needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum SnapshotVersion {
    V1 = 1,
}

/// Frozen view of the active site-patch tier for reproducible builds.
///
/// Written by `ocx patch freeze`, read at compose-time so the overlay
/// prefers the pinned digests over live tag lookups. Serialised as JSON
/// (pretty-printed, deterministic `BTreeMap` key order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchSnapshot {
    /// Format version. Unknown versions are rejected on deserialise.
    pub version: SnapshotVersion,
    /// Companion packages pinned by the snapshot.
    ///
    /// Key: `registry/repository` (Display of
    /// [`oci::Identifier`] without the digest suffix).
    /// Value: pinned digest at freeze time.
    pub companions: BTreeMap<String, oci::Digest>,
    /// Patch descriptor sources pinned by the snapshot (drives descriptor
    /// SELECTION at compose time — C8).
    ///
    /// Key: the descriptor source's canonical `registry/repository` (the global
    /// root and each package-specific source, from
    /// [`SitePatchRoots::descriptor_pins`](crate::package_manager::tasks::resolve::SitePatchRoots::descriptor_pins)).
    /// Value: the descriptor's manifest digest at freeze time.
    ///
    /// Under an active snapshot the overlay loads each descriptor by its pinned
    /// manifest digest from the CAS rather than the live tag store, so a
    /// post-freeze `ocx patch sync` cannot change which companions a frozen
    /// build composes.  A source absent here is not composed by a frozen build.
    pub descriptors: BTreeMap<String, oci::Digest>,
}

impl PatchSnapshot {
    /// Build a snapshot from live [`SitePatchRoots`].
    ///
    /// Companion key = the full identifier display string with the digest
    /// stripped: `registry/repository`. Descriptor key = the registry string
    /// stored in the `(registry, digest)` tuple.
    ///
    /// `BTreeMap` insertion is ordered so repeated calls with the same roots
    /// yield byte-identical output.
    pub fn from_roots(roots: &SitePatchRoots) -> Self {
        // Build the companions map: key = "registry/repository" (no tag, no digest suffix),
        // value = the pinned digest. BTreeMap insertion guarantees deterministic key order.
        let mut companions = BTreeMap::new();
        for pinned in &roots.companions {
            let key = format!("{}/{}", pinned.registry(), pinned.repository());
            companions.insert(key, pinned.digest());
        }

        // Build the descriptors map: key = the descriptor SOURCE's canonical
        // "registry/repository" (the global root + each package-specific source),
        // value = the manifest digest pinned at freeze time. This drives
        // descriptor SELECTION at compose time under an active snapshot (C8): the
        // overlay loads the frozen descriptor by this digest instead of the live
        // tag store, so a post-freeze `ocx patch sync` that advances a descriptor
        // cannot change which companions a frozen build composes. `BTreeMap`
        // insertion guarantees deterministic key order. (Built from
        // `roots.descriptor_pins`, which `resolve_site_patch_roots` already
        // dedups by source key.)
        let mut descriptors = BTreeMap::new();
        for (source_key, digest) in &roots.descriptor_pins {
            descriptors.insert(source_key.clone(), digest.clone());
        }

        Self {
            version: SnapshotVersion::V1,
            companions,
            descriptors,
        }
    }

    /// Write this snapshot to the given path as pretty-printed JSON.
    ///
    /// The parent directory is created automatically if absent.
    ///
    /// # Errors
    ///
    /// Returns an error if the path cannot be created or the JSON cannot be
    /// serialised.
    pub async fn write(&self, path: &Path) -> crate::Result<()> {
        use crate::prelude::SerdeExt;
        self.write_json(path).await
    }

    /// Read a snapshot from the given path.
    ///
    /// Returns `Ok(None)` when the file is absent so callers can fall back to
    /// live lookups without treating a missing snapshot as an error.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed.
    pub async fn read(path: &Path) -> crate::Result<Option<Self>> {
        use crate::prelude::SerdeExt;

        match Self::read_json(path).await {
            Ok(snapshot) => Ok(Some(snapshot)),
            Err(crate::Error::InternalFile(_, ref io)) if io.kind() == std::io::ErrorKind::NotFound => {
                // Absent file is not an error — fall back to live lookups.
                Ok(None)
            }
            Err(other) => Err(other),
        }
    }
}

// ── Phase 5B specification tests — PatchSnapshot + SnapshotVersion ──────────
//
// Traceability:
//   Test 1 — PatchSnapshot round-trips JSON deterministically (BTreeMap key order);
//             SnapshotVersion rejects an unknown version on deserialise.
//   Test 2 — PatchSnapshot::from_roots maps companions → digests and
//             descriptors → digests correctly from a SitePatchRoots.
//
// These tests MUST compile and FAIL against the unimplemented!() stub in
// PatchSnapshot::from_roots (the read/write paths are already implemented).

#[cfg(test)]
mod spec_tests {
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    use crate::{
        oci::{Digest, Identifier, PinnedIdentifier},
        package_manager::SitePatchRoots,
        patch::snapshot::{PatchSnapshot, SnapshotVersion},
    };

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned_id(registry: &str, repo: &str, hex_char: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, registry).clone_with_digest(sha256(hex_char));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Build a minimal `PatchSnapshot` with one companion and one descriptor.
    fn minimal_snapshot() -> PatchSnapshot {
        let mut companions = BTreeMap::new();
        companions.insert("example.com/ca-bundle".to_string(), sha256('c'));

        let mut descriptors = BTreeMap::new();
        descriptors.insert("patches.example.com".to_string(), sha256('d'));

        PatchSnapshot {
            version: SnapshotVersion::V1,
            companions,
            descriptors,
        }
    }

    // ── Test 1 — JSON round-trip + BTreeMap determinism + unknown version ─────

    /// A `PatchSnapshot` serialised to JSON and deserialised back must produce
    /// byte-identical output on repeated serialisation (BTreeMap key order is
    /// deterministic).
    ///
    /// Traceability: Phase 5B spec test 1 — round-trip + BTreeMap determinism.
    #[tokio::test(flavor = "multi_thread")]
    async fn snapshot_round_trips_json_deterministically() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("patches.snapshot.json");

        let original = minimal_snapshot();
        // write() delegates to SerdeExt::write_json — already implemented.
        original.write(&path).await.expect("write must succeed");

        let restored = PatchSnapshot::read(&path)
            .await
            .expect("read must not error")
            .expect("file exists; must return Some");

        assert_eq!(original.version, restored.version, "version must round-trip");
        assert_eq!(
            original.companions, restored.companions,
            "companions BTreeMap must round-trip"
        );
        assert_eq!(
            original.descriptors, restored.descriptors,
            "descriptors BTreeMap must round-trip"
        );

        // Determinism: serialise twice and compare bytes (BTreeMap guarantees order).
        let bytes1 = serde_json::to_string_pretty(&original).unwrap();
        let bytes2 = serde_json::to_string_pretty(&restored).unwrap();
        assert_eq!(
            bytes1, bytes2,
            "repeated serialisation must produce byte-identical output"
        );
    }

    /// A JSON blob with `"version": 99` (unknown) must be rejected on
    /// deserialise. `serde_repr` rejects unknown integer values automatically.
    ///
    /// Traceability: Phase 5B spec test 1 — unknown version rejected.
    #[test]
    fn unknown_snapshot_version_is_rejected_on_deserialise() {
        let json = r#"{"version":99,"companions":{},"descriptors":{}}"#;
        let result = serde_json::from_str::<PatchSnapshot>(json);
        assert!(
            result.is_err(),
            "unknown version 99 must be rejected on deserialise; got: {result:?}"
        );
    }

    /// A missing snapshot file must return `Ok(None)` — absent file is not an error.
    ///
    /// Traceability: Phase 5B spec test 1 — absent file returns Ok(None).
    #[tokio::test(flavor = "multi_thread")]
    async fn absent_snapshot_file_returns_ok_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.json");
        let result = PatchSnapshot::read(&path).await.expect("absent file must not error");
        assert!(result.is_none(), "absent file must return None; got: {result:?}");
    }

    // ── Test 2 — PatchSnapshot::from_roots ───────────────────────────────────

    /// `PatchSnapshot::from_roots` must map each `SitePatchRoots::companions`
    /// entry (a `PinnedIdentifier`) to the key `"registry/repository"` (the
    /// Display string without the `@digest` suffix) and the value = the pinned
    /// digest.
    ///
    /// It must also map each `SitePatchRoots::descriptors` entry
    /// `(registry_string, digest)` to the key = registry_string, value = digest.
    ///
    /// Traceability: Phase 5B spec test 2 — from_roots key/value mapping.
    ///
    /// NOTE: This test FAILS against the current stub (`unimplemented!()`).
    #[test]
    fn from_roots_maps_companions_and_descriptors_correctly() {
        let companion_digest = sha256('c');
        let descriptor_digest = sha256('d');

        let companion = pinned_id("example.com", "ca-bundle", 'c');
        let roots = SitePatchRoots {
            companions: vec![companion.clone()],
            // GC blob list — not consulted by from_roots.
            descriptors: vec![],
            // Per-source descriptor pin: key = the source's "registry/repository".
            descriptor_pins: vec![("patches.example.com/acme/cli".to_string(), descriptor_digest.clone())],
        };

        let snapshot = PatchSnapshot::from_roots(&roots);

        assert_eq!(snapshot.version, SnapshotVersion::V1, "snapshot version must be V1");

        // Companion key: "registry/repository" — no digest suffix, no tag.
        let expected_companion_key = format!("{}/{}", companion.registry(), companion.repository());
        assert!(
            snapshot.companions.contains_key(&expected_companion_key),
            "companion key '{expected_companion_key}' must be present; got keys: {:?}",
            snapshot.companions.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            snapshot.companions[&expected_companion_key], companion_digest,
            "companion digest must equal the pinned identifier's digest"
        );

        // Descriptor key: the source's canonical "registry/repository" (drives
        // frozen descriptor selection at compose time — C8).
        let expected_descriptor_key = "patches.example.com/acme/cli";
        assert!(
            snapshot.descriptors.contains_key(expected_descriptor_key),
            "descriptor key '{expected_descriptor_key}' must be present; got keys: {:?}",
            snapshot.descriptors.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            snapshot.descriptors[expected_descriptor_key], descriptor_digest,
            "descriptor digest must equal the pinned manifest digest"
        );
    }

    /// Multiple companions in the same `SitePatchRoots` produce multiple
    /// `companions` map entries in deterministic (BTreeMap-sorted) order.
    ///
    /// Traceability: Phase 5B spec test 2 — multiple companions in BTreeMap.
    ///
    /// NOTE: This test FAILS against the current stub (`unimplemented!()`).
    #[test]
    fn from_roots_multiple_companions_are_in_btreemap_order() {
        // Two companions whose `registry/repository` keys sort differently.
        let c1 = pinned_id("alpha.example.com", "tool-a", 'a');
        let c2 = pinned_id("beta.example.com", "tool-b", 'b');

        let roots = SitePatchRoots {
            companions: vec![c2.clone(), c1.clone()], // deliberately reversed order
            descriptors: vec![],
            descriptor_pins: vec![],
        };

        // This PANICS with unimplemented!() until Phase 5B is implemented.
        let snapshot = PatchSnapshot::from_roots(&roots);

        let keys: Vec<_> = snapshot.companions.keys().cloned().collect();
        let mut expected_keys = vec![
            format!("{}/{}", c1.registry(), c1.repository()),
            format!("{}/{}", c2.registry(), c2.repository()),
        ];
        expected_keys.sort();
        assert_eq!(
            keys, expected_keys,
            "BTreeMap must produce alphabetically sorted companion keys regardless of input order"
        );
    }
}
