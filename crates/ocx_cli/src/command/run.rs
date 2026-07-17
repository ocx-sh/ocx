// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier `ocx run` command.
//!
//! `ocx run` is the project-tier counterpart to the OCI-tier `ocx exec`.
//! Symbols are binding names from `ocx.toml`, not OCI identifiers. The
//! command selects the bindings in the requested groups (resolution-free,
//! whole-scope duplicate validation), narrows that selection to the requested
//! `NAME`s, resolves the host leaf of **only** the named subset through
//! `ocx.lock` (digest-pinned), composes the child environment from those
//! packages, and execs the given `ARGV` in that environment — mirroring
//! `ocx exec`'s child-spawn mechanics but driven entirely by the project
//! toolchain declaration.
//!
//! Resolution is scoped to the named subset: a tool elsewhere in scope that
//! ships no leaf for the current host (`NoHostLeaf`, exit 78) only aborts the
//! run when it is among the composed tools (the named subset, or every tool in
//! scope when no `NAME` is given).
//!
//! # NOTE: clap floor
//!
//! The `value_terminator = "--"` on `names` combined with `last = true` on
//! `argv` requires clap ≥ 4.5.57. clap 4.5.55 introduced a regression in
//! this combination; 4.5.57 fixed it. The floor is set in `Cargo.toml`.

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::env;
use ocx_lib::project::{
    ALL_GROUP, DEFAULT_GROUP, Origin, SelectedTool, expand_all_keyword, resolve_selected_tools, select_tool_set,
};
use ocx_lib::utility::child_process;

use crate::app::project_context::load_project_with_lock;

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
    /// Restrict the env composition to the named group(s).
    ///
    /// Repeatable and comma-separated: `-g ci,lint -g release`. The
    /// reserved name `default` selects the top-level `[tools]` table.
    /// The reserved name `all` expands to `default` + every declared
    /// `[group.*]`. When omitted, scope is exactly `[tools]`
    /// (matches `ocx pull` precedent: omitted `-g` does NOT mean
    /// "everything"; it means "the default group").
    #[arg(short = 'g', long = "group", value_delimiter = ',')]
    pub groups: Vec<String>,

    /// Start with a clean environment containing only the package
    /// variables, instead of inheriting the current shell environment.
    #[arg(long = "clean", default_value_t = false)]
    pub clean: bool,

    /// Expose each package's full env, including its private (self-only)
    /// entries. See `ocx exec --self` for the cross-cutting flag contract.
    #[arg(long = "self", default_value_t = false)]
    pub self_view: bool,

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
    /// `select_tool_set` (resolution-free; whole-scope duplicate validation),
    /// narrows the selection to the requested `names`, resolves the host
    /// leaves of that named subset via `resolve_selected_tools`, and execs
    /// `argv` with the resulting package environment. Exit code is forwarded
    /// byte-for-byte from the child process on success.
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
        use ocx_lib::cli;

        // Strict isolation (C2.6): `run` composes exactly the in-effect
        // project file. Root `--global` only re-targets which single file
        // that is (the global one) — `select_tool_set` below is still fed
        // one tier (`&ctx.config`/`&ctx.lock`), never a union with a project.

        // ── Phase A: parse-time validation ───────────────────────────────

        // Reject empty comma segments (`-g ci,,lint`) BEFORE any filesystem
        // or network work. `clap`'s `value_delimiter = ','` splits the value
        // into `["ci", "", "lint"]`; an empty string is a user-typing error.
        for raw in &self.groups {
            if raw.is_empty() {
                return Err(cli::UsageError::new("empty group segment in --group value").into());
            }
        }

        // ── Phase B: project context ──────────────────────────────────────
        // Errors propagate to the `main.rs` boundary: logged once and
        // classified by `app::classify_error` from `ProjectContextError`'s
        // `ClassifyExitCode` impl (NoProject→64, LockMissing→78, StaleLock→65).
        let ctx = load_project_with_lock(&context).await?;

        // Phase B.3: validate `-g` groups against the loaded config.
        // `default` and `all` are always valid (all is expanded later).
        // Anything else must appear in config.groups.
        for raw in &self.groups {
            if raw == DEFAULT_GROUP || raw == ALL_GROUP {
                continue;
            }
            if !ctx.config.groups.contains_key(raw) {
                return Err(cli::UsageError::new(format!("unknown group '{raw}' in --group filter")).into());
            }
        }

        // ── Phase C: `all` expansion + default scope ───────────────────────

        let mut expanded = expand_all_keyword(&self.groups, &ctx.config);
        // Default scope: if groups is empty (no -g flags) or expansion produced
        // an empty list, scope = [DEFAULT_GROUP] — matches pull semantics.
        if expanded.is_empty() {
            expanded = vec![DEFAULT_GROUP.to_owned()];
        }

        // ── Phase D: resolution-free selection ────────────────────────────
        // `select_tool_set` performs whole-scope duplicate-across-groups
        // validation but does NOT resolve host leaves, so an unnamed sibling
        // with no leaf for this host cannot abort a narrowly-named run. The
        // host platform is computed here but consumed in Phase F.
        let host = ocx_lib::oci::Platform::current().unwrap_or_else(ocx_lib::oci::Platform::any);
        let selected = select_tool_set(&ctx.config, Some(&ctx.lock), &expanded, &[])?;

        // ── Phase E: NAME filter ──────────────────────────────────────────

        let filtered = match filter_by_names(selected, &self.names) {
            Ok(v) => v,
            Err(RunFilterError::Unknown { name }) => {
                return Err(cli::UsageError::new(format!("binding '{name}' not found in selected groups")).into());
            }
            Err(RunFilterError::Ambiguous { name, groups }) => {
                let groups_str = groups.join(", ");
                return Err(cli::UsageError::new(format!(
                    "binding '{name}' exists in multiple selected groups: [{groups_str}]; pass `-g <group>` to narrow scope"
                ))
                .into());
            }
        };

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
        let scope = ocx_lib::package_manager::PatchScope::Project(no_patches.clone());
        let entries = manager
            .resolve_env_with_patch_boundary(&install_infos, self.self_view, scope)
            .await?
            .0;

        // ── Phase G: spawn child ──────────────────────────────────────────

        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
        process_env.apply_entries(&entries);
        // Forward the running ocx's resolution-affecting config (binary path,
        // offline/remote, config file, index) to any child ocx (e.g. through
        // a generated entrypoint launcher). Runs after `Env::clean()` /
        // `Env::new()` so the outer ocx's parsed state is the sole authority
        // for `OCX_*` keys on the child env — no ambient parent-shell export
        // can override it.
        //
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
        let mut forwarded = context.config_view().clone();
        if let Some(patches) = forwarded.patches.as_mut() {
            patches.no_patches = forwarded_no_patches;
        }
        process_env.apply_ocx_config(&forwarded);
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

/// Errors from the CLI-layer NAME filter applied after `select_tool_set`.
///
/// Both variants correspond to exit 64 (`UsageError`) — the user named a
/// binding that is either absent from or ambiguous within the composed set.
/// Private to this module; the command maps these to `eprintln` + return
/// before surfacing them as `anyhow::Error`.
#[derive(Debug, thiserror::Error)]
enum RunFilterError {
    /// The requested binding name was not found in any tool in the composed set.
    #[error("binding '{name}' not found in selected groups")]
    Unknown { name: String },

    /// The requested binding name matched entries in two or more selected
    /// groups (defense-in-depth — not reachable through normal flow in v1;
    /// see plan §NAME Filter for rationale).
    #[error("binding '{name}' exists in multiple selected groups: {groups:?}; pass `-g <group>` to narrow scope")]
    Ambiguous { name: String, groups: Vec<String> },
}

/// Filter the selected tool set to the explicitly-requested binding names.
///
/// Operates on the resolution-free [`SelectedTool`]s from `select_tool_set`,
/// reading only `binding` and `origin`; host-leaf resolution happens after
/// this narrowing so an unrelated, unnamed sibling never participates.
///
/// When `names` is empty, the full `selected` set is returned unchanged —
/// every binding in scope participates.
///
/// When `names` is non-empty, user-supplied name order wins: the output
/// preserves the order of `names`, not the order of `selected`. Duplicate
/// names in `names` are silently deduplicated (same binding enumerated twice
/// is not a usage error).
///
/// # Errors
///
/// - [`RunFilterError::Unknown`] — a requested name has no match in `selected`.
/// - [`RunFilterError::Ambiguous`] — a requested name matches multiple entries
///   (defense-in-depth; not reachable through normal v1 flow — see plan §NAME Filter).
fn filter_by_names(selected: Vec<SelectedTool>, names: &[String]) -> Result<Vec<SelectedTool>, RunFilterError> {
    if names.is_empty() {
        return Ok(selected);
    }

    // Build a `binding -> Vec<index_into_selected>` lookup once. Replaces the
    // previous O(N·M) `selected.iter().filter` scan with an O(1) probe per
    // user-supplied name. The hits are stored as indices so we can move out
    // of `selected` without reborrowing during the user-order walk below.
    let mut hits_by_binding: std::collections::HashMap<&str, Vec<usize>> =
        std::collections::HashMap::with_capacity(selected.len());
    for (i, tool) in selected.iter().enumerate() {
        hits_by_binding.entry(tool.binding.as_str()).or_default().push(i);
    }

    // Iterate names in user order. Dedup by tracking seen names.
    let mut out = Vec::with_capacity(names.len());
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for name in names {
        if !seen.insert(name.as_str()) {
            // Duplicate name — silently skip.
            continue;
        }
        let hit_indices = hits_by_binding.get(name.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        match hit_indices {
            [] => return Err(RunFilterError::Unknown { name: name.clone() }),
            [single] => out.push(selected[*single].clone()),
            [_, _, ..] => {
                let groups: Vec<String> = hit_indices
                    .iter()
                    .filter_map(|&i| match &selected[i].origin {
                        Origin::Group(g) => Some(g.clone()),
                        Origin::Explicit => None,
                    })
                    .collect();
                return Err(RunFilterError::Ambiguous {
                    name: name.clone(),
                    groups,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::oci::{Digest, Identifier, PinnedIdentifier, Platform};
    use ocx_lib::project::{LockMetadata, LockVersion, LockedTool, ProjectConfig, ProjectLock, ToolSource};
    use std::collections::BTreeMap;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn pin(repo: &str, tag: Option<&str>, c: char) -> PinnedIdentifier {
        let mut id = Identifier::new_registry(repo, "ocx.sh");
        if let Some(t) = tag {
            id = id.clone_with_tag(t);
        }
        let id = id.clone_with_digest(Digest::Sha256(sha(c)));
        PinnedIdentifier::try_from(id).expect("digest present")
    }

    fn tool(binding: &str, c: char, group: &str) -> SelectedTool {
        SelectedTool {
            binding: binding.into(),
            origin: Origin::Group(group.into()),
            source: ToolSource::Explicit(pin(binding, None, c).into()),
        }
    }

    // ── filter_by_names ──────────────────────────────────────────────────────

    /// Plan §Phase 3.1: `filter_empty_names_returns_full_set`
    ///
    /// When names is empty every binding in scope participates.
    #[test]
    fn filter_empty_names_returns_full_set() {
        let composed = vec![tool("cmake", 'a', "default"), tool("ninja", 'b', "default")];
        let result = filter_by_names(composed.clone(), &[]).expect("empty names must succeed");
        assert_eq!(result.len(), composed.len());
        assert!(result.iter().any(|r| r.binding == "cmake"));
        assert!(result.iter().any(|r| r.binding == "ninja"));
    }

    /// Plan §Phase 3.1: `filter_unknown_name_errors`
    ///
    /// A name with no match in the composed set → `RunFilterError::Unknown`.
    #[test]
    fn filter_unknown_name_errors() {
        let composed = vec![tool("cmake", 'a', "default")];
        let err = filter_by_names(composed, &["does-not-exist".into()]).expect_err("unknown name must fail");
        assert!(
            matches!(&err, RunFilterError::Unknown { name } if name == "does-not-exist"),
            "expected Unknown {{ name: \"does-not-exist\" }}; got: {err}"
        );
    }

    /// Plan §Phase 3.1: `filter_ambiguous_name_errors_with_groups_listed`
    ///
    /// Synthetic composed set: two entries with same binding but different
    /// group origins (defense-in-depth — not reachable through normal v1 flow).
    #[test]
    fn filter_ambiguous_name_errors_with_groups_listed() {
        let composed = vec![
            SelectedTool {
                binding: "tool".into(),
                origin: Origin::Group("ci".into()),
                source: ToolSource::Explicit(pin("tool", None, 'a').into()),
            },
            SelectedTool {
                binding: "tool".into(),
                origin: Origin::Group("release".into()),
                source: ToolSource::Explicit(pin("tool", None, 'b').into()),
            },
        ];
        let err = filter_by_names(composed, &["tool".into()]).expect_err("ambiguous name must fail");
        let RunFilterError::Ambiguous { name, groups } = &err else {
            panic!("expected Ambiguous; got: {err}");
        };
        assert_eq!(name, "tool");
        assert!(
            groups.contains(&"ci".to_string()),
            "groups must contain 'ci'; got: {groups:?}"
        );
        assert!(
            groups.contains(&"release".to_string()),
            "groups must contain 'release'; got: {groups:?}"
        );
    }

    /// Plan §Phase 3.1: `filter_unique_name_picks_single_entry`
    ///
    /// Happy path: exactly one matching entry is returned.
    #[test]
    fn filter_unique_name_picks_single_entry() {
        let composed = vec![tool("cmake", 'a', "default"), tool("ninja", 'b', "default")];
        let result = filter_by_names(composed, &["cmake".into()]).expect("ok");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].binding, "cmake");
    }

    /// Plan §Phase 3.1: `filter_preserves_compose_order` — actually user order wins.
    ///
    /// When names is non-empty the output preserves user-supplied name order,
    /// NOT compose order. Composed `[a, b, c]`, names `[b, a]` → output `[b, a]`.
    #[test]
    fn filter_preserves_user_name_order_not_compose_order() {
        let composed = vec![
            tool("a", 'a', "default"),
            tool("b", 'b', "default"),
            tool("c", 'c', "default"),
        ];
        let names: Vec<String> = vec!["b".into(), "a".into()];
        let result = filter_by_names(composed, &names).expect("ok");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].binding, "b", "user-order: b must come first");
        assert_eq!(result[1].binding, "a", "user-order: a must come second");
    }

    /// Plan §Phase 3.1: `filter_duplicate_names_dedupe`
    ///
    /// Duplicate names in the names list are silently deduplicated.
    /// Composed has one `cmake`; names `[cmake, cmake]` → output `[cmake]` once.
    #[test]
    fn filter_duplicate_names_deduplicated_silently() {
        let composed = vec![tool("cmake", 'a', "default")];
        let names: Vec<String> = vec!["cmake".into(), "cmake".into()];
        let result = filter_by_names(composed, &names).expect("dedup must succeed");
        assert_eq!(result.len(), 1, "duplicate name must be silently deduped to one entry");
        assert_eq!(result[0].binding, "cmake");
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
}
