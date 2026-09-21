// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_config::env;
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
    ///
    /// See https://ocx.sh/docs/in-depth/environments#visibility-views for the full view semantics.
    ///
    /// Cannot be combined with `--lazy-mode always`: a generated shim is a
    /// launcher, launchers are consumer-facing, and a package's private view
    /// bypasses them, so those two ask for contradictory things (exit 64).
    /// `--lazy-mode never` agrees with this view and is accepted, as is an
    /// `always` coming from `OCX_LAZY_MODE`, which composes eagerly.
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    /// Remove the packages from the store once the command finishes.
    ///
    /// A package this invocation downloaded leaves nothing behind. Only what
    /// nothing else holds is removed: a package that is also installed, one a
    /// project's `ocx.lock` pins, and a site-patch companion all stay, and each
    /// is named on stderr as kept.
    ///
    /// The exit code is the command's, always. A removal that fails warns on
    /// stderr and leaves the exit code alone.
    ///
    /// Without this flag ocx replaces itself with the command on Unix. With it,
    /// ocx stays running as the command's parent, and the command's process id
    /// is no longer ocx's. A kill of ocx itself skips the removal.
    #[clap(long = "rm", default_value_t = false)]
    rm: bool,

    #[clap(flatten)]
    env: options::EnvOverride,

    #[clap(flatten)]
    platform: options::PlatformOption,

    /// Top tier of the `lazy-mode` ladder for every package this command
    /// composes into the child environment.
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
    ///
    /// `required = true` + `num_args = 1..` means clap rejects the invocation
    /// before [`Self::execute`] runs when the slice would be empty, so the
    /// `.split_first().expect(...)` below is sound: clap is the single source
    /// of truth for non-emptiness, and we depend on its guarantee rather than
    /// duplicating the check.
    #[clap(allow_hyphen_values = true, required = true, num_args = 1..)]
    command: Vec<String>,
}

impl Exec {
    /// Compose the packages' environment and replace this process with the
    /// requested command.
    ///
    /// `launch::exec` diverges on success on every platform — Unix
    /// `execvp(2)`s, Windows spawns + waits + `process::exit`s — so this
    /// function only returns when start-up itself fails, or when a record the
    /// policy marked `required` could not be written.
    ///
    /// `--rm` is the one exception, and the only reason the `Ok` arm exists:
    /// cleanup has to run after the child, and a replaced process image has
    /// nowhere to run it, so that arm spawns and waits instead and returns the
    /// child's status as this command's.
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let manager = context.manager();
        let platform = platform_or_default(self.platform.platform.clone());

        // Reject a malformed `--env` before any registry or filesystem work.
        // A relative `:path` value anchors to the invocation directory, the
        // same base the project tier's flag uses.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let mut env_overrides = self.env.entries(&cwd)?;

        // Folded here, before any registry work, for the same reason as the
        // `--env` check above: a malformed name template is a configuration
        // error, and the operator should hear about it before the packages are
        // installed rather than after.
        let records = context.records(self.records.options())?;

        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        let mode = resolved_lazy_mode(self.lazy_mode.mode(), self.self_view)?;
        // Cloned rather than consumed: `identifiers` is also the record's
        // requested set, and a request list is the wrong thing to re-derive it
        // from once a deferred entry has been dropped.
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
        // Which packages this invocation materialized on the spot — half of the
        // drift signal an execution record publishes. Reported by
        // `compose_roots` itself, which is the only layer that sees the per-root
        // `Cached`/`Pulled` outcome: `composer::Materialization` is the policy
        // going in, `ComposeRoots::pulled` the answer coming out.
        let auto_installed = composed.pulled;
        // `Package`, not `Project`: this tier reads no `ocx.toml` and never
        // will. The only thing a caller can contribute here is the override it
        // typed on this invocation — that is a CLI argument, not project
        // configuration, so carrying it does not cross the tier boundary.
        //
        // `resolve_env_with_attribution` rather than the attribution-dropping
        // wrapper: the record names which package claimed each executable on
        // `PATH`, and that derivation already exists here.
        // The patch provenance is kept, not dropped: the record names every
        // companion the site tier overlaid onto this composition, and this call
        // is the only place that attribution exists.
        let (mut entries, _, patch_companions, admitted) = manager
            .resolve_env_with_attribution(
                &install_infos,
                self.self_view,
                ocx_package_manager::EnvScope::Package {
                    env: env_overrides.clone(),
                },
                &platform,
            )
            .await?;
        // W-11: `entries` (composed, applied to this process) and
        // `env_overrides` (forwarded raw over `OCX_ENV` for a re-entrant
        // launcher) are disjoint `Vec`s holding independent copies of the
        // `--env` overrides — reconcile them together so a package-established
        // `list` separator reaches the forwarded copy.
        reconcile_list_separators(entries.iter_mut().chain(env_overrides.iter_mut()))?;

        let mut process_env = if self.clean {
            env::Env::clean()
        } else {
            reconcile::inherited_env()
        };
        // Hand the resolved sink down. The config and environment tiers a child
        // re-derives for itself; the flag tier it cannot, so without this a
        // generated launcher's re-entry (`ocx launcher exec`) would record
        // somewhere else — or, since `apply_ocx_config` is set-or-remove,
        // nowhere — and the entrypoint pair would lose its inner half.
        let mut forwarded_config = context.config_view().clone();
        forwarded_config.records = records.forwarded();
        // Composed entries + forwarded ocx config + forwarded overrides, in the
        // one order that is correct — see `Env::apply_child_env`. On this tier
        // the `--env` overrides are the whole forwarded slice: there is no
        // project or group `[env]` to carry.
        process_env.apply_child_env(
            ChildEnv {
                composed: &entries,
                forwarded: &env_overrides,
            },
            &forwarded_config,
        );
        // No PATHEXT manipulation: the Windows launcher is now a native
        // `<name>.exe` shim and `.EXE` is unconditionally in the default
        // Windows PATHEXT, so the child resolves it via the OS default.

        // clap enforces `required = true, num_args = 1..` on the `command`
        // field — `self.command` is always non-empty at this point.
        let (command, _) = self
            .command
            .split_first()
            .expect("clap required=true guarantees at least one command element");

        // Resolved once, then handed to both the record and the launch: a second
        // resolution could disagree with the first and make the audit trail name
        // a binary other than the one that ran.
        // C-057/S-010: a name the composition does not provide is now an error
        // propagated here rather than a bare name handed to `execvp`, which
        // would have repeated the lookup against the ambient `PATH`.
        // `CommandResolutionError` already classifies to `DataError`, so this
        // `?` is the whole of exit 65 — and nothing is spawned on the way out.
        let resolved = process_env.resolve_command(command)?;
        let launch = Launch::recording(
            process_env,
            RecordInputs {
                packages: &install_infos,
                admitted: &admitted,
                patch_companions: &patch_companions,
                executable: &resolved,
                store_root: context.file_structure().packages.root(),
                shim_root: context.file_structure().shims.root(),
                argv: &self.command,
                config: context.config_view(),
                insecure_registries: context.insecure_hosts(),
                // Already in memory: the snapshot is read once at `try_init` and
                // identity-gated there, so naming it here costs no I/O on the
                // exec path.
                managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
                // Likewise read once at `try_init`, alongside the pins it
                // describes.
                patch_snapshot_digest: context.patch_snapshot_digest(),
                platform: Some(&platform),
                clean_env: self.clean,
                auto_installed: &auto_installed,
                scope: Scope::Package { requested: identifiers },
            },
            &records,
        )?;

        if !self.rm {
            // Replace this process with the child on Unix (PID inherited via
            // `execvp(2)`); on Windows spawn+wait then `process::exit`, since
            // `CreateProcess` has no exec equivalent. Either way the seam
            // diverges on success — only start-up failures fall through to the
            // error-wrapping path below. Byte-identical to the behaviour every
            // invocation that does not type `--rm` has always had.
            return Err(anyhow::Error::from(launch::exec(launch).await));
        }

        // `--rm` has work to do *after* the child, and `execvp` leaves no
        // process to do it in — the shape `package test` already uses for its
        // tempdir guard. Three flag-scoped consequences, none of them papered
        // over: ocx stays resident for the tool's lifetime so the tool's pid is
        // not ocx's; the Unix pre-exec record guarantee weakens to the
        // spawn-and-wait ordering Windows already has; and a SIGKILL of ocx
        // itself skips the cleanup, the same hole `docker run --rm` has.
        // Signal forwarding and `kill_on_drop` are `spawn_and_wait`'s own.
        let status = launch::spawn_and_wait(launch).await.map_err(anyhow::Error::from)?;
        let exit_code = child_process::propagate_exit_code(status);

        // The identities to offer up, taken from the composed roots rather than
        // the requested list: a request is a tag, and what landed on disk is a
        // digest. Read after `spawn_and_wait` consumed the launch that borrowed
        // them.
        let pinned: Vec<_> = install_infos.iter().map(|info| info.identifier().clone()).collect();

        // Housekeeping, not the command's result. Whatever happens below, the
        // exit code stays the child's: a script reading `$?` after
        // `ocx package exec` is reading the tool's status, and a removal that
        // failed is not the tool's status.
        match manager.purge_unrooted(&pinned).await {
            Ok(purged) => {
                for path in &purged.removed {
                    context.ui().status("Removed", path.display());
                }
                match purged.root_set {
                    // Retaining because something holds the package is the
                    // flag's designed outcome, not a problem: it is what keeps
                    // `--rm` from deleting an install out from under its own
                    // symlink. A status line, like the removals above.
                    RootSet::Determinate => {
                        for identifier in &purged.retained {
                            context
                                .ui()
                                .status("Kept", format!("{identifier} (still held by something else)"));
                        }
                    }
                    // A different sentence, because it is a different fact: the
                    // root set could not be read, so nothing was tested and
                    // nothing was removed. Naming each identifier here would
                    // claim each is held, which is exactly what this run failed
                    // to establish. `collect_project_roots` has already logged
                    // which lock it was.
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
