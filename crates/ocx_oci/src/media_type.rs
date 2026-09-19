// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_util::compression::CompressionAlgorithm;

/// The media type for an OCI image index manifest.
pub const MEDIA_TYPE_OCI_IMAGE_INDEX: &str = crate::OCI_IMAGE_INDEX_MEDIA_TYPE;
/// The media type for an OCI image manifest.
pub const MEDIA_TYPE_OCI_IMAGE_MANIFEST: &str = crate::OCI_IMAGE_MEDIA_TYPE;
/// The media type of a ocx package, which is the artifact type of the corresponding oci index entry.
pub const MEDIA_TYPE_PACKAGE_V1: &str = "application/vnd.sh.ocx.package.v1";
/// The media type of a manifest containing the package metadata.
pub const MEDIA_TYPE_PACKAGE_METADATA_V1: &str = "application/vnd.sh.ocx.package.v1+json";
/// The media type of a layer containing a tarball of the package contents, compressed with gzip.
pub const MEDIA_TYPE_TAR_GZ: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
/// The media type of a layer containing a tarball of the package contents, compressed with xz.
pub const MEDIA_TYPE_TAR_XZ: &str = "application/vnd.oci.image.layer.v1.tar+xz";
/// The media type of a layer containing a tarball of the package contents, compressed with zstd.
pub const MEDIA_TYPE_TAR_ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
/// The artifact type for a description manifest (README + optional logo).
pub const MEDIA_TYPE_DESCRIPTION_V1: &str = "application/vnd.sh.ocx.description.v1";
/// The OCI empty config media type, per the OCI image spec.
pub const MEDIA_TYPE_OCI_EMPTY_CONFIG: &str = "application/vnd.oci.empty.v1+json";
/// Markdown content media type.
pub const MEDIA_TYPE_MARKDOWN: &str = "application/markdown";
/// PNG image media type.
pub const MEDIA_TYPE_PNG: &str = "image/png";
/// SVG image media type.
pub const MEDIA_TYPE_SVG: &str = "image/svg+xml";

pub const ACCEPTED_MANIFEST_MEDIA_TYPES: &[&str; 2] = &[MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_OCI_IMAGE_INDEX];

/// Infers the media type of a package layer from the file name of the archive.
/// Currently supports .tar.gz, .tgz, .tar.xz, .txz, .tar.zst, .tzst and .tar.zstd extensions.
/// Returns None if the file extension is not recognized.
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

/// Infers the media type of a package layer from the file name of the archive.
/// For more details, see `media_type_from_filename`.
pub fn media_type_from_path(path: impl AsRef<std::path::Path>) -> Option<&'static str> {
    let path = path.as_ref();
    media_type_from_filename(path.file_name()?.to_str()?)
}

/// The compression a layer of this media type carries, or `None` when the type
/// names no layer OCX can decompress.
///
/// Lives beside the media-type constants it matches on, not on
/// [`CompressionAlgorithm`]: the mapping is a property of the OCI layer
/// vocabulary, and hanging it off the codec enum made the compression module —
/// which knows nothing else about OCI — the owner of three registry wire
/// strings.
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
/// The one way [`media_type_select`] can fail, so it is the whole of that
/// function's error type rather than an arm of a wider one. Its `Display` is
/// byte-identical to the wide variant it is carried by, which is what the
/// acceptance suite reads.
#[derive(Debug, thiserror::Error)]
#[error("unsupported media type '{media_type}', expected media types are: {supported}", supported = .expected.join(", "))]
pub struct UnsupportedMediaType {
    /// The media type that was offered.
    pub media_type: String,
    /// The media types the caller would have accepted.
    pub expected: &'static [&'static str],
}

/// Validates that the given media type is one of the expected media types, and returns it as a String.
/// If the media type is not one of the expected media types, returns an UnsupportedMediaType error.
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
