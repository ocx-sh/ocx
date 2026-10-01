// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier `ocx exec` command.
//!
//! `value_terminator = "--"` plus `last = true` on `argv` needs clap 4.5.57 or later (4.5.55 regressed it).

use std::process::ExitCode;

use clap::Parser;
use ocx_config::env;
use ocx_package::launch::LaunchIdentities;
use ocx_package::metadata::env::entry::Entry;
use ocx_package_manager::composer::{ComposeRequest, Materialization};
use ocx_package_manager::launch::{self, Launch};
use ocx_package_manager::record::{PackageBinding, RecordInputs, Scope};
use ocx_project::{
    DEFAULT_GROUP, Origin, check_duplicate_selection, expand_all_keyword, lazy_mode_for_tool, resolve_selected_tools,
    select_tool_set,
};

use crate::app::project_context::{filter_by_names, load_project_with_lock_consenting};
use crate::options;
use ocx_package::metadata::env::apply::{ChildEnv, EnvEntriesExt, ListSeparatorError, reconcile_list_separators};
use ocx_shell::shell::reconcile;

/// Run a command with the composed environment from the project toolchain.
///
/// Loads the nearest `ocx.toml` and its `ocx.lock`, selects the tool bindings
/// in the requested groups, composes their environment, and execs `ARGV` with
/// it. `--` is mandatory: before it is a binding name filter, after it the
/// command and arguments, forwarded unchanged. Composition order is the group
/// selection order (`-g` flags after `all` expansion, deduplicated), then
/// alphabetical by binding name within each group (lock-file order).
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
    /// composes for the child.
    #[clap(flatten)]
    pub pinned: options::Pinned,

    #[clap(flatten)]
    pub records: options::Records,

    // `--consent` (the default) / `--no-consent`; clap renders the flattened struct's docs, not a `///` here.
    #[clap(flatten)]
    pub consent: options::Consent,

    /// Binding names to compose into the child env. Each name must
    /// resolve unambiguously inside the selected scope. Only the named
    /// tools are resolved to a host leaf, so an unrelated tool in scope
    /// that ships no leaf for this host does not block the run. An empty
    /// list means "every binding in scope"; then every tool must resolve.
    // Stops name collection at `--`, so hyphen-prefixed argv is never read as more names.
    #[arg(num_args = 0.., value_terminator = "--")]
    pub names: Vec<String>,

    /// Command to execute, with arguments. The command runs with the
    /// composed package env. `--` is mandatory and at least one argv
    /// token is required.
    // `allow_hyphen_values`: flag-prefixed argv like `--format json` reaches the child unchanged.
    // `required` + `num_args = 1..`: an empty argv is a usage error, not a panic on `split_first`.
    #[arg(allow_hyphen_values = true, last = true, num_args = 1.., required = true)]
    pub argv: Vec<String>,
}

impl ToolchainExec {
    /// Execute the `ocx exec` command; the child's exit code is forwarded byte-for-byte.
    ///
    /// # Errors
    ///
    /// Exit 64 for no `ocx.toml`, an unknown group, an unknown or ambiguous name, or an empty `-g`
    /// segment; 78 for an absent `ocx.lock`; 65 for a stale one.
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Strict isolation: only the in-effect project file composes, never a union of tiers.

        // ── Phase A: parse-time validation ───────────────────────────────

        // Before any filesystem or network work: a typing error, not a resolution fault.
        crate::app::project_context::ensure_group_segments_nonempty(self.groups.names())?;

        // A relative `:path` anchors to the invocation directory, not the project root.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;

        // Before any I/O too, so a malformed record template is heard before tools install.
        let records = context.records(self.records.options())?;

        // ── Phase B: project context ──────────────────────────────────────
        // A resolve failure must name the selection, not CWD: a trampoline re-enters as `ocx --project '<home>' exec`.
        // The consent tri-state travels unresolved, so `OCX_NO_CONSENT` speaks only where no flag did.
        let ctx = load_project_with_lock_consenting(&context, self.consent.explicit())
            .await
            .map_err(|error| attribute_to_selected_project(context.project_path(), error))?;

        crate::app::project_context::ensure_groups_known(self.groups.names(), &ctx.config)?;

        // ── Phase C: `all` expansion + default scope ───────────────────────

        let mut expanded = expand_all_keyword(self.groups.names(), &ctx.config);
        if expanded.is_empty() {
            expanded = vec![DEFAULT_GROUP.to_owned()];
        }

        // ── Phase D: resolution-free selection ────────────────────────────
        // Resolution-free, so an unnamed sibling (no host leaf, a group collision) cannot abort a narrowly-named run.
        let host = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
        let selected = select_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[])?;

        // ── Phase E: NAME filter, then duplicate validation ───────────────
        // Order is load-bearing: the duplicate check must run over the narrowed set only.
        let filtered = filter_by_names(selected, &self.names)?;
        check_duplicate_selection(&filtered)?;

        // ── Phase F: resolve host leaves (named subset) + install ─────────
        // Named subset only, so `NoHostLeaf` (78) fires solely for a composed tool.
        let resolved = resolve_selected_tools(&filtered, &host)?;

        let manager = context.manager();

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
        // Tools materialized on the spot: half of the execution record's drift signal.
        let auto_installed = composed.pulled;
        // Bound once: drives the parent resolve and is forwarded, so a launcher's re-entry honours the same opt-out.
        let no_patches = ctx.config.no_patches_repositories();
        // Stages 4-6 (`[env]`, group `[env]`, `--env`), also forwarded over `OCX_ENV`, or a launcher's re-entry
        // reverts them to the package entries.
        let mut project_env = ocx_project::project_env_entries(&ctx.config, &ctx.config_path, &expanded);
        project_env.extend(env_overrides);
        // Derived here, so composition and the trampoline exclusion below agree on this project's tree.
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
        let scope = ocx_package_manager::EnvScope::Project {
            no_patches: no_patches.clone(),
            env: project_env.clone(),
            toolchain: Some(Box::new(toolchain)),
        };
        // Never the self view: dropping each package's own `entrypoints/` from PATH is a strictly worse toolchain.
        let (mut entries, _, _, admitted) = manager
            .resolve_env_with_attribution(&install_infos, false, scope, &host)
            .await?;

        // Together, or a forwarded project `GODEBUG` keeps `None`
        // and a re-entrant launcher folds it with `" "`, not `","`.
        reconcile_run_entries(&mut entries, &mut project_env)?;

        // ── Phase G: spawn child ──────────────────────────────────────────

        let mut process_env = if self.clean {
            env::Env::clean()
        } else {
            reconcile::inherited_env()
        };
        let mut forwarded_config = context.config_view().clone();
        if let Some(patches) = forwarded_config.patches.as_mut() {
            patches.no_patches = no_patches;
        }
        // A child cannot re-derive the flag tier, so without this a launcher's re-entry records elsewhere or nowhere.
        forwarded_config.records = records.forwarded();
        // argv does not cross a spawn: a refusal inherits downward, and `--consent` never clears an inherited one.
        forwarded_config.no_consent = self.consent.explicit() == Some(false);
        // Only stages 4-6 are forwarded: the launcher re-derives the package entries itself.
        process_env.apply_child_env(
            ChildEnv {
                composed: &entries,
                forwarded: &project_env,
                identities: Some(&LaunchIdentities::from_infos(&install_infos)),
            },
            &forwarded_config,
        );
        // The lock's declaration hash is reused, not re-derived, keeping file I/O off the exec path.
        let project_root = ctx.config_path.parent().unwrap_or(&ctx.config_path).to_path_buf();
        let declaration_digest = ocx_oci::Digest::try_from(ctx.lock.metadata.declaration_hash.as_str())?;
        let bindings = project_bindings(&resolved, &install_infos);

        let (command, _) = self
            .argv
            .split_first()
            .expect("clap last=true + num_args=1.. + required=true guarantees non-empty argv");

        // Resolved once for record and launch, or the audit trail can name a different binary.
        // Only this lookup copy of `PATH` excludes the trampoline dirs; the child's keeps them for sibling re-entry.
        let excluded = trampoline_lookup_exclusions(context.file_structure(), &toolchain_home);
        let executable = process_env.resolve_command_excluding(command, &excluded)?;
        let launch = Launch::recording(
            process_env,
            RecordInputs {
                packages: &install_infos,
                admitted: &admitted,
                executable: &executable,
                store_root: context.file_structure().packages.root(),
                shim_root: context.file_structure().shims.root(),
                argv: &self.argv,
                config: context.config_view(),
                insecure_registries: context.insecure_hosts(),
                managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
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

        // Diverges on success (`execvp` on Unix, spawn+wait+exit on Windows); only start-up failures return.
        Err(anyhow::Error::from(launch::exec(launch).await))
    }
}

/// Pair each root package with the `ocx.toml` binding it was selected under.
///
/// Zipped by index: under `Materialization::Install` `compose_roots` yields one root per request, in order.
fn project_bindings(
    resolved: &[ocx_project::ResolvedTool],
    install_infos: &[std::sync::Arc<ocx_package::install_info::InstallInfo>],
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

/// Settles `list` separators across the composed `entries` and the forwarded `project_env` in one pass.
///
/// # Errors
///
/// [`ListSeparatorError`] for conflicting explicit separators on one key, or a separator that edges its value.
fn reconcile_run_entries(entries: &mut [Entry], project_env: &mut [Entry]) -> Result<(), ListSeparatorError> {
    reconcile_list_separators(entries.iter_mut().chain(project_env.iter_mut()))
}

/// The trampoline dirs the lookup `PATH` excludes, for both the project and global tree.
///
/// Both spellings per home: the exclusion is segment-exact, so a `PATH` carrying the physical one would
/// otherwise resolve to a self-re-entering trampoline.
fn trampoline_lookup_exclusions(
    file_structure: &ocx_store::file_structure::FileStructure,
    project_home: &ocx_store::file_structure::ToolchainHome,
) -> Vec<std::path::PathBuf> {
    // Spelled out, not imported, so dropping an entry reds this function's test, not an unused-import lint.
    vec![
        // From the store's resolver, never a literal `join`, or the list drifts silently from the tree shape.
        file_structure.toolchain.bin(),
        project_home.bin(),
        file_structure
            .toolchain
            .shell_bin(ocx_store::file_structure::DEFAULT_SHELL),
        project_home.shell_bin(ocx_store::file_structure::DEFAULT_SHELL),
    ]
}

/// Re-attribute a missing selected project to [`ProjectContextError::NoProjectIn`] (64), not the config tier's 79.
///
/// A trampoline re-enters with its baked home as `--project`; every other error passes through unchanged.
fn attribute_to_selected_project(
    selected: Option<&std::path::Path>,
    error: crate::app::project_context::ProjectContextError,
) -> crate::app::project_context::ProjectContextError {
    use crate::app::project_context::ProjectContextError;

    let Some(selected) = selected else { return error };
    match error {
        // Sound only while `ProjectConfig::resolve` is this path's sole `Config` source: a new
        // `FileNotFound` elsewhere would be misremapped to 64.
        ProjectContextError::Config(ocx_config::error::Error::FileNotFound { .. }) => {
            ProjectContextError::NoProjectIn {
                dir: selected.to_path_buf(),
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_oci::{Digest, Platform};
    use ocx_project::{LockMetadata, LockVersion, LockedTool, ProjectConfig, ProjectLock};
    use std::collections::BTreeMap;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    // ── reconcile_run_entries (chained-vector wiring) ───────────────────────────

    fn list_entry(key: &str, value: &str, separator: Option<&str>) -> Entry {
        Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind: ocx_package::metadata::env::modifier::ModifierKind::List,
            separator: separator.map(str::to_owned),
        }
    }

    /// The motivating case: a package's explicit separator (in `entries`)
    /// must reach the project's own entry for the same key even though it
    /// lives in the disjoint `project_env` vector — `ocx_config::env`'s own
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
        assert!(matches!(&error, ListSeparatorError::Conflict { key, .. } if key == "GODEBUG"));
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
                repository: ocx_oci::Repository::new("ocx.sh", name),
                platforms,
            }
        }

        let mut lock = lock_v3(vec![
            leaf("cmake", "linux/amd64", 'a'),
            leaf("winonly", "windows/amd64", 'b'),
        ]);
        // Declarations matching the lock, so it binds current.
        let declared = ["cmake", "winonly"].map(|name| {
            (
                name.to_owned(),
                ocx_oci::PackageRef::new_registry(name, "ocx.sh").clone_with_tag("1.0"),
            )
        });
        let config = ProjectConfig::from_parts(BTreeMap::from(declared), BTreeMap::new());
        lock.metadata.declaration_hash = config.declaration_hash_cached().to_owned();
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

    // ── no-strip clap surface ────────────────────────────────────────────
    //
    // `--global` is no longer a per-command flag — it is a single root-level
    // selector on `ContextOptions` (peer of `--project`), so `ToolchainExec` carries no
    // `global` field and `ocx exec --global` parses as `ocx --global exec`.
    // Root-flag parsing is clap-derived; the `--global` ⟂ `--project`
    // exclusivity is covered by `app::context` unit tests and the acceptance
    // suite (`test/tests/test_run_global_isolation.py`).

    /// The no-strip contract: the `ToolchainExec` struct exposes no strip mechanism
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

    // ── the exclusion set is derived, never joined ───────────────────────────

    /// Four entries, all from the resolvers that
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
    /// in `toolchain` — which is exactly what a `toolchain_dir` relocation
    /// produces at `<root>/<project-key>/toolchain`, and what a
    /// `join("toolchain").join("bin")` at the call site would answer wrongly
    /// for. Testable only because this helper's signature takes no `&Context`:
    /// a `Context` is constructible solely through the async `try_init`, which
    /// installs the global tracing subscriber.
    ///
    /// RED: drop either physical entry from the returned vector — the set
    /// assertion, the literal assertion and the count all red, and the
    /// surviving spelling proves nothing about the one that left.
    #[test]
    fn the_exclusion_set_is_both_trees_own_bin_directories() {
        use ocx_store::file_structure::DEFAULT_SHELL;

        let home = std::path::PathBuf::from("/w/.ocx-home");
        let file_structure = ocx_store::file_structure::FileStructure::with_root(home);
        // A relocated project home: the segment before `bin` is a project key,
        // not the literal `toolchain`.
        let project_home = ocx_store::file_structure::ToolchainHome::new("/w/toolchains/0123456789abcdef/toolchain");

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

    // ── a trampoline's baked home, re-attributed ──────────────────────────────

    use crate::app::project_context::ProjectContextError;
    use ocx_exit::ExitCode;

    /// The shipped exit-code authority — `main.rs`'s own classifier, walking
    /// the source chain for CLI-local types before delegating to the library.
    ///
    /// Every code assertion below goes through it rather than restating an
    /// integer, so a mapping that is correct in the enum and lost on the way to
    /// the process's status still reds.
    fn code_of(error: &ProjectContextError) -> ExitCode {
        crate::exit::classify_error(error)
    }

    /// The `config::Error` a *missing* explicit `--project` selection really
    /// produces, from the shipped loader rather than a hand-built variant.
    async fn selection_error(selected: &std::path::Path) -> ocx_config::error::Error {
        ocx_config::loader::ConfigLoader::project_path(None, Some(selected))
            .await
            .expect_err("an explicit --project naming an absent path is an error")
    }

    /// The defect this test names, pinned as a **control** so the mapping below is
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

    /// An explicit selection that did not resolve becomes
    /// `NoProjectIn`, **naming the selection**, and classifies as **64**.
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
            matches!(&mapped, ProjectContextError::NoProjectIn { dir } if dir == &baked),
            "C-068 — the re-attributed error must name the *selected* home, got: {mapped:?}"
        );
        assert_eq!(
            code_of(&mapped),
            ExitCode::UsageError,
            "C-068 — `NoProjectIn` is exit 64, and the trampoline path inherits it"
        );
    }

    /// The boundary: with **no** explicit selection there is nothing to
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
            !matches!(
                passed_through,
                ProjectContextError::NoProject { .. } | ProjectContextError::NoProjectIn { .. }
            ),
            "with no explicit selection there is no home to attribute the failure to"
        );
        assert_eq!(
            code_of(&passed_through),
            ExitCode::NotFound,
            "an untouched error keeps its own classification"
        );
    }

    /// A project file that **does exist** but does not parse is not a
    /// missing project, even under an explicit selection.
    ///
    /// Mutation that reds it: a blanket `_ => NoProjectIn { dir: selection }`,
    /// which would answer 64 for a broken `ocx.toml` and send the user looking
    /// for a file that is right there.
    #[tokio::test]
    async fn a_parse_failure_in_an_existing_project_file_is_not_re_attributed() {
        let tmp = tempfile::tempdir().expect("a tempdir is creatable");
        let project = tmp.path().to_path_buf();
        std::fs::write(project.join("ocx.toml"), "this is not = = toml").expect("the manifest is writable");
        let resolved = ocx_config::loader::ConfigLoader::project_path(None, Some(&project))
            .await
            .expect("a directory holding an ocx.toml resolves (RUL-55)")
            .expect("the file is present");
        let bytes = tokio::fs::read(&resolved).await.expect("the manifest is readable");
        let parse_failure = ocx_project::ProjectConfig::from_toml_bytes_with_path(&bytes, resolved)
            .expect_err("an unparsable manifest is an error");
        let error = ProjectContextError::from(parse_failure);
        let before = code_of(&error);

        let mapped = attribute_to_selected_project(Some(&project), error);
        assert!(
            !matches!(
                mapped,
                ProjectContextError::NoProject { .. } | ProjectContextError::NoProjectIn { .. }
            ),
            "a manifest that exists and does not parse is not `NoProjectIn`"
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

    /// The two lock arms, both under an explicit selection: they already
    /// classify correctly and already carry a path, so re-attribution must
    /// leave them alone.
    ///
    /// `Missing` → **78**, `Stale` → **65** — the other two thirds of the
    /// three-way mapping, each keeping the baked path in its message.
    #[test]
    fn the_two_lock_arms_keep_their_own_codes_and_paths() {
        let baked = std::path::PathBuf::from("/w/proj");
        let lock_path = baked.join("ocx.lock");

        let missing = ProjectContextError::from(ocx_project::LockCurrency::Missing {
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

        let stale = ProjectContextError::from(ocx_project::LockCurrency::Stale {
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
