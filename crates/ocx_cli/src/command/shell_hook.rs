// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;

use clap::Parser;
use ocx_lib::{
    env,
    package::metadata::env::modifier::ModifierKind,
    project::{MissingState, collect_applied, load_project_state},
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

        // Project tier ONLY in Phase 7. See `shell_direnv.rs` for the same
        // policy rationale.
        let cwd = env::current_dir()?;
        let project = match load_project_state(&cwd, context.project_path()).await? {
            Ok(state) => state,
            Err(MissingState::NoProject) => {
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

        // Architect boundary: hook MUST NOT contact the registry, regardless
        // of `--remote`. Force an offline-only `PackageManager` clone so any
        // incidental index lookup (e.g. via `resolve` to walk the cached
        // manifest chain) cannot escape into the network.
        let manager = context.manager().offline_view(context.local_index().clone());
        let platforms = supported_platforms();

        // ── Existence-driven fingerprint compute (Codex P2 fix) ──────
        //
        // Walk the lock's default-group tools, filter to those whose
        // content is actually present in the object store via
        // `find_plain`. `collect_applied` returns one
        // `AppliedEntry`/`InstallInfo` pair per resolvable tool; tools
        // missing from the store are recorded in `applied.missing` and
        // excluded from the fingerprint. Sourcing `manifest_digest` from
        // `LockedTool::pinned.digest()` (done inside `collect_applied`)
        // keeps the fingerprint stable across machines and platforms.
        let applied = collect_applied(&manager, &project.lock, &platforms).await?;

        // The fingerprint we ultimately export is computed over the
        // *actually-installed* tools (excluding `missing`) so
        // `ocx uninstall` / `ocx clean` causes the next prompt to detect
        // the change and re-export with a fresh sentinel.
        let new_fingerprint = compute_fingerprint(&applied.entries);

        let prev_raw = env::var("_OCX_APPLIED");
        let prev_hex = prev_raw.as_deref().and_then(parse_applied);
        let expected_hex =
            parse_applied(&new_fingerprint).expect("compute_fingerprint output must round-trip parse_applied");

        if prev_hex == Some(expected_hex) {
            // Fast path — actually-installed set is unchanged since last
            // prompt. No `resolve_env`, no exports, no missing-tool notes
            // (the prior export emitted those already; surfacing them on
            // every prompt would spam the user's terminal).
            return Ok(ExitCode::SUCCESS);
        }

        // ── Diff path: emit notes + unset + new exports + sentinel ───
        for name in &applied.missing {
            eprintln!("# ocx: {name} not installed; run `ocx pull` to fetch");
        }

        // Diff path: build the fresh export entries first, then emit unset
        // lines for every key in the export set (so the shell sees a clean
        // re-set), then the exports themselves, then the new sentinel.
        let entries = manager.resolve_env(&applied.infos, false).await?;

        // Only emit `unset` lines when there *was* a prior applied state —
        // otherwise we have nothing to invalidate.
        if prev_hex.is_some() {
            for entry in &entries {
                if let Some(line) = shell.unset(&entry.key) {
                    println!("{line}");
                }
            }
        }

        for entry in &entries {
            // `export_path` / `export_constant` return `None` when the
            // env-var key fails POSIX validation; skip + stderr note, do
            // not abort the whole diff.
            let line = match entry.kind {
                ModifierKind::Path => shell.export_path(&entry.key, &entry.value),
                ModifierKind::Constant => shell.export_constant(&entry.key, &entry.value),
            };
            match line {
                Some(line) => println!("{line}"),
                None => eprintln!("# ocx: skipping invalid env-var key {:?}", entry.key),
            }
        }

        // Always emit the fresh sentinel — even when the new applied set
        // is empty, downstream invocations need to see *some* `_OCX_APPLIED`
        // so the next prompt's fast-path check has a value to compare.
        // `_OCX_APPLIED` is a fixed valid POSIX env-var name, so the
        // emitter must produce `Some(_)`.
        println!(
            "{}",
            shell
                .export_constant("_OCX_APPLIED", &new_fingerprint)
                .expect("_OCX_APPLIED is a valid env-var name")
        );

        Ok(ExitCode::SUCCESS)
    }
}
