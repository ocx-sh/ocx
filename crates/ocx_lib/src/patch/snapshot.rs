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
//! - **`companions` map** — key = `registry/repository:tag`, built by
//!   [`companion_key`] and read back by [`companion_key_identifier`], value =
//!   the pinned digest. The tag is load-bearing: a descriptor may name one
//!   repository at two tags, and the overlay composes each as its own
//!   companion, so a repository-only key would let a freeze drop one of them.
//!   Both halves of the scheme go through that one pair of functions, and
//!   `companion_key_round_trips` pins them as inverses.
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
/// `serde_repr` rejects unknown integer values on deserialise automatically.
/// Only the current generation is representable — a snapshot is a derived
/// file that `ocx patch freeze` rewrites in seconds, so an older one is
/// refused with that remedy rather than parsed by a second code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize_repr, Deserialize_repr)]
#[repr(u8)]
pub enum SnapshotVersion {
    /// Companion keys carry the tag (`registry/repository:tag`).
    V2 = 2,
}

impl SnapshotVersion {
    /// The version every snapshot this binary writes carries, and the only one
    /// it reads.
    pub const CURRENT: Self = Self::V2;
}

/// The [`PatchSnapshot::companions`] key for a companion identifier:
/// `registry/repository:tag`.
///
/// The tag is resolved through `tag_or_latest()` so the key matches the patch
/// tier's own record, which is keyed by the same value inside
/// `state/patch-companions/<registry>/<repository>.json`. Write and read both
/// go through here; [`companion_key_identifier`] is the inverse.
pub fn companion_key(companion_id: &oci::Identifier) -> String {
    format!(
        "{}/{}:{}",
        companion_id.registry(),
        companion_id.repository(),
        companion_id.tag_or_latest()
    )
}

/// Inverse of [`companion_key`]: the tagged identifier a key names, or `None`
/// when the key is not in that grammar.
///
/// The split is unambiguous in both directions. A registry is a bare host
/// authority, so it ends at the first `/` even when it carries a port; a
/// repository cannot contain `:`, so the tag begins at the last one.
pub fn companion_key_identifier(key: &str) -> Option<oci::Identifier> {
    let (registry, rest) = key.split_once('/')?;
    let (repository, tag) = rest.rsplit_once(':')?;
    Some(oci::Identifier::new_registry(repository, registry).clone_with_tag(tag))
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
    /// Key: `registry/repository:tag`, from [`companion_key`].
    /// Value: pinned digest at freeze time.
    ///
    /// Keyed by tag, not by repository: one repository named at two tags is
    /// two companions in the overlay, each composing its own package.
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
    /// Companion key = [`companion_key`] of the pinned identifier
    /// (`registry/repository:tag`). Descriptor key = the source key stored in
    /// the `(source_key, digest)` tuple.
    ///
    /// `BTreeMap` insertion is ordered so repeated calls with the same roots
    /// yield byte-identical output.
    pub fn from_roots(roots: &SitePatchRoots) -> Self {
        // Build the companions map through the shared key helper, so the read
        // side (`companion_pin`) cannot drift from what a freeze writes.
        // BTreeMap insertion guarantees deterministic key order.
        let mut companions = BTreeMap::new();
        for pinned in &roots.companions {
            companions.insert(companion_key(pinned.as_identifier()), pinned.digest());
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
            version: SnapshotVersion::CURRENT,
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

    /// Read a snapshot from the given path, with the digest of the bytes it was
    /// parsed from.
    ///
    /// Returns `Ok(None)` when the file is absent so callers can fall back to
    /// live lookups without treating a missing snapshot as an error.
    ///
    /// The digest rides along rather than being a second call the caller makes,
    /// because it is the identity of *this* parse: an execution record naming a
    /// snapshot digest that a later read produced would describe a file the
    /// invocation never composed against.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be parsed, or if it
    /// carries a `version` other than [`SnapshotVersion::CURRENT`] — a
    /// snapshot is derived state, so the remedy is to rewrite it with
    /// `ocx patch freeze` rather than to keep a reader for the older shape.
    pub async fn read(path: &Path) -> crate::Result<Option<(Self, oci::Digest)>> {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            // Absent file is not an error — fall back to live lookups.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(crate::error::file_error(path, error)),
        };

        // Peek at `version` before the struct parse. `serde_repr` would reject
        // an older generation as an opaque "unknown variant" with no remedy in
        // it; a snapshot is cheap to rebuild, so the error says how.
        let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
        if let Some(found) = raw.get("version").and_then(serde_json::Value::as_u64)
            && found != SnapshotVersion::CURRENT as u64
        {
            return Err(crate::patch::PatchError::UnsupportedSnapshotVersion {
                path: path.display().to_string(),
                found,
                expected: SnapshotVersion::CURRENT as u64,
            }
            .into());
        }

        let digest = oci::Algorithm::Sha256.hash(&bytes);
        Ok(Some((serde_json::from_slice(&bytes)?, digest)))
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
        patch::snapshot::{PatchSnapshot, SnapshotVersion, companion_key, companion_key_identifier},
    };

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned_id(registry: &str, repo: &str, hex_char: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, registry).clone_with_digest(sha256(hex_char));
        PinnedIdentifier::try_from(id).unwrap()
    }

    fn pinned_id_tagged(registry: &str, repo: &str, tag: &str, hex_char: char) -> PinnedIdentifier {
        let id = Identifier::new_registry(repo, registry)
            .clone_with_tag(tag)
            .clone_with_digest(sha256(hex_char));
        PinnedIdentifier::try_from(id).unwrap()
    }

    /// Build a minimal `PatchSnapshot` with one companion and one descriptor.
    fn minimal_snapshot() -> PatchSnapshot {
        let mut companions = BTreeMap::new();
        companions.insert("example.com/ca-bundle:latest".to_string(), sha256('c'));

        let mut descriptors = BTreeMap::new();
        descriptors.insert("patches.example.com".to_string(), sha256('d'));

        PatchSnapshot {
            version: SnapshotVersion::CURRENT,
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

        let (restored, digest) = PatchSnapshot::read(&path)
            .await
            .expect("read must not error")
            .expect("file exists; must return Some");

        assert_eq!(
            digest,
            Digest::Sha256(hex::encode(<sha2::Sha256 as sha2::Digest>::digest(
                std::fs::read(&path).expect("the file just written")
            ))),
            "the reported digest must be of the snapshot file's own bytes"
        );
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
    /// entry (a `PinnedIdentifier`) to the key `"registry/repository:tag"`
    /// ([`companion_key`], no `@digest` suffix) and the value = the pinned
    /// digest. A tagless pinned identifier keys under `latest`, mirroring the
    /// record's own `tag_or_latest()` slot.
    ///
    /// It must also map each `SitePatchRoots::descriptors` entry
    /// `(registry_string, digest)` to the key = registry_string, value = digest.
    ///
    /// Traceability: Phase 5B spec test 2 — from_roots key/value mapping.
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

        assert_eq!(
            snapshot.version,
            SnapshotVersion::CURRENT,
            "snapshot version must be the current one"
        );

        // Companion key: "registry/repository:tag" — no digest suffix.
        let expected_companion_key = companion_key(companion.as_identifier());
        assert_eq!(
            expected_companion_key, "example.com/ca-bundle:latest",
            "a tagless companion must key under the record's `latest` slot"
        );
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

    /// One companion repository pinned at TWO tags is two companions, and a
    /// freeze must record both.
    ///
    /// Two tags of one repository coexist in the overlay by design — the
    /// descriptor names each tag separately and each composes its own package.
    /// A repository-keyed `companions` map collapses them: the second tag
    /// overwrites the first at freeze time, so the frozen build silently
    /// composes one version twice. The key therefore carries the tag.
    #[test]
    fn from_roots_keeps_both_tags_of_one_companion_repository() {
        let first = pinned_id_tagged("example.com", "ca-bundle", "1.0.0", 'a');
        let second = pinned_id_tagged("example.com", "ca-bundle", "2.0.0", 'b');

        let roots = SitePatchRoots {
            companions: vec![first, second],
            descriptors: vec![],
            descriptor_pins: vec![],
        };

        let snapshot = PatchSnapshot::from_roots(&roots);

        assert_eq!(
            snapshot.companions.len(),
            2,
            "both tags of one repository must survive the freeze; got: {:?}",
            snapshot.companions
        );
        assert_eq!(
            snapshot.companions.get("example.com/ca-bundle:1.0.0"),
            Some(&sha256('a')),
            "the first tag must keep its own digest; got: {:?}",
            snapshot.companions
        );
        assert_eq!(
            snapshot.companions.get("example.com/ca-bundle:2.0.0"),
            Some(&sha256('b')),
            "the second tag must keep its own digest; got: {:?}",
            snapshot.companions
        );
    }

    /// Multiple companions in the same `SitePatchRoots` produce multiple
    /// `companions` map entries in deterministic (BTreeMap-sorted) order.
    ///
    /// Traceability: Phase 5B spec test 2 — multiple companions in BTreeMap.
    #[test]
    fn from_roots_multiple_companions_are_in_btreemap_order() {
        // Two companions whose keys sort differently.
        let c1 = pinned_id("alpha.example.com", "tool-a", 'a');
        let c2 = pinned_id("beta.example.com", "tool-b", 'b');

        let roots = SitePatchRoots {
            companions: vec![c2.clone(), c1.clone()], // deliberately reversed order
            descriptors: vec![],
            descriptor_pins: vec![],
        };

        let snapshot = PatchSnapshot::from_roots(&roots);

        let keys: Vec<_> = snapshot.companions.keys().cloned().collect();
        let mut expected_keys = vec![companion_key(c1.as_identifier()), companion_key(c2.as_identifier())];
        expected_keys.sort();
        assert_eq!(
            keys, expected_keys,
            "BTreeMap must produce alphabetically sorted companion keys regardless of input order"
        );
    }

    // ── Key scheme — write and read are inverses ─────────────────────────────

    /// `companion_key` and `companion_key_identifier` must round-trip, including
    /// a port-bearing registry (the `:` in `localhost:5000` must not be read as
    /// a tag separator) and a multi-segment repository.
    #[test]
    fn companion_key_round_trips() {
        for (registry, repository, tag) in [
            ("example.com", "ca-bundle", "1.0.0"),
            ("localhost:5000", "acme/certs", "2026-01"),
            ("registry.example.com", "a/b/c", "latest"),
        ] {
            let identifier = Identifier::new_registry(repository, registry).clone_with_tag(tag);
            let key = companion_key(&identifier);
            assert_eq!(key, format!("{registry}/{repository}:{tag}"));

            let decoded = companion_key_identifier(&key).expect("a key this helper wrote must decode");
            assert_eq!(decoded.registry(), registry, "registry must round-trip from '{key}'");
            assert_eq!(
                decoded.repository(),
                repository,
                "repository must round-trip from '{key}'"
            );
            assert_eq!(decoded.tag(), Some(tag), "tag must round-trip from '{key}'");
        }
    }

    /// A key outside the grammar decodes to `None` rather than a wrong
    /// identifier — GC skips it instead of seeding a bogus root.
    #[test]
    fn companion_key_identifier_rejects_a_foreign_key() {
        for key in ["example.com/ca-bundle", "no-slash:1.0.0", ""] {
            assert!(
                companion_key_identifier(key).is_none(),
                "'{key}' is not in the key grammar and must not decode"
            );
        }
    }

    // ── Version gate — an older snapshot is refused with its remedy ───────────

    /// A snapshot file carrying a superseded `version` is refused with an error
    /// naming `ocx patch freeze`. There is no reader for the older shape: a
    /// snapshot is derived state, re-resolved offline in seconds.
    #[tokio::test(flavor = "multi_thread")]
    async fn superseded_snapshot_version_is_refused_with_a_freeze_remedy() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("patches.snapshot.json");
        std::fs::write(
            &path,
            r#"{"version":1,"companions":{"example.com/ca-bundle":"sha256:cc"},"descriptors":{}}"#,
        )
        .unwrap();

        let error = PatchSnapshot::read(&path)
            .await
            .expect_err("a superseded snapshot version must not be read");

        let message = format!("{error}");
        assert!(
            message.contains("ocx patch freeze"),
            "the refusal must name the command that rewrites the snapshot; got: {message}"
        );
        assert!(
            message.contains("version 1"),
            "the refusal must name the version it found; got: {message}"
        );
        assert_eq!(
            crate::cli::ClassifyExitCode::classify(&error),
            Some(crate::cli::ExitCode::DataError),
            "a stale persisted format is malformed input for this binary (65)"
        );
    }
}
