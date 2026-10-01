// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_package_manager::composer::{ComposeRequest, Materialization};
use ocx_project::{DEFAULT_GROUP, MissingState, expand_all_keyword, lazy_mode_for_tool, load_project_state};
use ocx_shell::shell;

use crate::conventions::emit_lines;
use crate::options;
use ocx_package::metadata::env::apply::reconcile_list_separators;

/// Prints stateless shell export statements for the project toolchain.
///
/// Reads the nearest project `ocx.toml` (no home-tier fallback) and its
/// `ocx.lock`, and prints bash export lines for the selected packages, for
/// `eval "$(ocx direnv export)"` in an `.envrc` (`ocx direnv init` writes one
/// selecting the default group). A missing package is pulled unless
/// `--no-pull` or no reachable registry keeps the command offline, else noted
/// on stderr and skipped. A stale lock warns and is still used, a missing
/// package never fails the prompt, and no project `ocx.toml` exits 0 silently.
#[derive(Parser)]
pub struct DirenvExport {
    #[clap(flatten)]
    groups: options::GroupSelection,

    #[clap(flatten)]
    env: options::EnvOverride,

    #[clap(flatten)]
    pull: options::Pull,

    /// Top tier of the `lazy-mode` ladder for every package this command exports.
    ///
    /// `always` exports a package as a generated shim: its declared names reach
    /// `PATH` immediately and its content downloads the first time one of them
    /// runs. A package whose metadata is not already local is noted on stderr
    /// and omitted, exactly as a not-materialised package is; this command never
    /// fails a prompt.
    // Without it, `lazy-mode = "always"` composes shims under `ocx env` but eager content under direnv.
    #[clap(flatten)]
    lazy_mode: options::LazyMode,
}

impl DirenvExport {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Always bash: direnv evaluates `.envrc` in bash and translates to the interactive shell itself.
        let shell = shell::Shell::Bash;

        // Argv faults fail loudly (64) before any filesystem work: an `.envrc` typo must be seen.
        crate::app::project_context::ensure_group_segments_nonempty(self.groups.names())?;

        let cwd = ocx_util::env::current_dir()?;
        // Relative `:path` values anchor here (the `.envrc` directory) and resolve absolute, so the
        // export is stable wherever direnv replays it.
        let env_overrides = self.env.entries(&cwd)?;
        let project = match load_project_state(&cwd, context.project_path()).await? {
            Ok(state) => state,
            Err(MissingState::NoProject) => {
                // No `ocx.toml` in scope contributes nothing.
                return Ok(ExitCode::SUCCESS);
            }
            Err(MissingState::LockMissing { lock_path }) => {
                // Not an error: this runs every prompt, and failing would break a fresh clone's terminal.
                eprintln!(
                    "# ocx: ocx.lock not found at {}; run `ocx lock` to fetch",
                    lock_path.display()
                );
                return Ok(ExitCode::SUCCESS);
            }
        };

        // Warn and use the stale digests, unlike `ocx exec`'s 65, so the shell stays usable until a re-lock.
        if project.stale {
            eprintln!("# ocx: ocx.lock is stale (it does not match ocx.toml); using stale digests");
        }

        // An unknown `-g` is an `.envrc` typo and fails (64) rather than silently exporting nothing.
        crate::app::project_context::ensure_groups_known(self.groups.names(), &project.config)?;
        let mut expanded = expand_all_keyword(self.groups.names(), &project.config);
        if expanded.is_empty() {
            expanded = vec![DEFAULT_GROUP.to_owned()];
        }

        let platform = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);

        // Each tool's `lazy-mode` comes from the same ladder `ocx env`/`ocx exec` apply; a tool with no
        // host leaf is dropped with a note. A stale lock exports untagged, never a new tag on an old digest.
        let mut names: Vec<String> = Vec::new();
        let mut requests: Vec<ComposeRequest> = Vec::new();
        for (tool, identifier) in project
            .lock
            .lenient_host_identifiers(Some(&project.config), &platform)
            .into_iter()
            .filter(|(tool, _)| expanded.contains(&tool.group))
        {
            let Ok(identifier) = identifier else {
                eprintln!("# ocx: {} ships no build for this platform; skipping", tool.name);
                continue;
            };
            let mode = lazy_mode_for_tool(
                &project.config,
                &identifier,
                Some(tool.group.as_str()),
                self.lazy_mode.mode(),
            );
            names.push(tool.name.clone());
            requests.push(ComposeRequest {
                identifier: identifier.into(),
                mode,
            });
        }

        // Probe offline first, so a present tool resolves with no registry contact and a missing one is
        // omitted, not fetched.
        let offline = context.manager().offline_view(context.local_index().clone());
        let mut composed = offline
            .compose_roots(&requests, &platform, Materialization::LocalOnly, context.concurrency())
            .await?;

        // Pull what the probe omitted, unless `--no-pull` or no reachable registry; a failed pull only
        // leaves those tools omitted.
        if self.pull.enabled(true) && !composed.omitted.is_empty() && !context.manager().is_offline() {
            // Only the omitted: retrying the whole set re-walks every tool that already composed.
            let missing: Vec<ocx_oci::PackageRef> = composed
                .omitted
                .iter()
                .map(|omission| omission.identifier.clone())
                .collect();
            let retry: Vec<ComposeRequest> = requests
                .iter()
                .filter(|request| missing.contains(&request.identifier))
                .cloned()
                .collect();
            match context
                .manager()
                .compose_roots(&retry, &platform, Materialization::Install, context.concurrency())
                .await
            {
                Ok(installed) => {
                    // `roots` has no slot for an omission, so re-interleave by replaying `requests`, not by index.
                    let mut probed = std::mem::take(&mut composed.roots).into_iter();
                    let mut pulled = installed.roots.into_iter();
                    composed.roots = requests
                        .iter()
                        .filter_map(|request| {
                            if missing.contains(&request.identifier) {
                                pulled.next()
                            } else {
                                probed.next()
                            }
                        })
                        .collect();
                    composed.advisories.extend(installed.advisories);
                    composed.omitted = installed.omitted;
                }
                Err(err) => eprintln!("# ocx: pull failed ({err}); using locally available packages"),
            }
        }

        for omission in &composed.omitted {
            let name = requests
                .iter()
                .position(|request| request.identifier == omission.identifier)
                .and_then(|index| names.get(index).cloned())
                .unwrap_or_else(|| omission.identifier.to_string());
            eprintln!("# ocx: {name} not installed; run `ocx pull` to fetch");
        }
        for advisory in &composed.advisories {
            eprintln!("# ocx: {advisory}");
        }

        // As in `ocx exec`/`ocx env`: project `[env]`, each group's `[env]` in `-g` order, then `--env`.
        let mut project_env = ocx_project::project_env_entries(&project.config, &project.config_path, &expanded);
        project_env.extend(env_overrides);
        // Heals the groups before any link path is emitted. No `--pinned` flag here, so `ocx.toml` /
        // `OCX_TOOLCHAIN_PINNED` decide.
        let toolchain = crate::app::project_context::toolchain_links(
            &context,
            &project.config_path,
            &project.config,
            &project.lock,
            &expanded,
            None,
        )
        .await?;
        let no_patches = project.config.no_patches_repositories();
        let scope = ocx_package_manager::EnvScope::Project {
            no_patches: no_patches.clone(),
            env: project_env,
            toolchain: Some(Box::new(toolchain)),
        };
        let (mut entries, _, _) = offline
            .resolve_env_with_patch_boundary(&composed.roots, false, scope, &platform)
            .await?;
        let inherited = ocx_util::env::var(ocx_config::env::keys::OCX_LAUNCH_IDENTITIES);
        entries.extend(offline.launch_identity_entry(&composed.roots, &no_patches, inherited.as_deref()));

        // A package's explicit separator must be what `None`-separator contributors (`[env]`, `--env`)
        // inherit, not the fold's default; nothing is forwarded, so one pass suffices.
        reconcile_list_separators(entries.iter_mut())?;

        emit_lines(shell, &entries);

        Ok(ExitCode::SUCCESS)
    }
}
