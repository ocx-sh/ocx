// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! OCI manifest utilities — platform membership checks.
//!
//! For pure manifest *construction* (assembling `ImageManifest` + config blob
//! bytes from a `package::info::Info` + layer descriptors, with no I/O),
//! see [`crate::oci::manifest_builder`].

use super::{Digest, Manifest, Platform};

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
/// digest, so a caller (e.g. the canonical-tag push,
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
