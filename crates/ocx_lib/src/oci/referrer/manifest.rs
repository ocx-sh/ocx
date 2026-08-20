// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI referrer manifest (image manifest carrying a `subject` descriptor).
//!
//! Phase 1 stub — shape only. The `ReferrerManifest` represents an OCI 1.1
//! image manifest whose `subject` field points at the target being referred
//! to (signature, SBOM, attestation). See
//! [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md)
//! for the full push-side state machine.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::media_types::{
    ANNOTATION_BUNDLE_CONTENT, ANNOTATION_BUNDLE_PREDICATE_TYPE, EMPTY_CONFIG, EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_SIZE,
};
use crate::oci::annotations::CREATED;
use crate::oci::sign::error::SignErrorKind;
use crate::oci::{Descriptor, OCI_IMAGE_MEDIA_TYPE};

/// OCI 1.1 image manifest carrying a `subject` descriptor.
///
/// Serializes to an OCI image manifest (`application/vnd.oci.image.manifest.v1+json`)
/// with `artifactType` set to the referrer's media type (e.g.
/// [`SIGSTORE_BUNDLE_V03`](super::media_types::SIGSTORE_BUNDLE_V03)) and a
/// `subject` descriptor identifying the manifest being referred to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferrerManifest {
    /// OCI schema version (always `2` for OCI 1.x manifests).
    #[serde(rename = "schemaVersion")]
    pub schema_version: u8,

    /// Top-level media type (`application/vnd.oci.image.manifest.v1+json`).
    #[serde(rename = "mediaType")]
    pub media_type: String,

    /// Artifact-specific type (e.g., [`SIGSTORE_BUNDLE_V03`](super::media_types::SIGSTORE_BUNDLE_V03)).
    #[serde(rename = "artifactType")]
    pub artifact_type: String,

    /// Empty-config descriptor per OCI empty-descriptor convention.
    pub config: Descriptor,

    /// Referrer payload layers (e.g., the Sigstore bundle blob).
    pub layers: Vec<Descriptor>,

    /// Descriptor of the subject this referrer refers to.
    pub subject: Descriptor,

    /// Sigstore bundle annotations (ADR D1), built by [`bundle_annotations`].
    ///
    /// `skip_serializing_if` is load-bearing, not tidiness: [`Self::to_canonical_json`]
    /// is a plain `serde_json::to_vec(self)` and the registry addresses the
    /// referrer by the SHA-256 of exactly those bytes. Without it every
    /// manifest built with `None` would gain `"annotations": null` and change
    /// digest. `BTreeMap` for byte-stable key order (DATA-DET-01).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,
}

impl ReferrerManifest {
    /// Build a referrer manifest for the given subject with a single payload layer.
    ///
    /// `artifact_type` is the referrer's media type (e.g.
    /// [`SIGSTORE_BUNDLE_V03`](super::media_types::SIGSTORE_BUNDLE_V03)).
    /// `payload` is the descriptor of the pushed payload blob. The config is the
    /// OCI empty-config descriptor per the empty-descriptor convention.
    /// `annotations` is [`bundle_annotations`]' output for a bundle referrer,
    /// or `None` — which serializes the key away entirely, leaving the bytes
    /// byte-identical to a manifest built before the field existed.
    ///
    /// cosign additionally stamps `config.artifactType` with the same value as
    /// the top-level `artifactType`. OCX does not: the day-1 spike (cosign
    /// v3.1.1 against zot) confirmed cosign's own read path discriminates by
    /// parsed bundle content and never reads `config.artifactType`, so omitting
    /// it breaks interop in neither direction.
    pub fn build(
        subject: Descriptor,
        artifact_type: &str,
        payload: Descriptor,
        annotations: Option<BTreeMap<String, String>>,
    ) -> Self {
        let config = Descriptor {
            media_type: EMPTY_CONFIG.to_string(),
            digest: EMPTY_CONFIG_DIGEST.to_string(),
            size: EMPTY_CONFIG_SIZE as i64,
            ..Descriptor::default()
        };
        Self {
            schema_version: 2,
            media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
            artifact_type: artifact_type.to_string(),
            config,
            layers: vec![payload],
            subject,
            annotations,
        }
    }

    /// Serialize the manifest to JSON bytes for push.
    ///
    /// The registry addresses the referrer by the SHA-256 of exactly these
    /// bytes, so the caller must digest the same buffer it pushes.
    ///
    /// # Errors
    ///
    /// Returns [`SignErrorKind::Internal`] when JSON serialization fails.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, SignErrorKind> {
        serde_json::to_vec(self).map_err(|e| SignErrorKind::Internal(Box::new(e)))
    }
}

/// Build the Sigstore bundle annotation set cosign writes (ADR D1).
///
/// `created` is [`bundle_created`]'s output, taken as an argument so the map
/// stays a pure function of its inputs. `predicate_type` is `Some` for an
/// attestation referrer and `None` for a signature referrer, which has no
/// predicate.
pub(crate) fn bundle_annotations(
    created: &str,
    content: &str,
    predicate_type: Option<&str>,
) -> BTreeMap<String, String> {
    let mut annotations = BTreeMap::new();
    annotations.insert(CREATED.to_string(), created.to_string());
    annotations.insert(ANNOTATION_BUNDLE_CONTENT.to_string(), content.to_string());
    if let Some(predicate_type) = predicate_type {
        annotations.insert(ANNOTATION_BUNDLE_PREDICATE_TYPE.to_string(), predicate_type.to_string());
    }
    annotations
}

/// The one instant a bundle push is stamped with: `SOURCE_DATE_EPOCH` when set,
/// else the wall clock.
///
/// Returned as a `DateTime` rather than a formatted string because the
/// attestation path needs the same instant twice — once as the
/// [`CREATED`] value and once inside the signed cosign predicate
/// wrapper. Two independent clock reads would let one of them stop honouring
/// `SOURCE_DATE_EPOCH` without anything noticing.
pub(crate) fn bundle_now() -> chrono::DateTime<chrono::Utc> {
    // var_os, not var: `var` folds a non-UTF-8 value into an Err that an
    // `.ok()` would drop silently, and a value we cannot read is exactly the
    // malformed case the branch below exists to report.
    let raw = std::env::var_os("SOURCE_DATE_EPOCH");
    let fixed = raw.as_deref().and_then(|raw| {
        let parsed = raw.to_str().and_then(created_from_epoch);
        if parsed.is_none() {
            // Reproducible-builds says a builder SHOULD reject a malformed
            // value. Refusing here would need a new error variant on the sign
            // path for a field with no security role, so we fall back to the
            // clock and make the lost determinism visible instead of silent.
            crate::log::warn!("ignoring malformed SOURCE_DATE_EPOCH {raw:?}; using the current time");
        }
        parsed
    });
    fixed.unwrap_or_else(chrono::Utc::now)
}

/// Formats [`bundle_now`]'s instant for [`CREATED`].
///
/// RFC 3339, second precision, explicit `Z` — byte-identical to the Go
/// `time.RFC3339` layout cosign formats with. Pure, so the format is
/// assertable against a literal without touching the environment or the clock.
pub(crate) fn bundle_created(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Parses a `SOURCE_DATE_EPOCH` value: decimal seconds since the Unix epoch,
/// surrounding whitespace tolerated. Split out from [`bundle_created`] so the
/// epoch path is reachable from a test without mutating the process
/// environment (TEST-05).
///
/// `None` for anything else, an out-of-range timestamp included — the caller
/// reports it and falls back to the clock.
fn created_from_epoch(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // .ok(): a parse failure and an out-of-range timestamp are one outcome
    // here, and the caller reports it.
    raw.trim()
        .parse::<i64>()
        .ok()
        .and_then(chrono::DateTime::from_timestamp_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::referrer::media_types::{BUNDLE_CONTENT_DSSE, BUNDLE_CONTENT_MESSAGE_SIGNATURE};

    fn descriptor(digest: &str, size: i64) -> Descriptor {
        Descriptor {
            media_type: OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: digest.to_string(),
            size,
            ..Descriptor::default()
        }
    }

    fn manifest_json(annotations: Option<BTreeMap<String, String>>) -> serde_json::Value {
        let manifest = ReferrerManifest::build(
            descriptor(&format!("sha256:{}", "a".repeat(64)), 2),
            "application/vnd.dev.sigstore.bundle.v0.3+json",
            descriptor(&format!("sha256:{}", "b".repeat(64)), 512),
            annotations,
        );
        let bytes = manifest.to_canonical_json().expect("manifest serializes");
        serde_json::from_slice(&bytes).expect("manifest is JSON")
    }

    /// The annotation keys are the referrer's registry address by way of the
    /// manifest digest, so they are asserted as literals, not through the
    /// constants they came from.
    #[test]
    fn signature_annotations_carry_created_and_message_signature_content() {
        let annotations = bundle_annotations("2026-08-20T12:34:56Z", BUNDLE_CONTENT_MESSAGE_SIGNATURE, None);

        assert_eq!(
            annotations.get("org.opencontainers.image.created").map(String::as_str),
            Some("2026-08-20T12:34:56Z")
        );
        assert_eq!(
            annotations.get("dev.sigstore.bundle.content").map(String::as_str),
            Some("message-signature")
        );
        assert_eq!(annotations.len(), 2, "a signature carries no predicateType");
    }

    #[test]
    fn attestation_annotations_add_the_predicate_type() {
        let annotations = bundle_annotations(
            "2026-08-20T12:34:56Z",
            BUNDLE_CONTENT_DSSE,
            Some("https://cyclonedx.org/bom"),
        );

        assert_eq!(
            annotations.get("dev.sigstore.bundle.content").map(String::as_str),
            Some("dsse-envelope")
        );
        assert_eq!(
            annotations.get("dev.sigstore.bundle.predicateType").map(String::as_str),
            Some("https://cyclonedx.org/bom")
        );
        assert_eq!(annotations.len(), 3);
    }

    /// Golden shape of what the sign pipeline pushes: `artifactType` stays at
    /// the top level, and the annotations object carries exactly cosign's two
    /// signature keys. cosign also stamps `config.artifactType`; the day-1
    /// spike showed its read path never consults that field, so OCX omits it
    /// and this test pins the omission.
    #[test]
    fn signature_referrer_manifest_golden_shape() {
        let value = manifest_json(Some(bundle_annotations(
            "2026-08-20T12:34:56Z",
            BUNDLE_CONTENT_MESSAGE_SIGNATURE,
            None,
        )));

        assert_eq!(
            value.get("artifactType").and_then(|v| v.as_str()),
            Some("application/vnd.dev.sigstore.bundle.v0.3+json")
        );
        assert!(
            value.get("config").and_then(|c| c.get("artifactType")).is_none(),
            "config.artifactType is deliberately not written"
        );

        let annotations = value.get("annotations").expect("annotations present");
        assert_eq!(
            annotations
                .get("org.opencontainers.image.created")
                .and_then(|v| v.as_str()),
            Some("2026-08-20T12:34:56Z")
        );
        assert_eq!(
            annotations.get("dev.sigstore.bundle.content").and_then(|v| v.as_str()),
            Some("message-signature")
        );
        assert_eq!(
            annotations.as_object().map(serde_json::Map::len),
            Some(2),
            "no third key on a signature referrer"
        );
    }

    /// `skip_serializing_if` is load-bearing: the registry addresses the
    /// referrer by the SHA-256 of these exact bytes, so a `None` must leave no
    /// trace at all — not `"annotations": null`, not `{}`.
    #[test]
    fn absent_annotations_serialize_to_no_key_at_all() {
        let value = manifest_json(None);

        assert!(
            !value.as_object().expect("object").contains_key("annotations"),
            "None must not emit the key: it would change the manifest digest"
        );
    }

    /// Go's `time.RFC3339` layout, which is what cosign formats `created` with:
    /// second precision, literal `Z`, no offset.
    fn assert_rfc3339_utc_seconds(created: &str) {
        assert_eq!(created.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ, got {created:?}");
        assert!(created.ends_with('Z'), "expected a literal Z, got {created:?}");
        let parsed = chrono::DateTime::parse_from_rfc3339(created).expect("parses as RFC 3339");
        assert_eq!(parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true), created);
    }

    /// Asserts the shape rather than a value, so it holds whether or not
    /// `SOURCE_DATE_EPOCH` is set in the environment running the test.
    #[test]
    fn bundle_created_is_rfc3339_utc_with_second_precision() {
        assert_rfc3339_utc_seconds(&bundle_created(bundle_now()));
    }

    fn epoch_as_created(raw: &str) -> Option<String> {
        created_from_epoch(raw).map(bundle_created)
    }

    /// The documented `SOURCE_DATE_EPOCH` behaviour, pinned to a literal so
    /// the reproducible-build path has an assertion that can fail.
    #[test]
    fn source_date_epoch_pins_created_to_that_instant() {
        assert_eq!(epoch_as_created("1700000000").as_deref(), Some("2023-11-14T22:13:20Z"));
    }

    /// Surrounding whitespace is tolerated: a value threaded through a shell
    /// or a CI variable routinely arrives padded.
    #[test]
    fn source_date_epoch_tolerates_surrounding_whitespace() {
        assert_eq!(
            epoch_as_created(" 1700000000 ").as_deref(),
            Some("2023-11-14T22:13:20Z")
        );
    }

    /// `None` is what sends the caller down the warn-and-use-the-clock branch,
    /// rather than resolving to some other fixed instant.
    #[test]
    fn malformed_source_date_epoch_is_rejected() {
        assert_eq!(created_from_epoch("nonsense"), None);
    }

    /// An empty value is set-but-unusable, not absent, so it takes the same
    /// branch as any other malformed value.
    #[test]
    fn empty_source_date_epoch_is_rejected() {
        assert_eq!(created_from_epoch(""), None);
    }
}
