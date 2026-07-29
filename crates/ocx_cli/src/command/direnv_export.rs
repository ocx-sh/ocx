// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    env, oci,
    package_manager::collect_applied,
    project::{DEFAULT_GROUP, MissingState, expand_all_keyword, host_leaf_identifier, load_project_state},
    shell,
};

use crate::conventions::emit_lines;
use crate::options;

/// Prints stateless shell export statements for the project toolchain.
///
/// Reads the nearest project `ocx.toml` (project tier only — no home-tier
/// fallback in this phase), loads the matching `ocx.lock`, looks up each
/// selected tool in the local object store, and prints bash export
/// lines for the resolved environment. The command is stateless: it does
/// not consult or update `_OCX_APPLIED`, making it suitable for use from
/// `direnv`'s `.envrc` via `eval "$(ocx direnv export)"`.
///
/// `ocx direnv init` writes an `.envrc` that calls this command with no
/// arguments, which selects the default group. Edit that line to widen the
/// scope or add an override — `eval "$(ocx direnv export -g ci --env
/// FORCE_COLOR=1)"` — and direnv picks it up on the next reload.
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
    groups: options::GroupSelection,

    #[clap(flatten)]
    env: options::EnvOverride,

    #[clap(flatten)]
    pull: options::Pull,
}

impl DirenvExport {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let shell = shell::Shell::Bash;

        // Parse-level validation first, before any filesystem work. Both are
        // usage errors (exit 64) — the one place this command is allowed to
        // fail loudly, because a malformed argument in `.envrc` is a typo the
        // user must see, not a transient toolchain state to warn past.
        crate::app::project_context::ensure_group_segments_nonempty(self.groups.names())?;

        // Project tier ONLY in Phase 7 — Phase 9 will add home-tier
        // fallback. The OCX_NO_PROJECT=1 kill switch is honored by
        // `load_project_state` via `ProjectConfig::resolve`.
        let cwd = env::current_dir()?;
        // A relative `:path` value anchors here, to the directory ocx runs in
        // — which under direnv is the directory holding `.envrc`. Resolved to
        // an absolute value before the entry exists, so the emitted export
        // line is stable regardless of where direnv later replays it.
        let env_overrides = self.env.entries(&cwd)?;
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

        // Group selection is validated against the loaded config, so a `-g`
        // naming a group that no longer exists fails loudly (exit 64) rather
        // than silently exporting nothing. That is an argv typo in a
        // hand-edited `.envrc`, not a transient toolchain state — the
        // never-fail-the-prompt contract covers the latter, not the former.
        crate::app::project_context::ensure_groups_known(self.groups.names(), &project.config)?;
        let mut expanded = expand_all_keyword(self.groups.names(), &project.config);
        if expanded.is_empty() {
            expanded = vec![DEFAULT_GROUP.to_owned()];
        }

        // Probe the local object store first through an offline `PackageManager`
        // clone: any incidental index lookup (V1 legacy locks walk the cached
        // index->manifest chain; V2 locks read the pinned leaf directly) stays
        // local, so a present tool resolves with no registry contact and a
        // not-materialised tool buckets into `missing`.
        let offline = context.manager().offline_view(context.local_index().clone());
        let platform = oci::Platform::current().unwrap_or_else(oci::Platform::any);
        let mut applied = collect_applied(&offline, &project.lock, &platform, &expanded).await?;

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
                .filter(|tool| expanded.contains(&tool.group) && missing.contains(tool.name.as_str()))
                .filter_map(|tool| host_leaf_identifier(tool, &platform).ok())
                .collect();
            if !to_install.is_empty() {
                // Best-effort: a per-prompt hook must never fail on a transient
                // registry error. A failed pull leaves the tools in `missing`,
                // so they are warned about + omitted below rather than breaking
                // the prompt.
                match context
                    .manager()
                    .find_or_install_all(&to_install, platform.clone(), context.concurrency())
                    .await
                {
                    Ok(_) => applied = collect_applied(&offline, &project.lock, &platform, &expanded).await?,
                    Err(err) => eprintln!("# ocx: pull failed ({err}); using locally available tools"),
                }
            }
        }

        for name in &applied.missing {
            eprintln!("# ocx: {name} not installed; run `ocx pull` to fetch");
        }

        // Stages 4-6, same assembly as `ocx run` and `ocx env`: the project's
        // `[env]`, each selected group's `[env]` in `-g` order, then `--env`.
        let mut project_env =
            crate::app::project_context::project_env_entries(&project.config, &project.config_path, &expanded);
        project_env.extend(env_overrides);
        let scope = ocx_lib::package_manager::EnvScope::Project {
            no_patches: project.config.no_patches_repositories(),
            env: project_env,
        };
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
