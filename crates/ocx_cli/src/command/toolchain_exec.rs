// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier `ocx exec` command.
//!
//! `ocx exec` is the project-tier counterpart to the OCI-tier
//! `ocx package exec` — one verb, two addressing modes, the same way
//! `ocx pull` pairs with `ocx package pull`. The module is named
//! `toolchain_exec` for the same reason `toolchain_env` is: two commands share
//! a CLI name across the tiers, so the project-tier one carries the prefix.
//! Symbols are binding names from `ocx.toml`, not OCI identifiers. The
//! command selects the bindings in the requested groups (resolution-free),
//! narrows that selection to the requested `NAME`s, resolves the host leaf of
//! **only** the named subset through `ocx.lock` (digest-pinned), composes the
//! child environment from those packages, and execs the given `ARGV` in that
//! environment — mirroring `ocx exec`'s child-spawn mechanics but driven
//! entirely by the project toolchain declaration.
//!
//! Both validations are scoped to the named subset: a tool elsewhere in scope
//! that ships no leaf for the current host (`NoHostLeaf`, exit 78) or that two
//! selected groups resolve differently (`DuplicateToolAcrossSelectedGroups`,
//! exit 64) only aborts the run when it is among the composed tools — the named
//! subset, or every tool in scope when no `NAME` is given.
//!
//! # NOTE: clap floor
//!
//! The `value_terminator = "--"` on `names` combined with `last = true` on
//! `argv` requires clap ≥ 4.5.57. clap 4.5.55 introduced a regression in
//! this combination; 4.5.57 fixed it. The floor is set in `Cargo.toml`.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::env;
use ocx_lib::launch::{self, Launch};
use ocx_lib::package::metadata::env::entry::Entry;
use ocx_lib::package_manager::composer::{ComposeRequest, Materialization};
use ocx_lib::project::{
    DEFAULT_GROUP, Origin, check_duplicate_selection, expand_all_keyword, lazy_mode_for_tool, resolve_selected_tools,
    select_tool_set,
};
use ocx_lib::record::{PackageBinding, RecordInputs, Scope};

use crate::app::project_context::{filter_by_names, load_project_with_lock_consenting};
use crate::options;

/// Run a command with the composed environment from the project toolchain.
///
/// Loads the nearest `ocx.toml` together with its sibling `ocx.lock`, selects
/// the tool bindings in the requested groups, composes their environment, and
/// execs `ARGV` with that environment.
///
/// `--` is mandatory: everything before `--` is a binding name filter; everything
/// after is the command and arguments forwarded to the child process unchanged.
///
/// # Composition order
///
/// Group-selection order (the order of `-g` flags after `all` expansion,
/// deduplicated); then alphabetical by binding name within each group
/// (lock-file order).
#[derive(Parser, Clone)]
pub struct ToolchainExec {
    #[clap(flatten)]
    pub groups: options::GroupSelection,

    /// Start with a clean environment containing only the package
    /// variables, instead of inheriting the current shell environment.
    #[arg(long = "clean", default_value_t = false)]
    pub clean: bool,

    #[clap(flatten)]
    pub env: options::EnvOverride,

    /// Top tier of the `lazy-mode` ladder for every tool this command composes
    /// into the child environment.
    ///
    /// `always` composes a tool as a generated shim: its declared names reach
    /// the child's `PATH` immediately and its content downloads the first time
    /// one of them runs. The shim directory sits *below* the tool's own
    /// `entrypoints/` and `bin/`, so a second invocation of the same name
    /// inside the same child resolves to the materialized binary directly.
    #[clap(flatten)]
    pub lazy_mode: options::LazyMode,

    /// Top tier of the `pinned` ladder for the environment this command
    /// composes for the child (C-055, C-066).
    ///
    /// Declared before `names` and `argv`, per the project's
    /// flags-before-positional-arguments convention.
    #[clap(flatten)]
    pub pinned: options::Pinned,

    #[clap(flatten)]
    pub records: options::Records,

    // `--consent` (the default) / `--no-consent`. Declared before `names` and
    // `argv`, per the project's flags-before-positional-arguments convention.
    // A `///` here would be dead text: clap renders the flattened struct's own
    // field docs, not this one.
    #[clap(flatten)]
    pub consent: options::Consent,

    /// Binding names to compose into the child env. Each name must
    /// resolve unambiguously inside the selected scope. Only the named
    /// tools are resolved to a host leaf, so an unrelated tool in scope
    /// that ships no leaf for this host does not block the run. An empty
    /// list means "every binding in scope"; then every tool must resolve.
    ///
    /// `value_terminator = "--"` so clap stops collecting names at the
    /// mandatory `--` separator without trying to interpret subsequent
    /// hyphen-prefixed argv as more names.
    #[arg(num_args = 0.., value_terminator = "--")]
    pub names: Vec<String>,

    /// Command to execute, with arguments. The command runs with the
    /// composed package env. `--` is mandatory and at least one argv
    /// token is required (`required = true` + `num_args = 1..`).
    ///
    /// `allow_hyphen_values = true` so flag-prefixed argv like
    /// `--format json` is forwarded to the child unchanged. `last = true`
    /// makes clap parse everything before the first `--` into `names`
    /// and everything after into `argv`. `required = true` ensures
    /// clap rejects `ocx exec` / `ocx exec NAME` / `ocx exec NAME --` with
    /// a usage error (exit 2) instead of letting an empty argv slip
    /// through to a runtime panic on `split_first`.
    #[arg(allow_hyphen_values = true, last = true, num_args = 1.., required = true)]
    pub argv: Vec<String>,
}

impl ToolchainExec {
    /// Execute the `ocx exec` command.
    ///
    /// # Behavior
    ///
    /// Resolves the project context (ocx.toml + ocx.lock), expands `-g all`
    /// to the full group union, selects the expanded scope via
    /// `select_tool_set` (resolution-free), narrows the selection to the
    /// requested `names`, validates the narrowed set via
    /// `check_duplicate_selection`, resolves its host leaves via
    /// `resolve_selected_tools`, and execs `argv` with the resulting package
    /// environment. Exit code is forwarded byte-for-byte from the child process
    /// on success.
    ///
    /// Composition order: group-selection order (the order of `-g` flags
    /// after `all` expansion, deduplicated), then alphabetical by binding
    /// name within each group (lock-file order).
    ///
    /// # Errors
    ///
    /// - Exit 64 (`UsageError`): no `ocx.toml` found, unknown group, unknown
    ///   or ambiguous binding name, empty `-g` segment.
    /// - Exit 78 (`ConfigError`): `ocx.lock` absent.
    /// - Exit 65 (`DataError`): `ocx.lock` stale (hash mismatch).
    /// - Other exit codes from package-manager / registry errors forwarded
    ///   via the existing `ClassifyExitCode` chain.
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Strict isolation (C2.6): `exec` composes exactly the in-effect
        // project file. Root `--global` only re-targets which single file
        // that is (the global one) — `select_tool_set` below is still fed
        // one tier (`&ctx.config`/`&ctx.lock`), never a union with a project.

        // ── Phase A: parse-time validation ───────────────────────────────

        // Reject empty comma segments (`-g ci,,lint`) BEFORE any filesystem
        // or network work. `clap`'s `value_delimiter = ','` splits the value
        // into `["ci", "", "lint"]`; an empty string is a user-typing error.
        crate::app::project_context::ensure_group_segments_nonempty(self.groups.names())?;

        // Reject a malformed `--env` before any filesystem or network work,
        // for the same reason as the group check above. Bound here; the
        // composition stage that appends these as the highest-precedence
        // entries consumes them alongside the project/group env.
        //
        // A relative `:path` value anchors to the invocation directory — the
        // one base a calling script can compute — not the project root the
        // `ocx.toml` form uses.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;

        // Fold `[records]` with `OCX_RECORDS_*` and this invocation's flags, for
        // the same reason and at the same point: a malformed name template is a
        // configuration error, and the operator should hear about it before any
        // filesystem or network work rather than after the tools are installed.
        let records = context.records(self.records.options())?;

        // ── Phase B: project context ──────────────────────────────────────
        // Errors propagate to the `main.rs` boundary: logged once and
        // classified by `app::classify_error` from `ProjectContextError`'s
        // `ClassifyExitCode` impl (NoProject→64, LockMissing→78, StaleLock→65).
        // Consent write seam (C-024, A-29): `run` is one of the commands
        // that opt in. `load_project_with_lock`, which four read-only callers
        // share, stamps nothing.
        // C-068 — a rendered trampoline re-enters as
        // `ocx --project '<baked home>' exec` from whatever directory the user
        // was in, so a failure to resolve must name the **selection**, not the
        // working directory the walk happened to start at.
        //
        // The tri-state travels rather than a resolved bool: a generated
        // launcher re-enters as `ocx --project <baked home> exec` on a machine
        // whose operator never chose that checkout, so `OCX_NO_CONSENT` must be
        // able to suppress the stamp — but only where no flag spoke
        // (ocx-sh/ocx#400). The seam resolves the ladder.
        let ctx = load_project_with_lock_consenting(&context, self.consent.explicit())
            .await
            .map_err(|error| attribute_to_selected_project(context.project_path(), error))?;

        // Phase B.3: validate `-g` groups against the loaded config.
        // `default` and `all` are always valid (all is expanded later).
        // Anything else must appear in config.groups.
        crate::app::project_context::ensure_groups_known(self.groups.names(), &ctx.config)?;

        // ── Phase C: `all` expansion + default scope ───────────────────────

        let mut expanded = expand_all_keyword(self.groups.names(), &ctx.config);
        // Default scope: if groups is empty (no -g flags) or expansion produced
        // an empty list, scope = [DEFAULT_GROUP] — matches pull semantics.
        if expanded.is_empty() {
            expanded = vec![DEFAULT_GROUP.to_owned()];
        }

        // ── Phase D: resolution-free selection ────────────────────────────
        // `select_tool_set` neither resolves host leaves nor reports a
        // duplicate binding, so an unnamed sibling — whether it ships no leaf
        // for this host or collides with another selected group — cannot abort
        // a narrowly-named run. The host platform is computed here but consumed
        // in Phase F.
        let host = ocx_lib::oci::Platform::current().unwrap_or_else(ocx_lib::oci::Platform::any);
        let selected = select_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[])?;

        // ── Phase E: NAME filter, then duplicate validation ───────────────
        // Order is load-bearing: the check runs over the narrowed set, so a
        // collision between two selected groups only fails the run when the
        // colliding binding is actually being composed.
        let filtered = filter_by_names(selected, &self.names)?;
        check_duplicate_selection(&filtered)?;

        // ── Phase F: resolve host leaves (named subset) + install ─────────
        // Resolve host leaves for the named subset ONLY — `NoHostLeaf` (78)
        // can fire here solely for a tool actually being composed.
        let resolved = resolve_selected_tools(&filtered, &host)?;

        let manager = context.manager();

        // One entry point for both halves of the lazy split: a tool whose
        // resolved `lazy-mode` is `always` reaches the child's `PATH` as a
        // generated shim with no content downloaded; every other tool is
        // installed-on-miss exactly as before.
        let requests: Vec<ComposeRequest> = resolved
            .iter()
            .map(|tool| ComposeRequest {
                identifier: tool.identifier.clone(),
                mode: lazy_mode_for_tool(
                    &ctx.config,
                    &tool.identifier,
                    match &tool.origin {
                        Origin::Group(name) => Some(name.as_str()),
                        Origin::Explicit => None,
                    },
                    self.lazy_mode.mode(),
                ),
            })
            .collect();
        let composed = manager
            .compose_roots(&requests, &host, Materialization::Install, context.concurrency())
            .await?;
        for advisory in &composed.advisories {
            context.ui().warn(advisory.to_string());
        }
        let install_infos = composed.roots;
        // Which tools this invocation materialized on the spot — half of the drift
        // signal an execution record publishes. Reported by `compose_roots`
        // itself, which is the only layer that sees the per-root `Cached`/`Pulled`
        // outcome: `composer::Materialization` is the policy going in,
        // `ComposeRoots::pulled` the answer coming out.
        let auto_installed = composed.pulled;
        // Per-package opt-out set from the project `ocx.toml` (`no-patches`):
        // opted-out bases get no companion overlay unless the tier is
        // system-required. `toolchain_exec.rs` does not need the patch boundary index.
        // Bound once here: it drives the parent resolve below AND is forwarded
        // into the child's patch tier (Phase G) so a generated launcher's
        // re-entry (`ocx launcher exec`) honours the same opt-out.
        let no_patches = ctx.config.no_patches_repositories();
        // Stages 4-6 of the composition order: the project's `[env]`, then each
        // selected group's `[env]` in `-g` order, then `--env` last. Bound once
        // — the same vector both feeds the parent's own composition and is
        // forwarded over `OCX_ENV` (Phase G) so a generated launcher's re-entry
        // re-applies it after the package entries instead of reverting to them.
        let mut project_env = ocx_lib::project::project_env_entries(&ctx.config, &ctx.config_path, &expanded);
        project_env.extend(env_overrides);
        // C-065/C-070: the groups this invocation selected, healed and probed
        // once inside `resolve_env_with_attribution` before any link path is
        // emitted. Derived here rather than below because the same scope and
        // home answer the trampoline-exclusion question further down — one
        // derivation, so the composition and the `PATH` exclusion cannot
        // disagree about which tree this project has.
        let toolchain = crate::app::project_context::toolchain_links(
            &context,
            &ctx.config_path,
            &ctx.config,
            &ctx.lock,
            &expanded,
            self.pinned.pinned(),
        )
        .await?;
        let toolchain_home = toolchain.home.clone();
        let scope = ocx_lib::package_manager::EnvScope::Project {
            no_patches: no_patches.clone(),
            env: project_env.clone(),
            toolchain: Some(Box::new(toolchain)),
        };
        // Always the consumer surface. `--self` is package vocabulary: it
        // selects a package's own private surface, which by construction
        // DROPS that package's `entrypoints/` from PATH — the launchers exist
        // for consumers, and a package running itself calls `bin/` directly.
        // A toolchain consumer is a consumer of every tool it declares, so the
        // self view would compose a strictly worse toolchain. The flag belongs
        // on `ocx package exec` / `ocx package env`, and only there.
        //
        // `resolve_env_with_attribution` rather than the attribution-dropping
        // wrapper: the record names which package claimed each executable on
        // `PATH`, and that derivation already exists here.
        // The patch provenance is kept, not dropped: the record names every
        // companion the site tier overlaid onto this composition, and this call
        // is the only place that attribution exists.
        let (mut entries, _, patch_companions, admitted) = manager
            .resolve_env_with_attribution(&install_infos, false, scope, &host)
            .await?;

        // W-11: `entries` and `project_env` are disjoint `Vec`s (the latter is
        // ALSO forwarded raw over `OCX_ENV` below) — reconcile them together so
        // a package-established `list` separator reaches the forwarded copy.
        // This is the motivating case: without it, a package appending
        // `GODEBUG` with `","` plus this project's own `GODEBUG` entry (which
        // may omit the separator) would forward the project's copy with `None`,
        // and a re-entrant launcher would fold it with the bare `" "` default
        // instead of the separator the package actually established.
        reconcile_run_entries(&mut entries, &mut project_env)?;

        // ── Phase G: spawn child ──────────────────────────────────────────

        let mut process_env = if self.clean {
            env::Env::clean()
        } else {
            env::Env::inherited()
        };
        // Inject the project `no-patches` opt-out into the forwarded patch tier:
        // the base `config_view().patches` carries only the config-file tier
        // (empty `no_patches`). Forwarding the opt-out over `OCX_PATCHES` lets a
        // child launcher's `Context` reconstruct it. Only `patches.is_some()`
        // tiers forward — an absent tier has no companions to re-inject.
        //
        // A generated launcher resolves its base via `install_info_from_package_root`,
        // which mints a synthetic content-addressed identifier with no real
        // `registry/repository` (see `launcher/exec.rs`), so a repo-key alone
        // never matches there. Also forward each opted-out base's resolved
        // content digest (from the already-resolved `install_infos`) so the
        // launcher's digest-matching leg (`resolve.rs`) can recognise it. The
        // digest string form (`Digest::to_string()`, e.g. `sha256:<hex>`) must
        // match exactly what the resolver compares against.
        let mut forwarded_no_patches = no_patches.clone();
        for info in &install_infos {
            let id = info.identifier().as_identifier();
            let repo_key = format!("{}/{}", id.registry(), id.repository());
            if no_patches.contains(&repo_key) {
                forwarded_no_patches.insert(info.identifier().digest().to_string());
            }
        }
        let mut forwarded_config = context.config_view().clone();
        if let Some(patches) = forwarded_config.patches.as_mut() {
            patches.no_patches = forwarded_no_patches;
        }
        // Hand the resolved sink down. The config and environment tiers a child
        // re-derives for itself; the flag tier it cannot, so without this a
        // generated launcher's re-entry (`ocx launcher exec`) would record
        // somewhere else — or, since `apply_ocx_config` is set-or-remove,
        // nowhere — and the entrypoint pair would lose its inner half.
        forwarded_config.records = records.forwarded();
        // Same move, for the consent refusal: argv does not cross a spawn, so
        // without this a `--no-consent` would stop at this process and a nested
        // ocx the child launches would stamp the very project the caller
        // declined. Refusal inherits downward; permission does not, which is
        // why a `--consent` leaves this false rather than clearing an inherited
        // OCX_NO_CONSENT.
        forwarded_config.no_consent = self.consent.explicit() == Some(false);
        // Composed entries + forwarded ocx config + forwarded stages 4-6, in the
        // one order that is correct — see `Env::apply_child_env`. `project_env`
        // (stages 4-6) is the forwarded slice, NOT the whole composed set: the
        // launcher re-derives the package entries itself.
        process_env.apply_child_env(
            env::ChildEnv {
                composed: &entries,
                forwarded: &project_env,
            },
            &forwarded_config,
        );
        // No PATHEXT manipulation: the Windows launcher is now a native
        // `<name>.exe` shim and `.EXE` is unconditionally in the default
        // Windows PATHEXT, so the child resolves it via the OS default.

        // Project-tier facts the record's `scope` block names. The lock's
        // declaration hash is reused verbatim rather than recomputed: it is the
        // canonicalization the project tier already defines, and re-deriving it
        // would need the project config plus file I/O on the exec path.
        let project_root = ctx.config_path.parent().unwrap_or(&ctx.config_path).to_path_buf();
        let declaration_digest = ocx_lib::oci::Digest::try_from(ctx.lock.metadata.declaration_hash.as_str())?;
        let bindings = project_bindings(&resolved, &install_infos);

        // clap enforces `last = true, num_args = 1.., required = true` on the
        // `argv` field — `self.argv` is always non-empty at this point.
        let (command, _) = self
            .argv
            .split_first()
            .expect("clap last=true + num_args=1.. + required=true guarantees non-empty argv");

        // Resolved once, then handed to both the record and the launch: a second
        // resolution could disagree with the first and make the audit trail name
        // a binary other than the one that ran.
        // C-057/S-010: a name the composition does not provide is now an error
        // propagated here rather than a bare name handed to `execvp`, which
        // would have repeated the lookup against the ambient `PATH`.
        // `CommandResolutionError` already classifies to `DataError`, so this
        // `?` is the whole of exit 65 — and nothing is spawned on the way out.
        // C-058/C-010 — the lookup copy of `PATH` excludes both trampoline
        // directories, derived from the resolvers that produced the trees. The
        // child's own `PATH` is untouched: a tool that spawns a sibling tool
        // still resolves it through a trampoline, which is the re-entry the
        // trampolines exist to provide. C-069's `is_ocx_trampoline` re-check
        // over the resolved answer is the second, independent guard.
        let excluded = trampoline_lookup_exclusions(context.file_structure(), &toolchain_home);
        let executable = process_env.resolve_command_excluding(command, &excluded)?;
        let launch = Launch::recording(
            process_env,
            RecordInputs {
                packages: &install_infos,
                admitted: &admitted,
                patch_companions: &patch_companions,
                executable: &executable,
                store_root: context.file_structure().packages.root(),
                shim_root: context.file_structure().shims.root(),
                argv: &self.argv,
                config: context.config_view(),
                insecure_registries: context.insecure_hosts(),
                // Already in memory: the snapshot is read once at `try_init` and
                // identity-gated there, so naming it here costs no I/O on the
                // exec path.
                managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
                // Likewise read once at `try_init`, alongside the pins it
                // describes.
                patch_snapshot_digest: context.patch_snapshot_digest(),
                platform: Some(&host),
                clean_env: self.clean,
                auto_installed: &auto_installed,
                scope: Scope::Project {
                    root: project_root,
                    lock: ctx.lock_path.clone(),
                    declaration_digest,
                    groups: expanded,
                    bindings,
                },
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

/// Pair each root package with the `ocx.toml` binding it was selected under.
///
/// `compose_roots` returns one root per request in request order, and under
/// `Materialization::Install` every request yields one, so the two slices line
/// up index-for-index — the pinned identifier comes from the composition rather
/// than from the selection, because only the former is digest-complete.
///
/// A tool selected as a positional rather than through a group carries no group
/// name and is skipped: the record emits `sh.ocx.binding` and `sh.ocx.group`
/// together or not at all. `ocx exec` passes no positionals to `select_tool_set`,
/// so today the skip is unreachable.
fn project_bindings(
    resolved: &[ocx_lib::project::ResolvedTool],
    install_infos: &[std::sync::Arc<ocx_lib::package::install_info::InstallInfo>],
) -> Vec<PackageBinding> {
    resolved
        .iter()
        .zip(install_infos)
        .filter_map(|(tool, info)| match &tool.origin {
            Origin::Group(group) => Some(PackageBinding {
                binding: tool.binding.clone(),
                group: group.clone(),
                package: info.identifier().clone(),
            }),
            Origin::Explicit => None,
        })
        .collect()
}

/// Settles every `list` entry's separator across `entries` (composed, applied
/// to this process) and `project_env` (forwarded raw over `OCX_ENV` for a
/// re-entrant launcher) in one pass (W-11).
///
/// The two are disjoint `Vec`s holding independent [`Entry`] copies: without
/// chaining them through one [`env::reconcile_list_separators`] call, a
/// package's explicit separator would settle `entries` alone and leave
/// `project_env`'s own copy at whatever separator it was declared with —
/// `None` inherits nothing, and a re-entrant launcher would fold it with the
/// bare default instead of the separator the package actually established.
/// Extracted from [`ToolchainExec::execute`] so this exact wiring is unit-testable
/// without a full project/registry fixture.
///
/// # Errors
///
/// [`env::ListSeparatorError`] when two entries for one key declare different
/// explicit separators, or when the separator an entry settles on edges its
/// value.
fn reconcile_run_entries(entries: &mut [Entry], project_env: &mut [Entry]) -> Result<(), env::ListSeparatorError> {
    env::reconcile_list_separators(entries.iter_mut().chain(project_env.iter_mut()))
}

/// The trampoline directories this invocation's **lookup** `PATH` excludes
/// (C-058, C-010) — **four** paths: the PATH-facing `<home>/toolchain/active/bin`
/// and the physical `<home>/toolchain/shells/default/bin`, for the project tree
/// and for the global one.
///
/// # Derived, never joined
///
/// Every entry comes from the resolver that produced the home —
/// [`ToolchainStore::bin`](ocx_lib::file_structure::ToolchainStore::bin) and
/// [`ToolchainStore::shell_bin`](ocx_lib::file_structure::ToolchainStore::shell_bin)
/// for the global tree, the matching
/// [`ToolchainHome`](ocx_lib::file_structure::ToolchainHome) accessors for the
/// project's, the latter through
/// [`PackageManager::toolchain_home`](ocx_lib::package_manager::PackageManager::toolchain_home)
/// so `toolchain-dir` is honoured. A literal `join("toolchain").join("bin")`
/// would compile, pass every fixture, and drift silently the day the tree shape
/// moves — which is the whole of what C-010 forbids.
///
/// # Why both spellings per home (C-078)
///
/// The two are equal only *through* the `active` link, never by string
/// equality, and this exclusion is **segment-exact**:
/// `utility::path::remove_segment` compares segments and "is therefore not a
/// containment check and must not be read as one". A composed `PATH` carrying
/// the *physical* spelling — from a hand-written `.envrc`, an IDE recipe, or a
/// `$GITHUB_PATH` line — would otherwise survive the exclusion, so this lookup
/// could answer with a trampoline and that trampoline would re-enter itself:
/// the fork loop asdf#2166, opencodex#1439 and claude-code#47978 each shipped.
/// Listing one spelling would also leave C-069's `is_ocx_trampoline` as the
/// only guard, against this record's rule that deleting either guard must leave
/// the other observably firing.
///
/// # Why only the lookup copy
///
/// The child's `PATH` is untouched: a tool that spawns a sibling tool still
/// resolves it through a trampoline, which is the re-entry the trampolines exist
/// to provide. Excluding the directories here only stops **this** lookup from
/// answering with one. C-069's `is_ocx_trampoline` re-check over the resolved
/// answer — already shipped inside `Env::resolve_command_in` — is the second,
/// independent guard that closes the alias gap.
///
/// # Why the two homes arrive as values (RUL-69)
///
/// Neither is looked up here. `Context` is only constructible through the async
/// `Context::try_init`, which installs the global tracing subscriber, so a
/// signature taking one puts this derivation out of reach of every unit test —
/// and a derivation no test can reach is indistinguishable from the literal
/// join it exists to forbid. The caller resolves the project home (through
/// `PackageManager::toolchain_home`, the one derivation `ocx pull` also uses)
/// and hands both trees in.
fn trampoline_lookup_exclusions(
    file_structure: &ocx_lib::file_structure::FileStructure,
    project_home: &ocx_lib::file_structure::ToolchainHome,
) -> Vec<std::path::PathBuf> {
    // Spelled out rather than imported: a function-local `use` would go unused
    // the moment either physical entry is dropped, so the drop would red as an
    // unused-import lint before this function's own test could red on the
    // answer — a check whose red state is the compiler's, not the check's.
    vec![
        file_structure.toolchain.bin(),
        project_home.bin(),
        file_structure
            .toolchain
            .shell_bin(ocx_lib::file_structure::DEFAULT_SHELL),
        project_home.shell_bin(ocx_lib::file_structure::DEFAULT_SHELL),
    ]
}

/// Re-attribute a project-resolution failure to the project the invocation
/// **selected**, rather than to the directory it happened to run from (C-068).
///
/// A toolchain trampoline re-enters as `ocx --project '<baked home>' exec`, from
/// whatever working directory the user was in. When the baked home no longer
/// holds an `ocx.toml`, the shipped path answers about the wrong tier: an
/// explicit `--project` that is missing is a `config::Error::FileNotFound`
/// (exit 79) and one naming a directory is a `config::Error::Io` (exit 74),
/// while C-068 requires the three-way contract `ocx exec` already states —
/// [`ProjectContextError::NoProject`] → **64**,
/// [`LockCurrency::Missing`](ocx_lib::project::LockCurrency::Missing) → **78**,
/// [`LockCurrency::Stale`](ocx_lib::project::LockCurrency::Stale) → **65** —
/// with the baked path named in each.
///
/// The two lock arms already classify correctly and already carry a path, so
/// this maps only the first: an explicit selection that did not resolve becomes
/// `NoProject` naming the selection. Everything else passes through unchanged —
/// a parse error in a project file that *does* exist is not a missing project.
fn attribute_to_selected_project(
    selected: Option<&std::path::Path>,
    error: crate::app::project_context::ProjectContextError,
) -> crate::app::project_context::ProjectContextError {
    use crate::app::project_context::ProjectContextError;

    let Some(selected) = selected else { return error };
    let no_project = || ProjectContextError::NoProject {
        cwd: selected.to_path_buf(),
    };
    match error {
        // The loader answers `NoProject` with the **working directory** it
        // walked, which for a trampoline is wherever the user happened to be.
        // The selection is what the invocation actually named.
        ProjectContextError::NoProject { .. } => no_project(),
        // An explicit selection naming a path that is not there: `FileNotFound`,
        // exit 79. For `ocx exec` the baked home *is* the project, so its
        // absence is `no project` (64) — the same answer the directory branch
        // gives for a home whose `ocx.toml` was deleted. The variant is not
        // discriminated by tier because only one tier can produce it here:
        // `load_project_with_lock`'s single `ConfigError` source is
        // `ProjectConfig::resolve`, which resolves the project tier and nothing
        // else — the `--config` tier was resolved in `Context::try_init`, long
        // before this call.
        ProjectContextError::Config(ocx_lib::ConfigError::FileNotFound { .. }) => no_project(),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::oci::{Digest, Identifier, Platform};
    use ocx_lib::project::{LockMetadata, LockVersion, LockedTool, ProjectConfig, ProjectLock};
    use std::collections::BTreeMap;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    // ── W-11: reconcile_run_entries (chained-vector wiring) ────────────────────

    fn list_entry(key: &str, value: &str, separator: Option<&str>) -> Entry {
        Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind: ocx_lib::package::metadata::env::modifier::ModifierKind::List,
            separator: separator.map(str::to_owned),
        }
    }

    /// The motivating case: a package's explicit separator (in `entries`)
    /// must reach the project's own entry for the same key even though it
    /// lives in the disjoint `project_env` vector — `ocx_lib::env`'s own
    /// `reconcile_spans_two_disjoint_vectors` test proves the underlying
    /// primitive; this proves `toolchain_exec.rs` actually wires it with both vectors.
    #[test]
    fn reconcile_run_entries_lets_project_env_inherit_the_package_separator() {
        let mut entries = vec![list_entry("GODEBUG", "gctrace=1", Some(","))];
        let mut project_env = vec![list_entry("GODEBUG", "madvdontneed=1", None)];

        reconcile_run_entries(&mut entries, &mut project_env).expect("one explicit separator is agreement");

        assert_eq!(
            project_env[0].separator.as_deref(),
            Some(","),
            "the forwarded copy must inherit the package-established separator"
        );
    }

    /// Two explicit separators for the same key across the two vectors fail
    /// closed instead of silently picking one — the failure mode a wrong
    /// choice would corrupt for the consuming tool.
    #[test]
    fn reconcile_run_entries_rejects_a_conflict_across_the_two_vectors() {
        let mut entries = vec![list_entry("GODEBUG", "gctrace=1", Some(","))];
        let mut project_env = vec![list_entry("GODEBUG", "madvdontneed=1", Some(";"))];

        let error = reconcile_run_entries(&mut entries, &mut project_env)
            .expect_err("conflicting explicit separators must not both apply");
        assert!(matches!(&error, env::ListSeparatorError::Conflict { key, .. } if key == "GODEBUG"));
    }

    // ── select → filter → resolve (named scope regression) ────────────────────

    /// Regression (bugfix `run_named_scope_resolution`), integrated over the
    /// `ocx exec` Phase D/E/F pipeline: `select_tool_set` → `filter_by_names` →
    /// `resolve_selected_tools`. A windows-only sibling in the default group
    /// must NOT block `ocx exec cmake` on a linux host; but `ocx exec -- ...`
    /// (no NAME → whole group) must still surface the sibling's `NoHostLeaf`,
    /// locking the unnamed-run contract.
    #[test]
    fn named_subset_resolves_while_unnamed_whole_group_errors() {
        fn lock_v3(tools: Vec<LockedTool>) -> ProjectLock {
            ProjectLock {
                metadata: LockMetadata {
                    lock_version: LockVersion::V3,
                    declaration_hash_version: 1,
                    declaration_hash: format!("sha256:{}", sha('0')),
                    generated_by: "ocx test".into(),
                    generated_at: "2026-04-24T00:00:00Z".into(),
                },
                tools,
            }
        }
        fn leaf(name: &str, platform_key: &str, c: char) -> LockedTool {
            let mut platforms = BTreeMap::new();
            platforms.insert(platform_key.to_string(), Digest::Sha256(sha(c)));
            LockedTool {
                name: name.into(),
                group: "default".into(),
                repository: Identifier::new_registry(name, "ocx.sh"),
                platforms,
            }
        }

        let lock = lock_v3(vec![
            leaf("cmake", "linux/amd64", 'a'),
            leaf("winonly", "windows/amd64", 'b'),
        ]);
        let config = ProjectConfig::from_parts(BTreeMap::new(), BTreeMap::new());
        let host: Platform = "linux/amd64".parse().expect("valid host");
        let groups = vec!["default".to_owned()];

        // names = ["cmake"] → resolve only cmake → Ok.
        let selected = select_tool_set(&config, Some(&lock), &groups, &[]).expect("select ok");
        let named = filter_by_names(selected, &["cmake".to_owned()]).expect("filter ok");
        assert!(
            resolve_selected_tools(&named, &host).is_ok(),
            "named subset (cmake) must resolve on linux host"
        );

        // names = [] (whole group) → resolve every tool → Err (winonly NoHostLeaf).
        let selected_all = select_tool_set(&config, Some(&lock), &groups, &[]).expect("select ok");
        let unnamed = filter_by_names(selected_all, &[]).expect("filter ok");
        assert!(
            resolve_selected_tools(&unnamed, &host).is_err(),
            "unnamed whole-group run must still surface the windows-only sibling's NoHostLeaf"
        );
    }

    // ── C4: no-strip clap surface ────────────────────────────────────────────
    //
    // `--global` is no longer a per-command flag — it is a single root-level
    // selector on `ContextOptions` (peer of `--project`), so `ToolchainExec` carries no
    // `global` field and `ocx exec --global` parses as `ocx --global exec`.
    // Root-flag parsing is clap-derived; the `--global` ⟂ `--project`
    // exclusivity is covered by `app::context` unit tests and the acceptance
    // suite (`test/tests/test_run_global_isolation.py`).

    /// C4 (no-strip contract): the `ToolchainExec` struct exposes no strip mechanism
    /// (`--strip-global`, `--emit-global-path-strip`).
    ///
    /// Compile-and-parse structural proof: if a strip flag were re-introduced
    /// on `ToolchainExec`, clap would accept it and these assertions would fail —
    /// keeping the deletion explicit and enforced.
    #[test]
    fn run_no_strip_field_clap_surface() {
        // `--strip-global` or `--emit-strip` do not exist — clap must reject them.
        let result = ToolchainExec::try_parse_from(["exec", "--strip-global", "--", "echo", "hi"]);
        assert!(
            result.is_err(),
            "the strip mechanism (`--strip-global`) must not exist on `ToolchainExec`; clap must reject it"
        );

        let result = ToolchainExec::try_parse_from(["exec", "--emit-global-path-strip", "--", "echo", "hi"]);
        assert!(
            result.is_err(),
            "the strip mechanism (`--emit-global-path-strip`) must not exist on `ToolchainExec`; clap must reject it"
        );
    }

    /// `--self` was removed from `ocx exec` (a documented breaking change): the
    /// self view selects a package's own private surface, which by construction
    /// drops that package's `entrypoints/` from `PATH`, so a toolchain consumer
    /// asking for it composed a strictly worse toolchain. The flag survives on
    /// the package tier, where a package's own surface is the thing being asked
    /// about — this pins only that `ToolchainExec` no longer accepts it.
    #[test]
    fn run_rejects_the_removed_self_flag() {
        let result = ToolchainExec::try_parse_from(["exec", "--self", "--", "echo", "hi"]);
        assert!(
            result.is_err(),
            "`--self` was removed from `ocx exec`; clap must reject it"
        );
    }

    // ── C-058 / RUL-69 — the exclusion set is derived, never joined ──────────

    /// **C-058 / C-010 / C-078** — four entries, all from the resolvers that
    /// produced the trees, so the exclusion cannot drift from the tree shape.
    ///
    /// Two homes x two spellings. The PATH-facing `active/bin` is what this
    /// process put on `PATH`; the physical `shells/<shell>/bin` is what the
    /// renderer wrote, and a composed `PATH` can carry it from outside ocx
    /// entirely. `remove_segment` is segment-exact, so a spelling that is not
    /// listed is not excluded, and a lookup that answers with a trampoline
    /// re-enters itself.
    ///
    /// The discriminating input is a project home whose root does **not** end
    /// in `toolchain` — which is exactly what a `toolchain-dir` relocation
    /// produces at `<root>/<project-key>/toolchain`, and what a
    /// `join("toolchain").join("bin")` at the call site would answer wrongly
    /// for. Reachable only because RUL-69 took `&Context` out of the signature:
    /// a `Context` is constructible solely through the async `try_init`, which
    /// installs the global tracing subscriber.
    ///
    /// RED: drop either physical entry from the returned vector — the set
    /// assertion, the literal assertion and the count all red, and the
    /// surviving spelling proves nothing about the one that left.
    #[test]
    fn the_exclusion_set_is_both_trees_own_bin_directories() {
        use ocx_lib::file_structure::DEFAULT_SHELL;

        let home = std::path::PathBuf::from("/w/.ocx-home");
        let file_structure = ocx_lib::file_structure::FileStructure::with_root(home);
        // A relocated project home: the segment before `bin` is a project key,
        // not the literal `toolchain`.
        let project_home = ocx_lib::file_structure::ToolchainHome::new("/w/toolchains/0123456789abcdef/toolchain");

        let excluded = trampoline_lookup_exclusions(&file_structure, &project_home);

        assert_eq!(
            excluded,
            vec![
                file_structure.toolchain.bin(),
                project_home.bin(),
                file_structure.toolchain.shell_bin(DEFAULT_SHELL),
                project_home.shell_bin(DEFAULT_SHELL),
            ],
            "C-078 — every entry must be a resolver's own accessor, both spellings, both tiers"
        );

        // Literals beside the derivation: a derivation compared only against
        // itself agrees with any accessor, including a wrong one.
        //
        // The tail is joined with `MAIN_SEPARATOR_STR` rather than written with
        // `/`, because the accessors build it with `Path::join` and Windows
        // renders that as `\\`. Spelling it `/` asserted the separator instead
        // of the shape and reddened this row on the Windows leg for a reason
        // this test is not about. The roots keep their own `/` — they are given
        // as literals and no `join` touches them, on either platform.
        let sep = std::path::MAIN_SEPARATOR_STR;
        assert_eq!(
            excluded
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec![
                format!("/w/.ocx-home{sep}toolchain{sep}active{sep}bin"),
                format!("/w/toolchains/0123456789abcdef/toolchain{sep}active{sep}bin"),
                format!("/w/.ocx-home{sep}toolchain{sep}shells{sep}default{sep}bin"),
                format!("/w/toolchains/0123456789abcdef/toolchain{sep}shells{sep}default{sep}bin"),
            ],
            "C-010 — each entry is `<home root>/...`, never `<something>/toolchain/bin` re-joined"
        );

        let unique: std::collections::BTreeSet<_> = excluded.iter().collect();
        assert_eq!(
            unique.len(),
            4,
            "four distinct directories; collapsing any two would leave one unexcluded"
        );
    }

    // ── C-068 — a trampoline's baked home, re-attributed ─────────────────────

    use crate::app::project_context::ProjectContextError;
    use ocx_lib::cli::ExitCode;

    /// The shipped exit-code authority — `main.rs`'s own classifier, walking
    /// the source chain for CLI-local types before delegating to the library.
    ///
    /// Every code assertion below goes through it rather than restating an
    /// integer, so a mapping that is correct in the enum and lost on the way to
    /// the process's status still reds.
    fn code_of(error: &ProjectContextError) -> ExitCode {
        crate::app::classify_error(error)
    }

    /// The `config::Error` a *missing* explicit `--project` selection really
    /// produces, from the shipped loader rather than a hand-built variant.
    async fn selection_error(selected: &std::path::Path) -> ocx_lib::ConfigError {
        ocx_lib::ConfigLoader::project_path(None, Some(selected))
            .await
            .expect_err("an explicit --project naming an absent path is an error")
    }

    /// The defect C-068 names, pinned as a **control** so the mapping below is
    /// not asserted against a state that was already correct: a trampoline
    /// re-entering with a baked home that no longer exists classifies as
    /// `NotFound` (79) today, not as the 64 `ocx exec`'s own contract states.
    #[tokio::test]
    async fn an_unresolved_explicit_selection_classifies_as_not_found_before_re_attribution() {
        let tmp = tempfile::tempdir().expect("a tempdir is creatable");
        let baked = tmp.path().join("moved-away").join("ocx.toml");
        let error = ProjectContextError::from(selection_error(&baked).await);
        assert_eq!(
            code_of(&error),
            ExitCode::NotFound,
            "the control: without re-attribution the baked home's absence is a 79"
        );
    }

    /// **C-068** — an explicit selection that did not resolve becomes
    /// `NoProject`, **naming the selection**, and classifies as **64**.
    ///
    /// This is the trampoline's own path: a rendered body re-enters as
    /// `ocx --project '<baked home>' exec`, from whatever directory the user
    /// happened to be in, so the attribution must follow the selection and not
    /// the working directory.
    ///
    /// Mutation that reds it: returning the error unchanged (the shipped
    /// behaviour, pinned by the control above) — 79 instead of 64.
    #[tokio::test]
    async fn a_missing_baked_project_is_re_attributed_to_no_project_and_exits_64() {
        let tmp = tempfile::tempdir().expect("a tempdir is creatable");
        let baked = tmp.path().join("moved-away");
        let error = ProjectContextError::from(selection_error(&baked.join("ocx.toml")).await);

        let mapped = attribute_to_selected_project(Some(&baked), error);
        assert!(
            matches!(&mapped, ProjectContextError::NoProject { cwd } if cwd == &baked),
            "C-068 — the re-attributed error must name the *selected* home, got: {mapped:?}"
        );
        assert_eq!(
            code_of(&mapped),
            ExitCode::UsageError,
            "C-068 — `NoProject` is exit 64, and the trampoline path inherits it"
        );
    }

    /// C-068's boundary: with **no** explicit selection there is nothing to
    /// re-attribute to, so the error passes through unchanged.
    ///
    /// Mutation that reds it: re-attributing unconditionally, which would turn
    /// every unrelated config failure into "no project here".
    #[tokio::test]
    async fn without_a_selection_the_error_is_left_alone() {
        let tmp = tempfile::tempdir().expect("a tempdir is creatable");
        let absent = tmp.path().join("moved-away").join("ocx.toml");
        let error = ProjectContextError::from(selection_error(&absent).await);

        let passed_through = attribute_to_selected_project(None, error);
        assert!(
            !matches!(passed_through, ProjectContextError::NoProject { .. }),
            "with no explicit selection there is no home to attribute the failure to"
        );
        assert_eq!(
            code_of(&passed_through),
            ExitCode::NotFound,
            "an untouched error keeps its own classification"
        );
    }

    /// C-068 — a project file that **does exist** but does not parse is not a
    /// missing project, even under an explicit selection.
    ///
    /// Mutation that reds it: a blanket `_ => NoProject { cwd: selection }`,
    /// which would answer 64 for a broken `ocx.toml` and send the user looking
    /// for a file that is right there.
    #[tokio::test]
    async fn a_parse_failure_in_an_existing_project_file_is_not_re_attributed() {
        let tmp = tempfile::tempdir().expect("a tempdir is creatable");
        let project = tmp.path().to_path_buf();
        std::fs::write(project.join("ocx.toml"), "this is not = = toml").expect("the manifest is writable");
        let resolved = ocx_lib::ConfigLoader::project_path(None, Some(&project))
            .await
            .expect("a directory holding an ocx.toml resolves (RUL-55)")
            .expect("the file is present");
        let bytes = tokio::fs::read(&resolved).await.expect("the manifest is readable");
        let parse_failure = ocx_lib::project::ProjectConfig::from_toml_bytes_with_path(&bytes, resolved)
            .expect_err("an unparsable manifest is an error");
        let error = ProjectContextError::from(parse_failure);
        let before = code_of(&error);

        let mapped = attribute_to_selected_project(Some(&project), error);
        assert!(
            !matches!(mapped, ProjectContextError::NoProject { .. }),
            "a manifest that exists and does not parse is not `NoProject`"
        );
        assert_eq!(
            code_of(&mapped),
            before,
            "a parse failure keeps the classification it already had"
        );
        assert_ne!(
            before,
            ExitCode::UsageError,
            "the control: the parse failure's own code differs from 64, so the assertion above discriminates"
        );
    }

    /// C-068's two lock arms, both under an explicit selection: they already
    /// classify correctly and already carry a path, so re-attribution must
    /// leave them alone.
    ///
    /// `Missing` → **78**, `Stale` → **65** — the other two thirds of the
    /// three-way mapping, each keeping the baked path in its message.
    #[test]
    fn the_two_lock_arms_keep_their_own_codes_and_paths() {
        let baked = std::path::PathBuf::from("/w/proj");
        let lock_path = baked.join("ocx.lock");

        let missing = ProjectContextError::from(ocx_lib::project::LockCurrency::Missing {
            path: lock_path.clone(),
        });
        assert_eq!(code_of(&missing), ExitCode::ConfigError, "C-068 — an absent lock is 78");
        let missing = attribute_to_selected_project(Some(&baked), missing);
        assert_eq!(
            code_of(&missing),
            ExitCode::ConfigError,
            "C-068 — re-attribution must not move the absent-lock arm off 78"
        );
        assert!(
            missing.to_string().contains(&lock_path.display().to_string()),
            "C-068 — each arm names its path: {missing}"
        );

        let stale = ProjectContextError::from(ocx_lib::project::LockCurrency::Stale {
            lock_path: lock_path.clone(),
        });
        assert_eq!(code_of(&stale), ExitCode::DataError, "C-068 — a stale lock is 65");
        let stale = attribute_to_selected_project(Some(&baked), stale);
        assert_eq!(
            code_of(&stale),
            ExitCode::DataError,
            "C-068 — re-attribution must not move the stale-lock arm off 65"
        );
    }

    /// The three codes are three, and distinct. A mapping that collapsed any
    /// two of them would satisfy every individual assertion above that names
    /// only one arm.
    #[test]
    fn the_three_way_mapping_is_three_distinct_codes() {
        let codes = [ExitCode::UsageError, ExitCode::ConfigError, ExitCode::DataError];
        let unique: std::collections::BTreeSet<u8> = codes.iter().map(|code| *code as u8).collect();
        assert_eq!(
            unique.len(),
            3,
            "C-068 is a three-way mapping, not one code with three names"
        );
        assert_eq!(unique, [64u8, 65, 78].into_iter().collect());
    }
}
