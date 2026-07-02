// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    env,
    package_manager::collect_applied,
    project::{MissingState, load_project_state},
    shell,
};

use crate::conventions::{emit_lines, supported_platforms};

/// Prints stateless shell export statements for the project toolchain.
///
/// Reads the nearest project `ocx.toml` (project tier only — no home-tier
/// fallback in this phase), loads the matching `ocx.lock`, looks up each
/// default-group tool in the local object store, and prints bash export
/// lines for the resolved environment. The command is stateless: it does
/// not consult or update `_OCX_APPLIED`, making it suitable for use from
/// `direnv`'s `.envrc` via `eval "$(ocx direnv export)"`.
///
/// Output is always bash. `direnv` evaluates `.envrc` files in a bash
/// sub-shell regardless of the user's interactive shell; translation to
/// the interactive shell happens later, inside direnv, via `direnv export
/// <shell>`. Programs invoked via `eval` from `.envrc` therefore must emit
/// bash. There is no `--shell` flag on this command for the same reason.
///
/// The command never contacts the network and never installs or mutates
/// filesystem state. Tools missing from the object store produce a one-line
/// stderr note and are skipped; a stale lock produces a stderr warning but
/// the stale digests are still used. When no project `ocx.toml` is found,
/// the command exits 0 with no output.
#[derive(Parser)]
pub struct DirenvExport {}

impl DirenvExport {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let shell = shell::Shell::Bash;

        // Project tier ONLY in Phase 7 — Phase 9 will add home-tier
        // fallback. The OCX_NO_PROJECT=1 kill switch is honored by
        // `load_project_state` via `ProjectConfig::resolve`.
        let cwd = env::current_dir()?;
        let project = match load_project_state(&cwd, context.project_path()).await? {
            Ok(state) => state,
            Err(MissingState::NoProject) => {
                // No `ocx.toml` in scope → emit nothing, exit 0. Matches
                // direnv's expectation: a directory without project config
                // simply does not contribute to the shell environment.
                return Ok(ExitCode::SUCCESS);
            }
            Err(MissingState::LockMissing { lock_path }) => {
                // Missing lock is NOT an error here (unlike `ocx exec` /
                // `ocx pull`). The shell-hook fires on every prompt;
                // failing on a missing lock would render the user's
                // terminal unusable when they freshly clone a project.
                eprintln!(
                    "# ocx: ocx.lock not found at {}; run `ocx lock` to fetch",
                    lock_path.display()
                );
                return Ok(ExitCode::SUCCESS);
            }
        };

        // Stale-lock policy diverges from `ocx exec` (which exits 65) —
        // shell-hook warns but continues using the stale digests so the
        // interactive shell stays usable until the user re-locks.
        if project.stale {
            eprintln!("# ocx: ocx.lock is stale (ocx.toml changed since last `ocx lock`); using stale digests");
        }

        // Architect boundary: hook MUST NOT contact the registry, regardless
        // of `--remote`. We force an offline-only `PackageManager` clone so
        // any incidental index lookup (e.g. via `resolve` to walk the cached
        // manifest chain) cannot escape into the network.
        let manager = context.manager().offline_view(context.local_index().clone());
        let platforms = supported_platforms();
        let applied = collect_applied(&manager, &project.lock, &platforms).await?;
        for name in &applied.missing {
            eprintln!("# ocx: {name} not installed; run `ocx pull` to fetch");
        }

        let scope = ocx_lib::package_manager::PatchScope::Project(project.config.no_patches_repositories());
        let (entries, _, _) = manager
            .resolve_env_with_patch_boundary(&applied.infos, false, scope)
            .await?;
        // Delegate to the shared emit helper (C5 / conventions.rs).
        // `Shell::Bash` is fixed: direnv always evaluates `.envrc` in a bash
        // sub-shell regardless of the user's interactive shell.  There is no
        // `--shell` flag on `direnv export` for this reason.
        emit_lines(shell, &entries);

        Ok(ExitCode::SUCCESS)
    }
}
