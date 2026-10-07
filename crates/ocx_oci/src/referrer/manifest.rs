// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI referrer manifest (image manifest carrying a `subject` descriptor).
//!
//! Push-side state machine: [`adr_oci_referrers_signing_v1.md`](../../../../../.claude/artifacts/adr_oci_referrers_signing_v1.md).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::media_types::{
    ANNOTATION_BUNDLE_CONTENT, ANNOTATION_BUNDLE_PREDICATE_TYPE, EMPTY_CONFIG, EMPTY_CONFIG_DIGEST, EMPTY_CONFIG_SIZE,
};
use crate::annotations::CREATED;
use crate::{Descriptor, OCI_IMAGE_MEDIA_TYPE};

/// OCI 1.1 image manifest carrying a `subject` descriptor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferrerManifest {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u8,

    #[serde(rename = "mediaType")]
    pub media_type: String,

    #[serde(rename = "artifactType")]
    pub artifact_type: String,

    pub config: Descriptor,

    pub layers: Vec<Descriptor>,

    pub subject: Descriptor,

    /// Without `skip_serializing_if`, `None` serializes as `"annotations": null`
    /// and changes the referrer's digest; `BTreeMap` keeps the bytes stable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<BTreeMap<String, String>>,
}

impl ReferrerManifest {
    /// Build a referrer manifest for the given subject with a single payload layer.
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
    /// The [`serde_json::Error`] when serialization fails.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// Build the Sigstore bundle annotation set cosign writes.
///
/// A signature carries `predicate_type` too: it is what tells it from an
/// attestation in a listing.
pub fn bundle_annotations(created: &str, content: &str, predicate_type: &str) -> BTreeMap<String, String> {
    let mut annotations = BTreeMap::new();
    annotations.insert(CREATED.to_string(), created.to_string());
    annotations.insert(ANNOTATION_BUNDLE_CONTENT.to_string(), content.to_string());
    annotations.insert(ANNOTATION_BUNDLE_PREDICATE_TYPE.to_string(), predicate_type.to_string());
    annotations
}

/// The one instant a bundle push is stamped with: `SOURCE_DATE_EPOCH` when set,
/// else the wall clock.
///
/// Read once and reused for [`CREATED`] and the signed predicate, or one of two
/// clock reads can silently stop honouring `SOURCE_DATE_EPOCH`.
pub fn bundle_now() -> chrono::DateTime<chrono::Utc> {
    pinned_instant().unwrap_or_else(chrono::Utc::now)
}

/// The instant `SOURCE_DATE_EPOCH` pins, or `None` when unset, blank, or
/// malformed.
///
/// The single reader of the variable (also `ocx_shell::ci::annotations`), or a
/// malformed value warns on one path and stays silent on the other.
pub fn pinned_instant() -> Option<chrono::DateTime<chrono::Utc>> {
    let raw = ocx_env::SOURCE_DATE_EPOCH.get_raw()?.into_string().ok()?;
    let parsed = created_from_epoch(&raw);
    if parsed.is_none() {
        // Log the key, never the value: a CI job log reaches more readers than the environment.
        log::warn!("ignoring malformed SOURCE_DATE_EPOCH; using the current time");
    }
    parsed
}

/// Formats [`bundle_now`]'s instant for [`CREATED`], byte-identical to cosign's
/// Go `time.RFC3339` layout (second precision, explicit `Z`).
pub fn bundle_created(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Parses a `SOURCE_DATE_EPOCH` value: decimal seconds since the Unix epoch,
/// surrounding whitespace tolerated; `None` otherwise, out-of-range included.
pub fn created_from_epoch(raw: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    // .ok(): the caller reports the failure.
    raw.trim()
        .parse::<i64>()
        .ok()
        .and_then(chrono::DateTime::from_timestamp_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::referrer::media_types::BUNDLE_CONTENT_DSSE;

    /// cosign v3's image-signature `predicateType`. Spelled out rather than
    /// imported from `crate::attest`: this module is the generic referrer
    /// shape and names nothing under `crate::sign`/`attest`, and the constant's
    /// value is pinned against cosign's captured golden at its own site
    /// (`crate::attest::tests::cosign_sign_predicate_type_matches_the_golden_referrer`).
    const COSIGN_SIGN_PREDICATE_TYPE: &str = "https://sigstore.dev/cosign/sign/v1";

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
    fn attestation_annotations_add_the_predicate_type() {
        let annotations = bundle_annotations("2026-08-20T12:34:56Z", BUNDLE_CONTENT_DSSE, "https://cyclonedx.org/bom");

        assert_eq!(
            annotations.get("org.opencontainers.image.created").map(String::as_str),
            Some("2026-08-20T12:34:56Z")
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
            BUNDLE_CONTENT_DSSE,
            COSIGN_SIGN_PREDICATE_TYPE,
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
            Some("dsse-envelope")
        );
        assert_eq!(
            annotations
                .get("dev.sigstore.bundle.predicateType")
                .and_then(|v| v.as_str()),
            Some("https://sigstore.dev/cosign/sign/v1")
        );
        assert_eq!(
            annotations.as_object().map(serde_json::Map::len),
            Some(3),
            "no fourth key on a signature referrer"
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

    /// Locks [`pinned_instant`]'s contract — the one reader of
    /// `SOURCE_DATE_EPOCH` after W21 — for every non-warn observable: unset,
    /// blank and malformed all pin no instant; a valid epoch pins exactly that
    /// instant. W21's divergence was in *which path warns* on a blank value,
    /// and this test asserts nothing about logs, so it does not witness that
    /// half (the crate has no log-capture harness — see the WP report).
    #[test]
    fn pinned_instant_locks_its_source_date_epoch_contract() {
        let lock = ocx_env::overrides::lock();

        lock.remove(&ocx_env::SOURCE_DATE_EPOCH);
        assert_eq!(pinned_instant(), None, "an unset variable pins no instant");

        lock.set(&ocx_env::SOURCE_DATE_EPOCH, "1700000000");
        assert_eq!(
            pinned_instant().map(bundle_created).as_deref(),
            Some("2023-11-14T22:13:20Z"),
            "a valid epoch pins that instant"
        );

        lock.set(&ocx_env::SOURCE_DATE_EPOCH, "");
        assert_eq!(pinned_instant(), None, "an empty value pins no instant");

        lock.set(&ocx_env::SOURCE_DATE_EPOCH, "yesterday");
        assert_eq!(pinned_instant(), None, "a malformed value pins no instant");
    }
}
