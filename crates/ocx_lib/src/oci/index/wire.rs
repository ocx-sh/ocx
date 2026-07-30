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
//! The grammar this module owns is the **catalog**: the root document that
//! names tags, and the `c/index.json` map that names roots. What a tag points
//! *at* is not defined here and never was ours to define — an index is a
//! catalog of OCI artifacts, so `tags[].content` names an **OCI image index**,
//! whose shape belongs to the OCI image spec ([`crate::oci::ImageIndex`]) and
//! is stored byte-for-byte as the registry served it
//! (`adr_oci_index_only_dispatch.md` D1).
//!
//! Two documents carry the format's version pin — `config.json` and the
//! `c/index.json` envelope — so [`SUPPORTED_FORMAT_VERSION`] lives here, with
//! the grammar, rather than beside either reader. `config.json`'s own struct
//! (`IndexFormatConfig` in `ocx_index.rs`) stays next to its one reader.
//!
//! Everything else about the format (catalog digest-diff sync, dispatch-object
//! decode, `select_best` resolution) is downstream policy, not grammar — see
//! `adr_index_indirection.md` Decisions A/C/F for that layer.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::oci;

/// The `format_version` OCX understands, shared by every document that carries
/// one — `config.json` and the [`CatalogDocument`] envelope. A served value
/// other than this is rejected (`adr_index_indirection.md` F1).
pub const SUPPORTED_FORMAT_VERSION: u64 = 1;

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
    /// Machine lane: tag → dispatch-object pointer.
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
    /// The digest of the OCI image index this tag resolved to at the moment it
    /// was observed — a tag is a floating pointer, and recording what it pointed
    /// at is the whole job of an index. `oci::Digest`'s serde is exact-wire
    /// (`"algo:hex"`), so a malformed value fails the WHOLE [`IndexRoot`]
    /// deserialize, not just this one tag entry — the opposite blast-radius
    /// trade from [`CatalogIndex`] below
    /// (`adr_index_indirection.md` F1 blast-radius contract, amendment
    /// 2026-07-19).
    pub content: oci::Digest,
    /// Per-tag yank marker (human-governed, survives index regeneration).
    /// Wire shape is an object (`{"reason": "...", "at": "..."}`), never a
    /// bare boolean — absence means not yanked.
    #[serde(default)]
    pub yanked: Option<YankMarker>,
}

/// Per-tag yank marker wire object (●) — a publisher's reason + timestamp for
/// pulling a tag out of default resolution. A yank is a signal, never a
/// delete (`ocx_index::surface_root_status`); presence alone (`.is_some()`)
/// is what callers act on, the fields are for surfacing to the user.
///
/// Both fields are `#[serde(default)]`: an index root is read by many ocx
/// versions at once, so a root serving only one of them must not fail the
/// WHOLE [`IndexRoot`] parse and render the package unresolvable
/// (`arch-principles.md`, fleet forward-compat on fleet-read config). Since
/// callers act on `.is_some()`, an empty string degrades the surfaced text and
/// nothing else.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct YankMarker {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub at: String,
}

/// The `packages` map inside a [`CatalogDocument`] (● `{"<ns>/<pkg>":
/// "sha256:<root-digest>"}`).
///
/// Deliberately kept `String`-valued, NOT `oci::Digest` — a bad catalog
/// entry must never fail the whole catalog parse (F1 blast-radius contract,
/// `adr_index_indirection.md`): one malformed value is a per-entry
/// staleness/recovery concern, resolved at the point that reads it, never a
/// reason to lose every OTHER package's listing. [`RootTag::content`] above
/// takes the opposite trade on purpose — a malformed dispatch-object digest is
/// trust-boundary corruption, so it fails the whole document (amendment
/// 2026-07-19).
pub type CatalogIndex = BTreeMap<String, String>;

/// `c/index.json` catalog document (● `{"format_version": 1, "packages": {…}}`).
///
/// The envelope is the wire: the hosted site and every verbatim copy of it serve
/// the version pin alongside the listing, the same versioned shape `config.json`
/// carries. `packages` is the catalog; `format_version` is a gate, not data —
/// see [`Self::into_packages`].
///
/// `format_version` is deliberately **not** `#[serde(default)]`: a listing with
/// no version pin is not a catalog document, and defaulting it would silently
/// admit an unversioned body as version 1. `packages` does default, so a
/// freshly deployed index with nothing published yet reads as an empty catalog
/// rather than a parse failure.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogDocument {
    pub format_version: u64,
    #[serde(default)]
    pub packages: CatalogIndex,
}

impl CatalogDocument {
    /// Wraps `packages` at the current [`SUPPORTED_FORMAT_VERSION`] for writing.
    pub fn new(packages: CatalogIndex) -> Self {
        Self {
            format_version: SUPPORTED_FORMAT_VERSION,
            packages,
        }
    }

    /// Version-gates the envelope and yields the catalog map.
    ///
    /// **Fail-closed** — the identical policy `config.json` already applies in
    /// [`OcxIndex::check_format_version`](super::OcxIndex): an unknown version
    /// is refused outright, never "ignore the envelope and read `packages`
    /// anyway". One version pin, one policy, one error, whichever document
    /// carries it. Reading a newer catalog on the strength of a field name
    /// OCX happens to recognise is exactly the mis-parse F1 exists to prevent,
    /// and a catalog is the listing every other read fans out from.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat)
    /// when `format_version` is not [`SUPPORTED_FORMAT_VERSION`].
    pub fn into_packages(self) -> crate::Result<CatalogIndex> {
        if self.format_version != SUPPORTED_FORMAT_VERSION {
            return Err(super::error::Error::UnsupportedIndexFormat {
                version: self.format_version,
            }
            .into());
        }
        Ok(self.packages)
    }
}

/// Specification tests for the frozen ● wire shapes, cross-checked against
/// the exact JSON `test/src/static_index.py` emits (`write_package()`,
/// `index_bytes()`, `write_catalog()` — see that module's docstring for the
/// served-tree layout these bytes populate). Parse-only checks:
/// [`IndexRoot`] derives `Deserialize` only (the store keeps raw bytes
/// verbatim, A2/A4 — there is nothing to re-serialize and compare byte-for-byte
/// here).
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
                "3.27": {{ "content": "{}", "observed": "2026-01-01T00:00:00Z", "yanked": {{ "reason": "critical security issue", "at": "2026-02-01T00:00:00Z" }} }}
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
        let yanked = root.tags.get("3.27").unwrap().yanked.as_ref().unwrap();
        assert_eq!(yanked.reason, "critical security issue");
        assert_eq!(yanked.at, "2026-02-01T00:00:00Z");
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

    #[test]
    fn yank_marker_missing_a_field_still_yanks_the_tag() {
        // Fleet forward-compat: a root serving a partial yank marker must not
        // fail the WHOLE `IndexRoot` parse and make the package unresolvable.
        // Callers act on `.is_some()`, so the marker survives; only the
        // surfaced text degrades.
        let json = format!(
            r#"{{
            "repository": "oci://ghcr.io/kitware/cmake",
            "tags": {{
                "3.27": {{ "content": "{}", "yanked": {{ "reason": "critical security issue" }} }}
            }}
        }}"#,
            test_digest('c')
        );
        let root: IndexRoot = serde_json::from_str(&json).expect("a partial yank marker must not fail the root parse");
        let yanked = root
            .tags
            .get("3.27")
            .unwrap()
            .yanked
            .as_ref()
            .expect("the tag stays yanked");
        assert_eq!(yanked.reason, "critical security issue");
        assert_eq!(
            yanked.at, "",
            "an absent timestamp degrades to empty, never to a parse failure"
        );
    }

    // ── the dispatch object a tag points at ───────────────────────────────

    #[test]
    fn image_index_object_parses_as_oci_manifest_image_index() {
        // What `o/<algo>/<hex>.json` holds is a registry's own OCI image index,
        // verbatim. Registries pretty-print, order keys as they please, and
        // serve fields OCX does not model (`subject` is not on `OciImageIndex`).
        // The fixture is deliberately NOT the canonical serde encoding of the
        // parsed value: if it were, a re-serialising implementation would be
        // byte-indistinguishable from a byte-copying one and every verbatim
        // assertion downstream would be vacuous.
        let json = format!(
            r#"{{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.index.v1+json",
  "artifactType": "application/vnd.sh.ocx.package.v1",
  "subject": {{ "mediaType": "application/vnd.oci.image.manifest.v1+json", "digest": "{d}", "size": 7 }},
  "manifests": [
    {{
      "mediaType": "application/vnd.oci.image.manifest.v1+json",
      "digest": "{d}",
      "size": 1234,
      "platform": {{ "architecture": "amd64", "os": "linux" }},
      "annotations": {{ "org.opencontainers.image.created": "2026-01-01T00:00:00Z" }}
    }}
  ]
}}"#,
            d = test_digest('c')
        );
        match serde_json::from_str::<oci::Manifest>(&json).expect("a registry image index must parse") {
            oci::Manifest::ImageIndex(index) => {
                assert_eq!(index.schema_version, 2);
                assert_eq!(index.manifests.len(), 1);
                assert_eq!(index.manifests[0].digest, test_digest('c').to_string());
                assert_eq!(
                    index.artifact_type.as_deref(),
                    Some("application/vnd.sh.ocx.package.v1"),
                    "artifactType is stored, never rendered — but it must survive the parse"
                );
            }
            other => panic!("an image index must not match the Image arm: {other:?}"),
        }

        // The unmodelled `subject` proves the forward-compat half: no
        // `deny_unknown_fields`, so a newer writer's keys never brick a reader.
        assert!(
            json.contains("\"subject\""),
            "the fixture must carry a field the fork does not model"
        );
    }

    // ── CatalogDocument ───────────────────────────────────────────────────

    #[test]
    fn catalog_document_parses_the_static_index_py_shape() {
        // Mirrors static_index.py's write_catalog().
        let json =
            r#"{"format_version": 1, "packages": {"kitware/cmake": "sha256:root1", "stable/tool": "sha256:root2"}}"#;
        let document: CatalogDocument = serde_json::from_str(json).unwrap();
        let catalog = document.into_packages().unwrap();
        assert_eq!(catalog.get("kitware/cmake"), Some(&"sha256:root1".to_string()));
        assert_eq!(catalog.get("stable/tool"), Some(&"sha256:root2".to_string()));
    }

    #[test]
    fn catalog_document_without_the_envelope_is_refused() {
        // The bare map is not a catalog document — `format_version` carries no
        // serde default precisely so an unversioned body cannot pass as v1.
        let json = r#"{"kitware/cmake": "sha256:root1"}"#;
        serde_json::from_str::<CatalogDocument>(json)
            .expect_err("a bare package map must not deserialize as a catalog document");
    }

    #[test]
    fn catalog_document_with_no_packages_key_reads_as_empty() {
        // A freshly deployed index that has published nothing yet.
        let document: CatalogDocument = serde_json::from_str(r#"{"format_version": 1}"#).unwrap();
        assert!(document.into_packages().unwrap().is_empty());
    }

    #[test]
    fn unsupported_catalog_format_version_fails_closed() {
        // Same policy as `config.json`'s gate: refuse, never read `packages`
        // anyway. The `packages` map below is deliberately well-formed — it is
        // the VERSION that must stop the read, not the payload.
        let json = r#"{"format_version": 2, "packages": {"kitware/cmake": "sha256:root1"}}"#;
        let document: CatalogDocument = serde_json::from_str(json).unwrap();
        let error = document
            .into_packages()
            .expect_err("an unknown catalog format_version must fail closed");
        assert!(
            error.to_string().contains("format_version 2"),
            "the refusal must name the served version, got: {error}"
        );
    }
}
