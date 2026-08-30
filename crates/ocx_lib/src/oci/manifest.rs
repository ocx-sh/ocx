// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI manifest utilities — image-index admission and platform membership.
//!
//! For pure manifest *construction* (assembling `ImageManifest` + config blob
//! bytes from a `package::info::Info` + layer descriptors, with no I/O),
//! see [`crate::oci::manifest_builder`].

use super::{Digest, ImageIndex, Manifest, Platform};

/// An OCI image index that deserialised structurally but violates an invariant
/// of the image spec that OCX relies on.
///
/// Distinct from a serde failure: the document *is* shaped like an image index
/// (it carries `manifests`), so it can never be mistaken for a leaf manifest or
/// a cache miss — it is malformed index data and is refused as such.
#[derive(Debug, thiserror::Error)]
#[error("invalid OCI image index: {0}")]
pub struct InvalidImageIndex(String);

/// Validates the semantic invariants of an OCI image index at a trust boundary.
///
/// [`ImageIndex`] deserialisation only proves the *shape* — `schemaVersion` is
/// an unconstrained `u8` and every descriptor field is taken on faith, so
/// `{"schemaVersion":1,"manifests":[]}` parses happily. Registry and
/// public-index bytes are publisher-controlled, so the semantics are checked
/// here instead of being assumed:
///
/// - `schemaVersion` is [`INDEX_SCHEMA_VERSION`](super::INDEX_SCHEMA_VERSION)
///   — the only value the image spec allows.
/// - every entry declares a non-empty `mediaType` and `digest` (both REQUIRED
///   descriptor properties; either one empty leaves the child unaddressable).
/// - every entry declares a non-negative `size` (the wire type is `i64`).
///
/// The digest string is checked for presence only, never parsed: an index may
/// legitimately carry a digest algorithm this build does not implement, and
/// child selection already treats an unparseable digest as absent.
///
/// This is a **semantic** check, never `deny_unknown_fields` — the fleet reads
/// one another's documents, so an unknown sibling field a newer writer adds
/// must ride through untouched.
///
/// # Errors
///
/// [`InvalidImageIndex`] naming the first invariant the document violates.
pub fn validate_image_index(index: &ImageIndex) -> Result<(), InvalidImageIndex> {
    if index.schema_version != super::INDEX_SCHEMA_VERSION {
        return Err(InvalidImageIndex(format!(
            "schemaVersion is {}, expected {}",
            index.schema_version,
            super::INDEX_SCHEMA_VERSION
        )));
    }
    for (position, entry) in index.manifests.iter().enumerate() {
        if entry.media_type.is_empty() {
            return Err(InvalidImageIndex(format!(
                "manifests[{position}] has an empty mediaType"
            )));
        }
        if entry.digest.is_empty() {
            return Err(InvalidImageIndex(format!("manifests[{position}] has an empty digest")));
        }
        if entry.size < 0 {
            return Err(InvalidImageIndex(format!(
                "manifests[{position}] has a negative size {}",
                entry.size
            )));
        }
    }
    Ok(())
}

/// Returns `true` if the manifest contains an entry for the given platform.
///
/// # Strict equality semantics
///
/// This function uses **intentional strict struct-equality** on the serialized
/// `native::Platform` representation — it is testing exact manifest membership,
/// not host compatibility. This is **distinct** from [`super::is_compatible`],
/// which uses subset semantics on `os_features` (`offered ⊆ required`) for
/// install-resolution. Never replace this comparison with `is_compatible` —
/// the two functions serve different purposes. See [`super::is_compatible`]
/// for the subset-matching relation used during index resolution.
pub fn has_platform(manifest: &Manifest, platform: &Platform) -> bool {
    let Manifest::ImageIndex(index) = manifest else {
        return false;
    };
    let native = super::native::Platform::from(platform);
    let target = Some(native);
    index.manifests.iter().any(|e| e.platform == target)
}

/// Returns the digest of the manifest entry for the given platform, if present.
///
/// Complements [`has_platform`] (existence only) by extracting the pinned
/// digest, so a caller (e.g. the keep-tag push,
/// `adr_index_indirection.md` Decision E) can address that exact platform
/// manifest without re-deriving it from the layers/metadata that produced it.
pub fn platform_manifest_digest(manifest: &Manifest, platform: &Platform) -> Option<Digest> {
    let Manifest::ImageIndex(index) = manifest else {
        return None;
    };
    let target = Some(super::native::Platform::from(platform));
    index
        .manifests
        .iter()
        .find(|entry| entry.platform == target)
        .and_then(|entry| Digest::try_from(&entry.digest).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;

    fn make_index(entries: Vec<(&str, &str, i64)>) -> Manifest {
        let manifests = entries
            .into_iter()
            .map(|(platform_str, digest, size)| {
                let platform: Platform = platform_str.parse().unwrap();
                let native: oci::native::Platform = platform.into();
                oci::ImageIndexEntry {
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                    digest: digest.to_string(),
                    size,
                    platform: Some(native),
                    artifact_type: None,
                    annotations: None,
                }
            })
            .collect();
        Manifest::ImageIndex(oci::ImageIndex {
            schema_version: 2,
            media_type: None,
            artifact_type: None,
            manifests,
            annotations: None,
        })
    }

    // ── validate_image_index ─────────────────────────────────────────────────
    //
    // Every fixture below is a byte literal, deserialised through the same
    // `serde_json` path a registry response takes. `schemaVersion: 1` cannot be
    // produced by serialising an `ImageIndex` — every write site sets
    // `INDEX_SCHEMA_VERSION` — so a fixture built by the code under test could
    // not contradict it.

    fn parse(bytes: &[u8]) -> oci::ImageIndex {
        serde_json::from_slice(bytes).expect("the fixture must deserialise, or it tests the wrong thing")
    }

    #[test]
    fn wrong_schema_version_is_refused() {
        let error = validate_image_index(&parse(br#"{"schemaVersion":1,"manifests":[]}"#))
            .expect_err("schemaVersion 1 is not a valid image index");
        assert!(
            error.to_string().contains("schemaVersion is 1"),
            "the refusal must name the offending value: {error}"
        );
    }

    #[test]
    fn unaddressable_descriptors_are_refused() {
        let empty_media_type =
            br#"{"schemaVersion":2,"manifests":[{"mediaType":"","digest":"sha256:aa","size":1}]}"#.as_slice();
        let empty_digest =
            br#"{"schemaVersion":2,"manifests":[{"mediaType":"application/json","digest":"","size":1}]}"#.as_slice();
        let negative_size =
            br#"{"schemaVersion":2,"manifests":[{"mediaType":"application/json","digest":"sha256:aa","size":-1}]}"#
                .as_slice();
        for (fixture, expected) in [
            (empty_media_type, "empty mediaType"),
            (empty_digest, "empty digest"),
            (negative_size, "negative size"),
        ] {
            let error = validate_image_index(&parse(fixture)).expect_err("an unusable descriptor must be refused");
            assert!(
                error.to_string().contains(expected),
                "expected the refusal to mention '{expected}', got: {error}"
            );
        }
    }

    #[test]
    fn a_valid_index_with_unknown_sibling_fields_is_admitted() {
        // Semantic check, never `deny_unknown_fields`: `subject` is not modelled
        // by `ImageIndex`, and a newer writer's keys must ride through — the
        // fleet reads one another's documents.
        let fixture = br#"{
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [
                {"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:aa","size":0}
            ],
            "subject": {"mediaType":"application/json","digest":"sha256:bb","size":2},
            "sh.ocx.future": "a key this build has never heard of"
        }"#;
        validate_image_index(&parse(fixture)).expect("unknown sibling fields must not be a refusal");
    }

    #[test]
    fn an_empty_manifest_list_is_admitted() {
        // The image spec permits an empty `manifests` array; refusing it here
        // would reject a legitimate document. Only `schemaVersion` makes the
        // finding's `{"schemaVersion":1,"manifests":[]}` fixture invalid.
        validate_image_index(&parse(br#"{"schemaVersion":2,"manifests":[]}"#)).expect("an empty index is valid OCI");
    }

    #[test]
    fn has_platform_empty_index() {
        let manifest = make_index(vec![]);
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert!(!has_platform(&manifest, &platform));
    }

    #[test]
    fn has_platform_matching() {
        let manifest = make_index(vec![("linux/amd64", "sha256:abc", 100)]);
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert!(has_platform(&manifest, &platform));
    }

    #[test]
    fn has_platform_non_matching() {
        let manifest = make_index(vec![("linux/amd64", "sha256:abc", 100)]);
        let platform: Platform = "linux/arm64".parse().unwrap();
        assert!(!has_platform(&manifest, &platform));
    }

    #[test]
    fn has_platform_image_manifest_returns_false() {
        let manifest = Manifest::Image(oci::ImageManifest::default());
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert!(!has_platform(&manifest, &platform));
    }

    #[test]
    fn platform_manifest_digest_returns_matching_entry() {
        let hex_amd64 = "a".repeat(64);
        let hex_arm64 = "b".repeat(64);
        let manifest = make_index(vec![
            ("linux/amd64", &format!("sha256:{hex_amd64}"), 100),
            ("linux/arm64", &format!("sha256:{hex_arm64}"), 200),
        ]);
        let platform: Platform = "linux/arm64".parse().unwrap();
        let digest = platform_manifest_digest(&manifest, &platform).expect("entry present");
        assert_eq!(digest.to_string(), format!("sha256:{hex_arm64}"));
    }

    #[test]
    fn platform_manifest_digest_missing_platform_returns_none() {
        let manifest = make_index(vec![("linux/amd64", &format!("sha256:{}", "a".repeat(64)), 100)]);
        let platform: Platform = "linux/arm64".parse().unwrap();
        assert!(platform_manifest_digest(&manifest, &platform).is_none());
    }

    #[test]
    fn platform_manifest_digest_image_manifest_returns_none() {
        let manifest = Manifest::Image(oci::ImageManifest::default());
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert!(platform_manifest_digest(&manifest, &platform).is_none());
    }

    #[test]
    fn platform_manifest_digest_malformed_digest_returns_none() {
        // The digest string isn't validated at index-merge time — a bare
        // string like a mirror-generated placeholder can slip in. The
        // lookup must not panic; it treats an unparseable digest as absent.
        let manifest = make_index(vec![("linux/amd64", "sha256:not-valid-hex", 100)]);
        let platform: Platform = "linux/amd64".parse().unwrap();
        assert!(platform_manifest_digest(&manifest, &platform).is_none());
    }
}
