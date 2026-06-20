// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx env` — toolchain-tier composed-env command.
//!
//! Reads the in-scope `ocx.toml` + `ocx.lock` (project tier) or resolves the
//! global toolchain's installed `current` set offline (under `--global`) and
//! emits the composed environment for the default group.
//!
//! Scope = command location (toolchain-tier).
//! Format = context-level concern (root `--format` flag; default plain).
//! No subcommand `--format` flag — use `ocx --format json env` for JSON.
//! `--shell[=NAME]` is the only eval-safe output form.
//!
//! # Two resolution paths, no divergent global resolvers
//!
//! - **`--global`** routes through [`resolve_global_pinned_env`]: a strictly
//!   **offline** `$OCX_HOME/ocx.lock` → resolve each tool's **pinned digest**
//!   against the local object store → `resolve_env` composition (ADR
//!   `adr_global_toolchain_tier.md` Decision 5 **amended 2026-05-19**,
//!   handshake §1/§4). The global tier is the project tier with a different
//!   load site — the `current` symlink is a separate install/select-only
//!   abstraction and is NOT consulted (so `ocx --global upgrade` takes effect
//!   with no select step). The §4 login exporter
//!   `eval "$(ocx --global env --shell=sh)"` runs on every shell start — it
//!   MUST NOT contact the registry, install, or hang. A pinned tool not
//!   materialised locally ⇒ silently skipped. The global tier is LENIENT: ANY
//!   resolution failure — nothing configured ("no lock / nothing local") OR a
//!   real fault (corrupt/stale `$OCX_HOME/ocx.lock`, env-composition error) —
//!   yields the SAME **empty env** (exit 0). This does not depend on `--shell`,
//!   which only selects the output FORMAT, never the error semantics: one
//!   predictable rule, an unusable global toolchain is empty. A corrupt global
//!   lock surfaces loudly via the commands that rewrite it
//!   (`ocx --global lock`/`add`/`upgrade`), not via this read-only exporter; a
//!   genuine fault is logged at `debug`. Never exit 74. (The PROJECT tier stays
//!   strict — see below.)
//! - **project** (no `--global`) routes through `load_project_with_lock` +
//!   `compose_tool_set`, then a **single batched** `find_or_install_all` over
//!   all composed identifiers (mirrors `run.rs` — no per-tool N+1 network
//!   round-trip).
//!
//! [`resolve_global_pinned_env`] is relocated here from `shell_hook.rs`.
//! Rationale: it performs toolchain-tier `$OCX_HOME → lock → pinned-digest
//! resolve → resolve_env` composition specific to the `ocx --global env` code
//! path — it belongs with the command that consumes it, not in generic CLI
//! helpers (`conventions.rs` holds stateless helpers with no toolchain-tier
//! awareness).

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use ocx_lib::{
    package::metadata::env::entry::Entry,
    project::{DEFAULT_GROUP, ProjectLock, compose_tool_set, lock::lock_path_for},
};

use crate::{
    api,
    app::project_context::load_project_with_lock,
    conventions::{emit_lines, export_ci, platforms_or_default, resolve_ci_arg, resolve_shell_arg},
};

/// Emit the composed environment for the in-scope toolchain.
///
/// Reads `ocx.toml` + `ocx.lock` (project tier, CWD-walk / `--project` /
/// `OCX_PROJECT`) or resolves the global toolchain's offline `current` set
/// (when `--global` is set), composes the default-group env, and writes it to
/// stdout.
///
/// # Output
///
/// - Default (no `--shell`): structured report through the context `Api` —
///   the format is the context-level concern selected by the root `--format`
///   flag (default: plain table; use `ocx --format json env` for JSON).
///   This command does **not** have its own `--format` flag. NOT eval-safe.
/// - `--shell[=NAME]`: eval-safe shell export lines. The ONLY sourceable form.
///
/// `eval "$(ocx env)"` is a user error — use `eval "$(ocx env --shell=bash)"`.
///
/// # Exit codes
///
/// - 0 (`Success`): under `--global`, ANY resolution outcome short of a usage
///   error — nothing configured (no `$OCX_HOME/ocx.lock`, or nothing resolves
///   locally) AND a corrupt/stale global lock or composition fault — yields an
///   empty env on the report path AND the eval-safe `--shell` path. The global
///   tier is lenient; a corrupt global lock surfaces via `ocx --global lock`/
///   `add`/`upgrade`, not this read-only exporter.
/// - 64 (`UsageError`): no `ocx.toml` in scope (project tier); `--shell`
///   (bare) with undetectable `$SHELL`/parent; `--global` ⟂ `--project`
///   (clap `conflicts_with`, mapped to EX_USAGE 64 — NOT exit 2).
/// - 78 (`ConfigError`): `ocx.lock` absent (project tier).
/// - 65 (`DataError`): `ocx.lock` stale (project tier).
#[derive(Parser)]
pub struct ToolchainEnv {
    /// Target shell for eval-safe export lines.
    ///
    /// Must be supplied with `=` (`--shell=bash`).  Bare `--shell` (no `=`)
    /// triggers autodetection from `$SHELL`/parent process; exit 64 if
    /// undetectable.
    ///
    /// `--shell=sh` is an alias for `--shell=dash` (POSIX strict).
    #[arg(
        long,
        value_enum,
        value_name = "SHELL",
        num_args = 0..=1,
        require_equals = true
    )]
    shell: Option<Option<ocx_lib::shell::Shell>>,

    /// Write the composed environment into a CI system's persistence channel.
    ///
    /// `--ci=github` appends tool dirs and vars to `$GITHUB_PATH` /
    /// `$GITHUB_ENV`; `--ci=gitlab` writes JSON-lines to `--export-file` (or
    /// stdout). Bare `--ci` autodetects the provider from CI environment
    /// variables; exit 64 if none is detected. Must be supplied with `=`
    /// (`--ci=github`).
    ///
    /// Unlike `--shell` (which affects only the current step), the CI channel
    /// makes the environment available to later pipeline steps. Conflicts with
    /// `--shell`.
    #[arg(long, value_enum, value_name = "PROVIDER", num_args = 0..=1, require_equals = true, conflicts_with = "shell")]
    ci: Option<Option<ocx_lib::ci::CiFlavor>>,

    /// Write the GitLab export to this file instead of stdout.
    ///
    /// Requires `--ci`. Rejected for `--ci=github`, which infers its sink from
    /// `$GITHUB_PATH` / `$GITHUB_ENV`. Point this at GitLab's export file.
    #[arg(long, value_name = "PATH", requires = "ci")]
    export_file: Option<std::path::PathBuf>,

    /// Annotate each entry with its origin package or companion identifier.
    ///
    /// When `[patches]` is configured, companion overlay entries are appended
    /// after the toolchain's own entries.  `--show-patches` adds a Source column
    /// to the plain table (or a `"source"` field in JSON) so the origin of each
    /// entry is visible.
    ///
    /// Has no effect when `[patches]` is not configured.  Cannot be combined
    /// with `--shell` or `--ci`; use the plain or JSON structured report instead.
    #[arg(long, default_value_t = false, conflicts_with = "shell", conflicts_with = "ci")]
    show_patches: bool,
}

impl ToolchainEnv {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // `None` → default-format path; `Some(s)` → eval-safe emit.
        let shell = resolve_shell_arg(self.shell)?;
        // Resolve `--ci` early so a bare-`--ci` autodetect failure surfaces as a
        // usage error before the (potentially slow) entry resolution. `--ci` is
        // mutually exclusive with `--shell` (clap `conflicts_with`), so at most
        // one of these is set.
        let ci = resolve_ci_arg(self.ci)?;

        // ── Resolve entries: one global path, one project path ───────────────
        // Root `--global` / `OCX_GLOBAL` (already folded into the context via
        // `ContextOptions`) re-targets resolution to `$OCX_HOME/ocx.toml`;
        // `--global` ⟂ `--project` is rejected at `Context::try_init`.
        //
        // `patch_start` is the index at which companion-overlay entries begin
        // (used by the `--show-patches` annotation below). BOTH tiers apply the
        // Phase 4 overlay from local state and return the boundary: the global
        // path's `offline_view` preserves the patch tier (only the network is
        // disabled), so already-installed companions overlay the global env too.
        let (entries, patch_start) = if context.global() {
            // OFFLINE current-symlink resolution (ADR D5, handshake §1/§4).
            // The login exporter runs this every shell — never network/install.
            //
            // The global tier is LENIENT — it is an implicit, optional
            // toolchain. "No usable global toolchain" is a normal empty result,
            // not an error. Every non-success outcome — nothing configured
            // (`Ok(None)`: no `$OCX_HOME/ocx.lock`, or nothing materialised
            // locally) OR a real fault (`Err`: corrupt/stale lock,
            // env-composition error) — yields the SAME empty env (exit 0).
            //
            // This is INDEPENDENT of `--shell`: that flag selects the output
            // FORMAT (eval-safe lines vs structured report), never the error
            // semantics. One predictable rule — an unusable global toolchain is
            // empty, full stop — instead of behaviour that forks on how the
            // caller happens to format output.
            //
            // A genuine fault is logged at `debug` so it stays observable
            // (`-l debug`); a corrupt global lock also surfaces loudly via the
            // commands that actually rewrite it (`ocx --global lock`/`add`/
            // `upgrade`), so this read-only exporter need not. The PROJECT tier
            // (the `else` arm) stays strict: an explicit project's
            // missing/stale/corrupt `ocx.lock` IS an error.
            match resolve_global_pinned_env(&context).await {
                Ok(Some((entries, patch_start))) => (entries, patch_start),
                Ok(None) => (Vec::new(), 0),
                Err(error) => {
                    tracing::debug!("global toolchain env resolution failed; emitting empty env: {error:#}");
                    (Vec::new(), 0)
                }
            }
        } else {
            // Project tier: resolve + a SINGLE batched install (mirror run.rs).
            let ctx = load_project_with_lock(&context).await?;
            let host = ocx_lib::oci::Platform::current().unwrap_or_else(ocx_lib::oci::Platform::any);
            let composed = compose_tool_set(&ctx.config, Some(&ctx.lock), &[DEFAULT_GROUP.to_owned()], &[], &host)?;

            let identifiers: Vec<ocx_lib::oci::Identifier> = composed.into_iter().map(|tool| tool.identifier).collect();
            let platforms = platforms_or_default(&[]);
            let manager = context.manager();
            let infos: Vec<Arc<ocx_lib::package::install_info::InstallInfo>> = manager
                .find_or_install_all(identifiers, platforms, context.concurrency())
                .await?
                .into_iter()
                .map(Arc::new)
                .collect();
            manager.resolve_env_with_patch_boundary(&infos, false).await?
        };

        // ── Emit ─────────────────────────────────────────────────────────────
        if let Some(provider) = ci {
            // CI sink: persist the composed env for later pipeline steps. An
            // explicit `--ci=github` outside GitHub Actions legitimately fails
            // `from_env()` (the global-lenient contract covers *resolution*
            // failures only, not an explicit-channel misconfiguration).
            export_ci(provider, self.export_file.clone(), &entries)?;
            return Ok(ExitCode::SUCCESS);
        }

        if let Some(s) = shell {
            // Eval-safe: delegate to shared emit helper (C5).
            emit_lines(s, &entries);
            return Ok(ExitCode::SUCCESS);
        }

        // Structured report. Format is a context-level concern (root
        // `--format`); this command does not override it.
        let env_data: Vec<api::data::env::EnvEntry> = entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                // Annotate origin when `--show-patches` is enabled. Entries at index
                // `>= patch_start` came from companion overlay projections.
                let source = if self.show_patches && i >= patch_start {
                    Some(api::data::env::SOURCE_PATCH.to_owned())
                } else {
                    None
                };
                api::data::env::EnvEntry {
                    key: e.key,
                    value: e.value,
                    kind: e.kind,
                    source,
                }
            })
            .collect();

        // No synthetic PATHEXT entry: the Windows launcher is now a native
        // `<name>.exe` shim, and `.EXE` is unconditionally in the default
        // Windows PATHEXT — nothing to inject for bare-name resolution.

        if !context.api().is_json() {
            ocx_lib::log::warn!("default output is not eval-safe; use --shell=bash to activate");
        }

        context.api().report(&api::data::env::EnvVars::new(env_data))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Resolve the global toolchain's **lock-pinned** set into env entries.
///
/// Source = `$OCX_HOME/ocx.lock` default group. Each global-lock tool is
/// resolved by its **pinned digest** (the lock's `pinned` identifier), offline,
/// against the local object store — the same model as the project tier. The
/// `current` symlink is a **separate abstraction** (mutated only by
/// install/uninstall/select, targeted at devcontainer/IDE stable-anchor use)
/// and is deliberately NOT consulted here: `ocx --global upgrade` re-pins the
/// lock and the exported env follows immediately, with no select step.
///
/// A tool that is in the global lock but not yet materialised locally (e.g.
/// added then the object store was cleaned) fails the offline lookup and is
/// silently skipped — the login exporter must never block a shell.
///
/// GC: global lock-pinned packages are kept reachable by `clean`'s implicit
/// `$OCX_HOME/ocx.lock` root (see `tasks::clean::collect_project_roots`), not
/// by `current` back-refs — so dropping the `current` dependency here does not
/// expose them to garbage collection.
///
/// Returns `Ok(None)` when there is no global `ocx.lock`, or no global tool
/// resolves locally. A real failure (corrupt lock, composition error) returns
/// `Err`. The caller treats the global tier as lenient — it maps BOTH `Ok(None)`
/// and `Err` to a no-op empty env (exit 0) on the report and eval-safe paths
/// alike (logging the `Err` at `debug`); a corrupt global lock surfaces via the
/// lock-rewriting commands, not this read-only exporter. Never exit 74.
///
/// # Offline guarantee
///
/// Resolution goes through `manager().offline_view(...)` — it MUST NOT contact
/// the registry regardless of `--remote`. This is the §4 login-exporter
/// guarantee: `eval "$(ocx --global env --shell=sh)"` runs on every shell
/// start and must never hit the network, install, or hang.
///
/// # Errors
///
/// Propagates `resolve_env` / index errors.  Never contacts the network.
pub(crate) async fn resolve_global_pinned_env(
    context: &crate::app::Context,
) -> anyhow::Result<Option<(Vec<Entry>, usize)>> {
    let home = context.file_structure().root();
    let global_config = home.join("ocx.toml");
    let global_lock_path = lock_path_for(&global_config);

    let Some(lock) = ProjectLock::from_path(&global_lock_path).await? else {
        return Ok(None);
    };

    // Offline-only manager clone: MUST NOT contact the registry regardless
    // of `--remote` (architect boundary; §4 login-path guarantee).
    let manager = context.manager().offline_view(context.local_index().clone());
    let platforms = platforms_or_default(&[]);

    let mut infos = Vec::new();
    for tool in &lock.tools {
        if tool.group != DEFAULT_GROUP {
            continue;
        }
        // Resolve the lock entry to its host-platform identifier offline
        // against the local object store. V1 ([`LockedResolution::LegacyIndex`]):
        // the lock pins the ImageIndex manifest digest, so `find` walks the
        // (already-local) OCI chain from it. V2 ([`LockedResolution::PerPlatform`]):
        // reconstruct `repository`+host leaf and find that directly.
        let identifier: ocx_lib::oci::Identifier = match &tool.resolution {
            ocx_lib::project::LockedResolution::LegacyIndex(pinned) => pinned.clone().into(),
            ocx_lib::project::LockedResolution::PerPlatform {
                repository,
                platforms: leaves,
            } => {
                // V2: reconstruct the host-platform leaf directly (host key →
                // `"any"` fallback across the supported set). Absent leaf →
                // skip silently (the login exporter must never block a shell).
                let Some(leaf) = platforms
                    .iter()
                    .find_map(|platform| ocx_lib::project::lookup_host_leaf(leaves, platform))
                else {
                    continue;
                };
                repository.clone_with_digest(leaf.clone())
            }
        };
        match manager.find(&identifier, platforms.clone()).await {
            Ok(info) => infos.push(Arc::new(info)),
            // Pinned package not materialised locally — skip silently
            // (the login exporter must never block a shell).
            Err(_) => continue,
        }
    }

    if infos.is_empty() {
        return Ok(None);
    }

    // Patch overlays apply offline: companions are already installed locally and
    // `offline_view` preserves the patch tier (the network alone is disabled).
    // Return the companion-overlay boundary so `--show-patches` can annotate
    // companion entries on the global path, exactly as on the project path.
    let (entries, patch_start) = manager.resolve_env_with_patch_boundary(&infos, false).await?;
    Ok(Some((entries, patch_start)))
}
