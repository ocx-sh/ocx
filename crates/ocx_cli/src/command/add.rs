// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx add`: append bindings to `ocx.toml`, rewrite `ocx.lock` for the impacted tools, and install.

use std::process::ExitCode;

use clap::Parser;
use ocx_project::{ResolveLockOptions, add_binding_in_memory, resolve_lock, resolve_lock_touched};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{
    ensure_global_project_initialized, load_project_for_mutate, materialize_lock, record_activation_consent,
};
use crate::conventions;
use crate::options;

// User-facing help lives on `Command::Add`; a doc here renders nowhere.
#[derive(Parser, Clone)]
pub struct Add {
    /// Named group to add the bindings to. Defaults to the implicit
    /// `[tools]` table when omitted.
    #[arg(long = "group", short = 'g', value_name = "GROUP")]
    pub group: Option<String>,

    #[clap(flatten)]
    pub pull: options::Pull,

    #[clap(flatten)]
    pub platform: options::PlatformOption,

    /// Tool identifiers to add, each optionally prefixed with `NAME=`
    /// (e.g. `ocx.sh/cmake:3.28`, `glab=ocx.sh/gitlab/cli`).
    #[arg(required = true, num_args = 1.., value_name = "[NAME=]IDENTIFIER")]
    pub identifiers: Vec<String>,
}

impl Add {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        ensure_global_project_initialized(&context).await?;

        // Parsed before the flock, so a malformed identifier fails without touching `ocx.toml`.
        // This `:latest` default stays separate from `ProjectConfig::from_toml_str`'s — do not unify them.
        // `=` never appears in an OCI identifier, so the first one splits off `NAME=`.
        let bindings: Vec<(Option<String>, ocx_oci::PackageRef)> = self
            .identifiers
            .iter()
            .map(|raw| {
                let (name, reference) = match raw.split_once('=') {
                    Some((name, reference)) => (Some(name.to_owned()), reference),
                    None => (None, raw.as_str()),
                };
                let id = ocx_oci::PackageRef::parse_with_default_registry(reference, context.default_registry())?;
                let id = if id.tag().is_none() && id.digest().is_none() {
                    id.clone_with_tag("latest")
                } else {
                    id
                };
                Ok::<_, anyhow::Error>((name, id))
            })
            .collect::<Result<_, _>>()?;

        let guard = load_project_for_mutate(&context).await?;

        // Planned against the on-disk manifest before staging, so re-adding a declared binding never
        // reaches the duplicate refusal or rewrites `ocx.toml`.
        let plan = plan_bindings(
            guard.config(),
            guard.previous_lock().map_or(&[][..], |lock| lock.tools.as_slice()),
            self.group.as_deref(),
            &bindings,
        );
        let eager = self.pull.enabled(true);
        let platform = conventions::platform_or_default(self.platform.platform.clone());
        let config_path = guard.config_path().to_path_buf();

        // A key taken by a different identifier stays staged, so `add_binding_in_memory`'s refusal
        // aborts the batch before any disk write.
        let bindings_for_stage = plan.stage.clone();
        let group = self.group.clone();
        let staging_config_path = config_path.clone();
        let staged = guard.stage(move |cfg| {
            for (name, identifier) in &bindings_for_stage {
                add_binding_in_memory(cfg, &staging_config_path, identifier, name.as_deref(), group.as_deref())?;
            }
            Ok(())
        })?;

        // Nothing new staged: the commit writes `ocx.lock` only.
        let staged = if plan.stage.is_empty() {
            staged.lock_only()
        } else {
            staged
        };

        // After staging, so a batch that exits 64 never reports part of itself as added; staging is
        // pure, so the no-op path below can still roll the guard back.
        for key in &plan.already {
            context.ui().status(
                "Unchanged",
                format!("{key} is already added; use `ocx update {key}` to move it"),
            );
        }

        // `--no-pull` promises no downloads and the render's closure walk is one, so render offline;
        // a cold store degrades quietly and the deferred `ocx pull` renders instead.
        let render_manager = if eager {
            context.manager().clone()
        } else {
            context.manager().offline_view(context.local_index().clone())
        };
        // Derived while the guard is held (the no-op path renders after dropping it), and shared by
        // both exits so the result does not depend on whether the manifest changed.
        let scope = context.toolchain_render_scope(guard.config_path()).await?;

        // All declared and pinned: roll back, not commit, so `ocx.toml`/`ocx.lock` keep their mtimes and
        // nothing downstream re-fires; pull and render still run to finish an earlier failed download.
        if plan.stage.is_empty()
            && plan.touched.is_empty()
            && let Some(existing) = guard.previous_lock().cloned()
        {
            // The candidate answers for `pinned`, as on the committing path.
            let pinned = ocx_package_manager::pinned_for_project(None, staged.config());
            guard.rollback();
            record_activation_consent(&config_path, &existing, None).await;
            materialize_lock(&context, &existing, staged.config(), eager, platform.clone()).await?;
            render_and_warn(&context, &render_manager, &existing, pinned, &scope, &platform).await;
            report_lock(&context, &existing, &platform)?;
            return Ok(ExitCode::SUCCESS);
        }

        let touched = plan.touched.clone();
        let new_lock = match guard.previous_lock().cloned() {
            Some(prev) => {
                // Freshness anchors on the pre-mutation snapshot: the new bindings change the candidate's
                // hash, so anchoring on it would fail every clean add.
                resolve_lock_touched(
                    staged.config(), // candidate (post-mutation)
                    guard.config(),  // pre-mutation snapshot — freshness anchor
                    &prev,
                    context.default_index(),
                    &touched,
                    ResolveLockOptions::default(),
                )
                .await?
            }
            None => {
                resolve_lock(
                    staged.config(),
                    context.default_index(),
                    &[],
                    ResolveLockOptions::default(),
                )
                .await?
            }
        };

        // Cloned before the commit consumes `staged`; the eager pull binds the new lock to it.
        let config = staged.config().clone();
        // Through `commit_and_render`, never `MutationGuard::commit`, or the toolchain tree keeps
        // describing the previous lock.
        let commit = render_manager
            .commit_and_render(
                guard,
                staged,
                new_lock.clone(),
                ocx_package_manager::ToolchainRender {
                    scope: &scope,
                    toolchain_root: context.toolchain_root(),
                    platform: &platform,
                },
            )
            .await?
            .commit;

        // After the commit, so the stamp records the requested source set, not the one it replaced.
        record_activation_consent(&commit.config_path, &new_lock, None).await;

        // After the commit: a failed download leaves the binding declared.
        materialize_lock(&context, &new_lock, &config, eager, platform.clone()).await?;

        report_lock(&context, &new_lock, &platform)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Re-render the toolchain home for a lock this invocation did not commit, warning on stderr.
///
/// The whole-batch no-op never reaches `commit_and_render`; without this, a re-add under
/// `activate = "bin"` after an earlier `--no-pull` leaves `toolchain/active/bin` without
/// trampolines. A render outcome is never this command's exit code.
async fn render_and_warn(
    context: &crate::app::Context,
    manager: &ocx_package_manager::PackageManager,
    lock: &ocx_project::ProjectLock,
    pinned: bool,
    scope: &ocx_store::file_structure::RenderStampScope,
    platform: &ocx_oci::Platform,
) {
    let mut groups: Vec<String> = vec![ocx_project::DEFAULT_GROUP.to_owned()];
    for tool in &lock.tools {
        if !groups.iter().any(|group| group == &tool.group) {
            groups.push(tool.group.clone());
        }
    }

    let render = ocx_package_manager::ToolchainRender {
        scope,
        toolchain_root: context.toolchain_root(),
        platform,
    };

    match manager.render_home(lock, pinned, &render, &groups, false).await {
        Ok(report) => {
            for line in ocx_package_manager::skipped_render_warnings(&report) {
                context.ui().warn(line);
            }
        }
        // Quiet offline, as in `commit_and_render`: a cold store is ordinary under `--no-pull`.
        Err(error) if manager.is_offline() => {
            log::debug!("The toolchain home was not rendered: {error}");
        }
        Err(error) => context
            .ui()
            .warn(format!("The toolchain home was not rendered: {error}")),
    }
}

/// Report the full resulting lock, keyed on `--platform` (else the host). Shared by the commit and
/// no-op paths so the payload cannot drift into two shapes.
fn report_lock(
    context: &crate::app::Context,
    lock: &ocx_project::ProjectLock,
    platform: &ocx_oci::Platform,
) -> anyhow::Result<()> {
    let entries: Vec<LockEntry> = lock.tools.iter().map(|t| LockEntry::from_tool(t, platform)).collect();
    context.api().report(&LockReport::new(entries))?;
    Ok(())
}

/// What one `ocx add` invocation owes, decided per parsed binding.
#[derive(Debug, Default, PartialEq, Eq)]
struct AddPlan {
    /// Bindings to stage: new keys, and keys taken by a different identifier, left for
    /// `add_binding_in_memory` to refuse.
    stage: Vec<(Option<String>, ocx_oci::PackageRef)>,
    /// `(group, key)` pairs to re-resolve: every new binding, plus an
    /// already-declared one the predecessor lock holds no pin for.
    touched: Vec<(String, String)>,
    /// Keys already bound to exactly the requested identifier; one diagnostic line each.
    already: Vec<String>,
}

/// Partition `bindings` against what `config` declares in the target group and `locked` pins.
///
/// Keyed on the key the mutation would write (`NAME=` or [`ocx_project::binding_key`]), compared by
/// `PackageRef` equality. A key repeated in one batch is decided against its earlier occurrence:
/// `ocx add A A` collapses, `ocx add A:1 A:2` still conflicts.
fn plan_bindings(
    config: &ocx_project::ProjectConfig,
    locked: &[ocx_project::LockedTool],
    group: Option<&str>,
    bindings: &[(Option<String>, ocx_oci::PackageRef)],
) -> AddPlan {
    let lock_group = group.unwrap_or(ocx_project::DEFAULT_GROUP);
    let declared = match group {
        None => Some(&config.tools),
        Some(name) => config.groups.get(name).map(|group| &group.tools),
    };

    let mut plan = AddPlan::default();
    // ponytail: linear scans over a hand-typed argument list. A map would cost
    // more to build than the whole walk.
    let mut batch: Vec<(String, ocx_oci::PackageRef)> = Vec::new();
    for (name, identifier) in bindings {
        let key = name.clone().unwrap_or_else(|| ocx_project::binding_key(identifier));
        let in_manifest = declared.and_then(|tools| tools.get(&key));
        // An earlier occurrence in this batch first, else the manifest.
        let current = batch
            .iter()
            .find(|(seen, _)| *seen == key)
            .map(|(_, staged)| staged)
            .or(in_manifest)
            .cloned();

        match current {
            None => {
                plan.stage.push((name.clone(), identifier.clone()));
                plan.touched.push((lock_group.to_owned(), key.clone()));
                batch.push((key, identifier.clone()));
            }
            Some(current) if &current == identifier => {
                // A batch repeat is covered by its first occurrence. A manifest binding is re-resolved only
                // when unpinned: re-resolving a pinned one would silently advance it (`ocx update`'s job).
                if in_manifest.is_some() && !plan.already.contains(&key) {
                    let pinned = locked.iter().any(|tool| tool.group == lock_group && tool.name == key);
                    if !pinned {
                        plan.touched.push((lock_group.to_owned(), key.clone()));
                    }
                    plan.already.push(key);
                }
            }
            Some(_) => plan.stage.push((name.clone(), identifier.clone())),
        }
    }
    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ── helpers ──────────────────────────────────────────────────────────────

    fn parse(args: &[&str]) -> Add {
        // Always supply the required IDENTIFIER positional argument.
        Add::try_parse_from(args).unwrap()
    }

    /// Parse the way `execute` does: default registry applied, `:latest`
    /// injected for a bare identifier. Both sides of the equality test in
    /// `plan_bindings` reach it through this same normalization.
    fn identifier(text: &str) -> ocx_oci::PackageRef {
        let parsed = ocx_oci::PackageRef::parse_with_default_registry(text, "ocx.sh").unwrap();
        if parsed.tag().is_none() && parsed.digest().is_none() {
            parsed.clone_with_tag("latest")
        } else {
            parsed
        }
    }

    /// `ocx add`'s parsed-binding shape: optional `NAME=` key plus identifier.
    fn binding(name: Option<&str>, text: &str) -> (Option<String>, ocx_oci::PackageRef) {
        (name.map(str::to_owned), identifier(text))
    }

    /// A config declaring `pairs` in the default `[tools]` table.
    fn config_with_tools(pairs: &[(&str, &str)]) -> ocx_project::ProjectConfig {
        let mut config = ocx_project::ProjectConfig::default();
        for (key, text) in pairs {
            config.tools.insert((*key).to_owned(), identifier(text));
        }
        config
    }

    /// A lock entry pinning `(group, name)`. The platform map is irrelevant to
    /// the partition — only the `(group, name)` identity is consulted.
    fn locked(group: &str, name: &str, text: &str) -> ocx_project::LockedTool {
        ocx_project::LockedTool {
            name: name.to_owned(),
            group: group.to_owned(),
            repository: ocx_oci::Repository::from(&identifier(text)),
            platforms: std::collections::BTreeMap::new(),
        }
    }

    fn keys(staged: &[(Option<String>, ocx_oci::PackageRef)]) -> Vec<String> {
        staged
            .iter()
            .map(|(name, id)| name.clone().unwrap_or_else(|| ocx_project::binding_key(id)))
            .collect()
    }

    // ── cases ─────────────────────────────────────────────────────────────────

    /// `--pull`/`--no-pull` wire through the shared `options::Pull` flatten;
    /// `add` defaults to eager. The full flag matrix is tested on the
    /// flatten struct itself (`options/pull.rs`).
    #[test]
    fn pull_flags_flatten_with_eager_default() {
        assert!(parse(&["add", "tool:1"]).pull.enabled(true), "default must be eager");
        assert!(
            !parse(&["add", "--no-pull", "tool:1"]).pull.enabled(true),
            "--no-pull must defer"
        );
    }

    /// A single positional still parses (back-compat with the pre-plural form).
    #[test]
    fn parse_single_identifier() {
        let add = parse(&["add", "tool:1"]);
        assert_eq!(add.identifiers, vec!["tool:1".to_string()]);
    }

    /// Multiple positionals are all captured, in order.
    #[test]
    fn parse_multiple_identifiers() {
        let add = parse(&["add", "a:1", "b:2", "c:3"]);
        assert_eq!(
            add.identifiers,
            vec!["a:1".to_string(), "b:2".to_string(), "c:3".to_string()]
        );
    }

    /// A `NAME=IDENTIFIER` positional reaches `execute` verbatim — the split
    /// happens there, not in clap (`=` is not a flag separator for positionals).
    #[test]
    fn parse_keeps_named_positional_verbatim() {
        let add = parse(&["add", "glab=ocx.sh/gitlab/cli"]);
        assert_eq!(add.identifiers, vec!["glab=ocx.sh/gitlab/cli".to_string()]);
    }

    /// `num_args=1..` rejects zero positionals.
    #[test]
    fn parse_zero_identifiers_is_error() {
        assert!(
            Add::try_parse_from(["add"]).is_err(),
            "add with no identifier must fail"
        );
    }

    /// `--platform` accepts a single value alongside the positional identifier.
    #[test]
    fn parses_platform_flag() {
        let add = Add::try_parse_from(["add", "--platform", "linux/arm64", "cmake:3.28"]).unwrap();
        assert_eq!(
            add.platform.platform.map(|p| p.to_string()),
            Some("linux/arm64".to_owned())
        );
        assert_eq!(
            add.identifiers,
            vec!["cmake:3.28".to_owned()],
            "--platform must not swallow the identifier"
        );
    }

    /// A second `--platform` occurrence is a usage error — the flag takes at
    /// most one value (see `adr_platform_model_unification.md`).
    #[test]
    fn rejects_repeated_platform_flag() {
        assert!(
            Add::try_parse_from(["add", "--platform", "linux/arm64", "-p", "linux/amd64", "cmake:3.28"]).is_err(),
            "repeated --platform must be rejected"
        );
    }

    // ── plan_bindings ────────────────────────────────────────────────────────

    /// A key the manifest does not declare is an ordinary new binding: staged
    /// for the manifest edit and resolved.
    #[test]
    fn plan_stages_and_resolves_an_absent_key() {
        let plan = plan_bindings(
            &ocx_project::ProjectConfig::default(),
            &[],
            None,
            &[binding(None, "ocx.sh/cmake:3.30")],
        );
        assert_eq!(keys(&plan.stage), vec!["cmake".to_owned()], "a new key must be staged");
        assert_eq!(plan.touched, vec![("default".to_owned(), "cmake".to_owned())]);
        assert!(plan.already.is_empty(), "nothing was already declared");
    }

    /// The idempotent case: declared with the same identifier and already
    /// pinned. Nothing is staged, nothing is re-resolved — re-resolving would
    /// move the pin, which is `ocx update`'s job.
    #[test]
    fn plan_skips_a_declared_and_pinned_binding_entirely() {
        let plan = plan_bindings(
            &config_with_tools(&[("cmake", "ocx.sh/cmake")]),
            &[locked("default", "cmake", "ocx.sh/cmake")],
            None,
            &[binding(None, "ocx.sh/cmake")],
        );
        assert!(plan.stage.is_empty(), "the manifest must not be edited");
        assert!(plan.touched.is_empty(), "a pinned binding must never be re-resolved");
        assert_eq!(plan.already, vec!["cmake".to_owned()], "the diagnostic names the key");
    }

    /// Declared but not pinned: the manifest still stays untouched, but the
    /// binding is resolved so the commit writes the missing lock entry.
    #[test]
    fn plan_relocks_a_declared_but_unpinned_binding() {
        let plan = plan_bindings(
            &config_with_tools(&[("cmake", "ocx.sh/cmake")]),
            &[],
            None,
            &[binding(None, "ocx.sh/cmake")],
        );
        assert!(plan.stage.is_empty(), "the manifest must not be edited");
        assert_eq!(
            plan.touched,
            vec![("default".to_owned(), "cmake".to_owned())],
            "an unpinned binding must be resolved"
        );
        assert_eq!(plan.already, vec!["cmake".to_owned()]);
    }

    /// A lock entry for the same name in *another* group does not pin this
    /// one — the partition is group-scoped on both sides.
    #[test]
    fn plan_ignores_a_pin_belonging_to_another_group() {
        let plan = plan_bindings(
            &config_with_tools(&[("cmake", "ocx.sh/cmake")]),
            &[locked("ci", "cmake", "ocx.sh/cmake")],
            None,
            &[binding(None, "ocx.sh/cmake")],
        );
        assert_eq!(
            plan.touched,
            vec![("default".to_owned(), "cmake".to_owned())],
            "a pin in group 'ci' must not count as the default group's pin"
        );
    }

    /// A key taken by a different identifier stays in the staging set, so
    /// `add_binding_in_memory` raises `BindingAlreadyExists` (exit 64).
    #[test]
    fn plan_leaves_a_conflicting_identifier_for_the_library_to_refuse() {
        let plan = plan_bindings(
            &config_with_tools(&[("cmake", "ocx.sh/cmake:3.28")]),
            &[locked("default", "cmake", "ocx.sh/cmake")],
            None,
            &[binding(None, "ocx.sh/cmake:3.30")],
        );
        assert_eq!(
            keys(&plan.stage),
            vec!["cmake".to_owned()],
            "a conflicting identifier must reach the staging closure"
        );
        assert!(plan.already.is_empty(), "a conflict is not an idempotent re-add");
    }

    /// `ocx add A A` collapses: the second occurrence is decided against what
    /// the first one declared, not against an empty manifest.
    #[test]
    fn plan_collapses_the_same_identifier_repeated_in_one_batch() {
        let plan = plan_bindings(
            &ocx_project::ProjectConfig::default(),
            &[],
            None,
            &[binding(None, "ocx.sh/cmake"), binding(None, "ocx.sh/cmake")],
        );
        assert_eq!(keys(&plan.stage), vec!["cmake".to_owned()], "the key is staged once");
        assert_eq!(
            plan.touched,
            vec![("default".to_owned(), "cmake".to_owned())],
            "the key is resolved once"
        );
        assert!(
            plan.already.is_empty(),
            "an in-batch repeat of a NEW binding was not 'already added'"
        );
    }

    /// `ocx add A:1 A:2` still conflicts: both reach the staging closure, and
    /// the second one hits the library's duplicate refusal.
    #[test]
    fn plan_keeps_a_conflicting_in_batch_repeat_staged_twice() {
        let plan = plan_bindings(
            &ocx_project::ProjectConfig::default(),
            &[],
            None,
            &[binding(None, "ocx.sh/cmake:3.28"), binding(None, "ocx.sh/cmake:3.30")],
        );
        assert_eq!(
            keys(&plan.stage),
            vec!["cmake".to_owned(), "cmake".to_owned()],
            "both occurrences must reach the closure so the duplicate is refused there"
        );
    }

    /// `--group ci` compares against `[group.ci].tools`, so the same key in
    /// the default `[tools]` table is not a duplicate.
    #[test]
    fn plan_scopes_the_lookup_to_the_target_group() {
        let plan = plan_bindings(
            &config_with_tools(&[("cmake", "ocx.sh/cmake")]),
            &[locked("default", "cmake", "ocx.sh/cmake")],
            Some("ci"),
            &[binding(None, "ocx.sh/cmake")],
        );
        assert_eq!(
            keys(&plan.stage),
            vec!["cmake".to_owned()],
            "the default group's binding must not shadow the ci group"
        );
        assert_eq!(plan.touched, vec![("ci".to_owned(), "cmake".to_owned())]);
    }

    /// The `NAME=IDENTIFIER` form keys on the alias, not the repository
    /// basename — the same key the mutation would write.
    #[test]
    fn plan_keys_an_explicit_alias_on_the_alias() {
        let config = config_with_tools(&[("glab", "ocx.sh/gitlab/cli")]);
        let plan = plan_bindings(
            &config,
            &[locked("default", "glab", "ocx.sh/gitlab/cli")],
            None,
            &[binding(Some("glab"), "ocx.sh/gitlab/cli")],
        );
        assert!(plan.stage.is_empty(), "the alias is already declared");
        assert_eq!(plan.already, vec!["glab".to_owned()]);

        // Same identifier without the alias derives the basename `cli`, which
        // the manifest does not declare — a genuinely new binding.
        let bare = plan_bindings(&config, &[], None, &[binding(None, "ocx.sh/gitlab/cli")]);
        assert_eq!(keys(&bare.stage), vec!["cli".to_owned()]);
    }

    /// A mixed batch: the new binding drives the manifest edit, the declared
    /// one only joins the resolve set when it has no pin.
    #[test]
    fn plan_mixes_new_declared_and_pinned_bindings() {
        let plan = plan_bindings(
            &config_with_tools(&[("cmake", "ocx.sh/cmake"), ("ripgrep", "ocx.sh/ripgrep")]),
            &[locked("default", "cmake", "ocx.sh/cmake")],
            None,
            &[
                binding(None, "ocx.sh/cmake"),
                binding(None, "ocx.sh/ripgrep"),
                binding(None, "ocx.sh/jq:1.7"),
            ],
        );
        assert_eq!(keys(&plan.stage), vec!["jq".to_owned()], "only the new key is staged");
        assert_eq!(
            plan.touched,
            vec![
                ("default".to_owned(), "ripgrep".to_owned()),
                ("default".to_owned(), "jq".to_owned()),
            ],
            "the pinned binding stays out of the resolve set; the unpinned one joins it"
        );
        assert_eq!(plan.already, vec!["cmake".to_owned(), "ripgrep".to_owned()]);
    }
}
