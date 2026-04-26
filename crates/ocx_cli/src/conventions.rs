// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{oci, package::install_info::InstallInfo, package_manager::PackageManager};

use crate::options;

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
        if !ocx_lib::env::pathext_includes_launcher(&current_pathext) {
            tracing::warn!(
                "entrypoint launchers require .cmd in PATHEXT; current PATHEXT lacks it. \
                 add \".CMD\" to PATHEXT for entrypoints to launch correctly outside ocx exec."
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
