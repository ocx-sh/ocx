// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Frozen ● wire grammar for the ocx-index static-file format
//! (`adr_index_indirection.md` §Data Model), shared by the hosted `index.ocx.sh`
//! site and every local copy.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

/// The `format_version` OCX understands; any other served value is rejected.
pub const SUPPORTED_FORMAT_VERSION: u64 = 1;

/// The single wire-format version gate: every document carrying the pin routes
/// through it, so a fetched tree and a shipped copy are never trusted differently (CWE-501).
///
/// # Errors
///
/// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat)
/// when `version` is not [`SUPPORTED_FORMAT_VERSION`].
pub(crate) fn gate_format_version(version: u64) -> super::error::Result<()> {
    if version != SUPPORTED_FORMAT_VERSION {
        return Err(super::error::Error::UnsupportedIndexFormat { version });
    }
    Ok(())
}

/// `config.json` (● `{"format_version": 1}`); an absent document reads as [`Self::assumed_v1`].
// Field order is wire: `ocx-sh/index`'s `render.py` emits the same order, so reordering or adding a field breaks it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexFormatConfig {
    pub format_version: u64,
    /// Slash-separated segment count a package name must have within the
    /// namespace; absent = no constraint. Never a security control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_segments: Option<NonZeroU32>,
}

impl IndexFormatConfig {
    /// The config substituted for an absent `config.json`: version 1, no name constraint.
    pub fn assumed_v1() -> Self {
        Self {
            format_version: SUPPORTED_FORMAT_VERSION,
            name_segments: None,
        }
    }
}

/// `p/<ns>/<pkg>.json` root document (●): machine lane `tags`, the rest human-governed.
// No `deny_unknown_fields`: many client versions read one root, so a newer field must not fail older ones.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexRoot {
    /// Physical `oci://host/path` location content is fetched from; transport-only, never a storage key.
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
#[derive(Debug, Clone, Deserialize)]
pub struct RootTag {
    /// Digest of the OCI image index this tag resolved to; a malformed value
    /// fails the whole [`IndexRoot`], unlike [`CatalogIndex`].
    pub content: ocx_oci::Digest,
    /// Per-tag yank marker; absence means not yanked.
    #[serde(default)]
    pub yanked: Option<YankMarker>,
    /// Whether the publisher marked this tag as a moving pointer with no retention promise.
    /// Only a JSON `true` counts; any other value reads as durable and never fails the root parse.
    #[serde(default, deserialize_with = "lenient_flag")]
    pub ephemeral: bool,
}

impl RootTag {
    /// Whether a row's raw `ephemeral` value marks it ephemeral: only a JSON `true` does.
    pub fn is_ephemeral_marker(value: &serde_json::Value) -> bool {
        matches!(value, serde_json::Value::Bool(true))
    }
}

// A strict `bool` would make one foreign value fail the whole root, as the yank marker's defaults guard against.
fn lenient_flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(RootTag::is_ephemeral_marker(&serde_json::Value::deserialize(
        deserializer,
    )?))
}

/// Per-tag yank marker wire object (●); callers act on presence alone, the fields
/// only surface to the user.
// Both fields default, or a root serving only one fails the whole parse and the package becomes unresolvable.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct YankMarker {
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub at: String,
}

/// The `packages` map inside a [`CatalogDocument`] (● `{"<ns>/<pkg>": "sha256:<root-digest>"}`).
///
/// `String`-valued, not `Digest`, so one malformed entry never fails every other package's listing.
pub type CatalogIndex = BTreeMap<String, String>;

/// `c/index.json` catalog document (● `{"format_version": 1, "packages": {…}}`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CatalogDocument {
    // No `serde(default)`: it would admit an unversioned body as version 1.
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
    /// # Errors
    ///
    /// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat)
    /// when `format_version` is not [`SUPPORTED_FORMAT_VERSION`].
    pub fn into_packages(self) -> super::error::Result<CatalogIndex> {
        gate_format_version(self.format_version)?;
        Ok(self.packages)
    }
}

/// Specification tests for the frozen ● wire shapes, cross-checked against
/// the exact JSON `test/src/static_index.py` emits (`write_package()`,
/// `index_bytes()`, `write_catalog()` — see that module's docstring for the
/// served-tree layout these bytes populate). Parse-only checks:
/// [`IndexRoot`] derives `Deserialize` only (the store keeps raw bytes
/// verbatim — there is nothing to re-serialize and compare byte-for-byte here).
#[cfg(test)]
mod tests {
    use super::*;

    // ── IndexFormatConfig (config.json) ───────────────────────────────────

    /// C-001 — no `deny_unknown_fields`. A config carrying a key this ocx does
    /// not model (the withdrawn `min_ocx_version`, a future key) parses, so a
    /// newer index server never bricks an older client.
    #[test]
    fn config_tolerates_unknown_keys_for_forward_compat() {
        let config: IndexFormatConfig = serde_json::from_str(r#"{"format_version":1,"future_key":[]}"#)
            .expect("an unmodelled sibling key must not fail the parse");
        assert_eq!(config.format_version, 1);
        assert_eq!(
            config.name_segments, None,
            "an absent name_segments declares no constraint"
        );
    }

    /// C-001 — `format_version` carries no serde default, so an empty document
    /// cannot pass as version 1. Absence of the *file* reads as v1
    /// ([`IndexFormatConfig::assumed_v1`]); an empty *document* is malformed.
    #[test]
    fn config_without_format_version_is_refused() {
        serde_json::from_str::<IndexFormatConfig>("{}")
            .expect_err("a config with no format_version must not deserialize");
    }

    /// C-001 — `NonZeroU32` is the validator. There is no hand-written range
    /// check anywhere, so there is none to drift out of sync with the type.
    #[test]
    fn config_with_zero_name_segments_is_refused() {
        serde_json::from_str::<IndexFormatConfig>(r#"{"format_version":1,"name_segments":0}"#)
            .expect_err("name_segments 0 must fail deserialization");
    }

    /// C-001 — the value substituted for an absent `config.json` before the
    /// version gate runs (C-005), so the gate never needs a "was it there?"
    /// parameter.
    #[test]
    fn assumed_v1_is_version_one_with_no_name_segments() {
        let config = IndexFormatConfig::assumed_v1();
        assert_eq!(config.format_version, 1);
        assert_eq!(
            config.name_segments, None,
            "a tree that declares nothing can express every name"
        );
    }

    /// C-004 — absent is version 1, and the gate passes it. The substitution
    /// happens before the gate, so this is the pair that makes the "no `absent`
    /// parameter" claim hold.
    #[test]
    fn the_gate_passes_the_assumed_version_and_refuses_any_other() {
        gate_format_version(IndexFormatConfig::assumed_v1().format_version)
            .expect("the value substituted for an absent config.json must pass the gate");
        gate_format_version(SUPPORTED_FORMAT_VERSION).expect("the supported version passes");

        for version in [0, 2, u64::MAX] {
            let error = gate_format_version(version).expect_err("any other version is refused");
            assert!(
                matches!(error, super::super::error::Error::UnsupportedIndexFormat { .. }),
                "expected UnsupportedIndexFormat, got {error:?}"
            );
        }
    }

    /// C-004 — **one** version pin, one comparison. A second `!=` against
    /// [`SUPPORTED_FORMAT_VERSION`] is how two readers drift into trusting the
    /// same document differently (CWE-501), so the single-comparison property
    /// is pinned structurally rather than left to review.
    #[test]
    fn only_the_gate_compares_against_the_supported_version() {
        // ponytail: reads the source tree rather than listing sibling modules,
        // so a file added later is covered without editing this test.
        let mut offenders = Vec::new();
        let mut pending = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                pending.extend(std::fs::read_dir(&path).unwrap().map(|entry| entry.unwrap().path()));
                continue;
            }
            for (number, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") || !code.contains("SUPPORTED_FORMAT_VERSION") {
                    continue;
                }
                // `<` and `>` cover `<=` / `>=` too. They are in the set because
                // the deferred v2 change is `!=` → `<=`: a second reader added
                // with `<=` while the gate keeps `!=` is exactly the drift this
                // guards, and an `==`/`!=`-only scan would leave the count at 1
                // and pass.
                if ["==", "!=", "<", ">"].iter().any(|op| code.contains(op)) {
                    offenders.push(format!("{}:{}", path.display(), number + 1));
                }
            }
        }
        assert_eq!(
            offenders.len(),
            1,
            "within this crate, exactly one comparison against SUPPORTED_FORMAT_VERSION must \
             exist (inside gate_format_version); found: {offenders:?}"
        );
    }

    /// A syntactically valid `sha256:<hex>` digest for fixtures that need one
    /// but don't care which — `ocx_oci::Digest`'s serde requires exact-wire
    /// `"algo:hex"` with the right hex length, so a placeholder like
    /// `"sha256:aa"` (the pre-retype fixture value) no longer parses.
    fn test_digest(fill: char) -> ocx_oci::Digest {
        ocx_oci::Digest::Sha256(fill.to_string().repeat(64))
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
        // amendment 2026-07-19): `RootTag::content`'s `ocx_oci::Digest` deserialize
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
        //
        // `owners` below is the wrong shape — a real entry is an object
        // (`login`/`id`, plus the derived pre-0.5.0 `github`/`github_id`).
        // `IndexRoot` models no `owners` field at all, so nothing here parses
        // it, which is precisely the tolerance under test.
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

    /// Parses a one-tag root whose tag row carries `extra` verbatim after `content`.
    fn root_with_tag_row(extra: &str) -> IndexRoot {
        let json = format!(
            r#"{{"repository":"oci://ghcr.io/kitware/cmake","tags":{{"3.27":{{"content":"{}"{extra}}}}}}}"#,
            test_digest('c')
        );
        serde_json::from_str(&json).unwrap_or_else(|error| panic!("`{extra}` must not fail the root parse: {error}"))
    }

    #[test]
    fn ephemeral_true_marks_the_tag_ephemeral() {
        assert!(root_with_tag_row(r#","ephemeral":true"#).tags["3.27"].ephemeral);
    }

    #[test]
    fn ephemeral_absent_false_null_and_foreign_values_parse_as_durable() {
        // Forward-compat like the yank marker: a value this client does not understand must
        // never fail the whole root and make the package unresolvable.
        for extra in [
            "",
            r#","ephemeral":false"#,
            r#","ephemeral":null"#,
            r#","ephemeral":"true""#,
            r#","ephemeral":"yes""#,
            r#","ephemeral":1"#,
            r#","ephemeral":0"#,
            r#","ephemeral":{"at":"2026-01-01T00:00:00Z"}"#,
            r#","ephemeral":[true]"#,
        ] {
            assert!(
                !root_with_tag_row(extra).tags["3.27"].ephemeral,
                "`{extra}` must read as durable"
            );
        }
    }

    #[test]
    fn ephemeral_is_independent_of_the_yank_marker() {
        let tag = root_with_tag_row(r#","ephemeral":true,"yanked":{"reason":"r","at":"a"}"#)
            .tags
            .remove("3.27")
            .unwrap();
        assert!(tag.ephemeral);
        assert!(tag.yanked.is_some());
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
        match serde_json::from_str::<ocx_oci::Manifest>(&json).expect("a registry image index must parse") {
            ocx_oci::Manifest::ImageIndex(index) => {
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
