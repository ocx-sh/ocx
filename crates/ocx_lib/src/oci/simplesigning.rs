// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The cosign *simplesigning* claim — the payload a `sha256-<hex>.sig`
//! sidecar layer carries and a sidecar signature is taken over.
//!
//! This is a wire format, not a convenience struct. Everything here is shaped
//! by bytes cosign 3.1.1 actually pushed, captured in
//! `test/tests/fixtures/golden/simplesigning_*`.

use serde::{Deserialize, Serialize};

/// The one `critical.type` value cosign writes and accepts.
pub const SIMPLESIGNING_CLAIM_TYPE: &str = "cosign container image signature";

/// The cosign simplesigning claim — the bytes a `.sig` sidecar signs.
///
/// **Field order is the wire order** and must not be reordered: `serde_json`
/// emits struct fields in declaration order, and this file relies on that
/// deliberately, because the golden fixtures are what cosign actually pushed
/// and their SHA-256 *is* the layer's registry address.
///
/// **Trust boundary.** Verification checks the signature over the **raw layer
/// bytes** as served. Never re-serialize a parsed claim to reconstruct the
/// signed payload — a round trip is not guaranteed byte-identical, and a
/// reconstruction that differs is a silent verification bypass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleSigningClaim {
    /// The part of the claim a verifier is required to understand.
    pub critical: Critical,
    /// Free-form publisher annotations. Emitted as an explicit `null` when
    /// absent — **never omitted**; cosign writes the key unconditionally, and
    /// dropping it changes the layer digest.
    pub optional: Option<serde_json::Value>,
}

/// The `critical` object. Field order is wire order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Critical {
    /// What was signed, by name.
    pub identity: Identity,
    /// What was signed, by digest.
    pub image: Image,
    /// Always [`SIMPLESIGNING_CLAIM_TYPE`] on anything OCX writes.
    #[serde(rename = "type")]
    pub claim_type: String,
}

/// The signed object's repository name, in cosign's Docker spelling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The repository reference, without a tag or digest.
    #[serde(rename = "docker-reference")]
    pub docker_reference: String,
}

/// The signed object's manifest digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// The subject manifest digest, in `sha256:<hex>` spelling.
    #[serde(rename = "docker-manifest-digest")]
    pub docker_manifest_digest: String,
}

impl SimpleSigningClaim {
    /// Build the claim for `docker_reference` over `subject`.
    pub fn new(docker_reference: impl Into<String>, subject: &crate::oci::Digest) -> Self {
        Self {
            critical: Critical {
                identity: Identity {
                    docker_reference: docker_reference.into(),
                },
                image: Image {
                    docker_manifest_digest: subject.to_string(),
                },
                claim_type: SIMPLESIGNING_CLAIM_TYPE.to_owned(),
            },
            optional: None,
        }
    }

    /// The exact bytes to sign: compact JSON, no trailing newline.
    ///
    /// # Errors
    /// Propagates a [`serde_json`] failure. Unreachable for a claim built by
    /// [`SimpleSigningClaim::new`]; reachable only if `optional` was set to a
    /// value that cannot serialize.
    pub fn to_signing_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::Digest;
    use crate::oci::referrer::media_types::{
        ANNOTATION_COSIGN_CERTIFICATE, ANNOTATION_COSIGN_SIGNATURE, SIMPLESIGNING_MEDIA_TYPE,
    };

    /// cosign's own payload bytes, byte-exact. `include_bytes!` rather than a
    /// runtime read on purpose: a moved fixture becomes a compile error, and
    /// no reader can normalise the bytes on the way in.
    const KEY_PAYLOAD: &[u8] = include_bytes!("../../../../test/tests/fixtures/golden/simplesigning_key_payload.json");
    const KEYLESS_PAYLOAD: &[u8] =
        include_bytes!("../../../../test/tests/fixtures/golden/simplesigning_keyless_payload.json");

    /// The `sha256-<hex>.sig` manifests cosign pushed for those payloads.
    const KEY_MANIFEST: &str = include_str!("../../../../test/tests/fixtures/golden/simplesigning_key_manifest.json");
    const KEYLESS_MANIFEST: &str =
        include_str!("../../../../test/tests/fixtures/golden/simplesigning_keyless_manifest.json");

    /// Rebuild a claim from a golden payload's own field values, through the
    /// public constructor — so what is proven is that `new` + serialization
    /// reproduce cosign's bytes, not merely that a parse round-trips.
    fn claim_from_golden(payload: &[u8]) -> SimpleSigningClaim {
        let parsed: serde_json::Value = serde_json::from_slice(payload).expect("golden payload is JSON");
        let reference = parsed
            .pointer("/critical/identity/docker-reference")
            .and_then(serde_json::Value::as_str)
            .expect("golden payload names a docker-reference");
        let subject = parsed
            .pointer("/critical/image/docker-manifest-digest")
            .and_then(serde_json::Value::as_str)
            .expect("golden payload names a docker-manifest-digest");
        let subject = Digest::try_from(subject).expect("golden subject is an OCI digest");

        SimpleSigningClaim::new(reference, &subject)
    }

    /// The `layers[0]` descriptor of a golden `.sig` manifest.
    fn golden_layer(manifest: &str) -> serde_json::Value {
        let parsed: serde_json::Value = serde_json::from_str(manifest).expect("golden manifest is JSON");
        parsed
            .pointer("/layers/0")
            .cloned()
            .expect("golden .sig manifest carries one layer")
    }

    /// T-08. The constructed claim serializes to cosign's payload byte for
    /// byte. The fixtures carry no trailing newline — that is the signed
    /// form, and `assert_eq!` over slices would catch one if it appeared.
    #[test]
    fn simplesigning_claim_bytes_match_the_golden_payload() {
        for (label, payload) in [("key", KEY_PAYLOAD), ("keyless", KEYLESS_PAYLOAD)] {
            let bytes = claim_from_golden(payload)
                .to_signing_bytes()
                .expect("a constructed claim serializes");
            assert_eq!(
                String::from_utf8_lossy(&bytes),
                String::from_utf8_lossy(payload),
                "{label} payload drifted from cosign's bytes"
            );
        }
    }

    /// T-09. The strong one: the serialized bytes hash to the digest cosign
    /// **pushed**, and are as long as the descriptor says. This is an
    /// end-to-end wire proof against a registry response, not against a file
    /// this repository also authored — any byte drift (field order,
    /// whitespace, escaping, a dropped `null`) moves the digest.
    #[test]
    fn simplesigning_claim_bytes_hash_to_the_pushed_layer_digest() {
        for (label, payload, manifest) in [
            ("key", KEY_PAYLOAD, KEY_MANIFEST),
            ("keyless", KEYLESS_PAYLOAD, KEYLESS_MANIFEST),
        ] {
            let layer = golden_layer(manifest);
            let pushed_digest = layer["digest"].as_str().expect("layer descriptor names a digest");
            let pushed_size = layer["size"].as_u64().expect("layer descriptor names a size");

            let bytes = claim_from_golden(payload)
                .to_signing_bytes()
                .expect("a constructed claim serializes");

            assert_eq!(
                crate::oci::Algorithm::Sha256.hash(&bytes).to_string(),
                pushed_digest,
                "{label} claim does not hash to the layer cosign pushed"
            );
            assert_eq!(bytes.len() as u64, pushed_size, "{label} claim length moved");
        }
    }

    /// T-10. `optional` is written even when empty. cosign emits the key
    /// unconditionally, so a `skip_serializing_if` here would silently change
    /// every layer digest OCX produces.
    #[test]
    fn optional_is_emitted_as_explicit_null() {
        let bytes = claim_from_golden(KEY_PAYLOAD)
            .to_signing_bytes()
            .expect("a constructed claim serializes");
        let text = String::from_utf8(bytes).expect("the claim is UTF-8");
        assert!(text.ends_with(r#","optional":null}"#), "optional was omitted: {text}");
    }

    /// E-10 / E-11. A populated `optional` round-trips as an object, and a
    /// parse of cosign's own bytes yields the same claim the constructor
    /// builds — the round trip is proven here so that production code never
    /// has to rely on it (see the type's trust-boundary note).
    #[test]
    fn a_populated_optional_serializes_as_an_object() {
        let mut claim = claim_from_golden(KEY_PAYLOAD);
        assert_eq!(
            claim,
            serde_json::from_slice::<SimpleSigningClaim>(KEY_PAYLOAD).expect("golden payload parses as a claim")
        );

        claim.optional = Some(serde_json::json!({ "creator": "ocx" }));
        let text = String::from_utf8(claim.to_signing_bytes().expect("serializes")).expect("UTF-8");
        assert!(text.ends_with(r#","optional":{"creator":"ocx"}}"#), "{text}");
    }

    /// T-11. The annotation namespaces are two, not one, and the key-mode
    /// manifest carrying the signature **alone** is a legal shape (E-12).
    #[test]
    fn cosign_annotation_keys_match_the_golden_manifests() {
        // The two keys genuinely live in different namespaces in cosign's own
        // output. Unifying them is the mutation this test exists to catch.
        assert_ne!(
            ANNOTATION_COSIGN_SIGNATURE.rsplit_once('/').map(|(ns, _)| ns),
            ANNOTATION_COSIGN_CERTIFICATE.rsplit_once('/').map(|(ns, _)| ns),
            "the signature and certificate annotations share a namespace"
        );

        let keyless = golden_layer(KEYLESS_MANIFEST);
        assert_eq!(keyless["mediaType"].as_str(), Some(SIMPLESIGNING_MEDIA_TYPE));
        let mut keyless_keys = keyless["annotations"]
            .as_object()
            .expect("keyless layer is annotated")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        keyless_keys.sort_unstable();
        let mut expected = vec![ANNOTATION_COSIGN_SIGNATURE, ANNOTATION_COSIGN_CERTIFICATE];
        expected.sort_unstable();
        assert_eq!(keyless_keys, expected);

        let key = golden_layer(KEY_MANIFEST);
        assert_eq!(key["mediaType"].as_str(), Some(SIMPLESIGNING_MEDIA_TYPE));
        let key_keys = key["annotations"]
            .as_object()
            .expect("key layer is annotated")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(
            key_keys,
            vec![ANNOTATION_COSIGN_SIGNATURE],
            "the key-mode manifest carries the signature alone: no certificate, no chain, no bundle"
        );
    }
}
