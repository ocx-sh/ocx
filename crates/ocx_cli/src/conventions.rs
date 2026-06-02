// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{
    ci::CiFlavor,
    cli::{MetadataResolutionError, UsageError},
    oci,
    package::metadata::env::entry::Entry,
    publisher::LayerRef,
    shell::Shell,
};

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
///
/// Delegates to [`oci::Platform::supported_set`] — the canonical source of truth.
pub fn supported_platforms() -> Vec<oci::Platform> {
    oci::Platform::supported_set()
}

pub fn platforms_or_default(platforms: &[oci::Platform]) -> Vec<oci::Platform> {
    if platforms.is_empty() {
        supported_platforms()
    } else {
        platforms.to_vec()
    }
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
                    "could not autodetect shell from $SHELL or parent process; \
                     pass --shell=NAME explicitly. \
                     Legal values: bash, zsh, fish, ash, dash, ksh, sh, \
                     pwsh, elvish, nushell, batch (sh == dash POSIX alias)",
                )
            })?;
            Ok(Some(s))
        }
    }
}

/// Resolve a `--ci` clap argument to an explicit [`CiFlavor`], or `None` when
/// the flag is absent and the caller should take the non-CI path.
///
/// `--ci` is declared as `Option<Option<CiFlavor>>` with
/// `num_args=0..=1, require_equals=true` (mirroring `--shell`):
///
/// - `None` (flag absent) → `Ok(None)`: caller uses the structured-report /
///   `--shell` path.
/// - `Some(None)` (bare `--ci`) → autodetect from CI env vars
///   (`$GITHUB_ACTIONS`, `$GITLAB_CI`); a [`UsageError`] (exit 64) when no
///   provider is detected.
/// - `Some(Some(provider))` (explicit `--ci=NAME`) → `Ok(Some(provider))`.
///
/// Shared by `ocx env` and `ocx package env` so the bare-`--ci` autodetect and
/// its identical undetectable-provider `UsageError` exist exactly once.
pub fn resolve_ci_arg(ci: Option<Option<CiFlavor>>) -> anyhow::Result<Option<CiFlavor>> {
    match ci {
        None => Ok(None),
        Some(Some(provider)) => Ok(Some(provider)),
        Some(None) => {
            let provider = CiFlavor::detect()
                .ok_or_else(|| UsageError::new("could not autodetect CI provider; pass --ci=github or --ci=gitlab"))?;
            Ok(Some(provider))
        }
    }
}

/// Export resolved env entries into a CI system's persistence channel.
///
/// Shared by `ocx env` and `ocx package env`. Rejects `--export-file` for
/// GitHub Actions (which infers its two-file sink from `$GITHUB_ENV` /
/// `$GITHUB_PATH`); GitLab uses `export_file` as its output path, falling back
/// to stdout when `None`.
pub fn export_ci(provider: CiFlavor, export_file: Option<std::path::PathBuf>, entries: &[Entry]) -> anyhow::Result<()> {
    if provider == CiFlavor::GitHubActions && export_file.is_some() {
        return Err(UsageError::new(
            "--export-file is not supported with --ci=github; GitHub infers $GITHUB_ENV/$GITHUB_PATH",
        )
        .into());
    }
    provider.export(entries, export_file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{export_ci, resolve_ci_arg, resolve_shell_arg};
    use ocx_lib::ci::CiFlavor;
    use ocx_lib::cli::UsageError;
    use ocx_lib::package::metadata::env::{entry::Entry, modifier::ModifierKind};
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

    #[test]
    fn ci_arg_absent_is_none() {
        assert!(resolve_ci_arg(None).expect("absent is ok").is_none());
    }

    #[test]
    fn ci_arg_explicit_is_passed_through() {
        // Both providers pass through deterministically (no env reads). The
        // bare-`--ci` autodetect branch reads real CI env vars and is exercised
        // by the acceptance suite, not here (cf. `resolve_shell_arg`).
        assert_eq!(
            resolve_ci_arg(Some(Some(CiFlavor::GitHubActions))).expect("explicit is ok"),
            Some(CiFlavor::GitHubActions)
        );
        assert_eq!(
            resolve_ci_arg(Some(Some(CiFlavor::GitLab))).expect("explicit is ok"),
            Some(CiFlavor::GitLab)
        );
    }

    #[test]
    fn export_ci_github_rejects_export_file() {
        let result = export_ci(
            CiFlavor::GitHubActions,
            Some(std::path::PathBuf::from("/tmp/whatever")),
            &[],
        );
        let error = result.expect_err("github + --export-file must be rejected");
        assert!(
            error.downcast_ref::<UsageError>().is_some(),
            "rejection must be a UsageError (exit 64), got: {error:#}"
        );
    }

    #[test]
    fn export_ci_gitlab_writes_json_lines_to_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let export = tmp.path().join("export.env");
        let entries = vec![Entry {
            key: "JAVA_HOME".to_string(),
            value: "/pkg/java".to_string(),
            kind: ModifierKind::Constant,
        }];

        export_ci(CiFlavor::GitLab, Some(export.clone()), &entries).expect("gitlab export ok");

        let content = std::fs::read_to_string(&export).expect("read export");
        assert_eq!(content, "{\"name\":\"JAVA_HOME\",\"value\":\"/pkg/java\"}\n");
    }
}
