// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    env, oci,
    package_manager::collect_applied,
    project::{DEFAULT_GROUP, MissingState, host_leaf_identifier, load_project_state},
    shell,
};

use crate::conventions::emit_lines;
use crate::options;

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
/// By default a tool missing from the object store is materialised before
/// exporting: a tool already present resolves locally with no network (its
/// lock-pinned digest is content-addressed — nothing to look up), so only a
/// genuine miss falls through to the registry. Pass `--no-pull` to keep the
/// command strictly offline — missing tools then produce a one-line stderr
/// note and are skipped. Either way a stale lock produces a stderr warning but
/// the stale digests are still used, a missing tool never fails the prompt,
/// and when no project `ocx.toml` is found the command exits 0 with no output.
/// The pull fallback is also skipped whenever no registry is reachable
/// (`--offline` / no configured remote), so an offline shell never blocks.
#[derive(Parser)]
pub struct DirenvExport {
    #[clap(flatten)]
    pull: options::Pull,
}

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

        // Probe the local object store first through an offline `PackageManager`
        // clone: any incidental index lookup (V1 legacy locks walk the cached
        // index->manifest chain; V2 locks read the pinned leaf directly) stays
        // local, so a present tool resolves with no registry contact and a
        // not-materialised tool buckets into `missing`.
        let offline = context.manager().offline_view(context.local_index().clone());
        let platform = oci::Platform::current().unwrap_or_else(oci::Platform::any);
        let mut applied = collect_applied(&offline, &project.lock, &platform).await?;

        // Default: materialise anything the store is missing, then re-probe so
        // the freshly-pulled tools join the export. `--no-pull` opts out and the
        // command stays strictly offline. The pull is also skipped when no
        // registry is reachable (`--offline` / no remote) so an offline shell
        // never blocks. A tool that stays unresolvable (no host leaf, or a pull
        // that did not produce it) survives the re-probe and is warned + omitted
        // below — a missing tool must never fail the prompt.
        if self.pull.enabled(true) && !applied.missing.is_empty() && !context.manager().is_offline() {
            let missing: std::collections::HashSet<&str> = applied.missing.iter().map(String::as_str).collect();
            let to_install: Vec<oci::Identifier> = project
                .lock
                .tools
                .iter()
                .filter(|tool| tool.group == DEFAULT_GROUP && missing.contains(tool.name.as_str()))
                .filter_map(|tool| host_leaf_identifier(tool, &platform).ok())
                .collect();
            if !to_install.is_empty() {
                // Best-effort: a per-prompt hook must never fail on a transient
                // registry error. A failed pull leaves the tools in `missing`,
                // so they are warned about + omitted below rather than breaking
                // the prompt.
                match context
                    .manager()
                    .find_or_install_all(to_install, platform.clone(), context.concurrency())
                    .await
                {
                    Ok(_) => applied = collect_applied(&offline, &project.lock, &platform).await?,
                    Err(err) => eprintln!("# ocx: pull failed ({err}); using locally available tools"),
                }
            }
        }

        for name in &applied.missing {
            eprintln!("# ocx: {name} not installed; run `ocx pull` to fetch");
        }

        let scope = ocx_lib::package_manager::PatchScope::Project(project.config.no_patches_repositories());
        let (entries, _, _) = offline
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
