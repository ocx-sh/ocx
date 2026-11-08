// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier `ocx exec` command.
//!
//! `value_terminator = "--"` plus `last = true` on `argv` needs clap 4.5.57 or later (4.5.55 regressed it).

use std::process::ExitCode;

use clap::Parser;
use ocx_config::exec_state::{self, Owner, PinnedTier, ScopeId, Selection, SessionSource};
use ocx_package::launch::LaunchIdentities;
use ocx_package::metadata::env::entry::Entry;
use ocx_package_manager::EnvScope;
use ocx_package_manager::composer::{ComposeOmission, ComposeRequest, Materialization};
use ocx_package_manager::exec_env::{self, ChildEnvInputs, ExecMode};
use ocx_package_manager::launch::{self, Launch};
use ocx_package_manager::record::{PackageBinding, RecordInputs, Scope};
use ocx_project::{Origin, check_duplicate_selection, lazy_mode_for_tool, resolve_selected_tools, select_tool_set};

use crate::app::CliRefusal;
use crate::app::project_context::{filter_by_names, load_project_with_lock_consenting};
use crate::options;
use ocx_package::metadata::env::apply::{ListSeparatorError, reconcile_list_separators};

/// Run a command with the composed environment from the project toolchain.
///
/// Composes the selected groups' bindings from `ocx.toml` + `ocx.lock` in group
/// order, then binding name, and execs `ARGV` (after the mandatory `--`) with it.
/// The composition layers onto the calling environment; `--isolate` first reverts what
/// ocx applied to it, `--clean` starts from nothing. `--with` packages layer after the
/// project's tools and override a project tool of the same repository.
#[derive(Parser, Clone)]
pub struct ToolchainExec {
    #[clap(flatten)]
    pub groups: options::GroupSelection,

    #[clap(flatten)]
    pub mode: options::ExecModeOptions,

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

    #[clap(flatten)]
    pub with: options::With,

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

/// Which command composes: what the child's execution state records about its composition.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Frame {
    /// `ocx exec`: the composition is inherited values, and the scope it came from is remembered.
    Exec,
    /// `ocx shell`: the composition is session values of a pinned selection the prompt hook refreshes.
    Shell,
}

impl ToolchainExec {
    /// Execute the `ocx exec` command; the child's exit code is forwarded byte-for-byte.
    ///
    /// # Errors
    ///
    /// Exit 64 for no `ocx.toml`, an unknown group, an unknown or ambiguous name, or an empty `-g`
    /// segment; 78 for an absent `ocx.lock`; 65 for a stale one.
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        self.run(context, Frame::Exec).await
    }

    /// Execute `ocx shell`: the same composition, handed to the shell as a pinned session.
    pub(super) async fn open_shell(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        self.run(context, Frame::Shell).await
    }

    async fn run(&self, context: crate::app::Context, frame: Frame) -> anyhow::Result<ExitCode> {
        // Strict isolation: only the in-effect project file composes, never a union of tiers.

        // ── Phase A: parse-time validation ───────────────────────────────

        // Before any filesystem or network work: a typing error, not a resolution fault.
        crate::app::project_context::ensure_group_segments_nonempty(self.groups.names())?;

        // A relative `:path` anchors to the invocation directory, not the project root.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let env_overrides = self.env.entries(&cwd)?;
        let with_packages = self.with.packages(context.default_registry())?;

        // Before any I/O too, so a malformed record template is heard before tools install.
        let records = context.records(self.records.options())?;

        // ── Phase B: project context ──────────────────────────────────────
        // A resolve failure must name the selection, not CWD: a trampoline re-enters as `ocx --project '<home>' exec`.
        // The consent tri-state travels unresolved, so `OCX_NO_CONSENT` speaks only where no flag did.
        let ctx = load_project_with_lock_consenting(&context, self.consent.explicit())
            .await
            .map_err(|error| attribute_to_selected_project(context.project_path(), error))?;

        // ── Phase C: validation, `all` expansion + default scope ──────────
        let expanded = self.groups.effective(&ctx.config)?;

        // ── Phase D: resolution-free selection ────────────────────────────
        // Resolution-free, so an unnamed sibling (no host leaf, a group collision) cannot abort a narrowly-named run.
        let host = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
        let selected = select_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[])?;

        // ── Phase E: NAME filter, then duplicate validation ───────────────
        // Order is load-bearing: the duplicate check must run over the narrowed set only.
        let mut filtered = filter_by_names(selected, &expanded, &self.names)?;
        // After the name filter (a named, replaced binding is still a valid name), before the duplicate check
        // and host-leaf resolution: a replaced binding must not fail either.
        options::With::drop_overridden(&with_packages, &mut filtered);
        check_duplicate_selection(&filtered)?;

        // ── Phase F: resolve host leaves (named subset) + install ─────────
        // Named subset only, so `NoHostLeaf` (78) fires solely for a composed tool.
        let resolved = resolve_selected_tools(&filtered, &host)?;

        let manager = context.manager();

        // Composition prepends, so the last request leads PATH: `--with` packages go first and the
        // project tools after them, so a project tool wins a name clash.
        let mut requests: Vec<ComposeRequest> = options::With::requests(&with_packages, |identifier| {
            lazy_mode_for_tool(&ctx.config, identifier, None, self.lazy_mode.mode())
        });
        requests.extend(resolved.iter().map(|tool| ComposeRequest {
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
        }));
        // Deliberate deviation from `ocx env` = `ocx exec`: under the credential-helper marker nothing
        // installs, or the install waits on the pull that is waiting on this helper (#623).
        let from_credential_helper = ocx_env::__OCX_CREDENTIAL_HELPER.bool_or(false)?;
        let materialization = if from_credential_helper {
            Materialization::LocalOnly
        } else {
            Materialization::Install
        };
        let composed = manager
            .compose_roots(&requests, &host, materialization, context.concurrency())
            .await?;
        for advisory in &composed.advisories {
            context.ui().warn(advisory.to_string());
        }
        let install_infos = composed.roots;
        let omitted = composed.omitted;
        // Tools materialized on the spot: half of the execution record's drift signal.
        let auto_installed = composed.pulled;
        // Bound once: drives the parent resolve and is forwarded, so a launcher's re-entry honours the same opt-out.
        let no_patches = ctx.config.no_patches_repositories();
        // Stages 4-6 (`[env]`, group `[env]`, `--env`), also replayed through the execution state, or a
        // launcher's re-entry reverts them to the package entries.
        let stage_env = ocx_project::project_env_entries(&ctx.config, &ctx.config_path, &expanded);
        let mut project_env = stage_env.clone();
        project_env.extend(env_overrides.iter().cloned());
        // A shell records `--env` as its own invocation values, so the hook keeps them across a refresh
        // of the project layer; an exec composes them with the rest.
        let (composed_env, cli_invocation) = match frame {
            Frame::Exec => (project_env.clone(), Vec::new()),
            Frame::Shell => (stage_env, env_overrides.clone()),
        };
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
        let project_scope = |env: Vec<Entry>| EnvScope::Project {
            no_patches: no_patches.clone(),
            env,
            toolchain: Some(Box::new(toolchain.clone())),
        };
        // Never the self view: dropping each package's own `entrypoints/` from PATH is a strictly worse toolchain.
        let (mut entries, _, _, admitted) = manager
            .resolve_env_with_attribution(&install_infos, false, project_scope(composed_env.clone()), &host)
            .await?;

        // A shell records `--with` packages as invocation values, so they are composed apart from the
        // project's. The pass over everything above stays: it is what refuses a conflict between them.
        let mut invocation: Vec<Entry> = Vec::new();
        let with_roots = surviving_with_count(&omitted, &with_packages);
        if frame == Frame::Shell && with_roots > 0 {
            let (with_infos, project_infos) = install_infos.split_at(with_roots);
            (entries, _, _, _) = manager
                .resolve_env_with_attribution(project_infos, false, project_scope(composed_env), &host)
                .await?;
            let with_scope = EnvScope::Project {
                no_patches: no_patches.clone(),
                env: Vec::new(),
                toolchain: None,
            };
            (invocation, _, _, _) = manager
                .resolve_env_with_attribution(with_infos, false, with_scope, &host)
                .await?;
        }
        invocation.extend(cli_invocation);

        // Together, or a forwarded project `GODEBUG` keeps `None`
        // and a re-entrant launcher folds it with `" "`, not `","`.
        reconcile_run_entries(&mut entries, &mut invocation, &mut project_env)?;

        // ── Phase G: spawn child ──────────────────────────────────────────

        let mode = self.mode.mode();
        // Written for the child, from what the root flags and the received state say. Two command flags no
        // root flag carries are added: the records sink, which a launcher's re-entry cannot re-derive, and a
        // refused consent, which `--consent` never clears downward.
        let mut written = context.written_policy(mode.write_mode()).clone();
        let sink = records.forwarded();
        written.policy.records_name = sink.dir.as_ref().and(sink.name);
        written.policy.records_dir = sink.dir;
        written.narrowing.no_consent |= self.consent.explicit() == Some(false);
        let project_root = ctx.config_path.parent().unwrap_or(&ctx.config_path).to_path_buf();
        let (composed_owner, composed_scope, selection) = match frame {
            Frame::Exec => (
                Owner::Inherited,
                Some(if context.global() {
                    exec_state::Scope::Global
                } else {
                    exec_state::Scope::Project {
                        root: project_root.clone(),
                    }
                }),
                Selection::FollowCwd,
            ),
            Frame::Shell => {
                let (scope, tier) = if context.global() {
                    (ScopeId::Global, PinnedTier::Global)
                } else {
                    // Written canonical: the hook compares it against the received state's own spelling.
                    let dir = dunce::canonicalize(&project_root).unwrap_or_else(|_| project_root.clone());
                    (ScopeId::Project, PinnedTier::Project { dir })
                };
                let with_names: Vec<String> = with_packages.iter().map(ToString::to_string).collect();
                (
                    Owner::Session {
                        scope,
                        source: SessionSource::Composed,
                    },
                    None,
                    pinned_selection(tier, &expanded, &with_names, &env_overrides, self.pinned.pinned()),
                )
            }
        };
        // Only stages 4-6 are replayed: the launcher re-derives the package entries itself.
        let child = exec_env::apply_child_env(
            context.inherited(),
            &ChildEnvInputs {
                mode,
                written: &written,
                binary_pin: context.binary_pin(),
                composed: &entries,
                composed_owner,
                composed_scope,
                invocation: &invocation,
                replay_env: &project_env,
                identities: Some(&LaunchIdentities::from_infos(&install_infos).with_opt_out(&no_patches)),
                selection,
            },
        );
        crate::app::report_projection_warnings(&child.warnings);
        let process_env = child.env;
        // The lock's declaration hash is reused, not re-derived, keeping file I/O off the exec path.
        let declaration_digest = ocx_oci::Digest::try_from(ctx.lock.metadata.declaration_hash.as_str())?;
        let root_identifiers: Vec<_> = install_infos.iter().map(|info| info.identifier().clone()).collect();
        let bindings = project_bindings(&resolved, &omitted, &with_packages, &root_identifiers);

        let (command, _) = self
            .argv
            .split_first()
            .expect("clap last=true + num_args=1.. + required=true guarantees non-empty argv");

        // Resolved once for record and launch, or the audit trail can name a different binary.
        // Only this lookup copy of `PATH` excludes the trampoline dirs; the child's keeps them for sibling re-entry.
        let excluded = trampoline_lookup_exclusions(context.file_structure(), &toolchain_home);
        let executable = process_env
            .resolve_command_excluding(command, &excluded)
            .map_err(|error| match omitted.is_empty() {
                true => anyhow::Error::from(error),
                false => anyhow::Error::from(CliRefusal::CredentialHelperNotInstalled(format!(
                    "credential helper `{command}` is not installed; install it with `ocx pull` using other credentials (`OCX_AUTH_*`)"
                ))),
            })?;
        let launch = Launch::recording(
            process_env,
            RecordInputs {
                packages: &install_infos,
                admitted: &admitted,
                executable: &executable,
                store_root: context.file_structure().packages.root(),
                shim_root: context.file_structure().shims.root(),
                argv: &self.argv,
                config: context.recorded_config(),
                insecure_registries: context.insecure_hosts(),
                managed_config_digest: context.managed_config_snapshot().map(|snapshot| &snapshot.digest),
                patch_snapshot_digest: context.patch_snapshot_digest(),
                platform: Some(&host),
                clean_env: mode == ExecMode::Clean,
                auto_installed: &auto_installed,
                scope: Scope::Project {
                    root: project_root,
                    lock: ctx.lock_path.clone(),
                    declaration_digest,
                    groups: expanded,
                    bindings,
                    with: with_packages,
                    env_mode: mode,
                },
            },
            &records,
        )?;

        // Diverges on success (`execvp` on Unix, spawn+wait+exit on Windows); only start-up failures return.
        Err(anyhow::Error::from(launch::exec(launch).await))
    }
}

/// How many `--with` packages survive as roots; they lead the roots, since they are requested first.
fn surviving_with_count(omitted: &[ComposeOmission], with_packages: &[ocx_oci::PackageRef]) -> usize {
    with_packages
        .iter()
        .filter(|package| !omitted.iter().any(|omission| omission.identifier == **package))
        .count()
}

/// The selection `ocx shell` pins its child to: where it composed from and what it was asked for.
///
/// Names and keys are for display; the digest says whether two shells were opened with the same
/// invocation. Every `--env` pair is hashed verbatim.
fn pinned_selection(
    tier: PinnedTier,
    groups: &[String],
    with_names: &[String],
    env_overrides: &[Entry],
    pinned_paths: Option<bool>,
) -> Selection {
    let mut identity = String::new();
    for name in with_names {
        identity.push_str("with\t");
        identity.push_str(name);
        identity.push('\n');
    }
    for entry in env_overrides {
        identity.push_str(&format!("env\t{:?}\t{}\t{}\n", entry.kind, entry.key, entry.value));
    }
    Selection::Pinned(ocx_config::exec_state::Pinned {
        tier,
        groups: groups.to_vec(),
        with_names: with_names.to_vec(),
        env_keys: env_overrides.iter().map(|entry| entry.key.clone()).collect(),
        invocation_digest: ocx_oci::Algorithm::Sha256.hash(identity).hex().to_owned(),
        pinned_paths,
    })
}

/// Pair each root package with the `ocx.toml` binding it was selected under.
///
/// Zipped by index once the `omitted` requests are set aside: `compose_roots` yields one root per
/// surviving request, in order, the surviving `--with` roots leading (they are requested first).
fn project_bindings(
    resolved: &[ocx_project::ResolvedTool],
    omitted: &[ComposeOmission],
    with_packages: &[ocx_oci::PackageRef],
    roots: &[ocx_oci::PinnedPackageRef],
) -> Vec<PackageBinding> {
    let surviving_with = surviving_with_count(omitted, with_packages);
    resolved
        .iter()
        .filter(|tool| !omitted.iter().any(|omission| omission.identifier == tool.identifier))
        .zip(roots.iter().skip(surviving_with))
        .filter_map(|(tool, root)| match &tool.origin {
            Origin::Group(group) => Some(PackageBinding {
                binding: tool.binding.clone(),
                group: group.clone(),
                package: root.clone(),
            }),
            Origin::Explicit => None,
        })
        .collect()
}

/// Settles `list` separators across the composed `entries`, the shell's `invocation` values and the forwarded `project_env` in one pass.
///
/// # Errors
///
/// [`ListSeparatorError`] for conflicting explicit separators on one key, or a separator that edges its value.
fn reconcile_run_entries(
    entries: &mut [Entry],
    invocation: &mut [Entry],
    project_env: &mut [Entry],
) -> Result<(), ListSeparatorError> {
    reconcile_list_separators(
        entries
            .iter_mut()
            .chain(invocation.iter_mut())
            .chain(project_env.iter_mut()),
    )
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

        reconcile_run_entries(&mut entries, &mut [], &mut project_env).expect("one explicit separator is agreement");

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

        let error = reconcile_run_entries(&mut entries, &mut [], &mut project_env)
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
                declared: None,
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
        let named = filter_by_names(selected, &[], &["cmake".to_owned()]).expect("filter ok");
        assert!(
            resolve_selected_tools(&named, &host).is_ok(),
            "named subset (cmake) must resolve on linux host"
        );

        // names = [] (whole group) → resolve every tool → Err (winonly NoHostLeaf).
        let selected_all = select_tool_set(&config, Some(&lock), &groups, &[]).expect("select ok");
        let unnamed = filter_by_names(selected_all, &[], &[]).expect("filter ok");
        assert!(
            resolve_selected_tools(&unnamed, &host).is_err(),
            "unnamed whole-group run must still surface the windows-only sibling's NoHostLeaf"
        );
    }

    // ── project_bindings (index zip once omitted requests are set aside) ─────────

    fn pinned(repository: &str, hex_char: char) -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::pin(
            &ocx_oci::PackageRef::new_registry(repository, "ocx.sh"),
            Digest::Sha256(sha(hex_char)),
        )
    }

    fn tool(binding: &str, group: &str) -> ocx_project::ResolvedTool {
        ocx_project::ResolvedTool {
            binding: binding.to_owned(),
            identifier: ocx_oci::PackageRef::new_registry(binding, "ocx.sh").clone_with_tag("1"),
            origin: Origin::Group(group.to_owned()),
        }
    }

    fn omission(repository: &str) -> ComposeOmission {
        ComposeOmission {
            identifier: ocx_oci::PackageRef::new_registry(repository, "ocx.sh").clone_with_tag("1"),
            reason: ocx_package_manager::error::PackageErrorKind::NotFound,
        }
    }

    fn bound(bindings: &[PackageBinding]) -> Vec<(&str, &str)> {
        bindings
            .iter()
            .map(|binding| (binding.binding.as_str(), binding.package.repository()))
            .collect()
    }

    /// `--with` roots lead the composed roots; each project tool must pair with its own root, not a neighbour's.
    #[test]
    fn project_bindings_pair_each_tool_with_its_own_root() {
        let resolved = [tool("cmake", "default"), tool("ninja", "ci")];
        let with = [ocx_oci::PackageRef::new_registry("extra", "ocx.sh").clone_with_tag("1")];
        let roots = [pinned("extra", '1'), pinned("cmake", '2'), pinned("ninja", '3')];

        let bindings = project_bindings(&resolved, &[], &with, &roots);

        assert_eq!(bound(&bindings), [("cmake", "cmake"), ("ninja", "ninja")]);
        assert_eq!(bindings[1].group, "ci");
    }

    /// An omitted `--with` package shifts the leading roots by one, an omitted project tool drops out of the zip.
    #[test]
    fn project_bindings_skip_an_omitted_with_package_and_an_omitted_tool() {
        let resolved = [
            tool("cmake", "default"),
            tool("lazy", "default"),
            tool("ninja", "default"),
        ];
        let with = [
            ocx_oci::PackageRef::new_registry("gone", "ocx.sh").clone_with_tag("1"),
            ocx_oci::PackageRef::new_registry("extra", "ocx.sh").clone_with_tag("1"),
        ];
        let omitted = [omission("gone"), omission("lazy")];
        let roots = [pinned("extra", '1'), pinned("cmake", '2'), pinned("ninja", '3')];

        let bindings = project_bindings(&resolved, &omitted, &with, &roots);

        assert_eq!(bound(&bindings), [("cmake", "cmake"), ("ninja", "ninja")]);
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

    /// `--stack` is gone: layering is the default, so no flag asks for it.
    #[test]
    fn exec_rejects_the_removed_stack_flag() {
        let result = ToolchainExec::try_parse_from(["exec", "--stack", "--", "echo", "hi"]);
        assert!(
            result.is_err(),
            "`--stack` was removed from `ocx exec`; clap must reject it"
        );
    }

    /// The mode grammar: layer by default, `--isolate` and `--clean` each select theirs, never both.
    #[test]
    fn exec_selects_its_mode_from_the_flags() {
        use ocx_package_manager::exec_env::ExecMode;
        let mode = |flags: &[&str]| {
            let argv = ["exec"].into_iter().chain(flags.iter().copied()).chain(["--", "true"]);
            ToolchainExec::try_parse_from(argv).map(|exec| exec.mode.mode())
        };

        assert_eq!(mode(&[]).ok(), Some(ExecMode::Layer));
        assert_eq!(mode(&["--isolate"]).ok(), Some(ExecMode::Isolate));
        assert_eq!(mode(&["--clean"]).ok(), Some(ExecMode::Clean));
        assert!(mode(&["--isolate", "--clean"]).is_err());
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

    // ── shell: the pinned selection ──────────────────────────────────────────

    fn constant(key: &str, value: &str) -> Entry {
        Entry {
            key: key.to_owned(),
            value: value.to_owned(),
            kind: ocx_package::metadata::env::modifier::ModifierKind::Constant,
            separator: None,
        }
    }

    fn pinned_of(selection: Selection) -> ocx_config::exec_state::Pinned {
        match selection {
            Selection::Pinned(pinned) => pinned,
            Selection::FollowCwd => panic!("a shell pins its selection"),
        }
    }

    fn project_tier() -> PinnedTier {
        PinnedTier::Project {
            dir: std::path::PathBuf::from("/work/project"),
        }
    }

    #[test]
    fn shell_selection_names_the_groups_with_names_and_env_keys() {
        let pinned = pinned_of(pinned_selection(
            project_tier(),
            &["default".to_owned(), "ci".to_owned()],
            &["ocx.sh/cmake:3".to_owned()],
            &[constant("JAVA_HOME", "/opt/java"), constant("MODE", "fast")],
            Some(true),
        ));

        assert_eq!(pinned.groups, ["default", "ci"]);
        assert_eq!(pinned.with_names, ["ocx.sh/cmake:3"]);
        assert_eq!(pinned.env_keys, ["JAVA_HOME", "MODE"]);
        assert_eq!(pinned.pinned_paths, Some(true));
        assert_eq!(pinned.tier, project_tier());
        assert_eq!(pinned.invocation_digest.len(), 64);
        assert!(pinned.invocation_digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn shell_selection_never_holds_an_env_value() {
        let selection = pinned_selection(
            project_tier(),
            &[],
            &[],
            &[constant("JAVA_HOME", "fixture-env-value")],
            None,
        );

        let text = serde_json::to_string(&selection).expect("a selection serializes");
        assert!(!text.contains("fixture-env-value"), "{text}");
    }

    #[test]
    fn shell_selection_digest_follows_the_invocation() {
        let digest = |with: &[&str], env: &[Entry]| {
            let with: Vec<String> = with.iter().map(|name| (*name).to_owned()).collect();
            pinned_of(pinned_selection(project_tier(), &[], &with, env, None)).invocation_digest
        };

        let base = digest(&["ocx.sh/cmake:3"], &[constant("A", "1")]);
        assert_eq!(base, digest(&["ocx.sh/cmake:3"], &[constant("A", "1")]));
        assert_ne!(base, digest(&["ocx.sh/cmake:4"], &[constant("A", "1")]));
        assert_ne!(base, digest(&["ocx.sh/cmake:3"], &[constant("A", "2")]));
        assert_ne!(base, digest(&["ocx.sh/cmake:3"], &[constant("B", "1")]));
    }

    #[test]
    fn shell_selection_digest_hashes_a_formerly_secret_classed_value_verbatim() {
        let digest = |value: &str| {
            pinned_of(pinned_selection(
                project_tier(),
                &[],
                &[],
                &[constant("API_TOKEN", value)],
                None,
            ))
            .invocation_digest
        };

        assert_ne!(digest("fixture-secret-one"), digest("fixture-secret-two"));
        assert_eq!(digest("fixture-secret-one"), digest("fixture-secret-one"));
    }

    #[test]
    fn surviving_with_count_skips_the_omitted_packages() {
        let with = [
            ocx_oci::PackageRef::new_registry("kept", "ocx.sh").clone_with_tag("1"),
            ocx_oci::PackageRef::new_registry("gone", "ocx.sh").clone_with_tag("1"),
        ];

        assert_eq!(surviving_with_count(&[], &with), 2);
        assert_eq!(surviving_with_count(&[omission("gone")], &with), 1);
    }
}
