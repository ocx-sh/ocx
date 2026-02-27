use ocx_lib::oci;

/// Infers a metadata file path based on the archive file path.
/// For example, if the content path is `/path/to/package.tar.gz`, this function will return `/path/to/package-metadata.json`.
pub fn infer_metadata_file(content: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let content_parent = content
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid content path."))?;
    let mut content_name = content
        .file_stem()
        .ok_or_else(|| anyhow::anyhow!("Invalid content path."))?
        .to_string_lossy()
        .to_string();
    let known_archive_extensions = [".tar", ".tar.gz", ".tgz", ".zip"];
    for extension in known_archive_extensions {
        if content_name.ends_with(extension) {
            content_name.truncate(content_name.len() - extension.len());
            break;
        }
    }
    Ok(content_parent.join(format!("{}-metadata.json", content_name)))
}

/// List of platforms supported by the current system.
/// This is used as default for package installation, but can be overridden by the user.
pub fn supported_platforms() -> Vec<oci::Platform> {
    let mut platforms = Vec::new();
    if let Some(platform) = oci::Platform::current() {
        platforms.push(platform);
    }
    platforms.push(oci::Platform::any());
    platforms
}

pub fn platforms_or_default(platforms: &Vec<oci::Platform>) -> Vec<oci::Platform> {
    if platforms.is_empty() {
        supported_platforms()
    } else {
        platforms.clone()
    }
}
