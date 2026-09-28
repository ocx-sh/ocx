// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Patch-tier snapshot — the site-patch tier's equivalent of `ocx.lock`, written
//! by `ocx patch freeze` and preferred by compose over live tag lookups.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_repr::{Deserialize_repr, Serialize_repr};

use crate::SitePatchRoots;

/// File name of the patch snapshot, sibling to `ocx.lock`.
pub const PATCH_SNAPSHOT_FILE: &str = "patches.snapshot.json";

/// On-disk version tag for the patch snapshot format.
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
/// `registry/repository:tag`; write and read both go through here.
// `tag_or_latest()`, or the key misses the patch-companion record keyed by the same value.
pub fn companion_key(companion_id: &ocx_oci::PackageRef) -> String {
    format!(
        "{}/{}:{}",
        companion_id.registry(),
        companion_id.repository(),
        companion_id.tag_or_latest()
    )
}

/// Inverse of [`companion_key`]: the tagged identifier a key names, or `None`
/// when the key is not in that grammar.
// A registry (even with a port) ends at the first `/`; a repository has no `:`, so the tag starts at the last.
pub fn companion_key_identifier(key: &str) -> Option<ocx_oci::PackageRef> {
    let (registry, rest) = key.split_once('/')?;
    let (repository, tag) = rest.rsplit_once(':')?;
    Some(ocx_oci::PackageRef::new_registry(repository, registry).clone_with_tag(tag))
}

/// Frozen view of the active site-patch tier for reproducible builds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchSnapshot {
    /// Format version. Unknown versions are rejected on deserialise.
    pub version: SnapshotVersion,
    /// Companion packages pinned by the snapshot: [`companion_key`] → digest at freeze time.
    // Keyed by tag, not repository: one repository at two tags is two companions, and a
    // repository-only key makes a freeze drop one.
    pub companions: BTreeMap<String, ocx_oci::Digest>,
    /// Descriptor sources pinned by the snapshot: canonical `registry/repository`
    /// → manifest digest at freeze time. Compose loads each by digest, so a
    /// post-freeze `ocx patch sync` cannot change a frozen build; an absent source
    /// is not composed.
    pub descriptors: BTreeMap<String, ocx_oci::Digest>,
}

impl PatchSnapshot {
    /// Build a snapshot from live [`SitePatchRoots`]; equal roots yield byte-identical output.
    pub fn from_roots(roots: &SitePatchRoots) -> Self {
        let mut companions = BTreeMap::new();
        for pinned in &roots.companions {
            companions.insert(companion_key(pinned.as_identifier()), pinned.digest());
        }

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

    /// Write this snapshot to the given path as pretty-printed JSON, creating
    /// the parent directory if absent.
    ///
    /// # Errors
    ///
    /// The path cannot be created or the JSON cannot be serialised.
    pub async fn write(&self, path: &Path) -> crate::Result<()> {
        use ocx_util::prelude::SerdeExt;
        self.write_json(path).await.map_err(Into::into)
    }

    /// Read a snapshot from the given path, with the digest of the bytes it was
    /// parsed from.
    ///
    /// Returns `Ok(None)` when the file is absent. The digest is of *this* parse's
    /// bytes, or an execution record could name a file the invocation never composed against.
    ///
    /// # Errors
    ///
    /// The file exists but cannot be parsed, or carries a `version` other than
    /// [`SnapshotVersion::CURRENT`].
    pub async fn read(path: &Path) -> crate::Result<Option<(Self, ocx_oci::Digest)>> {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(crate::error::file_error(path, error)),
        };

        // Peek at `version` first, or `serde_repr` rejects an older file as an opaque error with no remedy.
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

        let digest = ocx_oci::Algorithm::Sha256.hash(&bytes);
        Ok(Some((serde_json::from_slice(&bytes)?, digest)))
    }
}

// ── PatchSnapshot + SnapshotVersion tests ───────────────────────────────────

#[cfg(test)]
mod spec_tests {
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    use crate::{
        SitePatchRoots,
        patch::snapshot::{PatchSnapshot, SnapshotVersion, companion_key, companion_key_identifier},
    };
    use ocx_oci::{Digest, PackageRef, PinnedPackageRef};

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn sha256(hex_char: char) -> Digest {
        Digest::Sha256(hex_char.to_string().repeat(64))
    }

    fn pinned_id(registry: &str, repo: &str, hex_char: char) -> PinnedPackageRef {
        let id = PackageRef::new_registry(repo, registry).clone_with_digest(sha256(hex_char));
        PinnedPackageRef::try_from(id).unwrap()
    }

    fn pinned_id_tagged(registry: &str, repo: &str, tag: &str, hex_char: char) -> PinnedPackageRef {
        let id = PackageRef::new_registry(repo, registry)
            .clone_with_tag(tag)
            .clone_with_digest(sha256(hex_char));
        PinnedPackageRef::try_from(id).unwrap()
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
    /// entry (a `PinnedPackageRef`) to the key `"registry/repository:tag"`
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
        // frozen descriptor selection at compose time).
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
            let identifier = PackageRef::new_registry(repository, registry).clone_with_tag(tag);
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
    }
}
