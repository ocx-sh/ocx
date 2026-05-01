// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `ocx launcher exec` subcommand — stable entry-point from generated launchers.
//!
//! Generated launcher scripts call:
//!   `ocx launcher exec '<pkg-root>' -- "$(basename "$0")" "$@"`
//!
//! This subcommand is the sole path from an installed launcher into the OCX runtime.
//! It hides all presentation flags, self-view selection, and binary pinning behind
//! the stable `launcher exec` name pair, reducing the launcher ABI surface from
//! 8 wire commitments to 2 (the `launcher` + `exec` subcommand names and positional shape).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli::UsageError;
use ocx_lib::package::metadata::env::entry::Entry as EnvEntry;
use ocx_lib::package_manager::launcher;
use ocx_lib::utility::child_process;
use ocx_lib::{env, env::OcxConfigView};

/// Entry point from generated launchers. Validates the package root, then
/// executes the resolved entrypoint with forced self-view and silent presentation.
#[derive(Parser)]
pub struct LauncherExec {
    /// Absolute path to the installed package root (the directory containing
    /// `metadata.json`). Baked into the launcher at install time.
    pkg_root: PathBuf,

    /// The launcher's own filename (argv0 passed after `--`), used to
    /// identify which entrypoint to dispatch.
    #[clap(last = true, required = true, num_args = 1..)]
    argv: Vec<String>,
}

impl LauncherExec {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let packages_root = context.file_structure().packages.root();
        let manager = context.manager();

        // Validate: pkg_root must be absolute, under $OCX_HOME/packages/, and
        // contain metadata.json. Errors surface as UsageError (exit 64).
        let validated = validate_launcher_pkg_root(&self.pkg_root, packages_root).await?;

        // Resolve env with self_view=true — the launcher always runs in the
        // package's own env (public + private surface). This is equivalent to
        // the former `--self` flag that was baked into every launcher template.
        let info = manager.install_info_from_package_root(&validated).await?;
        let entries = manager.resolve_env(&[std::sync::Arc::new(info)], true).await?;

        // argv[0] is the launcher's own filename — the entrypoint name.
        // argv[1..] are the user args.
        let (argv0, args) = self
            .argv
            .split_first()
            .expect("clap required=true guarantees at least one argv element");

        self.run_with_env(entries, args, argv0, context.config_view()).await
    }

    /// Run the resolved entrypoint with the given env.
    ///
    /// Presentation flags are forced here (not baked in the launcher template):
    /// - log_level=off, color=never, format=plain were previously baked into the
    ///   launcher script; now they are applied on the *inner* ocx invocation from
    ///   within this subcommand (i.e. if this subcommand itself spawns child ocx,
    ///   which it does not — it execs the entrypoint binary directly).
    ///
    /// `child_process::exec` diverges on success on every platform — Unix
    /// `execvp(2)`s, Windows spawns + waits + `process::exit`s — so this
    /// function only returns when start-up itself fails.
    async fn run_with_env(
        &self,
        entries: Vec<EnvEntry>,
        args: &[String],
        command: &str,
        config_view: &OcxConfigView,
    ) -> anyhow::Result<ExitCode> {
        let mut process_env = env::Env::new();
        process_env.apply_entries(&entries);
        // Forward resolution-affecting OCX config to any grandchild ocx processes.
        process_env.apply_ocx_config(config_view);
        // Ensure the child PATHEXT lists the OCX launcher extension on Windows.
        launcher::emplace_pathext(&mut process_env);

        let resolved = process_env.resolve_command(command);

        let err = child_process::exec(&resolved, args, process_env);
        Err(anyhow::Error::from(err).context(format!("failed to run '{}'", resolved.display())))
    }
}

/// Validate a package root path for use from a launcher.
///
/// The path must:
/// - Be absolute
/// - Canonicalize to a location inside `packages_root`
/// - Contain `metadata.json`
///
/// This mirrors the former `validate_package_root` from `options/package_ref.rs`,
/// now inlined here (its only remaining caller) with error messages updated to
/// reference `launcher exec` instead of `file://`.
async fn validate_launcher_pkg_root(
    dir: &std::path::Path,
    packages_root: &std::path::Path,
) -> Result<PathBuf, UsageError> {
    if !dir.is_absolute() {
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root must be absolute, got '{}'",
            dir.display()
        )));
    }

    // Canonicalize both sides so symlinks and `..` components cannot smuggle
    // a path outside the packages root.
    let canonical_dir = tokio::fs::canonicalize(dir).await.map_err(|e| {
        UsageError::new(format!(
            "launcher exec: pkg-root '{}' cannot be resolved: {e}",
            dir.display()
        ))
    })?;
    let canonical_root = tokio::fs::canonicalize(packages_root).await.map_err(|e| {
        UsageError::new(format!(
            "launcher exec: cannot resolve packages root ({e}): {}",
            packages_root.display()
        ))
    })?;

    if !canonical_dir.starts_with(&canonical_root) {
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root must point inside {} (got {})",
            canonical_root.display(),
            canonical_dir.display()
        )));
    }

    // Existence check on metadata.json — canonical signal that this is a package root.
    let metadata = canonical_dir.join("metadata.json");
    if !tokio::fs::try_exists(&metadata).await.unwrap_or(false) {
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root is not a package root (missing metadata.json): {}",
            canonical_dir.display()
        )));
    }

    Ok(canonical_dir)
}
