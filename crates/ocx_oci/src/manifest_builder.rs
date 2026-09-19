// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Pure assembly of OCI image manifests.
//!
//! The builder is intentionally decoupled from any OCX domain type — callers
//! supply the artifact-type and config-blob media types directly. This keeps
//! the module reusable across the package push path, the description push
//! path, and the local-test path without any of them sharing knowledge of
//! one another's payload shapes.

use super::client::error::ClientError;

/// Fluent assembler for [`crate::ImageManifest`]. No I/O, no network.
///
/// Consuming-builder shape: each setter takes `self` and returns `Self` so
/// callers can chain. Required pieces (config) are checked at [`build`]
/// time; missing config is rejected as [`ClientError::InvalidManifest`].
///
/// # Example
///
/// ```ignore
/// let artifact = ManifestBuilder::new()
///     .artifact_type(MEDIA_TYPE_PACKAGE_V1)
///     .config_serialized(MEDIA_TYPE_PACKAGE_METADATA_V1, &package_info.metadata)?
///     .layers(layer_descriptors)
///     .build()?;
/// ```
///
/// [`build`]: ManifestBuilder::build
#[derive(Default)]
pub struct ManifestBuilder {
    artifact_type: Option<String>,
    config: Option<ManifestConfigBlob>,
    layers: Vec<crate::Descriptor>,
    annotations: Option<std::collections::BTreeMap<String, String>>,
}

struct ManifestConfigBlob {
    media_type: String,
    bytes: Vec<u8>,
}

impl ManifestBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the manifest's `artifactType`.
    pub fn artifact_type(mut self, t: impl Into<String>) -> Self {
        self.artifact_type = Some(t.into());
        self
    }

    /// Serialize `value` as JSON and use the result as the config blob.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Serialization`] if `value` cannot be serialized.
    pub fn config_serialized<T: serde::Serialize>(
        mut self,
        media_type: impl Into<String>,
        value: &T,
    ) -> Result<Self, ClientError> {
        let bytes = serde_json::to_vec(value).map_err(ClientError::Serialization)?;
        self.config = Some(ManifestConfigBlob {
            media_type: media_type.into(),
            bytes,
        });
        Ok(self)
    }

    /// Use raw bytes as the config blob.
    pub fn config_bytes(mut self, media_type: impl Into<String>, bytes: Vec<u8>) -> Self {
        self.config = Some(ManifestConfigBlob {
            media_type: media_type.into(),
            bytes,
        });
        self
    }

    /// Append one layer descriptor.
    pub fn layer(mut self, descriptor: crate::Descriptor) -> Self {
        self.layers.push(descriptor);
        self
    }

    /// Append several layer descriptors. Order is preserved.
    pub fn layers(mut self, descriptors: impl IntoIterator<Item = crate::Descriptor>) -> Self {
        self.layers.extend(descriptors);
        self
    }

    /// Set manifest-level annotations.
    pub fn annotations(mut self, annotations: std::collections::BTreeMap<String, String>) -> Self {
        self.annotations = Some(annotations);
        self
    }

    /// Assemble the [`ManifestArtifacts`]. Validates layer sizes (negative
    /// sizes are rejected) and serializes the manifest into canonical JSON.
    ///
    /// # Errors
    ///
    /// - [`ClientError::InvalidManifest`] when no config was set or any
    ///   descriptor has a negative `size`.
    /// - [`ClientError::Serialization`] if the manifest cannot be serialized.
    pub fn build(self) -> Result<ManifestArtifacts, ClientError> {
        use crate::Algorithm;
        use crate::media_type::MEDIA_TYPE_OCI_IMAGE_MANIFEST;

        let config = self
            .config
            .ok_or_else(|| ClientError::InvalidManifest("config blob is required".to_string()))?;

        for descriptor in &self.layers {
            if descriptor.size < 0 {
                return Err(ClientError::InvalidManifest(format!(
                    "blob size {} is invalid (must be non-negative)",
                    descriptor.size,
                )));
            }
        }

        let config_bytes_len = config.bytes.len();
        let config_digest = Algorithm::Sha256.hash(&config.bytes);
        let config_size = i64::try_from(config_bytes_len).map_err(|_| {
            ClientError::InvalidManifest(format!("config blob size {config_bytes_len} exceeds i64::MAX"))
        })?;

        let manifest = crate::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: self.artifact_type,
            config: crate::Descriptor {
                media_type: config.media_type,
                digest: config_digest.to_string(),
                size: config_size,
                urls: None,
                artifact_type: None,
                annotations: None,
            },
            layers: self.layers,
            annotations: self.annotations,
            ..Default::default()
        };

        let manifest_bytes = serde_json::to_vec(&manifest).map_err(ClientError::Serialization)?;
        let manifest_digest = Algorithm::Sha256.hash(&manifest_bytes);

        Ok(ManifestArtifacts {
            manifest,
            manifest_bytes,
            manifest_digest,
            config_bytes: config.bytes,
            config_digest,
        })
    }
}

/// Built artifact returned by [`ManifestBuilder::build`].
///
/// Carries the assembled manifest, its canonical bytes + digest, and the
/// config blob with its digest. All fields owned; consume with field access
/// or convert into the bare manifest via `.into()`.
pub struct ManifestArtifacts {
    pub manifest: crate::ImageManifest,
    pub manifest_bytes: Vec<u8>,
    pub manifest_digest: crate::Digest,
    pub config_bytes: Vec<u8>,
    pub config_digest: crate::Digest,
}

impl From<ManifestArtifacts> for crate::ImageManifest {
    fn from(artifact: ManifestArtifacts) -> Self {
        artifact.manifest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Algorithm;
    use crate::media_type::{
        MEDIA_TYPE_DESCRIPTION_V1, MEDIA_TYPE_MARKDOWN, MEDIA_TYPE_OCI_EMPTY_CONFIG, MEDIA_TYPE_OCI_IMAGE_MANIFEST,
        MEDIA_TYPE_PACKAGE_METADATA_V1, MEDIA_TYPE_PACKAGE_V1, MEDIA_TYPE_TAR_GZ,
    };

    /// An opaque serializable config payload.
    ///
    /// The builder is decoupled from any OCX domain type by construction, so
    /// its own tests use a stand-in rather than `package::metadata::Metadata`.
    /// The frozen package-manifest goldens that DO pin real metadata bytes live
    /// beside the chain that produces them, at `package::info`.
    fn fixture_config() -> serde_json::Value {
        serde_json::json!({ "type": "bundle", "version": 1 })
    }

    fn fixture_layer_descriptor() -> crate::Descriptor {
        let digest = "sha256:".to_string() + &"ab".repeat(32);
        crate::Descriptor {
            media_type: MEDIA_TYPE_TAR_GZ.to_string(),
            digest,
            size: 1024,
            urls: None,
            artifact_type: None,
            annotations: None,
        }
    }

    #[test]
    fn build_matches_legacy_push() {
        let metadata = fixture_config();
        let layer = fixture_layer_descriptor();

        let config_bytes = serde_json::to_vec(&metadata).expect("metadata serializes");
        let config_digest = Algorithm::Sha256.hash(&config_bytes);
        let config_size = i64::try_from(config_bytes.len()).expect("config fits i64");

        let expected_manifest = crate::ImageManifest {
            media_type: Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()),
            artifact_type: Some(MEDIA_TYPE_PACKAGE_V1.to_string()),
            config: crate::Descriptor {
                media_type: MEDIA_TYPE_PACKAGE_METADATA_V1.to_string(),
                digest: config_digest.to_string(),
                size: config_size,
                urls: None,
                artifact_type: None,
                annotations: None,
            },
            layers: vec![layer.clone()],
            annotations: None,
            ..Default::default()
        };
        let expected_manifest_bytes = serde_json::to_vec(&expected_manifest).expect("manifest serializes");
        let expected_manifest_digest = Algorithm::Sha256.hash(&expected_manifest_bytes);

        let built = ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_PACKAGE_V1)
            .config_serialized(MEDIA_TYPE_PACKAGE_METADATA_V1, &metadata)
            .expect("config serializes")
            .layer(layer)
            .build()
            .expect("build succeeds");

        assert_eq!(built.config_bytes, config_bytes);
        assert_eq!(built.config_digest.to_string(), config_digest.to_string());
        assert_eq!(built.manifest_bytes, expected_manifest_bytes);
        assert_eq!(built.manifest_digest.to_string(), expected_manifest_digest.to_string());
        assert_eq!(built.manifest.media_type, expected_manifest.media_type);
        assert_eq!(built.manifest.artifact_type, expected_manifest.artifact_type);
        assert_eq!(built.manifest.layers.len(), 1);
    }

    #[test]
    fn description_manifest_builds_correctly() {
        // Mirror the description-push flow: empty config blob, a README layer,
        // optional manifest annotations.
        let config_data = b"{}".to_vec();
        let config_digest = Algorithm::Sha256.hash(&config_data);

        let readme = b"# Hello".to_vec();
        let readme_digest = Algorithm::Sha256.hash(&readme);
        let readme_size = i64::try_from(readme.len()).unwrap();
        let readme_descriptor = crate::Descriptor {
            media_type: MEDIA_TYPE_MARKDOWN.to_string(),
            digest: readme_digest.to_string(),
            size: readme_size,
            urls: None,
            artifact_type: None,
            annotations: Some([(crate::annotations::TITLE.to_string(), "README.md".to_string())].into()),
        };

        let mut annotations = std::collections::BTreeMap::new();
        annotations.insert(crate::annotations::TITLE.to_string(), "MyTool".to_string());

        let built = ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_DESCRIPTION_V1)
            .config_bytes(MEDIA_TYPE_OCI_EMPTY_CONFIG, config_data.clone())
            .layer(readme_descriptor.clone())
            .annotations(annotations.clone())
            .build()
            .expect("description manifest builds");

        assert_eq!(built.config_digest.to_string(), config_digest.to_string());
        assert_eq!(built.config_bytes, config_data);
        assert_eq!(
            built.manifest.artifact_type,
            Some(MEDIA_TYPE_DESCRIPTION_V1.to_string())
        );
        assert_eq!(built.manifest.config.media_type, MEDIA_TYPE_OCI_EMPTY_CONFIG);
        assert_eq!(built.manifest.config.size, 2);
        assert_eq!(built.manifest.layers.len(), 1);
        assert_eq!(built.manifest.layers[0].digest, readme_descriptor.digest);
        assert_eq!(built.manifest.layers[0].size, readme_descriptor.size);
        assert_eq!(built.manifest.annotations.as_ref(), Some(&annotations));
    }

    #[test]
    fn missing_config_rejected() {
        let result = ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_PACKAGE_V1)
            .layer(fixture_layer_descriptor())
            .build();
        assert!(matches!(result, Err(ClientError::InvalidManifest(_))));
    }

    #[test]
    fn config_only_artifact_no_layers_succeeds() {
        let metadata = fixture_config();
        let built = ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_PACKAGE_V1)
            .config_serialized(MEDIA_TYPE_PACKAGE_METADATA_V1, &metadata)
            .expect("config serializes")
            .build()
            .expect("zero-layer manifest builds");
        assert!(built.manifest.layers.is_empty());
    }

    #[test]
    fn multi_layer_ordering_preserved() {
        let metadata = fixture_config();
        let make = |i: u8| crate::Descriptor {
            media_type: MEDIA_TYPE_TAR_GZ.to_string(),
            digest: "sha256:".to_string() + &format!("{i:02x}").repeat(32),
            size: 1,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let built = ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_PACKAGE_V1)
            .config_serialized(MEDIA_TYPE_PACKAGE_METADATA_V1, &metadata)
            .expect("config serializes")
            .layer(make(1))
            .layer(make(2))
            .layers(vec![make(3), make(4)])
            .build()
            .expect("multi-layer manifest builds");
        let digests: Vec<_> = built.manifest.layers.iter().map(|d| d.digest.clone()).collect();
        assert_eq!(digests.len(), 4);
        // Order: 01, 02, 03, 04.
        assert!(digests[0].ends_with(&"01".repeat(32)));
        assert!(digests[1].ends_with(&"02".repeat(32)));
        assert!(digests[2].ends_with(&"03".repeat(32)));
        assert!(digests[3].ends_with(&"04".repeat(32)));
    }

    #[test]
    fn negative_descriptor_size_rejected() {
        let metadata = fixture_config();
        let invalid = crate::Descriptor {
            media_type: MEDIA_TYPE_TAR_GZ.to_string(),
            digest: "sha256:".to_string() + &"ff".repeat(32),
            size: -1,
            urls: None,
            artifact_type: None,
            annotations: None,
        };
        let result = ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_PACKAGE_V1)
            .config_serialized(MEDIA_TYPE_PACKAGE_METADATA_V1, &metadata)
            .expect("config serializes")
            .layer(invalid)
            .build();
        assert!(matches!(result, Err(ClientError::InvalidManifest(_))));
    }
}
