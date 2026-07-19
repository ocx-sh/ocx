// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Frozen ● wire grammar for the ocx-index static-file format
//! (`adr_index_indirection.md` §Data Model).
//!
//! These shapes are the ones shared verbatim by both consumers of the
//! ocx-index format (Decision H) — the hosted `index.ocx.sh` site and every
//! verbatim local copy of it (Decision A2):
//!
//! - [`crate::file_structure::IndexStore`] — the local index collection,
//!   reading/writing these shapes verbatim as bytes on disk.
//! - [`super::OcxIndex`] — the remote `index.ocx.sh` client, parsing them
//!   straight off the wire.
//!
//! `config.json` (the `{"format_version": 1}` version pin) is equally frozen
//! but stays a small, source-specific concept next to its one reader
//! (`IndexFormatConfig` in `ocx_index.rs`) rather than moving here.
//!
//! Everything else about the format (catalog conditional-GET, dispatch-object
//! decode, `select_best` resolution) is downstream policy, not grammar — see
//! `adr_index_indirection.md` Decisions A/C/F for that layer.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::oci;

/// `p/<ns>/<pkg>.json` root document (●).
///
/// The machine lane is `tags`; the remaining fields are the human-governed
/// lane surfaced (never silently acted on) per F3. No `deny_unknown_fields` —
/// index documents are read by many client versions at once and must tolerate
/// newer fields (fleet forward-compat).
#[derive(Debug, Clone, Deserialize)]
pub struct IndexRoot {
    /// Physical OCI location the leaf manifests/layers are fetched from — an
    /// `oci://host/path` reference (transport-only, never a storage key; C2/C3).
    pub repository: String,
    /// Machine lane: tag → observation pointer.
    #[serde(default)]
    pub tags: BTreeMap<String, RootTag>,
    /// Human lane: package-level status (`"yanked"` / `"deprecated"` / …).
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub deprecated_message: Option<String>,
    #[serde(default)]
    pub superseded_by: Option<String>,
}

/// A single tag pointer in a root's machine lane (●).
///
/// The wire object also carries an `observed` timestamp; it is tolerated (no
/// `deny_unknown_fields`) but not consumed here — the snapshot stamps its own
/// `observed` when it records the tag pointer.
#[derive(Debug, Clone, Deserialize)]
pub struct RootTag {
    /// The observation-object digest this tag points at. `oci::Digest`'s
    /// serde is exact-wire (`"algo:hex"`), so a malformed value fails the
    /// WHOLE [`IndexRoot`] deserialize, not just this one tag entry — the
    /// opposite blast-radius trade from [`CatalogIndex`] below
    /// (`adr_index_indirection.md` F1 blast-radius contract, amendment
    /// 2026-07-19).
    pub content: oci::Digest,
    /// Per-tag yank marker (human-governed, survives index regeneration).
    #[serde(default)]
    pub yanked: Option<bool>,
}

/// `o/sha256/<hex>.json` observation object (● — immutable CAS).
///
/// `platforms[].platform` is a verbatim OCI platform object (may carry
/// `os.version` / CPU `features`); `platforms[].digest` is the **platform
/// manifest** digest, never an image-index digest.
#[derive(Debug, Clone, Deserialize)]
pub struct Observation {
    #[serde(default)]
    pub platforms: Vec<ObservationPlatform>,
}

/// One `(platform, manifest-digest)` leaf inside an [`Observation`] (●).
#[derive(Debug, Clone, Deserialize)]
pub struct ObservationPlatform {
    /// Verbatim OCI platform object; converted to native [`oci::Platform`] with
    /// the warn-drop policy (`adr_platform_model_unification.md` D2) at select
    /// time via [`oci::Platform::try_from`].
    pub platform: oci::native::Platform,
    /// The platform-manifest digest. Same exact-wire `oci::Digest` serde as
    /// [`RootTag::content`] — a malformed value fails the whole
    /// [`Observation`] deserialize.
    pub digest: oci::Digest,
}

/// `c/index.json` catalog (● `{"<ns>/<pkg>": "sha256:<root-digest>"}`).
///
/// Deliberately kept `String`-valued, NOT `oci::Digest` — a bad catalog
/// entry must never fail the whole catalog parse (F1 blast-radius contract,
/// `adr_index_indirection.md`): one malformed value is a per-entry
/// staleness/recovery concern, resolved at the point that reads it, never a
/// reason to lose every OTHER package's listing. `RootTag::content` and
/// `ObservationPlatform::digest` above take the opposite trade on purpose —
/// a malformed root/obs digest is trust-boundary corruption, so it fails the
/// whole document (amendment 2026-07-19).
pub type CatalogIndex = BTreeMap<String, String>;

/// Specification tests for the frozen ● wire shapes, cross-checked against
/// the exact JSON `test/src/static_index.py` emits (`write_package()`,
/// `observation_bytes()`, `write_catalog()` — see that module's docstring for
/// the served-tree layout these bytes populate). Parse-only round-trip
/// checks: [`IndexRoot`]/[`Observation`] derive `Deserialize` only (the store
/// keeps raw bytes verbatim, A2/A4 — there is nothing to re-serialize and
/// compare byte-for-byte here).
#[cfg(test)]
mod tests {
    use super::*;

    /// A syntactically valid `sha256:<hex>` digest for fixtures that need one
    /// but don't care which — `oci::Digest`'s serde requires exact-wire
    /// `"algo:hex"` with the right hex length, so a placeholder like
    /// `"sha256:aa"` (the pre-retype fixture value) no longer parses.
    fn test_digest(fill: char) -> oci::Digest {
        oci::Digest::Sha256(fill.to_string().repeat(64))
    }

    // ── IndexRoot / RootTag ──────────────────────────────────────────────

    #[test]
    fn index_root_parses_the_minimal_static_index_py_shape() {
        // Mirrors static_index.py's write_package() with no optional
        // status/deprecated_message/superseded_by/yanked fields set.
        let json = format!(
            r#"{{
            "repository": "oci://ghcr.io/kitware/cmake",
            "tags": {{
                "3.28": {{ "content": "{}", "observed": "2026-01-01T00:00:00Z" }}
            }}
        }}"#,
            test_digest('a')
        );
        let root: IndexRoot = serde_json::from_str(&json).unwrap();
        assert_eq!(root.repository, "oci://ghcr.io/kitware/cmake");
        assert_eq!(root.tags.len(), 1);
        let tag = root.tags.get("3.28").unwrap();
        assert_eq!(tag.content, test_digest('a'));
        assert_eq!(
            tag.yanked, None,
            "yanked absent from the wire must parse as None, not Some(false)"
        );
        assert_eq!(root.status, None, "status absent from the wire must parse as None");
        assert_eq!(root.deprecated_message, None);
        assert_eq!(root.superseded_by, None);
    }

    #[test]
    fn index_root_parses_the_full_human_governed_lane() {
        let json = format!(
            r#"{{
            "repository": "oci://ghcr.io/kitware/cmake",
            "tags": {{
                "3.27": {{ "content": "{}", "observed": "2026-01-01T00:00:00Z", "yanked": true }}
            }},
            "status": "deprecated",
            "deprecated_message": "use 3.28 instead",
            "superseded_by": "kitware/cmake:3.28"
        }}"#,
            test_digest('b')
        );
        let root: IndexRoot = serde_json::from_str(&json).unwrap();
        assert_eq!(root.status.as_deref(), Some("deprecated"));
        assert_eq!(root.deprecated_message.as_deref(), Some("use 3.28 instead"));
        assert_eq!(root.superseded_by.as_deref(), Some("kitware/cmake:3.28"));
        assert_eq!(root.tags.get("3.27").unwrap().yanked, Some(true));
    }

    #[test]
    fn index_root_fails_whole_document_on_malformed_tag_content_digest() {
        // Locks the accepted failure mode (`adr_index_indirection.md`
        // amendment 2026-07-19): `RootTag::content`'s `oci::Digest` deserialize
        // is exact-wire, so a malformed value fails the WHOLE `IndexRoot`
        // parse — never a partial parse that drops just the bad tag. This is
        // the opposite blast-radius trade from `CatalogIndex` (a bad catalog
        // entry never fails the whole catalog, F1).
        let json = r#"{
            "repository": "oci://ghcr.io/kitware/cmake",
            "tags": {
                "3.28": { "content": "not-a-digest", "observed": "2026-01-01T00:00:00Z" }
            }
        }"#;
        let result: Result<IndexRoot, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "a malformed tag content digest must fail the whole IndexRoot parse"
        );
    }

    #[test]
    fn index_root_treats_explicit_null_the_same_as_absent() {
        let json = r#"{
            "repository": "oci://ghcr.io/kitware/cmake",
            "tags": {},
            "status": null,
            "deprecated_message": null,
            "superseded_by": null
        }"#;
        let root: IndexRoot = serde_json::from_str(json).unwrap();
        assert_eq!(root.status, None);
        assert_eq!(root.deprecated_message, None);
        assert_eq!(root.superseded_by, None);
    }

    #[test]
    fn index_root_tolerates_unknown_fields_for_fleet_forward_compat() {
        // No `deny_unknown_fields` — a newer index server may add fields an
        // older client must still parse (fleet forward-compat, `arch-principles.md`).
        let json = r#"{
            "repository": "oci://ghcr.io/kitware/cmake",
            "tags": {},
            "owners": ["alice"],
            "desc": "sha256:deadbeef"
        }"#;
        let result: Result<IndexRoot, _> = serde_json::from_str(json);
        assert!(
            result.is_ok(),
            "unknown fields must not fail parsing: {:?}",
            result.err()
        );
    }

    // ── Observation / ObservationPlatform ────────────────────────────────

    #[test]
    fn observation_parses_the_static_index_py_shape() {
        // Mirrors static_index.py's observation_bytes().
        let json = format!(
            r#"{{"platforms":[{{"platform":{{"architecture":"amd64","os":"linux"}},"digest":"{}"}}]}}"#,
            test_digest('c')
        );
        let obs: Observation = serde_json::from_str(&json).unwrap();
        assert_eq!(obs.platforms.len(), 1);
        assert_eq!(obs.platforms[0].digest, test_digest('c'));
    }

    #[test]
    fn observation_defaults_platforms_to_empty_when_absent() {
        let obs: Observation = serde_json::from_str("{}").unwrap();
        assert!(obs.platforms.is_empty());
    }

    #[test]
    fn observation_platform_digest_is_a_bare_string_field() {
        // Wire-shape sanity: `digest` is a bare string on ObservationPlatform
        // (the platform-manifest leaf), never nested under an image-index-
        // shaped structure.
        let json = format!(
            r#"{{"platform":{{"architecture":"arm64","os":"darwin"}},"digest":"{}"}}"#,
            test_digest('d')
        );
        let entry: ObservationPlatform = serde_json::from_str(&json).unwrap();
        assert_eq!(entry.digest, test_digest('d'));
    }

    #[test]
    fn observation_fails_whole_document_on_malformed_platform_digest() {
        // Same exact-wire contract as `RootTag::content` above — a malformed
        // platform-manifest digest fails the WHOLE `Observation` deserialize
        // (`adr_index_indirection.md` amendment 2026-07-19).
        let json = r#"{"platforms":[{"platform":{"architecture":"amd64","os":"linux"},"digest":"not-a-digest"}]}"#;
        let result: Result<Observation, _> = serde_json::from_str(json);
        assert!(
            result.is_err(),
            "a malformed platform manifest digest must fail the whole Observation parse"
        );
    }

    // ── CatalogIndex ──────────────────────────────────────────────────────

    #[test]
    fn catalog_index_parses_the_static_index_py_shape() {
        // Mirrors static_index.py's write_catalog().
        let json = r#"{"kitware/cmake": "sha256:root1", "stable/tool": "sha256:root2"}"#;
        let catalog: CatalogIndex = serde_json::from_str(json).unwrap();
        assert_eq!(catalog.get("kitware/cmake"), Some(&"sha256:root1".to_string()));
        assert_eq!(catalog.get("stable/tool"), Some(&"sha256:root2".to_string()));
    }
}
