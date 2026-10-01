// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_config::env;
use ocx_package::launch::LaunchIdentities;
use ocx_package_manager::RootSet;
use ocx_package_manager::composer::{ComposeRequest, Materialization};
use ocx_package_manager::launch::{self, Launch};
use ocx_package_manager::record::{RecordInputs, Scope};
use ocx_util::child_process;

use crate::{conventions::*, options};
use ocx_package::metadata::env::apply::{ChildEnv, EnvEntriesExt, reconcile_list_separators};
use ocx_shell::shell::reconcile;

/// Runs installed packages.
///
/// Each positional accepts an OCI identifier (e.g. `node:20`).
/// Packages are resolved through the index and auto-installed when missing.
#[derive(Parser)]
pub struct Exec {
    /// Start with a clean environment containing only the package variables, instead of inheriting the current shell environment.
    #[clap(long = "clean", default_value_t = false)]
    clean: bool,

    /// Expose the package's full env, including its private (self-only)
    /// entries. Off by default: only public + interface entries are loaded
    /// (the consumer view). Generated launchers use `ocx launcher exec` which
    /// enables self-view internally.
    #[arg(long_help = "\
        Expose the package's full env, including its private (self-only) entries. Off by default: \
        only public + interface entries are loaded (the consumer view). Generated launchers use \
        `ocx launcher exec` which enables self-view internally.\n\n\
        See https://ocx.sh/docs/in-depth/environments#visibility-views for the full view semantics.\n\n\
        Cannot be combined with `--lazy-mode always`: a generated shim is a launcher, launchers are \
        consumer-facing, and a package's private view bypasses them, so those two ask for \
        contradictory things (exit 64). `--lazy-mode never` agrees with this view and is accepted, \
        as is an `always` coming from `OCX_LAZY_MODE`, which composes eagerly.")]
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    /// Remove the packages from the store once the command finishes.
    #[arg(long_help = "\
        Remove the packages from the store once the command finishes.\n\n\
        A package this invocation downloaded leaves nothing behind. Only what nothing else holds is \
        removed: a package that is also installed, one a project's `ocx.lock` pins, and a \
        site-patch companion all stay. Removed and kept packages are logged at the `info` level.\n\n\
        The exit code is the command's, always. A removal that fails warns on stderr and leaves the \
        exit code alone.\n\n\
        Without this flag ocx replaces itself with the command on Unix. With it, ocx stays running \
        as the command's parent, and the command's process id is no longer ocx's. A kill of ocx \
        itself skips the removal.")]
    #[clap(long = "rm", default_value_t = false)]
    rm: bool,

    #[clap(flatten)]
    env: options::EnvOverride,

    #[clap(flatten)]
    platform: options::PlatformOption,

    /// Top tier of the `lazy-mode` ladder for every package composed into the child environment.
    ///
    /// `always` composes a package as a generated shim: its declared names
    /// reach the child's `PATH` immediately and its content downloads the first
    /// time one of them runs. The shim directory sits *below* the package's own
    /// `entrypoints/` and `bin/`, so a second invocation of the same name
    /// inside the same child resolves to the materialized binary directly.
    ///
    /// Only this flag and `OCX_LAZY_MODE` apply here: the `ocx.toml` tiers
    /// belong to the toolchain commands, and this one reads no project file.
    #[clap(flatten)]
    lazy_mode: options::LazyMode,

    #[clap(flatten)]
    records: options::Records,

    /// Package identifiers to layer environment from.
    ///
    /// Each value is a bare OCI identifier (e.g. `node:20`); identifiers are
    /// resolved through the index and auto-installed when missing.
    #[clap(required = true, num_args = 1.., value_terminator = "--")]
    packages: Vec<options::Identifier>,

    /// Command to execute, with arguments. The command will be executed with the environment with the packages.
    // clap enforces non-emptiness, which the `.expect` in `execute` relies on.
    #[clap(allow_hyphen_values = true, required = true, num_args = 1..)]
    command: Vec<String>,
}

impl Exec {
    /// Compose the packages' environment and replace this process with the requested command; returns
    /// only when start-up fails or a `required` record could not be written. `--rm` spawns and waits
    /// instead, since cleanup must run after the child.
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let manager = context.manager();
        let platform = platform_or_default(self.platform.platform.clone());

        // Before any registry work; a relative `:path` anchors to the invocation directory.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let mut env_overrides = self.env.entries(&cwd)?;

        // Before any registry work, so a malformed name template fails before installing.
        let records = context.records(self.records.options())?;

        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        let mode = resolved_lazy_mode(self.lazy_mode.mode(), self.self_view)?;
        // Cloned: `identifiers` is also the record's requested set.
        let requests: Vec<ComposeRequest> = identifiers
            .iter()
            .map(|identifier| ComposeRequest {
                identifier: identifier.clone(),
                mode,
            })
            .collect();
        let composed = manager
            .compose_roots(&requests, &platform, Materialization::Install, context.concurrency())
            .await?;
        for advisory in &composed.advisories {
            context.ui().warn(advisory.to_string());
        }
        let install_infos = composed.roots;
        // What this invocation pulled: half of the record's drift signal.
        let auto_installed = composed.pulled;
        // `Package` scope: this tier never reads `ocx.toml`.
        let (mut entries, _, _, admitted) = manager
            .resolve_env_with_attribution(
                &install_infos,
                self.self_view,
                ocx_package_manager::EnvScope::Package {
                    env: env_overrides.clone(),
                },
                &platform,
            )
            .await?;
        // `entries` and `env_overrides` (forwarded over `OCX_ENV`) hold independent copies of the
        // overrides; reconcile both so a package's `list` separator reaches the forwarded copy.
        reconcile_list_separators(entries.iter_mut().chain(env_overrides.iter_mut()))?;

        let mut process_env = if self.clean {
            env::Env::clean()
        } else {
            reconcile::inherited_env()
        };
        // A child cannot re-derive the flag tier of the record sink, so without this a launcher
        // re-entry records elsewhere or nowhere and the entrypoint pair loses its inner half.
        let mut forwarded_config = context.config_view().clone();
        forwarded_config.records = records.forwarded();
        // On this tier the `--env` overrides are the whole forwarded slice.
        process_env.apply_child_env(
            ChildEnv {
                composed: &entries,
                forwarded: &env_overrides,
                identities: Some(&LaunchIdentities::from_infos(&install_infos)),
            },
            &forwarded_config,
        );
        let (command, _) = self
            .command
            .split_first()
            .expect("clap required=true guarantees at least one command element");

        // Resolved once for record and launch, or the audit trail could misname the binary. An
        // unprovided name is 65 here, never an ambient `PATH` lookup.
        let resolved = process_env.resolve_command(command)?;
        let launch = Launch::recording(
            process_env,
            RecordInputs {
                packages: &install_infos,
                admitted: &admitted,
                executable: &resolved,
                store_root: context.file_structure().packages.root(),
                shim_root: context.file_structure().shims.root(),
                argv: &self.command,
                config: context.config_view(),
                insecure_registries: context.insecure_hosts(),
                // Read once at `try_init`; no I/O on the exec path.
                managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
                patch_snapshot_digest: context.patch_snapshot_digest(),
                platform: Some(&platform),
                clean_env: self.clean,
                auto_installed: &auto_installed,
                scope: Scope::Package { requested: identifiers },
            },
            &records,
        )?;

        if !self.rm {
            return Err(anyhow::Error::from(launch::exec(launch).await));
        }

        // Spawned, so the pre-exec record guarantee drops to spawn-and-wait ordering and a SIGKILL of
        // ocx skips cleanup.
        let status = launch::spawn_and_wait(launch).await.map_err(anyhow::Error::from)?;
        let exit_code = child_process::propagate_exit_code(status);

        // The composed roots, not the request: a request is a tag, what landed is a digest.
        let pinned: Vec<_> = install_infos.iter().map(|info| info.identifier().clone()).collect();

        // The exit code stays the child's whatever happens below: a failed removal is not the tool's status.
        match manager.purge_unrooted(&pinned).await {
            // Log records, never `ui().status`: a status line bypasses `--log-level`, and the terminal is the child's.
            Ok(purged) => {
                match purged.root_set {
                    // The designed outcome: it keeps `--rm` from deleting an install under its own symlink.
                    RootSet::Determinate => {
                        for identifier in &purged.retained {
                            log::info!("kept {identifier}: still held by something else");
                        }
                    }
                    // Nothing was tested: naming identifiers would claim each is held, which this run never checked.
                    RootSet::Indeterminate => context.ui().warn(
                        "retained every package: a project lock could not be read this run, so nothing was removed",
                    ),
                }
            }
            Err(error) => context
                .ui()
                .warn(format!("could not remove the packages this run composed: {error}")),
        }

        Ok(exit_code)
    }
}
