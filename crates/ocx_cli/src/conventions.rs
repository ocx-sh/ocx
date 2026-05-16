// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{
    cli::{MetadataResolutionError, UsageError},
    oci,
    package::install_info::InstallInfo,
    package::metadata::env::entry::Entry,
    package_manager::PackageManager,
    publisher::LayerRef,
    shell::Shell,
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

/// Emit shell-sourceable export lines for a slice of env entries.
///
/// This is the single shared emit helper consumed by:
/// - `ocx env` (toolchain-tier, new Phase 2 command)
/// - `ocx package env` (OCI-tier, delegates here for `--shell` output)
/// - `ocx direnv export` (delegates here instead of inlining the loop)
///
/// Wraps [`Shell::export_path`] / [`Shell::export_constant`] and skips entries
/// whose key fails POSIX validation (emitting a `# ocx:` note to stderr so the
/// caller is informed without aborting the full output).
///
/// `Shell::Bash` is the fixed shell for `direnv export` (direnv always evals
/// `.envrc` in a bash sub-shell — no `--shell` flag on that command). For
/// `ocx env` / `ocx package env` the caller passes the user-selected shell.
///
/// # Panics
///
/// This function is infallible — `None` from `export_path` / `export_constant`
/// is handled by a stderr note.
pub fn emit_lines(shell: Shell, entries: &[Entry]) {
    use ocx_lib::package::metadata::env::modifier::ModifierKind;
    for entry in entries {
        let line = match entry.kind {
            ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
            ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
        };
        match line {
            Some(line) => println!("{line}"),
            None => eprintln!("# ocx: skipping invalid env-var key {:?}", entry.key),
        }
    }
}

/// Resolve a `--shell` clap argument to an explicit [`Shell`], or `None`
/// when the default-format (JSON / `--format plain`) path should be taken.
///
/// `--shell` is declared as `Option<Option<Shell>>` with
/// `num_args=0..=1, require_equals=true` (clap 4.x produces `Some(None)` for
/// a bare `--shell`, `Some(Some(s))` for `--shell=NAME`, `None` when absent —
/// `require_equals` keeps a following positional from being swallowed):
///
/// - `None` (flag absent) → `Ok(None)`: caller uses the default-format path.
/// - `Some(None)` (bare `--shell`) → autodetect from `$SHELL`/parent; a
///   [`UsageError`] (exit 64) when undetectable.
/// - `Some(Some(s))` (explicit `--shell=NAME`) → `Ok(Some(s))`.
///
/// Shared by `ocx env` and `ocx package env` so the bare-shell autodetect and
/// its identical undetectable-shell `UsageError` exist exactly once.
pub fn resolve_shell_arg(shell: Option<Option<Shell>>) -> anyhow::Result<Option<Shell>> {
    match shell {
        None => Ok(None),
        Some(Some(s)) => Ok(Some(s)),
        Some(None) => {
            let s = Shell::detect().ok_or_else(|| {
                UsageError::new(
                    "bare --shell requires a detectable $SHELL or parent process; \
                     use --shell=bash (or another explicit shell name)",
                )
            })?;
            Ok(Some(s))
        }
    }
}
#[cfg(test)]
mod tests {
    use super::resolve_shell_arg;
    use ocx_lib::shell::Shell;

    #[test]
    fn shell_arg_absent_is_default_format() {
        assert!(resolve_shell_arg(None).expect("absent is ok").is_none());
    }

    #[test]
    fn shell_arg_explicit_is_passed_through() {
        let resolved = resolve_shell_arg(Some(Some(Shell::Bash))).expect("explicit is ok");
        assert!(matches!(resolved, Some(Shell::Bash)));
    }
}
