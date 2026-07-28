// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::env;
use ocx_lib::launch::{self, Launch};
use ocx_lib::record::{RecordInputs, Scope};

use crate::{conventions::*, options};

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
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    #[clap(flatten)]
    env: options::EnvOverride,

    #[clap(flatten)]
    platform: options::PlatformOption,

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
    /// policy marked `required` could not be written. The
    /// `anyhow::Result<ExitCode>` shape is kept for symmetry with sibling
    /// commands; the `Ok` arm is unreachable.
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let manager = context.manager();
        let platform = platform_or_default(self.platform.platform.clone());

        // Reject a malformed `--env` before any registry or filesystem work.
        // A relative `:path` value anchors to the invocation directory, the
        // same base the project tier's flag uses.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;

        // Folded here, before any registry work, for the same reason as the
        // `--env` check above: a malformed name template is a configuration
        // error, and the operator should hear about it before the packages are
        // installed rather than after.
        let records = context.records(self.records.options())?;

        let identifiers = options::Identifier::transform_all(self.packages.clone(), context.default_registry())?;
        let found = manager
            .find_or_install_all(&identifiers, platform.clone(), context.concurrency())
            .await?;
        // Which packages this invocation materialized on the spot. Half of the
        // drift signal an execution record publishes — the other half is a
        // surviving advisory tag on the package itself. Results come back in
        // input order, so the zip is sound.
        let auto_installed: Vec<ocx_lib::oci::Identifier> = identifiers
            .iter()
            .zip(&found)
            .filter(|(_, found)| found.materialization == ocx_lib::package_manager::Materialization::Pulled)
            .map(|(identifier, _)| identifier.clone())
            .collect();
        let install_infos: Vec<std::sync::Arc<ocx_lib::package::install_info::InstallInfo>> =
            found.into_iter().map(|found| std::sync::Arc::new(found.info)).collect();
        // `Package`, not `Project`: this tier reads no `ocx.toml` and never
        // will. The only thing a caller can contribute here is the override it
        // typed on this invocation — that is a CLI argument, not project
        // configuration, so carrying it does not cross the tier boundary.
        //
        // `resolve_env_with_attribution` rather than the attribution-dropping
        // wrapper: the record names which package claimed each executable on
        // `PATH`, and that derivation already exists here.
        let (entries, _, _, admitted) = manager
            .resolve_env_with_attribution(
                &install_infos,
                self.self_view,
                ocx_lib::package_manager::EnvScope::Package {
                    env: env_overrides.clone(),
                },
            )
            .await?;

        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
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
            env::ChildEnv {
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
        let resolved = process_env.resolve_command(command);
        let launch = Launch::recording(
            process_env,
            RecordInputs {
                packages: &install_infos,
                admitted: &admitted,
                executable: &resolved,
                store_root: context.file_structure().packages.root(),
                argv: &self.command,
                config: context.config_view(),
                // Already in memory: the snapshot is read once at `try_init` and
                // identity-gated there, so naming it here costs no I/O on the
                // exec path.
                managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
                platform: Some(&platform),
                clean_env: self.clean,
                auto_installed: &auto_installed,
                scope: Scope::Package { requested: identifiers },
            },
            &records,
        )?;

        // Replace this process with the child on Unix (PID inherited via
        // `execvp(2)`); on Windows spawn+wait then `process::exit`, since
        // `CreateProcess` has no exec equivalent. Either way the seam diverges
        // on success — only start-up failures fall through to the
        // error-wrapping path below.
        Err(anyhow::Error::from(launch::exec(launch).await))
    }
}
