// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    env,
    package::metadata::env::modifier::ModifierKind,
    package_manager::collect_applied,
    project::{MissingState, load_project_state},
    shell::{self, compute_fingerprint, parse_applied},
};

use crate::conventions::supported_platforms;

/// Prints stateful shell export statements for a prompt hook.
///
/// Reads the nearest project `ocx.toml`, loads the matching `ocx.lock`, and
/// computes a fingerprint over the *actually-installed* default-group tools
/// (sorted `(group, name, manifest_digest)` triples → SHA-256, wrapped as
/// `v1:<hex>`). The previous fingerprint is read from the `_OCX_APPLIED`
/// environment variable. When the fingerprint is unchanged the command
/// emits nothing and returns 0 (fast path).
///
/// On a fingerprint change the command emits `unset` lines for every tool
/// var the *current* applied set would export, then the new exports, then
/// `export _OCX_APPLIED=v1:<new-hex>`. Like `shell direnv` it never contacts
/// the network and never installs; missing tools produce a stderr note and
/// are skipped. When no project `ocx.toml` is found the command exits 0 with
/// no output.
///
/// # Fast-path semantics (post Codex P2)
///
/// The fingerprint reflects *what is actually installed in the object
/// store*, not just what the lock declares. We always run
/// [`collect_applied`] (which does the per-tool `resolve` + `find_plain`
/// walk against the offline-mode local index — no registry contact, no
/// network) and compute the fingerprint over the resulting entries. If the
/// fingerprint matches `_OCX_APPLIED` we short-circuit before
/// [`PackageManager::resolve_env`] / export emission.
///
/// Why this shape: deriving the fingerprint from `lock.tools` alone made
/// `ocx uninstall <tool>` / `ocx clean` invisible to the running shell —
/// the lock was unchanged so `_OCX_APPLIED` still matched, leaving stale
/// PATH/ENV pointers. The cost of the always-on existence check is bounded
/// by the lock's default-group tool count, dominated by file stats inside
/// `find_plain`. Empirically <1ms per tool on warm caches.
///
/// # v1 trade-off
///
/// The `_OCX_APPLIED` env var only carries the fingerprint, not the
/// previously-applied key list. The diff path therefore approximates
/// "previously-applied keys" with "keys we are about to export" — clean
/// re-set works for *replaced* tools, but variables whose names are not
/// in the new export set cannot be unset by this command (e.g. when a
/// tool is removed from `ocx.toml` outright). See `shell::applied_set`
/// module docs.
#[derive(Parser)]
pub struct ShellHook {
    /// The shell to generate the environment configuration for. If not specified, it will be auto-detected.
    #[clap(short = 's', long = "shell")]
    shell: Option<shell::Shell>,
}

impl ShellHook {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let shell = match self.shell {
            Some(s) => s,
            None => match shell::Shell::detect() {
                Some(s) => s,
                None => {
                    anyhow::bail!("could not detect the current shell; specify it using the --shell option");
                }
            },
        };

        // Project tier ONLY in Phase 7. See `direnv_export.rs` for the same
        // policy rationale.
        let cwd = env::current_dir()?;
        let project = match load_project_state(&cwd, context.project_path()).await? {
            Ok(state) => state,
            Err(MissingState::NoProject) => {
                // No project in effect: emit the global toolchain's
                // installed `current` set (source = global `ocx.lock` ∩
                // installed `current` symlinks). Reuses the same export
                // path `shell hook` uses for project state — no parallel
                // emitter (feedback_extend_dont_duplicate). On leaving a
                // project the global bin dir is restored here.
                emit_global_current_set(&context, shell).await?;
                return Ok(ExitCode::SUCCESS);
            }
            Err(MissingState::LockMissing { lock_path }) => {
                eprintln!(
                    "# ocx: ocx.lock not found at {}; run `ocx lock` to fetch",
                    lock_path.display()
                );
                return Ok(ExitCode::SUCCESS);
            }
        };

        // Stale-lock policy: warn but continue. Same rationale as shell-hook.
        if project.stale {
            eprintln!("# ocx: ocx.lock is stale (ocx.toml changed since last `ocx lock`); using stale digests");
        }

        // Emit the project toolchain's installed set through the shared
        // sentinel-diff emitter (the same one the global `NoProject` arm
        // reuses — feedback_extend_dont_duplicate).
        //
        // Entering a project: project state supersedes the global set
        // (strict isolation at the shell layer too —
        // adr_global_toolchain_tier.md §Decision 6, CODEX-BLOCK-4). The
        // static `$OCX_HOME/init.<shell>` entrypoint permanently prepends
        // the global bin dir, so "project supersedes" is NOT automatic:
        // rebuild PATH from the saved pre-OCX baseline (the `_OCX_APPLIED`
        // sentinel diff/unset mechanism the emitter already runs) and
        // explicitly strip the global bin dir so a global-only tool is
        // unreachable inside a project. This strip is threaded through the
        // emitter as a *diff-path-only* pre-emit step: it fires only after
        // the unchanged-fingerprint short-circuit, so an unchanged prompt
        // emits nothing at all (no breadcrumb, no strip, no export) — the
        // fast path wins exactly as before W2 (regression
        // test_shell_hook_unchanged_fingerprint_emits_empty).
        emit_locked_set(&context, shell, &project.lock, true).await?;
        Ok(ExitCode::SUCCESS)
    }
}

/// Emit `unset` + export + sentinel lines for a [`ProjectLock`]'s
/// installed default-group set, using the stateful `_OCX_APPLIED`
/// fingerprint fast-path.
///
/// Sole emitter shared by the project-state path and the global
/// `NoProject` arm ([`emit_global_current_set`]) so the two never drift
/// into parallel pipelines (feedback_extend_dont_duplicate). The fast
/// path short-circuits when the *actually-installed* set is unchanged
/// since the last prompt (so `ocx uninstall` / `ocx clean` is visible).
///
/// `strip_global` is set only by the project-state path: it makes the
/// emitter prepend a global-toolchain PATH strip (+ breadcrumb) onto the
/// **diff path only**, so an unchanged-fingerprint prompt still emits
/// absolutely nothing (the fast path wins — strict isolation is enforced
/// only when the project set actually changes). The global `NoProject`
/// arm passes `false`.
///
/// # Errors
/// Propagates `resolve_env` / index errors. Never contacts the network
/// (the manager clone is forced offline).
async fn emit_locked_set(
    context: &crate::app::Context,
    shell: shell::Shell,
    lock: &ocx_lib::project::ProjectLock,
    strip_global: bool,
) -> anyhow::Result<()> {
    // Architect boundary: hook MUST NOT contact the registry, regardless
    // of `--remote`. Force an offline-only `PackageManager` clone so any
    // incidental index lookup (e.g. via `resolve` to walk the cached
    // manifest chain) cannot escape into the network.
    let manager = context.manager().offline_view(context.local_index().clone());
    let platforms = supported_platforms();

    // ── Existence-driven fingerprint compute (Codex P2 fix) ──────
    //
    // Walk the lock's default-group tools, filter to those whose content
    // is actually present in the object store via `find_plain`.
    // `collect_applied` returns one `AppliedEntry`/`InstallInfo` pair per
    // resolvable tool; tools missing from the store are recorded in
    // `applied.missing` and excluded from the fingerprint. Sourcing
    // `manifest_digest` from `LockedTool::pinned.digest()` (done inside
    // `collect_applied`) keeps the fingerprint stable across machines and
    // platforms. For the global tier the installed-`current` set is the
    // global lock's default group ∩ what is actually materialised — the
    // intersection `collect_applied` already computes via `find_plain`.
    let applied = collect_applied(&manager, lock, &platforms).await?;

    for name in &applied.missing {
        // Missing-tool notes ride alongside the diff path. The shared
        // emitter below short-circuits before printing these on the
        // unchanged fast path, so they cannot spam every prompt.
        if !is_applied_unchanged(&compute_fingerprint(&applied.entries)) {
            eprintln!("# ocx: {name} not installed; run `ocx pull` to fetch");
        }
    }

    let new_fingerprint = compute_fingerprint(&applied.entries);
    let entries = manager.resolve_env(&applied.infos, false).await?;
    emit_env_with_sentinel(shell, &entries, &new_fingerprint, strip_global, context);
    Ok(())
}

/// `true` when `new_fingerprint` equals the `_OCX_APPLIED` sentinel
/// currently in the environment (the unchanged fast path).
fn is_applied_unchanged(new_fingerprint: &str) -> bool {
    let prev = env::var("_OCX_APPLIED");
    let prev_hex = prev.as_deref().and_then(parse_applied);
    let expected = parse_applied(new_fingerprint).expect("compute_fingerprint output must round-trip parse_applied");
    prev_hex == Some(expected)
}

/// Shared `_OCX_APPLIED` sentinel-diff emitter: the SOLE place that turns a
/// resolved env-entry set into `unset` + `export` + sentinel shell lines.
///
/// Consumed by the project-state path ([`emit_locked_set`]) and the global
/// `NoProject` arm ([`emit_global_current_set`]) so the two cannot drift
/// into parallel pipelines (feedback_extend_dont_duplicate). The fast path
/// short-circuits when the set is unchanged since the last prompt, so
/// `ocx uninstall` / `ocx clean` / a global-toolchain change are all
/// visible at the next prompt.
///
/// `strip_global` (project-state path only) prepends the global-toolchain
/// PATH strip + breadcrumb — but **only on the diff path**, after the
/// unchanged-fingerprint short-circuit. An unchanged prompt therefore
/// emits absolutely nothing (no breadcrumb, no strip, no export): the
/// pre-W2 fast path is preserved exactly (regression
/// `test_shell_hook_unchanged_fingerprint_emits_empty`). Strict isolation
/// is still enforced because the strip rides every actual change to the
/// project set, which is the only moment a stale global PATH could leak.
fn emit_env_with_sentinel(
    shell: shell::Shell,
    entries: &[ocx_lib::package::metadata::env::entry::Entry],
    new_fingerprint: &str,
    strip_global: bool,
    context: &crate::app::Context,
) {
    let prev_raw = env::var("_OCX_APPLIED");
    let prev_hex = prev_raw.as_deref().and_then(parse_applied);

    if is_applied_unchanged(new_fingerprint) {
        // Fast path — set unchanged since last prompt. No exports, no
        // sentinel re-emit, and (critically) no global strip / breadcrumb:
        // the strip is a diff-path-only concern so an unchanged prompt is
        // byte-for-byte empty stdout.
        return;
    }

    // Diff path only: now that the project set is actually changing, strip
    // the global toolchain from PATH so a global-only tool cannot leak
    // into the project (strict isolation at the shell layer —
    // adr_global_toolchain_tier.md §Decision 6, CODEX-BLOCK-4).
    if strip_global {
        emit_global_path_strip(context, shell);
    }

    // Diff path: emit unset lines for every key in the export set (clean
    // re-set), then the exports, then the new sentinel. Only emit `unset`
    // when there *was* a prior applied state to invalidate.
    if prev_hex.is_some() {
        for entry in entries {
            if let Some(line) = shell.unset(&entry.key) {
                println!("{line}");
            }
        }
    }

    for entry in entries {
        // `export_path` / `export_constant` return `None` when the
        // env-var key fails POSIX validation; skip + stderr note, do not
        // abort the whole diff.
        let line = match entry.kind {
            ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
            ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
        };
        match line {
            Some(line) => println!("{line}"),
            None => eprintln!("# ocx: skipping invalid env-var key {:?}", entry.key),
        }
    }

    // Always emit the fresh sentinel — even when the new applied set is
    // empty, downstream invocations need *some* `_OCX_APPLIED` so the next
    // prompt's fast-path check has a value to compare. `_OCX_APPLIED` is a
    // fixed valid POSIX env-var name, so the emitter must produce `Some`.
    println!(
        "{}",
        shell
            .export_constant("_OCX_APPLIED", new_fingerprint)
            .expect("_OCX_APPLIED is a valid env-var name")
    );
}

/// Resolve the global toolchain's installed `current` set into
/// `(env entries, fingerprint)`.
///
/// Source = the global `ocx.lock` (`$OCX_HOME/ocx.lock`) default group ∩
/// installed `current` symlinks. Each global lock tool is resolved through
/// its **stable** `current` symlink (`find_symlink(Current)`); a tool that
/// was `ocx add --global`'d but never `ocx install --global`-selected has
/// no `current` symlink and is silently skipped (it was never selected
/// into the global PATH). Resolving via `current` (not the object store)
/// is the deliberate refinement recorded in the plan's Living Design
/// Record: the PATH segments must be stable `$OCX_HOME/symlinks/.../current`
/// paths so the static entrypoint is stable and the strict-isolation strip
/// is a single prefix filter.
///
/// `Ok(None)` when there is no `$OCX_HOME` resolvable, no global lock, or
/// no selected global tool — the caller emits nothing.
///
/// # Errors
/// Propagates `resolve_env` / index errors. Never contacts the network.
pub(crate) async fn resolve_global_current_env(
    context: &crate::app::Context,
) -> anyhow::Result<Option<(Vec<ocx_lib::package::metadata::env::entry::Entry>, String)>> {
    use ocx_lib::file_structure::SymlinkKind;
    use ocx_lib::project::{DEFAULT_GROUP, ProjectLock, lock::lock_path_for};
    use ocx_lib::shell::AppliedEntry;

    let home = context.file_structure().root();
    let global_config = home.join("ocx.toml");
    let global_lock_path = lock_path_for(&global_config);

    let Some(lock) = ProjectLock::from_path(&global_lock_path).await? else {
        return Ok(None);
    };

    // Offline-only manager clone: the hook MUST NOT contact the registry
    // regardless of `--remote` (architect boundary, same as the project
    // path).
    let manager = context.manager().offline_view(context.local_index().clone());

    let mut infos = Vec::new();
    let mut applied_entries = Vec::new();
    for tool in &lock.tools {
        if tool.group != DEFAULT_GROUP {
            continue;
        }
        // `current` is keyed by registry/repository only — strip the
        // lock's digest pin (find_symlink rejects digest-pinned ids and
        // ignores tags for `current`).
        let identifier: ocx_lib::oci::Identifier = tool.pinned.clone().into();
        let lookup = identifier.without_specifiers();
        match manager.find_symlink(&lookup, SymlinkKind::Current).await {
            Ok(info) => {
                applied_entries.push(AppliedEntry {
                    name: tool.name.clone(),
                    manifest_digest: tool.pinned.digest().to_string(),
                    group: tool.group.clone(),
                });
                infos.push(std::sync::Arc::new(info));
            }
            // Not `install --global`-selected (or symlink absent) — skip.
            Err(_) => continue,
        }
    }

    if infos.is_empty() {
        return Ok(None);
    }

    let fingerprint = compute_fingerprint(&applied_entries);
    let entries = manager.resolve_env(&infos, false).await?;
    Ok(Some((entries, fingerprint)))
}

/// Emit shell exports for the global toolchain's installed `current` set.
///
/// Fires only when no project `ocx.toml` is in effect. Reuses the shared
/// sentinel-diff emitter (strict isolation: never merged into project
/// resolution — adr_global_toolchain_tier.md §Decision 6). On leaving a
/// project this restores the global bin dir (a fresh export + sentinel).
async fn emit_global_current_set(context: &crate::app::Context, shell: ocx_lib::shell::Shell) -> anyhow::Result<()> {
    let Some((entries, fingerprint)) = resolve_global_current_env(context).await? else {
        // The global set resolved empty (no global lock, no selected global
        // tool, or the last global tool was just `ocx uninstall`'d /
        // deselected). If a *prior* global set was applied to this shell
        // (`_OCX_APPLIED` carries a parseable fingerprint), the previously
        // exported global vars + the global bin dir are now stale and must
        // be cleared — otherwise the shell keeps the departed global
        // PATH/env forever. Route through the *same* sentinel-diff emitter
        // every other path uses (feedback_extend_dont_duplicate): an empty
        // entry set with the empty-set fingerprint takes the diff path
        // (fingerprint differs from the prior non-empty one), strips the
        // global bin dir from PATH (`strip_global = true`), and resets
        // `_OCX_APPLIED` to the empty-set sentinel. When there was *no*
        // prior applied state the empty-set fingerprint matches and the
        // emitter short-circuits — empty stdout, fast path preserved
        // exactly as before (regression
        // test_shell_hook_unchanged_fingerprint_emits_empty).
        let empty_fingerprint = compute_fingerprint(&[]);
        emit_env_with_sentinel(shell, &[], &empty_fingerprint, true, context);
        return Ok(());
    };
    emit_env_with_sentinel(shell, &entries, &fingerprint, false, context);
    Ok(())
}

/// On project activation, explicitly remove the global bin dir from PATH
/// (CODEX-BLOCK-4).
///
/// The static `$OCX_HOME/init.<shell>` entrypoint permanently prepends the
/// global `current` bin dir to PATH, so resolution-layer strict isolation
/// is not automatically reflected in the interactive shell. The `_OCX_APPLIED`
/// sentinel diff/unset mechanism (run by the caller right after this) handles
/// the project-var unset/re-export; this additionally emits a per-shell PATH
/// filter that drops every segment under `$OCX_HOME/symlinks` — so a
/// global-only tool is *removed* (not merely shadowed) inside a project. On
/// leaving the project the global bin dir is restored by
/// [`emit_global_current_set`].
///
/// Called **only from the emitter's diff path**, after the
/// unchanged-fingerprint short-circuit: an unchanged prompt emits nothing,
/// so this breadcrumb + strip never appear on the fast path (regression
/// `test_shell_hook_unchanged_fingerprint_emits_empty`).
fn emit_global_path_strip(context: &crate::app::Context, shell: ocx_lib::shell::Shell) {
    // The global static entrypoint prepends PATH segments rooted at
    // `$OCX_HOME/symlinks/.../current/...`. Strip every PATH element
    // containing that prefix so a global-only tool cannot resolve inside a
    // project (strict isolation at the shell layer). The project set is
    // emitted by the caller right after this (project supersedes — no
    // merge); the `_OCX_APPLIED` sentinel diff there handles the
    // project-var unset/re-export.
    let symlinks_root = context.file_structure().symlinks.root();
    let needle = symlinks_root.to_string_lossy().into_owned();

    if let Some(line) = shell.filter_path_excluding(&needle) {
        // Inert `# ocx:` breadcrumb (same shell-comment convention as the
        // failure note below): the diff path silently strips the global
        // toolchain, so without this a global-only tool becomes "command
        // not found" with no trace. Two callers: a project becoming active
        // (project supersedes the global set), or the global set itself
        // departing while no project is in scope (last global tool
        // uninstalled/deselected). The line is a no-op when the hook
        // output is sourced.
        println!("# ocx: global toolchain removed from PATH");
        println!("{line}");
    } else {
        eprintln!("# ocx: cannot strip the global bin dir for this shell; global tools may leak into the project");
    }
}
