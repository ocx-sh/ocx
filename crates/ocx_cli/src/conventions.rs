// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{
    cli::MetadataResolutionError, oci, package::install_info::InstallInfo, package_manager::PackageManager,
    publisher::LayerRef,
};

use crate::options;

/// Infers a metadata file path based on the archive file path.
/// For example, if the content path is `/path/to/package.tar.gz`, this function will return `/path/to/package-metadata.json`.
pub fn infer_metadata_file(content: &std::path::Path) -> Result<std::path::PathBuf, MetadataResolutionError> {
    let content_parent = content
        .parent()
        .ok_or_else(|| MetadataResolutionError::InvalidLayerPath {
            layer: content.to_path_buf(),
            reason: "no parent directory".into(),
        })?;
    let mut content_name = content
        .file_stem()
        .ok_or_else(|| MetadataResolutionError::InvalidLayerPath {
            layer: content.to_path_buf(),
            reason: "no file stem".into(),
        })?
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

/// Resolves the metadata path used by `ocx package push` and `ocx package
/// test`.
///
/// When `explicit` is `Some`, it wins. Otherwise the helper walks the file
/// layers, infers a candidate metadata path for each, and dedups: zero file
/// layers → [`MetadataResolutionError::Required`], multiple distinct
/// candidates → [`MetadataResolutionError::Ambiguous`].
pub fn resolve_metadata_path(
    layers: &[LayerRef],
    explicit: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, MetadataResolutionError> {
    if let Some(p) = explicit {
        return Ok(p.to_path_buf());
    }
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for layer in layers {
        if let LayerRef::File(file) = layer {
            let candidate = infer_metadata_file(file)?;
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }
    match candidates.len() {
        0 => Err(MetadataResolutionError::Required),
        1 => Ok(candidates.into_iter().next().unwrap()),
        _ => Err(MetadataResolutionError::Ambiguous { candidates }),
    }
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

pub fn platforms_or_default(platforms: &[oci::Platform]) -> Vec<oci::Platform> {
    if platforms.is_empty() {
        supported_platforms()
    } else {
        platforms.to_vec()
    }
}

/// Emits a `tracing::warn!` on Windows when the current process `PATHEXT`
/// does not include the OCX launcher extension (`.cmd`).
///
/// Called at the start of commands whose env output is **consumed outside
/// `ocx exec`**: `install`, `select`, `shell env`, `ci export`,
/// `shell profile load`. Those commands emit paths that include `.cmd`
/// launchers in `entrypoints/`; if `PATHEXT` lacks `.cmd` the launchers will
/// not be found by the parent shell.
///
/// The warning fires per command invocation regardless of whether the packages
/// involved actually declare entrypoints — inspecting metadata at this point
/// would require an extra async round-trip and is intentionally avoided.
///
/// On non-Windows platforms this function is a no-op.
pub fn warn_if_pathext_missing_launcher() {
    #[cfg(target_os = "windows")]
    {
        let current_pathext = std::env::var("PATHEXT").unwrap_or_default();
        if !ocx_lib::package_manager::launcher::includes_launcher(&current_pathext) {
            tracing::warn!(
                "entrypoint launchers require {ext} in PATHEXT (case-insensitive); \
                 current PATHEXT lacks it. add \"{ext}\" to PATHEXT for entrypoints \
                 to launch correctly outside ocx exec.",
                ext = ocx_lib::package_manager::launcher::LAUNCHER_EXT,
            );
        }
    }
}

/// Resolves packages using either symlink-based or platform-based lookup.
///
/// When `content_path` specifies a symlink kind (candidate/current), packages
/// are resolved via `find_symlink_all`. Otherwise falls back to `find_all`
/// with platform matching.
pub async fn resolve_packages(
    packages: impl IntoIterator<Item = options::Identifier>,
    platforms: &[oci::Platform],
    content_path: &options::ContentPath,
    manager: &PackageManager,
    default_registry: &str,
) -> anyhow::Result<Vec<InstallInfo>> {
    let platforms = platforms_or_default(platforms);
    let identifiers = options::Identifier::transform_all(packages, default_registry)?;

    let package_infos = if let Some(kind) = content_path.symlink_kind() {
        manager.find_symlink_all(identifiers, kind).await?
    } else {
        manager.find_all(identifiers, platforms).await?
    };
    Ok(package_infos)
}
