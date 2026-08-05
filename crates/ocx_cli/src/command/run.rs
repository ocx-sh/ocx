// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier `ocx run` command.
//!
//! `ocx run` is the project-tier counterpart to the OCI-tier `ocx exec`.
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
use ocx_lib::package::metadata::env::entry::Entry;
use ocx_lib::project::{
    DEFAULT_GROUP, check_duplicate_selection, expand_all_keyword, resolve_selected_tools, select_tool_set,
};
use ocx_lib::utility::child_process;

use crate::app::project_context::{filter_by_names, load_project_with_lock};
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
pub struct Run {
    #[clap(flatten)]
    pub groups: options::GroupSelection,

    /// Start with a clean environment containing only the package
    /// variables, instead of inheriting the current shell environment.
    #[arg(long = "clean", default_value_t = false)]
    pub clean: bool,

    #[clap(flatten)]
    pub env: options::EnvOverride,

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
    /// clap rejects `ocx run` / `ocx run NAME` / `ocx run NAME --` with
    /// a usage error (exit 2) instead of letting an empty argv slip
    /// through to a runtime panic on `split_first`.
    #[arg(allow_hyphen_values = true, last = true, num_args = 1.., required = true)]
    pub argv: Vec<String>,
}

impl Run {
    /// Execute the `ocx run` command.
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
        // Strict isolation (C2.6): `run` composes exactly the in-effect
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

        // ── Phase B: project context ──────────────────────────────────────
        // Errors propagate to the `main.rs` boundary: logged once and
        // classified by `app::classify_error` from `ProjectContextError`'s
        // `ClassifyExitCode` impl (NoProject→64, LockMissing→78, StaleLock→65).
        let ctx = load_project_with_lock(&context).await?;

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

        let identifiers: Vec<_> = resolved.iter().map(|r| r.identifier.clone()).collect();
        let infos = manager
            .find_or_install_all(identifiers, host.clone(), context.concurrency())
            .await?;
        let install_infos: Vec<std::sync::Arc<ocx_lib::package::install_info::InstallInfo>> =
            infos.into_iter().map(std::sync::Arc::new).collect();
        // Per-package opt-out set from the project `ocx.toml` (`no-patches`):
        // opted-out bases get no companion overlay unless the tier is
        // system-required. `run.rs` does not need the patch boundary index.
        // Bound once here: it drives the parent resolve below AND is forwarded
        // into the child's patch tier (Phase G) so a generated launcher's
        // re-entry (`ocx launcher exec`) honours the same opt-out.
        let no_patches = ctx.config.no_patches_repositories();
        // Stages 4-6 of the composition order: the project's `[env]`, then each
        // selected group's `[env]` in `-g` order, then `--env` last. Bound once
        // — the same vector both feeds the parent's own composition and is
        // forwarded over `OCX_ENV` (Phase G) so a generated launcher's re-entry
        // re-applies it after the package entries instead of reverting to them.
        let mut project_env =
            crate::app::project_context::project_env_entries(&ctx.config, &ctx.config_path, &expanded);
        project_env.extend(env_overrides);
        let scope = ocx_lib::package_manager::EnvScope::Project {
            no_patches: no_patches.clone(),
            env: project_env.clone(),
        };
        // Always the consumer surface. `--self` is package vocabulary: it
        // selects a package's own private surface, which by construction
        // DROPS that package's `entrypoints/` from PATH — the launchers exist
        // for consumers, and a package running itself calls `bin/` directly.
        // A toolchain consumer is a consumer of every tool it declares, so the
        // self view would compose a strictly worse toolchain. The flag belongs
        // on `ocx package exec` / `ocx package env`, and only there.
        let mut entries = manager
            .resolve_env_with_patch_boundary(&install_infos, false, scope, &host)
            .await?
            .0;

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

        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
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

        // clap enforces `last = true, num_args = 1.., required = true` on the
        // `argv` field — `self.argv` is always non-empty at this point.
        let (command, args) = self
            .argv
            .split_first()
            .expect("clap last=true + num_args=1.. + required=true guarantees non-empty argv");

        let resolved = process_env.resolve_command(command);

        // Replace this process with the child on Unix (PID inherited via
        // `execvp(2)`); on Windows spawn+wait then `process::exit`, since
        // `CreateProcess` has no exec equivalent. Either way the helper
        // diverges on success — only start-up failures fall through to
        // the error-wrapping path below.
        let err = child_process::exec(&resolved, args, process_env);
        Err(anyhow::Error::from(err).context(format!("failed to run '{}'", resolved.display())))
    }
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
/// Extracted from [`Run::execute`] so this exact wiring is unit-testable
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
    /// primitive; this proves `run.rs` actually wires it with both vectors.
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
    /// `ocx run` Phase D/E/F pipeline: `select_tool_set` → `filter_by_names` →
    /// `resolve_selected_tools`. A windows-only sibling in the default group
    /// must NOT block `ocx run cmake` on a linux host; but `ocx run -- ...`
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
    // selector on `ContextOptions` (peer of `--project`), so `Run` carries no
    // `global` field and `ocx run --global` parses as `ocx --global run`.
    // Root-flag parsing is clap-derived; the `--global` ⟂ `--project`
    // exclusivity is covered by `app::context` unit tests and the acceptance
    // suite (`test/tests/test_run_global_isolation.py`).

    /// C4 (no-strip contract): the `Run` struct exposes no strip mechanism
    /// (`--strip-global`, `--emit-global-path-strip`).
    ///
    /// Compile-and-parse structural proof: if a strip flag were re-introduced
    /// on `Run`, clap would accept it and these assertions would fail —
    /// keeping the deletion explicit and enforced.
    #[test]
    fn run_no_strip_field_clap_surface() {
        // `--strip-global` or `--emit-strip` do not exist — clap must reject them.
        let result = Run::try_parse_from(["run", "--strip-global", "--", "echo", "hi"]);
        assert!(
            result.is_err(),
            "the strip mechanism (`--strip-global`) must not exist on `Run`; clap must reject it"
        );

        let result = Run::try_parse_from(["run", "--emit-global-path-strip", "--", "echo", "hi"]);
        assert!(
            result.is_err(),
            "the strip mechanism (`--emit-global-path-strip`) must not exist on `Run`; clap must reject it"
        );
    }

    /// `--self` was removed from `ocx run` (a documented breaking change): the
    /// self view selects a package's own private surface, which by construction
    /// drops that package's `entrypoints/` from `PATH`, so a toolchain consumer
    /// asking for it composed a strictly worse toolchain. The flag survives on
    /// the package tier, where a package's own surface is the thing being asked
    /// about — this pins only that `Run` no longer accepts it.
    #[test]
    fn run_rejects_the_removed_self_flag() {
        let result = Run::try_parse_from(["run", "--self", "--", "echo", "hi"]);
        assert!(
            result.is_err(),
            "`--self` was removed from `ocx run`; clap must reject it"
        );
    }
}
