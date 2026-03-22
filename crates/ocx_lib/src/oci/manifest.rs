// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use super::{Manifest, Platform};

/// Returns `true` if the manifest contains an entry for the given platform.
pub fn has_platform(manifest: &Manifest, platform: &Platform) -> bool {
    let Manifest::ImageIndex(index) = manifest else {
        return false;
    };
    let native = super::native::Platform::from(platform);
    let target = Some(native);
    index.manifests.iter().any(|e| e.platform == target)
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
}
