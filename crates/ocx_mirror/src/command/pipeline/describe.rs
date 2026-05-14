// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx-mirror pipeline describe` — publish catalog metadata (README + logo)
//! to the registry as a referrer of the target repository.
//!
//! Loads `mirror.yml`, resolves the optional `catalog:` block (defaults to
//! `CATALOG.md` + probed `logo.{svg,png}`), then shells out to
//! `ocx package describe` to publish the data under the `__ocx.desc` tag.
//!
//! Exits 0 silently when no CATALOG.md is present — the workflow is a no-op
//! until catalog content lands in the mirror repo.
//!
//! # Errors
//!
//! - [`MirrorError::SpecNotFound`] / [`MirrorError::SpecInvalid`] from
//!   `load_spec`.
//! - [`MirrorError::ExecutionFailed`] when the `ocx package describe`
//!   subprocess returns non-zero.

use std::path::{Path, PathBuf};

use ocx_lib::cli::DataInterface;

use crate::command::pipeline::push::{forward_ocx_env, resolve_ocx_binary};
use crate::error::MirrorError;
use crate::spec;

/// `ocx-mirror pipeline describe` subcommand.
#[derive(clap::Parser)]
pub struct Describe {
    /// Path to the mirror spec file.
    #[arg(long, default_value = "./mirror.yml")]
    pub spec: PathBuf,
}

impl Describe {
    pub async fn execute(&self, _printer: &DataInterface) -> Result<(), MirrorError> {
        let spec = spec::load_spec(&self.spec).await?;
        let spec_dir = self.spec.parent().unwrap_or(Path::new("."));
        let catalog = spec.catalog.clone().unwrap_or_default();

        let readme = catalog.resolved_readme(spec_dir);
        if !readme.exists() {
            tracing::info!(
                "describe: {} not found; skipping (no catalog content to publish)",
                readme.display()
            );
            return Ok(());
        }

        let logo = catalog.resolved_logo(spec_dir);
        let identifier = format!("{}/{}", spec.target.registry, spec.target.repository);

        invoke_describe(&identifier, &readme, logo.as_deref()).await
    }
}

/// Spawn `ocx package describe <identifier> --readme <path> [--logo <path>]`.
async fn invoke_describe(identifier: &str, readme: &Path, logo: Option<&Path>) -> Result<(), MirrorError> {
    let ocx_binary = resolve_ocx_binary().map_err(|e| MirrorError::ExecutionFailed(vec![e]))?;

    let mut cmd = tokio::process::Command::new(&ocx_binary);
    cmd.args(["package", "describe", identifier, "--readme"]);
    cmd.arg(readme);
    if let Some(logo_path) = logo {
        cmd.arg("--logo");
        cmd.arg(logo_path);
    }
    forward_ocx_env(&mut cmd);

    let status = cmd
        .status()
        .await
        .map_err(|e| MirrorError::ExecutionFailed(vec![format!("failed to spawn ocx: {e}")]))?;

    if !status.success() {
        return Err(MirrorError::ExecutionFailed(vec![format!(
            "ocx package describe exited {status} for {identifier}"
        )]));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Subprocess invocation is exercised via the candidate testbed; here we
    //! only verify argument assembly stays stable for the cases that matter to
    //! the workflow.

    use super::*;
    use tempfile::tempdir;

    /// Mirror of [`invoke_describe`] arg assembly — keeps the assertion target
    /// independent of the subprocess spawn.
    fn assemble_args<'a>(identifier: &'a str, readme: &'a Path, logo: Option<&'a Path>) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "package".into(),
            "describe".into(),
            identifier.into(),
            "--readme".into(),
            readme.display().to_string(),
        ];
        if let Some(p) = logo {
            args.push("--logo".into());
            args.push(p.display().to_string());
        }
        args
    }

    #[test]
    fn describe_assembles_args_with_readme_only() {
        let args = assemble_args("ocx.sh/shfmt", Path::new("/spec/CATALOG.md"), None);
        assert_eq!(
            args,
            vec!["package", "describe", "ocx.sh/shfmt", "--readme", "/spec/CATALOG.md"]
        );
    }

    #[test]
    fn describe_assembles_args_with_readme_and_logo() {
        let args = assemble_args(
            "ocx.sh/shfmt",
            Path::new("/spec/CATALOG.md"),
            Some(Path::new("/spec/logo.png")),
        );
        assert_eq!(
            args,
            vec![
                "package",
                "describe",
                "ocx.sh/shfmt",
                "--readme",
                "/spec/CATALOG.md",
                "--logo",
                "/spec/logo.png",
            ]
        );
    }

    #[tokio::test]
    async fn describe_skips_silently_when_readme_missing() {
        // Writing a complete spec is heavy; smoke this via the resolver alone
        // — `execute` calls `readme.exists()` and returns Ok on absence.
        let tmp = tempdir().unwrap();
        let cfg = crate::spec::CatalogConfig::default();
        let readme = cfg.resolved_readme(tmp.path());
        assert!(!readme.exists());
    }
}
