// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx env` — toolchain-tier composed-env command.
//!
//! Reads the in-scope `ocx.toml` + `ocx.lock` (project tier) or resolves the
//! global toolchain's installed `current` set offline (under `--global`) and
//! emits the composed environment for the selected group(s).
//!
//! `-g/--group` scopes composition: omitted → the top-level `[tools]` table;
//! `-g <group>` → only that group; `-g default -g lint` → both; `-g all` →
//! `default` + every declared `[group.*]`. (Omitting `-g` yields the default
//! group only, like `ocx run` — unlike `ocx pull`, which warms every group
//! when `-g` is omitted.) The reserved names `all`/`default` are rejected as
//! literal `[group.*]` keys at config parse time.
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
//!   abstraction and is NOT consulted (so `ocx --global update` takes effect
//!   with no select step). The §4 login exporter
//!   `eval "$(ocx --global env --shell=sh)"` runs on every shell start — it
//!   MUST NOT contact the registry, install, or hang. A pinned tool not
//!   materialised locally ⇒ silently skipped. The global tier is LENIENT about
//!   AVAILABILITY: nothing configured ("no lock / nothing local") OR a
//!   corrupt/stale `$OCX_HOME/ocx.lock` yields an **empty env** (exit 0) — a
//!   corrupt lock surfaces loudly via the commands that rewrite it
//!   (`ocx --global lock`/`add`/`update`), not via this read-only exporter.
//!   But leniency stops at SECURITY: once a toolchain resolves, its patch overlay
//!   is composed with project-tier strictness, so a C7 fail-closed failure (a
//!   `required`/`system_required` companion missing) PROPAGATES rather than
//!   silently dropping an operator-mandated overlay. This does not depend on
//!   `--shell`, which only selects the output FORMAT, never the error semantics.
//!   Still offline, never installs, never hangs. (The PROJECT tier stays strict —
//!   see below.)
//! - **project** (no `--global`) routes through `load_project_with_lock` +
//!   `compose_tool_set`. Materialization is gated by the `--[no-]pull` pair
//!   (`options::Pull`, eager default): the default runs the **single batched**
//!   `find_or_install_all` over all composed identifiers (mirrors `run.rs` — a
//!   present lock-pinned tool resolves locally with no network; only a genuine
//!   miss falls through to pull). `--no-pull` opts out: it probes the local
//!   store through an offline `PackageManager` clone, warning + omitting a
//!   not-materialised tool (mirrors `direnv export`), and never contacts the
//!   registry. The global tier ignores the pair — it never installs by contract.
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
    oci::{PinnedIdentifier, Platform},
    package::metadata::{BinaryName, EntrypointName, env::entry::Entry},
    package_manager::PatchProvenance,
    project::{ALL_GROUP, DEFAULT_GROUP, ProjectLock, compose_tool_set, expand_all_keyword, lock::lock_path_for},
};

use crate::{
    api,
    app::project_context::load_project_with_lock,
    conventions::{emit_lines, export_ci, platform_or_default, resolve_ci_arg, resolve_shell_arg},
    options,
};

/// Emit the composed environment for the in-scope toolchain.
///
/// Reads `ocx.toml` + `ocx.lock` (project tier, CWD-walk / `--project` /
/// `OCX_PROJECT`) or resolves the global toolchain's offline `current` set
/// (when `--global` is set), composes the selected group(s)' env, and writes it
/// to stdout.
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
/// # Materialization (`--pull` / `--no-pull`)
///
/// By default this command installs any missing tool before composing: a
/// lock-pinned tool already in the object store resolves locally with no
/// network, and only a genuine miss falls through to the registry to
/// materialise it. Pass `--no-pull` to skip that fallback — missing tools are
/// then reported on stderr (`run \`ocx pull\` to fetch`) and omitted, and the
/// command never contacts the registry. The flags use POSIX last-wins
/// semantics; the global tier never installs regardless.
///
/// # Exit codes
///
/// - 0 (`Success`): under `--global`, nothing configured (no `$OCX_HOME/ocx.lock`,
///   or nothing resolves locally) or a corrupt/stale global lock yields an empty
///   env on the report path AND the eval-safe `--shell` path. The global tier is
///   lenient about availability; a corrupt lock surfaces via `ocx --global lock`/
///   `add`/`update`, not this read-only exporter. A C7 patch fail-closed failure
///   (a `required`/`system_required` companion missing on a resolved global
///   toolchain) instead PROPAGATES as a non-zero exit — the same fail-closed
///   posture as the project tier.
/// - 64 (`UsageError`): no `ocx.toml` in scope (project tier); unknown
///   `--group` (project tier); empty `--group` comma segment; more than one
///   `--platform` value; `--shell`
///   (bare) with undetectable `$SHELL`/parent; `--global` ⟂ `--project`
///   (clap `conflicts_with`, mapped to EX_USAGE 64 — NOT exit 2). The global
///   tier is lenient: an unknown `--group` matches nothing and yields an
///   empty env (exit 0).
/// - 78 (`ConfigError`): `ocx.lock` absent (project tier).
/// - 65 (`DataError`): `ocx.lock` stale (project tier).
#[derive(Parser)]
pub struct ToolchainEnv {
    /// Restrict the env composition to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table.
    /// The reserved name `all` expands to `default` + every declared
    /// `[group.*]`. When omitted, scope is exactly the top-level
    /// `[tools]` table, not every group.
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

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

    #[clap(flatten)]
    platform: options::PlatformOption,

    #[clap(flatten)]
    pull: options::Pull,

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
        // Reject empty comma segments (`-g ci,,lint`) BEFORE any tier split or
        // config load (parse-level, mirrors `run`/`pull` Phase A).
        crate::app::project_context::ensure_group_segments_nonempty(&self.groups)?;

        // `None` → default-format path; `Some(s)` → eval-safe emit.
        let shell = resolve_shell_arg(self.shell)?;
        // Resolve `--ci` early so a bare-`--ci` autodetect failure surfaces as a
        // usage error before the (potentially slow) entry resolution. `--ci` is
        // mutually exclusive with `--shell` (clap `conflicts_with`), so at most
        // one of these is set.
        let ci = resolve_ci_arg(self.ci)?;

        // The single target platform to compose for. `env` produces ONE
        // environment (a single PATH cannot hold two platforms' tool dirs);
        // clap's `Option<Platform>` `--platform` field already enforces at
        // most one value, so the default-to-host fallback is all that's left.
        let target = platform_or_default(self.platform.platform.clone());

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
        let (entries, patch_start, provenance, admitted_binaries, admitted_entrypoints) = if context.global() {
            // OFFLINE lock-pinned resolution (ADR D5, handshake §1/§4).
            // The login exporter runs this every shell — never network/install.
            //
            // The global tier is LENIENT about AVAILABILITY: "no usable global
            // toolchain" is a normal empty result, not an error. A missing global
            // lock, a corrupt/unreadable lock, or a toolchain whose packages are
            // not materialised locally all yield `Ok(None)` → empty env (exit 0).
            // A corrupt lock also surfaces loudly via the commands that rewrite it
            // (`ocx --global lock`/`add`/`update`), so this read-only exporter
            // need not.
            //
            // But leniency stops at SECURITY: once a global toolchain resolves,
            // its patch overlay is composed with the SAME strictness as the
            // project tier. A C7 fail-closed failure (a `required` /
            // `system_required` companion missing, or a corrupt required
            // descriptor) — or any other env-composition failure of the resolved
            // toolchain — propagates as `Err`, so an operator-mandated overlay can
            // never be silently dropped on the global tier. This is INDEPENDENT of
            // `--shell`, which only selects the output FORMAT, never the error
            // semantics. It stays offline, never installs, never hangs. The
            // PROJECT tier (the `else` arm) stays strict throughout: an explicit
            // project's missing/stale/corrupt `ocx.lock` IS an error.
            match resolve_global_pinned_env(&context, &target, self.groups.as_slice()).await {
                Ok(Some((entries, patch_start, provenance, binaries, entrypoints))) => {
                    (entries, patch_start, provenance, binaries, entrypoints)
                }
                Ok(None) => (Vec::new(), 0, Vec::new(), Vec::new(), Vec::new()),
                Err(error) => return Err(error),
            }
        } else {
            // Project tier: resolve + a SINGLE batched install (mirror run.rs).
            let ctx = load_project_with_lock(&context).await?;

            // Validate requested groups against the loaded config (`all` is
            // expanded below; unknown → exit 64).
            crate::app::project_context::ensure_groups_known(&self.groups, &ctx.config)?;

            // Expand `all` in place, then promote an empty scope to the default
            // group — identical to `ocx run` Phase C.
            let mut expanded = expand_all_keyword(&self.groups, &ctx.config);
            if expanded.is_empty() {
                expanded = vec![DEFAULT_GROUP.to_owned()];
            }

            // Project tier is strict: a tool that ships no leaf for `target`
            // surfaces `NoHostLeaf` (exit 78) from `compose_tool_set`.
            let composed = compose_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[], &target)?;

            let manager = context.manager();
            let infos: Vec<Arc<ocx_lib::package::install_info::InstallInfo>> = if self.pull.enabled(true) {
                // Default: resolve + install-on-miss, a SINGLE batched install
                // (mirror run.rs). A lock-pinned tool already in the object
                // store resolves locally with no network (cache-first on the
                // pinned digest); only a genuine miss falls through to the
                // registry to materialise it.
                let identifiers: Vec<ocx_lib::oci::Identifier> =
                    composed.into_iter().map(|tool| tool.identifier).collect();
                manager
                    .find_or_install_all(identifiers, target.clone(), context.concurrency())
                    .await?
                    .into_iter()
                    .map(Arc::new)
                    .collect()
            } else {
                // `--no-pull`: probe the local object store only, never the
                // registry. An offline `PackageManager` clone forces any
                // incidental index lookup (V1 legacy locks walk the cached
                // index->manifest chain; V2 locks read the pinned leaf
                // directly) to stay local, so a not-materialised tool is warned
                // about on stderr and omitted (mirrors `direnv export`); any
                // other find failure stays a hard error.
                let offline = manager.offline_view(context.local_index().clone());
                let mut infos = Vec::with_capacity(composed.len());
                for tool in &composed {
                    match offline.find(&tool.identifier, target.clone()).await {
                        Ok(info) => infos.push(Arc::new(info)),
                        Err(ocx_lib::package_manager::error::PackageErrorKind::NotFound) => {
                            context.ui().warn(format!(
                                "{} not installed; run `ocx pull` to fetch or drop --no-pull",
                                tool.binding
                            ));
                        }
                        Err(kind) => {
                            return Err(ocx_lib::package_manager::error::Error::FindFailed(vec![
                                ocx_lib::package_manager::error::PackageError::new(tool.identifier.clone(), kind),
                            ])
                            .into());
                        }
                    }
                }
                infos
            };
            // Per-package opt-out from the in-scope project `ocx.toml`, plus
            // its `[env]` and each selected group's `[env]` (stages 4-5 —
            // `ocx env` has no `--env`, so stage 6 is empty here). Uniformity
            // with `ocx run` is structural: both append to the same entry
            // vector, so what this command prints is what `ocx run` applies.
            let scope = ocx_lib::package_manager::PatchScope::Project {
                no_patches: ctx.config.no_patches_repositories(),
                env: crate::app::project_context::project_env_entries(&ctx.config, &ctx.config_path, &expanded),
            };
            let (entries, patch_start, provenance, attribution) =
                manager.resolve_env_with_attribution(&infos, false, scope).await?;
            (
                entries,
                patch_start,
                provenance,
                attribution.binaries,
                attribution.entrypoints,
            )
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
                // Annotate origin when `--show-patches` is enabled. The overlay
                // occupies `[patch_start .. patch_start + provenance.len())`;
                // the project/group `[env]` stages follow it, so the bound
                // check is what keeps a project entry from being mislabelled as
                // a companion (and from indexing past `provenance`).
                let source = if self.show_patches {
                    i.checked_sub(patch_start)
                        .and_then(|offset| provenance.get(offset))
                        .map(|prov| api::data::env::EntrySource::Patch {
                            rule: prov.rule_match.clone(),
                            companion: prov.companion.to_string(),
                        })
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

        let binaries = api::data::env::BinaryAttribution::from_pairs(&admitted_binaries);
        let entrypoints = api::data::env::BinaryAttribution::from_pairs(&admitted_entrypoints);

        context
            .api()
            .report(&api::data::env::EnvVars::new(env_data, binaries, entrypoints))?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Resolve the global toolchain's **lock-pinned** set into env entries.
///
/// Source = `$OCX_HOME/ocx.lock`, scoped to `groups` via
/// [`selected_groups_global`] (empty → the default group; `all` → every group
/// present in the lock). Each global-lock tool is
/// resolved by its **pinned digest** (the lock's `pinned` identifier), offline,
/// against the local object store — the same model as the project tier. The
/// `current` symlink is a **separate abstraction** (mutated only by
/// install/uninstall/select, targeted at devcontainer/IDE stable-anchor use)
/// and is deliberately NOT consulted here: `ocx --global update` re-pins the
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
/// The global `ocx.toml`'s own `[env]` / `[group.<name>.env]` is read
/// independently of the lock and applies whenever the global tier is the one
/// being resolved (ADR `adr_project_env_declaration.md` Q2). A declaration's
/// effect never depends on package availability, so a global file carrying only
/// `[env]` — or one whose locked tools are not materialised — still composes.
///
/// Returns `Ok(None)` only when NEITHER axis contributes: no global tool
/// resolves locally (no `ocx.lock`, a corrupt/unreadable one, or nothing
/// materialised) AND the global file declares no `[env]`. The caller maps
/// `Ok(None)` to an empty env (exit 0) — a corrupt lock surfaces via the
/// lock-rewriting commands (`ocx --global lock`/`add`/`update`), not this
/// read-only exporter.
///
/// Returns `Err` ONLY when a resolved toolchain's patch overlay / env composition
/// fails — most importantly a C7 fail-closed failure (a `required` /
/// `system_required` companion missing, or a corrupt required descriptor). The
/// caller PROPAGATES that error: the global tier fails closed on a mandated
/// overlay exactly as the project tier does. Never exit 74.
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
/// Propagates a `resolve_env_with_attribution` failure (C7 fail-closed or env
/// composition) for a resolved toolchain. Benign toolchain faults (no/corrupt
/// lock, nothing materialised) return `Ok(None)`, not `Err`. Never contacts the
/// network.
///
/// The last two tuple elements are the admitted-set `binaries`/`entrypoints`
/// claim attribution (`AdmittedBinaries::binaries` / `::entrypoints`,
/// `adr_declared_binaries_metadata.md` §4 Decision A), returned as raw
/// `(identifier, name)` pairs rather than the `ocx_lib` struct so this
/// `ocx_cli`-local signature only names already-public leaf types.
pub(crate) async fn resolve_global_pinned_env(
    context: &crate::app::Context,
    target: &Platform,
    groups: &[String],
) -> anyhow::Result<
    Option<(
        Vec<Entry>,
        usize,
        Vec<PatchProvenance>,
        Vec<(PinnedIdentifier, BinaryName)>,
        Vec<(PinnedIdentifier, EntrypointName)>,
    )>,
> {
    let home = context.file_structure().root();
    let global_config = home.join("ocx.toml");
    let global_lock_path = lock_path_for(&global_config);

    // Per-package opt-out AND the global file's own `[env]` / group `[env]`,
    // read BEFORE the lock: a declared `[env]` applies to the global tier on
    // its own authority (Q2), so its effect must not hinge on whether any
    // package happens to be locked and materialised. A missing or unparseable
    // file yields an empty opt-out and an empty env (lenient — the login
    // exporter must never fail on a malformed global config, matching this
    // path's overall posture).
    //
    // Strict isolation (Q2): this env belongs to the global tier alone. It
    // applies to `ocx --global env` / `ocx --global run` and never composes
    // into a project-tier resolution — the two tiers are resolved by disjoint
    // branches of `execute`, never unioned.
    let (no_patches, project_env) = match ocx_lib::project::ProjectConfig::from_path(&global_config).await {
        Ok(config) => {
            // Expand `all` against the CONFIG's groups, not the lock's: a group
            // that declares only `[group.<name>.env]` and no tools has no lock
            // entry, but its env is still selected by `-g all`.
            let mut env_groups = expand_all_keyword(groups, &config);
            if env_groups.is_empty() {
                env_groups = vec![DEFAULT_GROUP.to_owned()];
            }
            let env = crate::app::project_context::project_env_entries(&config, &global_config, &env_groups);
            (config.no_patches_repositories(), env)
        }
        Err(_) => (std::collections::BTreeSet::new(), Vec::new()),
    };

    // Offline-only manager clone: MUST NOT contact the registry regardless
    // of `--remote` (architect boundary; §4 login-path guarantee).
    let manager = context.manager().offline_view(context.local_index().clone());

    // A missing OR corrupt/unreadable global lock is benign here — the login
    // exporter stays lenient (no pinned tools contribute). Only a patch-enforcement
    // / env-composition failure of a RESOLVED toolchain (the
    // `resolve_env_with_attribution` call below) propagates. A corrupt lock
    // surfaces loudly via the commands that rewrite it (`ocx --global lock`/
    // `add`/`update`).
    let lock = match ProjectLock::from_path(&global_lock_path).await {
        Ok(lock) => lock,
        Err(error) => {
            tracing::debug!("global lock unreadable; emitting declared env only: {error:#}");
            None
        }
    };

    let mut infos = Vec::new();
    if let Some(lock) = &lock {
        let selected_groups = selected_groups_global(groups, lock);
        for tool in &lock.tools {
            // Global tier is lenient: a group named on the command line that no
            // lock entry carries simply matches nothing (no error, empty env).
            if !selected_groups.iter().any(|g| g == &tool.group) {
                continue;
            }
            // Resolve the lock entry to its `target`-platform identifier offline
            // against the local object store: reconstruct `repository`+target
            // leaf and find that directly. Absent OR ambiguous leaf → skip
            // silently (global tier is lenient; the login exporter must never
            // block a shell on a disambiguation it cannot perform).
            let ocx_lib::oci::Selection::Found((leaf, _key)) =
                ocx_lib::project::lookup_host_leaf(&tool.platforms, target)
            else {
                continue;
            };
            let identifier: ocx_lib::oci::Identifier = tool.repository.clone_with_digest(leaf.clone());
            match manager.find(&identifier, target.clone()).await {
                Ok(info) => infos.push(Arc::new(info)),
                // Pinned package not materialised locally — skip silently
                // (the login exporter must never block a shell).
                Err(_) => continue,
            }
        }
    }

    // Nothing to say only when NEITHER axis contributes. A declared `[env]`
    // with no usable package still composes: its effect is not conditional on
    // package availability.
    if infos.is_empty() && project_env.is_empty() {
        return Ok(None);
    }

    // Patch overlays apply offline: companions are already installed locally and
    // `offline_view` preserves the patch tier (the network alone is disabled).
    // Return the companion-overlay boundary so `--show-patches` can annotate
    // companion entries on the global path, exactly as on the project path.
    let scope = ocx_lib::package_manager::PatchScope::Project {
        no_patches,
        env: project_env,
    };
    let (entries, patch_start, provenance, attribution) =
        manager.resolve_env_with_attribution(&infos, false, scope).await?;
    Ok(Some((
        entries,
        patch_start,
        provenance,
        attribution.binaries,
        attribution.entrypoints,
    )))
}

/// Resolve the raw `-g` values into the concrete global-tier group set.
///
/// - empty → `[default]` (unchanged default-group behaviour)
/// - contains `all` → `default` + every distinct named group present in the
///   lock, sorted
/// - otherwise → the raw values verbatim
///
/// Used only for a membership test against `tool.group`, so order and
/// duplicates past the `all` case are irrelevant. Unknown names simply match
/// no tool — the global tier is lenient (no error).
///
// ponytail: enumerate the `all` set from the lock, not `ocx.toml` — the global
// exporter never reads config, and a declared-but-empty group contributes
// nothing to the env anyway, so lock-derived groups are the complete set.
fn selected_groups_global(raw: &[String], lock: &ProjectLock) -> Vec<String> {
    if raw.is_empty() {
        return vec![DEFAULT_GROUP.to_owned()];
    }
    if !raw.iter().any(|g| g == ALL_GROUP) {
        return raw.to_vec();
    }
    let mut named: Vec<String> = lock
        .tools
        .iter()
        .map(|tool| tool.group.clone())
        .filter(|group| group != DEFAULT_GROUP)
        .collect();
    named.sort();
    named.dedup();
    let mut groups = vec![DEFAULT_GROUP.to_owned()];
    groups.extend(named);
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn platform(s: &str) -> Platform {
        s.parse().expect("valid platform")
    }

    /// No `--platform` → the host native platform (or `any` when unsupported).
    #[test]
    fn platform_defaults_to_host() {
        // Never errors; the concrete value depends on the build target.
        platform_or_default(None);
    }

    /// `--platform` parses at the clap layer to exactly one value; clap itself
    /// rejects a second occurrence of the flag (Option<T>, not Vec<T>).
    #[test]
    fn parses_platform_flag() {
        let env = ToolchainEnv::try_parse_from(["env", "--platform", "linux/arm64"]).unwrap();
        assert_eq!(env.platform.platform, Some(platform("linux/arm64")));
    }

    /// `-g` is repeatable and comma-delimited: `-g ci,lint -g release` → 3.
    #[test]
    fn parses_repeatable_comma_group_flag() {
        let env = ToolchainEnv::try_parse_from(["env", "-g", "ci,lint", "-g", "release"]).unwrap();
        assert_eq!(
            env.groups,
            vec!["ci".to_owned(), "lint".to_owned(), "release".to_owned()]
        );
    }

    /// `ocx env` installs on miss by default; `--no-pull` opts out. Pins the
    /// eager default at the parse/wiring site so a default-flip regresses here
    /// in milliseconds instead of only in the acceptance suite.
    #[test]
    fn pull_flags_flatten_with_eager_default() {
        let default = ToolchainEnv::try_parse_from(["env"]).unwrap();
        assert!(
            default.pull.enabled(true),
            "env default must be eager (install on miss)"
        );

        let opt_out = ToolchainEnv::try_parse_from(["env", "--no-pull"]).unwrap();
        assert!(
            !opt_out.pull.enabled(true),
            "--no-pull must opt out of the install fallback"
        );
    }

    // ── selected_groups_global ────────────────────────────────────────────────

    fn lock_with_groups(groups: &[&str]) -> ProjectLock {
        use ocx_lib::oci::{Digest, Identifier};
        use ocx_lib::project::{LockMetadata, LockVersion, LockedTool};
        let tools = groups
            .iter()
            .enumerate()
            .map(|(i, group)| {
                let mut platforms = std::collections::BTreeMap::new();
                platforms.insert(
                    "linux/amd64".to_owned(),
                    Digest::Sha256(std::iter::repeat_n('a', 64).collect()),
                );
                LockedTool {
                    name: format!("tool{i}"),
                    group: (*group).to_owned(),
                    repository: Identifier::new_registry(format!("tool{i}"), "ocx.sh"),
                    platforms,
                }
            })
            .collect();
        ProjectLock {
            metadata: LockMetadata {
                lock_version: LockVersion::V3,
                declaration_hash_version: 1,
                declaration_hash: format!("sha256:{}", std::iter::repeat_n('0', 64).collect::<String>()),
                generated_by: "ocx test".into(),
                generated_at: "2026-04-24T00:00:00Z".into(),
            },
            tools,
        }
    }

    /// Empty `-g` → the default group only.
    #[test]
    fn selected_groups_global_empty_is_default() {
        let lock = lock_with_groups(&["default", "lint"]);
        assert_eq!(selected_groups_global(&[], &lock), vec!["default".to_owned()]);
    }

    /// `-g all` → default + every distinct named lock group, sorted.
    #[test]
    fn selected_groups_global_all_expands_from_lock() {
        let lock = lock_with_groups(&["default", "lint", "ci", "lint"]);
        assert_eq!(
            selected_groups_global(&["all".to_owned()], &lock),
            vec!["default".to_owned(), "ci".to_owned(), "lint".to_owned()]
        );
    }

    /// Named groups pass through verbatim (unknown names allowed — lenient tier).
    #[test]
    fn selected_groups_global_passthrough() {
        let lock = lock_with_groups(&["default", "lint"]);
        assert_eq!(
            selected_groups_global(&["lint".to_owned(), "missing".to_owned()], &lock),
            vec!["lint".to_owned(), "missing".to_owned()]
        );
    }
}
