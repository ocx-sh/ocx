// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

use super::metadata::Metadata;
use ocx_oci::client::error::ClientError;
use ocx_oci::manifest_builder::ManifestBuilder;

/// What a package **is** — its metadata, for one platform — independent of
/// where it is published or stored.
///
/// It carries no identifier on purpose. A publish writes it to a physical
/// [`OciIdentifier`](ocx_oci::OciIdentifier) the caller names; a local
/// materialization stores it under a package
/// [`PackageRef`](ocx_oci::PackageRef). One field could not be both without
/// a conversion between the two, which is the thing ocx#504 made impossible.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Info {
    pub metadata: Metadata,
    pub platform: ocx_oci::Platform,
}

impl Info {
    /// The half of an OCX **package** image manifest that the package layer
    /// owns: `artifactType = MEDIA_TYPE_PACKAGE_V1` plus the serialized
    /// metadata config blob. Layers are appended by whoever resolved them —
    /// the registry push path from its uploaded descriptors, the local
    /// materialization path from the ones it staged.
    ///
    /// Both paths build from this one chain, so adding a manifest-level field
    /// here reaches both and the drift class where one path gains a feature the
    /// other lacks cannot open.
    ///
    /// # Errors
    ///
    /// [`ClientError::Serialization`] if the metadata cannot be serialized.
    pub fn manifest_builder(&self) -> Result<ManifestBuilder, ClientError> {
        use ocx_oci::{media_type::MEDIA_TYPE_PACKAGE_METADATA_V1, media_type::MEDIA_TYPE_PACKAGE_V1};

        ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_PACKAGE_V1)
            .config_serialized(MEDIA_TYPE_PACKAGE_METADATA_V1, &self.metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::{
        bundle::Bundle, bundle::Version, dependency::Dependencies, entrypoint::Entrypoints, env::Env,
    };
    use ocx_oci::{media_type::MEDIA_TYPE_PACKAGE_METADATA_V1, media_type::MEDIA_TYPE_PACKAGE_V1};

    fn fixture_info() -> Info {
        Info {
            metadata: Metadata::Bundle(Bundle {
                binaries: None,
                version: Version::V1,
                strip_components: None,
                env: Env::default(),
                dependencies: Dependencies::default(),
                entrypoints: Entrypoints::default(),
                integrations: Default::default(),
            }),
            platform: "linux/amd64".parse().expect("platform parses"),
        }
    }

    fn fixture_layer_descriptor() -> ocx_oci::Descriptor {
        let digest = "sha256:".to_string() + &"ab".repeat(32);
        ocx_oci::Descriptor {
            media_type: ocx_oci::media_type::MEDIA_TYPE_TAR_GZ.to_string(),
            digest,
            size: 1024,
            urls: None,
            artifact_type: None,
            annotations: None,
        }
    }

    /// A5 (BC2 · W8/F2 — byte+digest golden): the DEFAULT (no per-layer layout)
    /// publish path must reproduce a frozen manifest byte-for-byte, digest
    /// included. Both descriptor-build sites — `client.rs::push_multi_layer_manifest`
    /// and `pull_local::stage_layers` — route through [`Info::manifest_builder`]
    /// (the shared builder chain that
    /// `manifest_builder_matches_explicit_builder_chain` asserts equal to
    /// the explicit chain), so freezing this helper's output freezes both. Also
    /// asserts every layer descriptor carries no annotations — the fast
    /// structural guard on top of the golden.
    #[test]
    fn default_publish_manifest_matches_byte_digest_golden() {
        // Captured 2026-07-02 from the current default publish path. The Part-2
        // stub keeps default → annotations None, so these bytes are identical to
        // pre-change; a `LayerLayoutSpec::default()` layer must reproduce them
        // exactly. Freezing them catches any future regression that changes the
        // default manifest serialization / field order / descriptor construction.
        const GOLDEN_MANIFEST_JSON: &str = r#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.sh.ocx.package.v1+json","digest":"sha256:186be378707a65d521086e7ae2e1e8aa328d8d583cb655d981bc0335fa0708f4","size":29},"layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha256:abababababababababababababababababababababababababababababababab","size":1024}],"artifactType":"application/vnd.sh.ocx.package.v1"}"#;
        const GOLDEN_MANIFEST_DIGEST: &str = "sha256:514d43fd877fcf6790844f6d5d82f29ee9d3868e26a35a525f155c0b64f8ba15";

        let info = fixture_info();
        let layer = fixture_layer_descriptor();
        assert!(
            layer.annotations.is_none(),
            "the default-path layer descriptor carries no annotations"
        );

        let built = info
            .manifest_builder()
            .expect("config serializes")
            .layers(vec![layer])
            .build()
            .expect("default publish manifest builds");

        assert_eq!(
            String::from_utf8(built.manifest_bytes.clone()).expect("manifest bytes are UTF-8"),
            GOLDEN_MANIFEST_JSON,
            "default publish manifest bytes drifted from the frozen golden (BC2 regression)"
        );
        assert_eq!(
            built.manifest_digest.to_string(),
            GOLDEN_MANIFEST_DIGEST,
            "default publish manifest digest drifted from the frozen golden (BC2 regression)"
        );
        for descriptor in &built.manifest.layers {
            assert!(
                descriptor.annotations.is_none(),
                "every layer descriptor on the default path must have annotations: None (BC2)"
            );
        }
    }

    #[test]
    fn manifest_builder_matches_explicit_builder_chain() {
        // Canary: if someone changes `Info::manifest_builder`'s call sequence
        // (drops an annotation, flips a media type, reorders), this fails. The
        // explicit chain mirrors what `Publisher::push_package` and
        // `pull_local::pull_local` would each have inlined before unification.
        let info = fixture_info();
        let layer = fixture_layer_descriptor();

        let helper = info
            .manifest_builder()
            .expect("config serializes")
            .layers(vec![layer.clone()])
            .build()
            .expect("helper builds");
        let explicit = ManifestBuilder::new()
            .artifact_type(MEDIA_TYPE_PACKAGE_V1)
            .config_serialized(MEDIA_TYPE_PACKAGE_METADATA_V1, &info.metadata)
            .expect("config serializes")
            .layers(vec![layer])
            .build()
            .expect("explicit builds");

        assert_eq!(helper.manifest_bytes, explicit.manifest_bytes);
        assert_eq!(helper.manifest_digest.to_string(), explicit.manifest_digest.to_string());
        assert_eq!(helper.config_bytes, explicit.config_bytes);
        assert_eq!(helper.config_digest.to_string(), explicit.config_digest.to_string());
    }
}
