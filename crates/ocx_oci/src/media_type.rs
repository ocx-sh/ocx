// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_util::compression::CompressionAlgorithm;

pub const MEDIA_TYPE_OCI_IMAGE_INDEX: &str = crate::OCI_IMAGE_INDEX_MEDIA_TYPE;
pub const MEDIA_TYPE_OCI_IMAGE_MANIFEST: &str = crate::OCI_IMAGE_MEDIA_TYPE;
/// The artifact type of an ocx package's image-index entry.
pub const MEDIA_TYPE_PACKAGE_V1: &str = "application/vnd.sh.ocx.package.v1";
pub const MEDIA_TYPE_PACKAGE_METADATA_V1: &str = "application/vnd.sh.ocx.package.v1+json";
pub const MEDIA_TYPE_TAR_GZ: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
pub const MEDIA_TYPE_TAR_XZ: &str = "application/vnd.oci.image.layer.v1.tar+xz";
pub const MEDIA_TYPE_TAR_ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
/// The artifact type of a description manifest (README + optional logo).
pub const MEDIA_TYPE_DESCRIPTION_V1: &str = "application/vnd.sh.ocx.description.v1";
pub const MEDIA_TYPE_OCI_EMPTY_CONFIG: &str = "application/vnd.oci.empty.v1+json";
pub const MEDIA_TYPE_MARKDOWN: &str = "application/markdown";
pub const MEDIA_TYPE_PNG: &str = "image/png";
pub const MEDIA_TYPE_SVG: &str = "image/svg+xml";

pub const ACCEPTED_MANIFEST_MEDIA_TYPES: &[&str; 2] = &[MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_OCI_IMAGE_INDEX];

/// Every manifest format a registry may hold under a version: the OCI pair plus the Docker v2 manifest and manifest
/// list. A registry that enforces `Accept` (ghcr.io) answers 404 for a media type missing from it.
pub const ANY_MANIFEST_TYPES: &[&str] = &[
    MEDIA_TYPE_OCI_IMAGE_MANIFEST,
    MEDIA_TYPE_OCI_IMAGE_INDEX,
    "application/vnd.docker.distribution.manifest.v2+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];

/// Manifest media types a signer or verifier accepts when it fetches a referrer or a sidecar: the OCI image manifest
/// and the Docker v2 manifest, never an index.
pub const SIGNABLE_MANIFEST_TYPES: &[&str] = &[
    MEDIA_TYPE_OCI_IMAGE_MANIFEST,
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// Manifest media types sign, attest and verify accept when they fetch the subject: [`SIGNABLE_MANIFEST_TYPES`] plus
/// the OCI image index. A registry that enforces `Accept` (ghcr.io) answers 404 for an index digest without it.
pub const SUBJECT_MANIFEST_TYPES: &[&str] = &[
    MEDIA_TYPE_OCI_IMAGE_MANIFEST,
    MEDIA_TYPE_OCI_IMAGE_INDEX,
    "application/vnd.docker.distribution.manifest.v2+json",
];

/// The signable media type a subject manifest document carries, or `None` when the bytes are not a JSON object or
/// declare a `mediaType` outside [`SUBJECT_MANIFEST_TYPES`]. When the field is absent (OCI makes it optional) the shape
/// decides: `manifests` marks an image index, anything else an image manifest.
pub fn manifest_media_type(bytes: &[u8]) -> Option<String> {
    let document: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = document.as_object()?;
    if let Some(declared) = object.get("mediaType") {
        let declared = declared.as_str()?;
        return SUBJECT_MANIFEST_TYPES.contains(&declared).then(|| declared.to_string());
    }
    Some(if object.contains_key("manifests") {
        MEDIA_TYPE_OCI_IMAGE_INDEX.to_string()
    } else {
        MEDIA_TYPE_OCI_IMAGE_MANIFEST.to_string()
    })
}

/// Infers a package layer's media type from its archive file name, or `None` for an unrecognized extension.
pub fn media_type_from_filename(file_name: impl AsRef<str>) -> Option<&'static str> {
    let file_name = file_name.as_ref();
    if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
        Some(MEDIA_TYPE_TAR_GZ)
    } else if file_name.ends_with(".tar.xz") || file_name.ends_with(".txz") {
        Some(MEDIA_TYPE_TAR_XZ)
    } else if file_name.ends_with(".tar.zst") || file_name.ends_with(".tzst") || file_name.ends_with(".tar.zstd") {
        Some(MEDIA_TYPE_TAR_ZSTD)
    } else {
        None
    }
}

/// [`media_type_from_filename`] over a path's file name.
pub fn media_type_from_path(path: impl AsRef<std::path::Path>) -> Option<&'static str> {
    let path = path.as_ref();
    media_type_from_filename(path.file_name()?.to_str()?)
}

/// The compression a layer of this media type carries, or `None` when the type
/// names no layer OCX can decompress.
pub fn compression_for(media_type: impl AsRef<str>) -> Option<CompressionAlgorithm> {
    match media_type.as_ref() {
        MEDIA_TYPE_TAR_GZ => Some(CompressionAlgorithm::Gzip),
        MEDIA_TYPE_TAR_XZ => Some(CompressionAlgorithm::Lzma),
        MEDIA_TYPE_TAR_ZSTD => Some(CompressionAlgorithm::Zstd),
        _ => None,
    }
}

/// A media type on the wire was not one the caller accepts.
///
/// The acceptance suite reads its `Display`, so the message text is a contract.
#[derive(Debug, thiserror::Error)]
#[error("unsupported media type '{media_type}', expected media types are: {supported}", supported = .expected.join(", "))]
pub struct UnsupportedMediaType {
    pub media_type: String,
    pub expected: &'static [&'static str],
}

/// Returns `media_type` owned when it is one of `expected`.
///
/// # Errors
///
/// [`UnsupportedMediaType`] when it is not.
pub fn media_type_select<S: AsRef<str>>(
    media_type: &S,
    expected: &'static [&'static str],
) -> Result<String, UnsupportedMediaType> {
    let media_type = media_type.as_ref().to_string();
    if expected.contains(&media_type.as_str()) {
        Ok(media_type)
    } else {
        Err(UnsupportedMediaType { media_type, expected })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signable_manifest_types_are_the_oci_and_docker_v2_manifests() {
        assert_eq!(
            SIGNABLE_MANIFEST_TYPES,
            [
                "application/vnd.oci.image.manifest.v1+json",
                "application/vnd.docker.distribution.manifest.v2+json"
            ]
        );
    }

    #[test]
    fn subject_manifest_types_add_only_the_oci_index_to_the_signable_types() {
        let mut expected = SIGNABLE_MANIFEST_TYPES.to_vec();
        expected.insert(1, MEDIA_TYPE_OCI_IMAGE_INDEX);
        assert_eq!(SUBJECT_MANIFEST_TYPES, expected.as_slice());
    }

    #[test]
    fn manifest_media_type_reads_the_declared_type_then_the_shape() {
        assert_eq!(
            manifest_media_type(br#"{"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#)
                .as_deref(),
            Some(MEDIA_TYPE_OCI_IMAGE_INDEX)
        );
        assert_eq!(
            manifest_media_type(br#"{"manifests":[]}"#).as_deref(),
            Some(MEDIA_TYPE_OCI_IMAGE_INDEX)
        );
        assert_eq!(
            manifest_media_type(br#"{"layers":[]}"#).as_deref(),
            Some(MEDIA_TYPE_OCI_IMAGE_MANIFEST)
        );
    }

    #[test]
    fn manifest_media_type_refuses_a_subject_that_is_not_a_json_object() {
        assert_eq!(manifest_media_type(b"not json"), None);
        assert_eq!(manifest_media_type(b"[]"), None);
    }

    #[test]
    fn manifest_media_type_refuses_a_declared_type_outside_the_signable_list() {
        assert_eq!(
            manifest_media_type(br#"{"mediaType":"application/vnd.oci.image.layer.v1.tar","layers":[]}"#),
            None
        );
        assert_eq!(manifest_media_type(br#"{"mediaType":7}"#), None);
    }

    #[test]
    fn filename_infers_gzip_and_xz() {
        assert_eq!(media_type_from_filename("pkg.tar.gz"), Some(MEDIA_TYPE_TAR_GZ));
        assert_eq!(media_type_from_filename("pkg.tgz"), Some(MEDIA_TYPE_TAR_GZ));
        assert_eq!(media_type_from_filename("pkg.tar.xz"), Some(MEDIA_TYPE_TAR_XZ));
        assert_eq!(media_type_from_filename("pkg.txz"), Some(MEDIA_TYPE_TAR_XZ));
    }

    #[test]
    fn filename_infers_zstd() {
        assert_eq!(media_type_from_filename("pkg.tar.zst"), Some(MEDIA_TYPE_TAR_ZSTD));
        assert_eq!(media_type_from_filename("pkg.tzst"), Some(MEDIA_TYPE_TAR_ZSTD));
        assert_eq!(media_type_from_filename("pkg.tar.zstd"), Some(MEDIA_TYPE_TAR_ZSTD));
    }

    #[test]
    fn filename_rejects_unknown() {
        assert_eq!(media_type_from_filename("pkg.zip"), None);
        assert_eq!(media_type_from_filename("pkg.tar"), None);
    }

    #[test]
    fn compression_for_infers_zstd() {
        assert!(matches!(
            compression_for(MEDIA_TYPE_TAR_ZSTD),
            Some(CompressionAlgorithm::Zstd)
        ));
    }
}
