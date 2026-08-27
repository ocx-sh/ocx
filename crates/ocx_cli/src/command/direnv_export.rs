// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    env, oci,
    package_manager::composer::{ComposeRequest, Materialization},
    project::{
        DEFAULT_GROUP, MissingState, expand_all_keyword, host_leaf_identifier, lazy_mode_for_tool, load_project_state,
    },
    shell,
};

use crate::conventions::emit_lines;
use crate::options;

/// Prints stateless shell export statements for the project toolchain.
///
/// Reads the nearest project `ocx.toml` (project tier only — no home-tier
/// fallback in this phase), loads the matching `ocx.lock`, looks up each
/// selected tool in the local object store, and prints bash export
/// lines for the resolved environment. The command is stateless, which is
/// what makes it usable from `direnv`'s `.envrc` via
/// `eval "$(ocx direnv export)"`.
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

    /// Top tier of the `lazy-mode` ladder for every tool this command exports.
    ///
    /// `always` exports a tool as a generated shim: its declared names reach
    /// `PATH` immediately and its content downloads the first time one of them
    /// runs. Without this, a project declaring `lazy-mode = "always"` would get
    /// shims under `ocx env` and eager content under direnv: one project with
    /// two environments depending on which door you came through.
    ///
    /// A tool whose metadata is not already local is noted on stderr and
    /// omitted, exactly as a not-materialised tool is. This command never fails
    /// a prompt.
    #[clap(flatten)]
    lazy_mode: options::LazyMode,
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

        let platform = oci::Platform::current().unwrap_or_else(oci::Platform::any);

        // One request per in-scope lock tool, each carrying the `lazy-mode` its
        // ladder resolved to — the same ladder `ocx env` and `ocx run` apply, so
        // a project cannot compose two different environments depending on
        // which door it came through. A tool that ships no leaf for this host is
        // dropped here with a note: `direnv export` never fails a prompt.
        let mut names: Vec<String> = Vec::new();
        let mut requests: Vec<ComposeRequest> = Vec::new();
        for tool in project.lock.tools.iter().filter(|tool| expanded.contains(&tool.group)) {
            let Ok(identifier) = host_leaf_identifier(tool, &platform) else {
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
            requests.push(ComposeRequest { identifier, mode });
        }

        // Probe first through an offline `PackageManager` clone: any incidental
        // index lookup (V1 legacy locks walk the cached index->manifest chain;
        // V2 locks read the pinned leaf directly) stays local, so a present tool
        // resolves with no registry contact and a not-materialised one is
        // omitted rather than fetched.
        let offline = context.manager().offline_view(context.local_index().clone());
        let mut composed = offline
            .compose_roots(&requests, &platform, Materialization::LocalOnly, context.concurrency())
            .await?;

        // Default: materialise anything the probe omitted, then re-probe so the
        // freshly-pulled tools join the export. `--no-pull` opts out and the
        // command stays strictly offline. The pull is also skipped when no
        // registry is reachable (`--offline` / no remote) so an offline shell
        // never blocks. Best-effort throughout: a per-prompt hook must never
        // fail on a transient registry error, so a failed pull leaves the tools
        // omitted and warned about rather than breaking the prompt.
        if self.pull.enabled(true) && !composed.omitted.is_empty() && !context.manager().is_offline() {
            // Only what the probe omitted. Handing the retry the whole set
            // would re-run `prepare_lazy` for every tool that already composed
            // — a second closure walk each, on a partially-warm store, because
            // one unrelated eager tool was missing.
            let missing: Vec<oci::Identifier> = composed
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
                    // `roots` carries one entry per surviving request, in
                    // request order, with no slot for an omission — so the two
                    // sets are re-interleaved by replaying `requests` rather
                    // than by index arithmetic over either vector alone.
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
                Err(err) => eprintln!("# ocx: pull failed ({err}); using locally available tools"),
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

        // Stages 4-6, same assembly as `ocx run` and `ocx env`: the project's
        // `[env]`, each selected group's `[env]` in `-g` order, then `--env`.
        let mut project_env = ocx_lib::project::project_env_entries(&project.config, &project.config_path, &expanded);
        project_env.extend(env_overrides);
        let scope = ocx_lib::package_manager::EnvScope::Project {
            no_patches: project.config.no_patches_repositories(),
            env: project_env,
        };
        let (mut entries, _, _) = offline
            .resolve_env_with_patch_boundary(&composed.roots, false, scope, &platform)
            .await?;

        // W-11: settle each `list` entry's separator before emitting — a
        // package's explicit separator must be the one every `None`-separator
        // contributor (project `[env]`, `--env`) inherits, not the fold's bare
        // default. No forwarded copy exists here (this command never spawns a
        // re-entrant launcher), so a single-vector pass is enough.
        env::reconcile_list_separators(entries.iter_mut())?;

        // Delegate to the shared emit helper (C5 / conventions.rs).
        // `Shell::Bash` is fixed: direnv always evaluates `.envrc` in a bash
        // sub-shell regardless of the user's interactive shell.  There is no
        // `--shell` flag on `direnv export` for this reason.
        emit_lines(shell, &entries);

        Ok(ExitCode::SUCCESS)
    }
}
