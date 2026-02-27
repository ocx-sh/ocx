use crate::{Error, Result, oci};

/// The media type for an OCI image index manifest.
pub const MEDIA_TYPE_OCI_IMAGE_INDEX: &str = oci::OCI_IMAGE_INDEX_MEDIA_TYPE;
/// The media type for an OCI image manifest.
pub const MEDIA_TYPE_OCI_IMAGE_MANIFEST: &str = oci::OCI_IMAGE_MEDIA_TYPE;
/// The media type of a ocx package, which is the artifact type of the corresponding oci index entry.
pub const MEDIA_TYPE_PACKAGE_V1: &str = "application/vnd.sh.ocx.package.v1";
/// The media type of a manifest containing the package metadata.
pub const MEDIA_TYPE_PACKAGE_METADATA_V1: &str = "application/vnd.sh.ocx.package.v1+json";
/// The media type of a layer containing a tarball of the package contents, compressed with gzip.
pub const MEDIA_TYPE_TAR_GZ: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
/// The media type of a layer containing a tarball of the package contents, compressed with xz.
pub const MEDIA_TYPE_TAR_XZ: &str = "application/vnd.oci.image.layer.v1.tar+xz";

pub const ACCEPTED_MANIFEST_MEDIA_TYPES: &[&str; 2] = &[MEDIA_TYPE_OCI_IMAGE_MANIFEST, MEDIA_TYPE_OCI_IMAGE_INDEX];

/// Infers the media type of a package layer from the file name of the archive.
/// Currently supports .tar.gz, .tgz, .tar.xz and .txz extensions.
/// Returns None if the file extension is not recognized.
pub fn media_type_from_filename(file_name: impl AsRef<str>) -> Option<&'static str> {
    let file_name = file_name.as_ref();
    if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
        Some(MEDIA_TYPE_TAR_GZ)
    } else if file_name.ends_with(".tar.xz") || file_name.ends_with(".txz") {
        Some(MEDIA_TYPE_TAR_XZ)
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

pub fn media_type_file_ext(media_type: impl AsRef<str>) -> Option<&'static str> {
    match media_type.as_ref() {
        MEDIA_TYPE_TAR_GZ => Some("tar.gz"),
        MEDIA_TYPE_TAR_XZ => Some("tar.xz"),
        _ => None,
    }
}

/// Validates that the given media type is one of the expected media types, and returns it as a String.
/// If the media type is not one of the expected media types, returns an UnsupportedMediaType error.
pub fn media_type_select_some<S: AsRef<str>>(
    media_type: &Option<S>,
    expected: &'static [&'static str],
) -> Result<String> {
    match media_type.as_ref().map(|s| s.as_ref().to_string()) {
        Some(media_type) if expected.contains(&media_type.as_str()) => Ok(media_type),
        Some(media_type) => Err(Error::UnsupportedMediaType(media_type, expected)),
        None => Err(Error::UnsupportedMediaType("<none>".to_string(), expected)),
    }
}

/// Validates that the given media type is one of the expected media types, and returns it as a String.
/// If the media type is not one of the expected media types, returns an UnsupportedMediaType error.
pub fn media_type_select<S: AsRef<str>>(media_type: &S, expected: &'static [&'static str]) -> Result<String> {
    let media_type = media_type.as_ref().to_string();
    if expected.contains(&media_type.as_str()) {
        Ok(media_type)
    } else {
        Err(Error::UnsupportedMediaType(media_type, expected))
    }
}
