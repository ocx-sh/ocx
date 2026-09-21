// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `ocx add [--group <name>] <identifier>...` — append one or more
//! bindings to `ocx.toml`, atomically rewrite `ocx.lock` for impacted
//! tools, and install.

use std::process::ExitCode;

use clap::Parser;
use ocx_project::{ResolveLockOptions, add_binding_in_memory, resolve_lock, resolve_lock_touched};

use crate::api::data::lock::{LockEntry, LockReport};
use crate::app::project_context::{
    ensure_global_project_initialized, load_project_for_mutate, materialize_lock, record_activation_consent,
};
use crate::conventions;
use crate::options;

/// Arguments of `ocx add`. The user-facing description lives on the
/// [`Command::Add`](crate::command::Command::Add) variant — that is the doc
/// clap renders, and this one reaches nothing but rustdoc.
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
        // F7: a `--global` mutator on an absent global file auto-creates
        // it (mirrors project `add` on a fresh project; the global tier is
        // the one sanctioned auto-scaffold site). No-op when not `--global`
        // or the file already exists.
        ensure_global_project_initialized(&context).await?;

        // Parse every positional up front — before the flock — applying the
        // default registry if unqualified and the `:latest` default for bare
        // identifiers (no tag, no digest). Parsing all of them first means a
        // malformed identifier fails fast without touching the flock or
        // `ocx.toml`. The `:latest` default is intentionally NOT a duplicate
        // of the config-parse-layer default in `ProjectConfig::from_toml_str`.
        //
        // A leading `NAME=` picks the binding key explicitly. `=` never
        // appears in a valid OCI identifier, so splitting on the FIRST `=` is
        // unambiguous. The name itself is validated by the library
        // (`InvalidBindingName`), so an empty `=foo` fails there, not here.
        let bindings: Vec<(Option<String>, ocx_oci::Identifier)> = self
            .identifiers
            .iter()
            .map(|raw| {
                let (name, reference) = match raw.split_once('=') {
                    Some((name, reference)) => (Some(name.to_owned()), reference),
                    None => (None, raw.as_str()),
                };
                let id = ocx_oci::Identifier::parse_with_default_registry(reference, context.default_registry())?;
                let id = if id.tag().is_none() && id.digest().is_none() {
                    id.clone_with_tag("latest")
                } else {
                    id
                };
                Ok::<_, anyhow::Error>((name, id))
            })
            .collect::<Result<_, _>>()?;

        // Resolve project, acquire flock, load snapshot + predecessor lock.
        // Errors propagate to the `main.rs` boundary: `log::error!` logs the
        // message once and `app::classify_error` derives the exit code from
        // `ProjectContextError`'s `ClassifyExitCode` impl.
        let guard = load_project_for_mutate(&context).await?;

        // Decide per binding what this invocation owes, against the manifest
        // as it is on disk — before anything is staged, so an `ocx add` of
        // what is already declared never reaches the library's duplicate
        // refusal and never rewrites `ocx.toml`.
        let plan = plan_bindings(
            guard.config(),
            guard.previous_lock().map_or(&[][..], |lock| lock.tools.as_slice()),
            self.group.as_deref(),
            &bindings,
        );
        let eager = self.pull.enabled(true);
        let platform = conventions::platform_or_default(self.platform.platform.clone());
        let config_path = guard.config_path().to_path_buf();

        // Stage: in-memory add of every binding the plan did not already
        // account for. A binding whose key is taken by a *different*
        // identifier is deliberately left in the staging set, so the refusal
        // (`BindingAlreadyExists`) keeps its single owner in the library and
        // aborts before any disk write. Atomic: all staged bindings land or
        // none do; `ocx.toml` is never left half-edited.
        //
        // Ahead of the status lines below, so a mixed batch that exits 64
        // never first claims that part of it was already added. Staging is
        // pure and the candidate is owned, so the no-op path can still roll
        // the guard back afterwards.
        let bindings_for_stage = plan.stage.clone();
        let group = self.group.clone();
        let staging_config_path = config_path.clone();
        let staged = guard.stage(move |cfg| {
            for (name, identifier) in &bindings_for_stage {
                add_binding_in_memory(cfg, &staging_config_path, identifier, name.as_deref(), group.as_deref())?;
            }
            Ok(())
        })?;

        // Nothing new reached the manifest: the commit writes `ocx.lock` only,
        // leaving `ocx.toml` byte-identical.
        let staged = if plan.stage.is_empty() {
            staged.lock_only()
        } else {
            staged
        };

        for key in &plan.already {
            context.ui().status(
                "Unchanged",
                format!("{key} is already added; use `ocx update {key}` to move it"),
            );
        }

        // `--no-pull` promises this invocation downloads nothing, and the
        // render's metadata closure walk is a download. It therefore runs
        // against the offline view (the shipped `--no-pull` idiom, as in
        // `ocx env --no-pull`): a warm store still re-renders, a cold one
        // degrades quietly and the deferred `ocx pull` renders instead.
        //
        // Both exits below render, so both derive the manager and the scope
        // from here: the tree the user ends up with must not depend on
        // whether this invocation happened to change the manifest. The scope
        // in particular is derived while the guard still exists, since the
        // no-op path renders after dropping it.
        let render_manager = if eager {
            context.manager().clone()
        } else {
            context.manager().offline_view(context.local_index().clone())
        };
        let scope = context.toolchain_render_scope(guard.config_path()).await?;

        // Everything asked for is already declared and already pinned. Drop the
        // guard without committing: `ocx.toml` and `ocx.lock` keep their bytes
        // AND their mtimes, so nothing downstream re-fires on a no-op. The pull
        // and the render still run — a re-add after a failed download, or after
        // an earlier `--no-pull`, is the case this path exists for, and the
        // command promises the toolchain home either way. `previous_lock` is
        // `Some` by construction here (a binding is only "already pinned" if a
        // lock holds the pin); the fall-through covers the unreachable `None`
        // without a panic.
        if plan.stage.is_empty()
            && plan.touched.is_empty()
            && let Some(existing) = guard.previous_lock().cloned()
        {
            // The candidate answers for `pinned`, exactly as it does on the
            // committing path — here it is byte-identical to the snapshot.
            let pinned = ocx_package_manager::pinned_for_project(None, staged.config());
            guard.rollback();
            record_activation_consent(&config_path, &existing, None).await;
            materialize_lock(&context, &existing, eager, platform.clone()).await?;
            render_and_warn(&context, &render_manager, &existing, pinned, &scope, &platform).await;
            report_lock(&context, &existing, &platform)?;
            return Ok(ExitCode::SUCCESS);
        }

        // Whole-file model (spec §4.3): re-resolve ONLY the new bindings and
        // carry every pre-existing lock entry forward verbatim (V2
        // byte-identical; V1 via exact-only pinned-index transcribe). The
        // freshness gate inside `resolve_lock_touched` anchors on the
        // pre-mutation snapshot (`guard.config()`) — the inserted bindings make
        // the candidate hash differ, so anchoring on the candidate would fail
        // every clean add — and stamps the candidate hash into the produced
        // lock. Drift on the pre-mutation snapshot surfaces as
        // `StaleLockOnPartial` (65, run `ocx lock`); a carried V1 entry whose
        // index is gone surfaces as `LockUpgradeRequired` (78, run
        // `ocx update`). Both propagate to the `main.rs` boundary.
        // `resolve_lock_touched` dedups the touched set internally.
        let touched = plan.touched.clone();
        let new_lock = match guard.previous_lock().cloned() {
            Some(prev) => {
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
            // Bootstrap: no predecessor to preserve, nothing to launder — a
            // direct resolve that must never fail closed.
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

        // Commit: lock-first, manifest-second, both atomic — then re-render the
        // toolchain home the new lock describes (C-054, D-V8). Both steps are
        // the one shared `commit_and_render`; a call site that reached
        // `MutationGuard::commit` directly would write a correct lock and leave
        // the tree describing the previous one. The scope is derived while the
        // guard still exists; a render failure after the commit never rolls it
        // back (RUL-53).
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

        // Consent write seam (C-024, A-29) — one of the commands allowed to
        // stamp, opting in explicitly. AFTER the commit, so the stamp records
        // the source set the user just asked for rather than the one it
        // replaced. Best-effort; never fails the mutation.
        record_activation_consent(&commit.config_path, &new_lock, None).await;

        // Best-effort pull AFTER the commit lands. A failure here does
        // not roll back the manifest/lock — the binding is declaratively
        // present even if the pull needs a retry.
        //
        // Symbol-free by design: `materialize_lock` warms the object
        // store via `pull_all`, never `install_all`. The signed
        // handshake §1 contract — "global IS the project toolchain, only
        // difference is the load site" — combined with ADR D5
        // (amended 2026-05-19, env = lock-pinned digest, current symlink
        // demoted to IDE-anchor abstraction not consulted by env) means
        // neither tier needs a candidate or `current` symlink to make
        // the added tool resolvable. Users that want a per-repo stable
        // anchor invoke `ocx package install` / `ocx package select`
        // explicitly.
        //
        // `--no-pull` opts out: lock write happens regardless; only the
        // object-store materialization is deferred.
        materialize_lock(&context, &new_lock, eager, platform.clone()).await?;

        report_lock(&context, &new_lock, &platform)?;

        Ok(ExitCode::SUCCESS)
    }
}

/// Re-render the toolchain home for a lock this invocation did not commit,
/// then say on **stderr** what the render could not do (C-050, RUL-53).
///
/// The committing path gets its render from `commit_and_render`; the
/// whole-batch no-op never reaches it, and the command promises the home
/// unconditionally. Under `activate = "bin"` the case this path exists for — a
/// re-add after an earlier `--no-pull` — would otherwise leave
/// `toolchain/active/bin` without its trampolines until some later `ocx pull`.
///
/// `groups` is the same set `commit_and_render` derives: the lock's groups plus
/// [`DEFAULT_GROUP`](ocx_project::DEFAULT_GROUP), which is always in scope
/// (RUL-70). `dry_run` is `ocx pull`'s alone, so this passes `false`.
///
/// A render outcome is never this command's exit code. The manifest and lock
/// are already what the user asked for before this runs; a read-only checkout,
/// a foreign-owned `.ocx/`, or a closure an offline invocation cannot walk are
/// all states where that stayed true.
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
        // Quiet on an offline manager, loud otherwise — the same split
        // `commit_and_render` makes for the committing path: `--no-pull`
        // renders through `offline_view` on purpose, so a cold store there is
        // the ordinary state and the next `ocx pull` renders.
        Err(error) if manager.is_offline() => {
            log::debug!("The toolchain home was not rendered: {error}");
        }
        Err(error) => context
            .ui()
            .warn(format!("The toolchain home was not rendered: {error}")),
    }
}

/// Report the full resulting lock to the user, keyed on the requested
/// platform when `--platform` was given (else the host).
///
/// Shared by the committing path and the already-declared-and-pinned no-op,
/// which reports the predecessor lock unchanged — the payload is the same
/// answer either way, so the two must not drift into two shapes.
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
    /// Bindings the staging closure still runs: the genuinely new keys, and
    /// the ones whose key is taken by a different identifier — those are left
    /// for `add_binding_in_memory` to refuse, so the refusal keeps one owner.
    stage: Vec<(Option<String>, ocx_oci::Identifier)>,
    /// `(group, key)` pairs to re-resolve: every new binding, plus an
    /// already-declared one the predecessor lock holds no pin for.
    touched: Vec<(String, String)>,
    /// Keys the manifest already binds to exactly the identifier that was
    /// asked for. One diagnostic line each.
    already: Vec<String>,
}

/// Partition `bindings` against what `config` already declares in the target
/// group and what `locked` already pins.
///
/// Keyed on what the mutation would write — the explicit `NAME=` key when
/// given, else [`ocx_project::binding_key`] — and scoped to the target group,
/// exactly as `add_binding_in_memory`'s own duplicate check is. Equality is
/// `Identifier` equality; both sides carry the `:latest` default, so
/// `ocx add cmake` twice compares equal.
///
/// A key repeated inside one batch is decided against the identifier the
/// earlier occurrence declared, so `ocx add A A` collapses to one binding
/// while `ocx add A:1 A:2` still conflicts.
///
/// Pure: no I/O, no ordering assumption beyond the batch's own order.
fn plan_bindings(
    config: &ocx_project::ProjectConfig,
    locked: &[ocx_project::LockedTool],
    group: Option<&str>,
    bindings: &[(Option<String>, ocx_oci::Identifier)],
) -> AddPlan {
    let lock_group = group.unwrap_or(ocx_project::DEFAULT_GROUP);
    let declared = match group {
        None => Some(&config.tools),
        Some(name) => config.groups.get(name).map(|group| &group.tools),
    };

    let mut plan = AddPlan::default();
    // ponytail: linear scans over a hand-typed argument list. A map would cost
    // more to build than the whole walk.
    let mut batch: Vec<(String, ocx_oci::Identifier)> = Vec::new();
    for (name, identifier) in bindings {
        let key = name.clone().unwrap_or_else(|| ocx_project::binding_key(identifier));
        let in_manifest = declared.and_then(|tools| tools.get(&key));
        // What the target group binds this key to already — from an earlier
        // occurrence in this same batch first, else from the manifest.
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
                // A repeat inside this batch is fully covered by its first
                // occurrence. A manifest binding earns the diagnostic, and is
                // re-resolved only when nothing pins it yet: re-resolving a
                // pinned binding would silently advance it, which is
                // `ocx update`'s job and nobody else's.
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
    fn identifier(text: &str) -> ocx_oci::Identifier {
        let parsed = ocx_oci::Identifier::parse_with_default_registry(text, "ocx.sh").unwrap();
        if parsed.tag().is_none() && parsed.digest().is_none() {
            parsed.clone_with_tag("latest")
        } else {
            parsed
        }
    }

    /// `ocx add`'s parsed-binding shape: optional `NAME=` key plus identifier.
    fn binding(name: Option<&str>, text: &str) -> (Option<String>, ocx_oci::Identifier) {
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
            repository: identifier(text),
            platforms: std::collections::BTreeMap::new(),
        }
    }

    fn keys(staged: &[(Option<String>, ocx_oci::Identifier)]) -> Vec<String> {
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
    /// most one value (D4 of `adr_platform_model_unification.md`).
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
