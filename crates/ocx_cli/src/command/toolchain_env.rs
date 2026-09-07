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
//! group only, like `ocx exec` — unlike `ocx pull`, which warms every group
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
//!   `find_or_install_all` over all composed identifiers (mirrors `toolchain_exec.rs` — a
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
    env,
    oci::Platform,
    package::metadata::env::entry::Entry,
    package_manager::{
        AdmittedClaims, PatchProvenance,
        composer::{ComposeRequest, ComposeRoots, Materialization},
    },
    project::{
        ALL_GROUP, DEFAULT_GROUP, Origin, ProjectLock, ResolvedTool, compose_tool_set, expand_all_keyword,
        lazy_mode_for_tool, lock::lock_path_for,
    },
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
    #[clap(flatten)]
    pub groups: options::GroupSelection,

    #[clap(flatten)]
    pub env: options::EnvOverride,

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

    /// Top tier of the `lazy-mode` ladder for every tool this command composes.
    ///
    /// `always` composes a tool as a generated shim: its declared names reach
    /// `PATH` immediately and its content downloads on first use. The shim
    /// directory sits *below* the tool's own `entrypoints/` and `bin/` in the
    /// composed `PATH`, so the same exported environment stops routing through
    /// it once the first invocation has materialized the package.
    ///
    /// Combined with `--no-pull`, a tool whose metadata is not already local
    /// is reported on stderr and omitted, exactly as a not-materialised tool
    /// is on the eager path.
    #[clap(flatten)]
    lazy_mode: options::LazyMode,

    /// Top tier of the `pinned` ladder for the environment this command
    /// composes (C-055, C-066).
    #[clap(flatten)]
    pinned: options::Pinned,

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
    /// The materialization policy this command applies to a tool whose
    /// `lazy-mode` resolved to `never`.
    ///
    /// The `--[no-]pull` pair, restated as the library's vocabulary: the eager
    /// default installs on a miss, `--no-pull` probes the local store only and
    /// warns-and-omits a miss. Folding the choice into one value is what lets
    /// the lazy split reuse this command's existing policy instead of growing a
    /// second one beside it — and it is the same value that decides whether a
    /// *deferred* tool whose metadata is not local is an error or an omission
    /// (S-009).
    ///
    /// The global tier ignores this: it never installs by contract.
    fn materialization(&self) -> Materialization {
        if self.pull.enabled(true) {
            Materialization::Install
        } else {
            Materialization::LocalOnly
        }
    }

    /// Report what only this command can report about the deferred tools it
    /// composed: the omissions (S-009) and the advisories (C-015 (d)).
    ///
    /// Omissions go to stderr through the user interface, in the same shape and
    /// with the same remedy as the eager `--no-pull` warning above them, so a
    /// caller cannot tell from the message which half of the split dropped a
    /// tool — only that it is absent and how to get it.
    ///
    /// Advisories are raised only for **deferred** tools, which is the fact
    /// that makes C-015 (d)'s "deferred tool only" clause testable: an
    /// eagerly-materialized tool never reaches `prepare_lazy`, so it can
    /// contribute none.
    fn report_deferred(&self, context: &crate::app::Context, composed: &ComposeRoots, tools: &[ResolvedTool]) {
        for omission in &composed.omitted {
            // Named by binding, not by identifier: this is the same sentence
            // the eager path printed before the lazy split existed, and a user
            // reads `ocx.toml` in binding names.
            let name = tools
                .iter()
                .find(|tool| tool.identifier == omission.identifier)
                .map_or_else(|| omission.identifier.to_string(), |tool| tool.binding.clone());
            context.ui().warn(format!(
                "{name} not installed; run `ocx pull` to fetch or drop --no-pull"
            ));
        }
        for advisory in &composed.advisories {
            context.ui().warn(advisory.to_string());
        }
    }

    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Reject empty comma segments (`-g ci,,lint`) BEFORE any tier split or
        // config load (parse-level, mirrors `run`/`pull` Phase A).
        crate::app::project_context::ensure_group_segments_nonempty(self.groups.names())?;

        // Same parse-level class as the group check, so it runs beside it and
        // before the slower emit-channel resolution below. A relative `:path`
        // value anchors to the invocation directory, exactly as under
        // `ocx exec` — and it is resolved to an absolute value here, so an
        // emitted export line means the same thing wherever it is evaluated.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;

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

        // Advisories raised by the deferred half of the project tier, kept for
        // the structured report below (C-015: warning-only, but readable —
        // a channel that only reaches a log is not a channel).
        let mut advisories: Vec<ocx_lib::package_manager::LazyAdvisory> = Vec::new();

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
        let (mut entries, patch_start, provenance, attribution) = if context.global() {
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
            match resolve_global_pinned_env(
                &context,
                &target,
                self.groups.names(),
                &env_overrides,
                self.pinned.pinned(),
            )
            .await
            {
                Ok(Some((entries, patch_start, provenance, claims))) => (entries, patch_start, provenance, claims),
                Ok(None) => (Vec::new(), 0, Vec::new(), AdmittedClaims::default()),
                Err(error) => return Err(error),
            }
        } else {
            // Project tier: resolve + a SINGLE batched install (mirror run.rs).
            let ctx = load_project_with_lock(&context).await?;

            // Validate requested groups against the loaded config (`all` is
            // expanded below; unknown → exit 64).
            crate::app::project_context::ensure_groups_known(self.groups.names(), &ctx.config)?;

            // Expand `all` in place, then promote an empty scope to the default
            // group — identical to `ocx exec` Phase C.
            let mut expanded = expand_all_keyword(self.groups.names(), &ctx.config);
            if expanded.is_empty() {
                expanded = vec![DEFAULT_GROUP.to_owned()];
            }

            // Project tier is strict: a tool that ships no leaf for `target`
            // surfaces `NoHostLeaf` (exit 78) from `compose_tool_set`.
            let composed = compose_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[], &target)?;

            // One entry point for both halves of the lazy split: a tool whose
            // resolved `lazy-mode` is `always` gets a generated shim tree and
            // reaches `PATH` without its content; every other tool takes this
            // command's own materialization policy, unchanged.
            //
            // `--no-pull` routes through an offline `PackageManager` clone so
            // any incidental index lookup (V1 legacy locks walk the cached
            // index->manifest chain) stays local — a not-materialised tool is
            // warned about and omitted, never fetched.
            let manager = context.manager();
            let materialization = self.materialization();
            let requests: Vec<ComposeRequest> = composed
                .iter()
                .map(|tool| ComposeRequest {
                    identifier: tool.identifier.clone(),
                    mode: lazy_mode_for_tool(
                        &ctx.config,
                        &tool.identifier,
                        group_of(&tool.origin),
                        self.lazy_mode.mode(),
                    ),
                })
                .collect();
            let composing = match materialization {
                Materialization::LocalOnly => manager.offline_view(context.local_index().clone()),
                _ => manager.clone(),
            };
            let roots = composing
                .compose_roots(&requests, &target, materialization, context.concurrency())
                .await?;
            self.report_deferred(&context, &roots, &composed);
            advisories = roots.advisories;
            let infos: Vec<Arc<ocx_lib::package::install_info::InstallInfo>> = roots.roots;
            // Per-package opt-out from the in-scope project `ocx.toml`, plus
            // its `[env]`, each selected group's `[env]`, and `--env` last —
            // stages 4-6, assembled exactly as `ocx exec` assembles them.
            // Uniformity is structural: both append to the same entry vector,
            // so what this command prints IS what `ocx exec` applies. That
            // equivalence is the reason `--env` belongs here at all — a caller
            // that builds an argv array must be able to export the environment
            // it would otherwise execute in.
            let mut project_env = ocx_lib::project::project_env_entries(&ctx.config, &ctx.config_path, &expanded);
            project_env.extend(env_overrides);
            // C-065/C-070: the groups this invocation selected, healed and
            // probed once inside `resolve_env_with_attribution`. `ocx env` and
            // `ocx exec` build this identically — that is what makes item 11's
            // parity oracle (`env` ≡ `exec`) hold in the link lane too.
            let toolchain = crate::app::project_context::toolchain_links(
                &context,
                &ctx.config_path,
                &ctx.config,
                &ctx.lock,
                &expanded,
                self.pinned.pinned(),
            )
            .await?;
            let scope = ocx_lib::package_manager::EnvScope::Project {
                no_patches: ctx.config.no_patches_repositories(),
                env: project_env,
                toolchain: Some(Box::new(toolchain)),
            };
            manager
                .resolve_env_with_attribution(&infos, false, scope, &target)
                .await?
        };

        // W-11: settle each `list` entry's separator before any downstream
        // branch (`--ci`, `--shell`, structured report) reads `entries`.
        // Neither resolution path (global lock-pinned or project) keeps a
        // second forwarded vector of its own — `project_env` above is moved
        // into the scope that produced `entries` — so a single-vector pass
        // covers both branches.
        env::reconcile_list_separators(entries.iter_mut())?;

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
        // The companion-overlay region, for `--show-patches` attribution. The
        // overlay is the MIDDLE region — the project / group `[env]` stages and
        // `--env` follow it — so the bound-checked accessor is what keeps a
        // project entry from being mislabelled as a companion (and from indexing
        // past `provenance`).
        let overlay = ocx_lib::package_manager::PatchOverlay::new(patch_start, &provenance);
        let env_data: Vec<api::data::env::EnvEntry> = entries
            .into_iter()
            .enumerate()
            .map(|(i, e)| {
                let source = self
                    .show_patches
                    .then(|| overlay.provenance_for(i))
                    .flatten()
                    .map(|prov| api::data::env::EntrySource::Patch {
                        rule: prov.rule_match.clone(),
                        companion: prov.companion.to_string(),
                    });
                api::data::env::EnvEntry {
                    key: e.key,
                    value: e.value,
                    kind: e.kind,
                    separator: e.separator,
                    source,
                }
            })
            .collect();

        // No synthetic PATHEXT entry: the Windows launcher is now a native
        // `<name>.exe` shim, and `.EXE` is unconditionally in the default
        // Windows PATHEXT — nothing to inject for bare-name resolution.

        // Only where the footgun is: a non-terminal stdout is the
        // `eval "$(ocx env)"` case. See `conventions::not_eval_safe_advisory`.
        if let Some(advisory) = crate::conventions::not_eval_safe_advisory(
            context.api().is_json(),
            std::io::IsTerminal::is_terminal(&std::io::stdout()),
        ) {
            ocx_lib::log::warn!("{advisory}");
        }

        let binaries = api::data::env::BinaryAttribution::from_pairs(&attribution.binaries);
        let entrypoints = api::data::env::BinaryAttribution::from_pairs(&attribution.entrypoints);
        let integrations = api::data::env::IntegrationAttribution::from_pairs(&attribution.integrations);

        context.api().report(
            &api::data::env::EnvVars::new(env_data, binaries, entrypoints, integrations)
                .with_advisories(api::data::env::LazyAdvisoryReport::from_advisories(&advisories)),
        )?;

        Ok(ExitCode::SUCCESS)
    }
}

/// The group tier of the `lazy-mode` ladder for one selected tool, or `None`
/// for a positional package — which has no group and therefore no group tier.
fn group_of(origin: &Origin) -> Option<&str> {
    match origin {
        Origin::Group(name) => Some(name.as_str()),
        Origin::Explicit => None,
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
/// # `pinned_cli` — the CLI tier of the `pinned` ladder (C-007, C-055)
///
/// `options::Pinned::pinned()` from the invocation, or `None` for a caller that
/// has no such flag. `None` means "neither flag was given", never `Some(false)`:
/// it leaves the global `ocx.toml`'s `pinned` key and `OCX_TOOLCHAIN_PINNED`
/// deciding the lane, which is what the per-prompt hook
/// ([`global_prompt_entries`](crate::command::self_group::activate)) passes so
/// that path resolves from the file and environment tiers exactly as before.
///
/// It reaches the ladder through the one resolver every tier uses,
/// [`pinned_for_project`](ocx_lib::package_manager::pinned_for_project), in
/// **both** arms of the config read — a global file that will not parse yields
/// the same absent file tier a file stating nothing would, and a typed
/// `--pinned` still outranks it. `ocx --global exec` already honoured the flag
/// (it routes through the project loader, which is `--global`-aware); this is
/// what stops its sibling from parsing the flag and discarding it.
///
/// This adds **no failure path**: `pinned_for_project` is total, so the
/// never-fail contract above is unchanged.
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
/// The last tuple element is the admitted-set claim attribution
/// ([`AdmittedClaims`] — `binaries`, `entrypoints`, `integrations`;
/// `adr_declared_binaries_metadata.md` §4 Decision A and
/// `adr_package_integrations.md` C-013), passed through as the one public
/// `ocx_lib` struct rather than one raw pair vector per claim kind.
pub(crate) async fn resolve_global_pinned_env(
    context: &crate::app::Context,
    target: &Platform,
    groups: &[String],
    env_overrides: &[Entry],
    pinned_cli: Option<bool>,
) -> anyhow::Result<Option<(Vec<Entry>, usize, Vec<PatchProvenance>, AdmittedClaims)>> {
    let home = context.file_structure().root();
    let global_config = ocx_lib::project::ProjectConfig::global_manifest_path(home);
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
    // The third element is the resolved `pinned` lane (C-007, RUL-104), taken
    // in the same arm as the config it reads: an unparseable global file yields
    // the floor, which is the following lane, exactly as a file stating nothing
    // would.
    let (no_patches, mut project_env, pinned) = match ocx_lib::project::ProjectConfig::from_path(&global_config).await {
        Ok(config) => {
            // Expand `all` against the CONFIG's groups, not the lock's: a group
            // that declares only `[group.<name>.env]` and no tools has no lock
            // entry, but its env is still selected by `-g all`.
            let mut env_groups = expand_all_keyword(groups, &config);
            if env_groups.is_empty() {
                env_groups = vec![DEFAULT_GROUP.to_owned()];
            }
            let env = ocx_lib::project::project_env_entries(&config, &global_config, &env_groups);
            let pinned = ocx_lib::package_manager::pinned_for_project(pinned_cli, &config);
            (config.no_patches_repositories(), env, pinned)
        }
        Err(_) => (
            std::collections::BTreeSet::new(),
            Vec::new(),
            ocx_lib::package_manager::pinned_for_project(pinned_cli, &ocx_lib::project::ProjectConfig::default()),
        ),
    };
    // Stage 6 last, on this tier too. An unparseable global file yields an
    // empty stage 4-5 above, but the caller's own `--env` is not the global
    // file's to lose: it was typed on this invocation and still applies.
    project_env.extend_from_slice(env_overrides);

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
    let mut toolchain = None;
    if let Some(lock) = &lock {
        let selected_groups = selected_groups_global(groups, lock);
        // RUL-104 — `ocx --global pull` renders `$OCX_HOME/toolchain/<group>/
        // <entry>`, so this emitter follows that tree like the other three
        // (C-065). The group set is the **lock-derived** one computed here, not
        // the config-expanded env-group set above: a group declaring only
        // `[group.<g>.env]` and no tools has no links, and C-070 is precisely
        // about not passing the wrong set.
        //
        // The login exporter is contracted never to fail on a corrupt global
        // lock, and `ComposePaths::resolve` raises `ToolchainPath` (exit 78) for
        // a name that cannot become a path component. Filtering those entries
        // out here keeps both contracts: each composes on its digest path
        // (C-067) and the shell still starts.
        toolchain = context
            .manager()
            .toolchain_home(&ocx_lib::file_structure::RenderStampScope::Global, None)
            .ok()
            .map(|home| {
                let mut followable = lock.clone();
                followable
                    .tools
                    .retain(|tool| home.entry(&tool.group, &tool.name).is_ok());
                Box::new(ocx_lib::package_manager::ToolchainLinks {
                    pinned,
                    home,
                    scope: ocx_lib::file_structure::RenderStampScope::Global,
                    lock: followable,
                    groups: selected_groups.clone(),
                })
            });
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
    let scope = ocx_lib::package_manager::EnvScope::Project {
        no_patches,
        env: project_env,
        // RUL-104 — the global tier follows the lane like the other three
        // composing emitters; `None` here only when there is no global lock to
        // derive links from, which is C-067's digest lane by construction.
        toolchain,
    };
    Ok(Some(
        manager
            .resolve_env_with_attribution(&infos, false, scope, target)
            .await?,
    ))
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
    use std::path::Path;

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
        assert_eq!(env.groups.names(), ["ci", "lint", "release"]);
    }

    /// `--env` reaches the composition on this command too, so an exporter can
    /// print the environment the equivalent `ocx exec` would execute in. Parsing
    /// is `options::EnvOverride`'s own contract; this pins only the wiring.
    #[test]
    fn parses_repeatable_env_flag() {
        let env = ToolchainEnv::try_parse_from(["env", "--env", "A=1", "--env", "PATH:path=/opt/bin"]).unwrap();
        let entries = env.env.entries(Path::new("/invocation")).expect("entries");
        assert_eq!(entries.len(), 2, "both occurrences must reach the command");
        assert_eq!(entries[0].key, "A");
        assert_eq!(entries[1].key, "PATH");
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

    // ── The `pinned` ladder's CLI tier on the GLOBAL tier ─────────────────────
    //
    // `ocx --global env` resolves through `resolve_global_pinned_env`, which is
    // a different code path from the project tier's `toolchain_links` — so the
    // CLI tier reaching one proves nothing about the other. These read the lane
    // at the COMMAND surface (`Cli::parse_from` → `execute`) rather than at a
    // function signature, which is what lets them compile against a tree where
    // the resolver takes no CLI tier at all and fail there.

    /// A global file that declares only an `[env]` — every `pinned` tier absent.
    const DECLARED_ENV: &str = "[env]\nWP18 = \"composed\"\n";

    /// The same, with the FILE tier of the ladder asking to pin.
    const PINNING_ENV: &str = "pinned = true\n[env]\nWP18 = \"composed\"\n";

    /// A `$OCX_HOME` carrying the two things the global resolver reads: a
    /// declared `[env]`, so it always has a contribution and never
    /// short-circuits to `Ok(None)`, and a one-tool `ocx.lock`, so it has a
    /// toolchain lane to choose between.
    fn global_home(config_body: &str) -> tempfile::TempDir {
        let home = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(home.path().join("ocx.toml"), config_body).expect("write ocx.toml");
        std::fs::write(
            home.path().join("ocx.lock"),
            lock_with_groups(&["default"])
                .to_toml_string()
                .expect("the fixture lock serializes"),
        )
        .expect("write ocx.lock");
        home
    }

    /// Runs the real `ocx --global env <flags>` over a fresh home and answers
    /// whether the **link lane** ran.
    ///
    /// C-066: `pinned = true` is answered "before any I/O: no heal, no probe,
    /// no link consulted" ([`ocx_lib::package_manager::ToolchainLinks::pinned`]),
    /// while the following lane heals first — and `heal_links` creates the
    /// toolchain home root before it repairs anything. So the root's existence
    /// after the run IS the lane the resolver chose, and it needs no
    /// materialised package to observe.
    ///
    /// The root's creation is conditional on the heal not being refused — a
    /// refused tree creates nothing and answers `HealOutcome::Refused` (crate-
    /// internal to `ocx_lib`, hence no intra-doc link). The premise holds for
    /// this fixture because [`global_home`] is a fresh
    /// tempdir with no symlink anywhere on the path, so the only reachable
    /// answer is a heal that ran.
    ///
    /// The home path is asked of the manager (the same call
    /// `resolve_global_pinned_env` makes), never joined by hand.
    async fn link_lane_ran(config_body: &str, flags: &[&str]) -> bool {
        use ocx_lib::cli::ColorModeConfig;
        use ocx_lib::file_structure::RenderStampScope;

        use crate::app::{Cli, Context, ManagedConfigGate};

        let home = global_home(config_body);
        // SAFETY: `OCX_HOME` is read through `ocx_lib::env::var`, whose
        // `#[cfg(test)]` override seam is internal to `ocx_lib` and therefore
        // unavailable from this crate; the process variable is the only seam.
        // nextest runs one test per process, so this cannot race a sibling.
        unsafe { std::env::set_var("OCX_HOME", home.path()) };

        let mut argv = vec!["ocx", "--global", "env"];
        argv.extend_from_slice(flags);
        let cli = Cli::parse_from(argv);
        let context = Context::try_init(
            &cli.context,
            ColorModeConfig {
                stdout: false,
                stderr: false,
                relayed: false,
            },
            ManagedConfigGate {
                enforce_required: false,
                onboarding: false,
            },
        )
        .await
        .expect("a context over the fixture home");
        assert_eq!(
            context.file_structure().root(),
            home.path(),
            "the context must resolve the tempdir as its home, or this reads someone else's \
             ocx.toml and proves nothing"
        );

        let root = context
            .manager()
            .toolchain_home(&RenderStampScope::Global, None)
            .expect("the global toolchain home")
            .root()
            .to_path_buf();
        assert!(
            !root.exists(),
            "precondition: nothing may have rendered '{}' before the command ran, or every \
             answer below is `true` for free",
            root.display()
        );

        let Some(crate::command::Command::Env(env)) = cli.command else {
            panic!("`ocx --global env` must parse to the env command");
        };
        env.execute(context)
            .await
            .expect("the global tier never fails on a well-formed home");

        let ran = root.exists();
        // SAFETY: see above.
        unsafe { std::env::remove_var("OCX_HOME") };
        ran
    }

    /// C-055 / C-066 — `ocx --global env --pinned` composes the DIGEST lane.
    ///
    /// The defect this pins: `resolve_global_pinned_env` re-derived the lane
    /// from the file and environment tiers alone, so the parsed flag was
    /// accepted and silently discarded — exit 0, nothing on stderr, and the
    /// opposite of what was asked.
    ///
    /// `global_env_with_no_flag_follows_the_links` is this test's positive
    /// control and must be read with it: without that one, a fixture whose lock
    /// failed to parse (no toolchain lane at all, hence no heal, hence no root)
    /// would satisfy this assertion for entirely the wrong reason. They are two
    /// tests rather than two assertions because `Context::try_init` installs the
    /// global tracing dispatcher, which a process may do exactly once.
    #[tokio::test]
    async fn global_env_pinned_selects_the_digest_lane() {
        assert!(
            !link_lane_ran(DECLARED_ENV, &["--pinned"]).await,
            "--pinned must reach the global resolver: C-066 short-circuits the lane before any \
             I/O, so no heal runs and no root is rendered"
        );
    }

    /// The control for [`global_env_pinned_selects_the_digest_lane`]: the same
    /// fixture with no flag follows the links, so the heal runs and the root
    /// appears. Proves the harness can answer `true` at all.
    #[tokio::test]
    async fn global_env_with_no_flag_follows_the_links() {
        assert!(
            link_lane_ran(DECLARED_ENV, &[]).await,
            "with every tier absent the floor is the following lane, so the heal runs"
        );
    }

    /// C-055 / RUL-60 — `ocx --global env --no-pinned` outranks a global
    /// `ocx.toml` that asked to pin.
    ///
    /// The mirror direction, and the one that proves the CLI tier is a real
    /// LADDER tier rather than an `Option`-collapsed default: the file states
    /// `pinned = true` and the flag has to beat it. Control:
    /// [`global_env_with_a_pinning_file_and_no_flag_pins`].
    #[tokio::test]
    async fn global_env_no_pinned_outranks_a_pinning_global_file() {
        assert!(
            link_lane_ran(PINNING_ENV, &["--no-pinned"]).await,
            "--no-pinned must reach the global resolver and beat the file tier, or the flag \
             cannot override an ocx.toml that asked to pin"
        );
    }

    /// The control for [`global_env_no_pinned_outranks_a_pinning_global_file`]:
    /// the file tier alone already pins, so no heal runs. Proves the fixture's
    /// `pinned = true` is actually read.
    #[tokio::test]
    async fn global_env_with_a_pinning_file_and_no_flag_pins() {
        assert!(
            !link_lane_ran(PINNING_ENV, &[]).await,
            "`pinned = true` in the global ocx.toml selects the digest lane on its own"
        );
    }

    /// C-055 — `ocx env` and `ocx exec` read the SAME CLI tier.
    ///
    /// A control, not a discriminator: the parse layer already agrees on the
    /// unfixed tree. It is here because the two commands' *wiring* is what
    /// diverged, and this is the assertion a future divergence trips — both
    /// feed this exact value into `pinned_for_project`.
    #[test]
    fn env_and_exec_read_the_same_pinned_cli_tier() {
        use crate::command::toolchain_exec::ToolchainExec;

        for (flags, expected) in [
            (Vec::new(), None),
            (vec!["--pinned"], Some(true)),
            (vec!["--no-pinned"], Some(false)),
        ] {
            let mut env_argv = vec!["env"];
            env_argv.extend_from_slice(&flags);
            let env = ToolchainEnv::try_parse_from(env_argv).expect("env parses");

            let mut exec_argv = vec!["exec"];
            exec_argv.extend_from_slice(&flags);
            exec_argv.extend_from_slice(&["--", "true"]);
            let exec = ToolchainExec::try_parse_from(exec_argv).expect("exec parses");

            assert_eq!(env.pinned.pinned(), expected, "env must read {flags:?} as {expected:?}");
            assert_eq!(
                exec.pinned.pinned(),
                env.pinned.pinned(),
                "the two siblings must never disagree about the CLI tier for {flags:?}"
            );
        }
    }
}
