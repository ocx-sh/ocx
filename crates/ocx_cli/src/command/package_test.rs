// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use ocx_lib::package_manager::launcher;
use ocx_lib::script::{ScriptOutcome, ScriptOutcomeKind};
use ocx_lib::utility::child_process;
use ocx_lib::utility::fs as ocx_fs;
use ocx_lib::{cli, cli::UsageError, env, oci, package, prelude::*, publisher::LayerRef};

use crate::{conventions, options};

/// Materialize a package locally (no registry round-trip) and run a command in its env.
///
/// Mirrors `ocx package push` inputs (identifier + `--platform` + `--metadata` +
/// layers). The package is built into a temp directory, its declared deps are
/// auto-installed into the regular packages store, env is composed via the
/// same path as `ocx exec`, and the trailing `-- CMD [ARGS...]` is invoked in
/// that env. The temp directory is auto-deleted on success and on failure
/// unless `--keep` or `--output` is given.
#[derive(Parser)]
pub struct PackageTest {
    /// Path to the package metadata JSON file. Defaults to a sibling of the
    /// first file layer (e.g. `pkg.tar.gz` -> `pkg-metadata.json`). Required
    /// when no file layers are provided.
    #[clap(short, long)]
    metadata: Option<PathBuf>,

    /// Target platform (e.g. `linux/amd64`). Required — parity with `package push`.
    #[clap(short, long, required = true)]
    platform: oci::Platform,

    /// Materialize into DIR instead of an auto-managed temp dir. DIR must not
    /// exist (created by ocx) or be empty. Implies keep — the dir is never
    /// deleted by ocx. Must reside on the same filesystem as
    /// `$OCX_HOME/layers/` — hardlink assembly does not fall back to copy.
    #[clap(short = 'o', long, conflicts_with = "keep")]
    output: Option<PathBuf>,

    /// Preserve the temp build directory after the command exits. Path is
    /// printed to stderr. Default temp root is `$OCX_HOME/temp/test/`.
    #[clap(long)]
    keep: bool,

    /// Compose the package's private env surface (default: interface surface).
    /// Same semantics as `ocx exec --self` / `ocx env --self`.
    #[clap(long = "self", default_value_t = false)]
    self_view: bool,

    /// Strip ambient parent env before composing — only `OCX_*` config and
    /// composed package vars reach the child. Mirrors `ocx exec --clean`.
    #[clap(long, default_value_t = false)]
    clean: bool,

    /// Identifier under which the package is materialized. Tag form
    /// (`repo:tag`) only; an explicit `@digest` is rejected (the digest is
    /// computed locally during this command and supplying one would conflict).
    #[clap(short = 'i', long = "identifier", required = true, value_terminator = "--")]
    identifier: options::Identifier,

    /// Layers, in order (base first, top last). Same syntax as `package push`:
    /// either a path to a `.tar.gz`/`.tar.xz` archive, or `sha256:<hex>.<ext>`
    /// referring to a layer already present in the target registry. Digest
    /// refs are auto-pulled from the registry on demand; in `--offline`,
    /// missing digest blobs error with `OfflineBlocked`.
    #[clap(num_args = 0.., value_terminator = "--")]
    layers: Vec<LayerRef>,

    /// Path to a Starlark test script. Mutually exclusive with the trailing
    /// command. When given, the materialized package env is interpreted by the
    /// embedded engine instead of exec'ing a command.
    ///
    /// The value `-` reads the script SOURCE from stdin (parsed with the
    /// filename label `<stdin>`). This is independent of the per-call
    /// `ocx.run(stdin=...)` kwarg, which feeds a *child* process's stdin.
    #[clap(long, conflicts_with = "command")]
    script: Option<PathBuf>,

    /// Command to execute inside the composed env, with arguments. Required
    /// unless `--script` is given (exactly one of the two forms must be
    /// supplied).
    #[clap(allow_hyphen_values = true, required_unless_present = "script", num_args = 1..)]
    command: Vec<String>,
}

impl PackageTest {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        // Step 1: Resolve identifier. Reject @digest — the digest is computed locally.
        let identifier = self.identifier.with_domain(context.default_registry())?;
        if identifier.digest().is_some() {
            return Err(anyhow::Error::from(UsageError::new(
                "package test rejects @digest in identifier; the digest is computed locally from the supplied layers",
            )));
        }

        // Step 2: Load metadata.
        let metadata_path = conventions::resolve_metadata_path(&self.layers, self.metadata.as_deref())?;

        let metadata = package::metadata::ValidMetadata::try_from(
            package::metadata::Metadata::read_json(&metadata_path)
                .await
                .with_context(|| format!("reading metadata from {}", metadata_path.display()))?,
        )?;
        let info = package::info::Info {
            identifier: identifier.clone(),
            metadata: metadata.into(),
            platform: self.platform.clone(),
        };

        let manager = context.manager();
        let fs = context.file_structure();
        let temp_test_root = fs.temp.root().join("test");

        // Step 3: Decide destination + tempdir lifecycle.
        //
        // Three cases:
        // a) --output DIR: validate same-filesystem + empty; keep it (no delete).
        // b) --keep: auto temp dir, do not delete, print path to stderr.
        // c) default: auto temp dir; delete explicitly before exec (RAII cannot be
        //    used because `child_process::exec` diverges on Unix — the process is
        //    replaced and Drop never runs). Errors before exec rely on the RAII
        //    guard to clean up; the guard is manually consumed (closed) just before
        //    the exec call in step 7.
        //
        // `td_guard` holds the TempDir when auto-cleanup is wanted (case c).
        // `keep_msg` is printed to stderr just before exec — the last line ocx writes.
        let (dest_path, td_guard, keep_msg): (PathBuf, Option<tempfile::TempDir>, Option<String>) =
            match (&self.output, self.keep) {
                (Some(out), _) => {
                    // Refuse if --output resolves through any symlink in its ancestor chain.
                    // Symlink traversal in a destination path can redirect writes to
                    // attacker-controlled locations.
                    ocx_fs::refuse_if_symlink_in_path(out).await?;
                    // Validate same filesystem as $OCX_HOME/layers/.
                    let layers_root = fs.layers.root();
                    if !ocx_fs::same_filesystem(out, layers_root).await? {
                        return Err(anyhow::Error::from(ocx_lib::Error::InternalFile(
                            out.clone(),
                            std::io::Error::new(
                                std::io::ErrorKind::CrossesDevices,
                                format!(
                                    "destination '{}' must be on the same filesystem as $OCX_HOME/layers ('{}'); \
                                     hardlink assembly does not fall back to copy",
                                    out.display(),
                                    layers_root.display(),
                                ),
                            ),
                        )));
                    }
                    // Ensure the directory is absent or empty.
                    ocx_fs::ensure_empty_or_absent(out).await?;
                    tokio::fs::create_dir_all(out)
                        .await
                        .map_err(|e| ocx_lib::error::file_error(out, e))?;
                    (out.clone(), None, None)
                }

                (None, true) => {
                    tokio::fs::create_dir_all(&temp_test_root)
                        .await
                        .map_err(|e| ocx_lib::error::file_error(&temp_test_root, e))?;
                    let td = tempfile::Builder::new()
                        .prefix("test-")
                        .tempdir_in(&temp_test_root)
                        .map_err(|e| ocx_lib::error::file_error(&temp_test_root, e))?;
                    let path = td.path().to_path_buf();
                    // Suppress RAII delete — caller wants to inspect on failure too.
                    // `TempDir::keep()` returns a `PathBuf` (infallible); `path` was
                    // captured above, so we discard the returned value.
                    let _kept_path = td.keep();
                    (path.clone(), None, Some(format!("kept at {}", path.display())))
                }

                (None, false) => {
                    tokio::fs::create_dir_all(&temp_test_root)
                        .await
                        .map_err(|e| ocx_lib::error::file_error(&temp_test_root, e))?;
                    let td = tempfile::Builder::new()
                        .prefix("test-")
                        .tempdir_in(&temp_test_root)
                        .map_err(|e| ocx_lib::error::file_error(&temp_test_root, e))?;
                    let path = td.path().to_path_buf();
                    // RAII guard: drops on any `?` error before exec, cleaning up.
                    // Before the exec call we explicitly close/drop to delete the
                    // dir (exec diverges, so implicit Drop never fires).
                    (path, Some(td), None)
                }
            };

        // Step 4: Materialize package via the local install pipeline.
        let _install_info = manager.pull_local(info, &self.layers, Some(&dest_path)).await?;

        // Step 5: Bridge to env composition via install_info_from_package_root.
        let info_via_root = manager
            .install_info_from_package_root(&dest_path)
            .await
            .context("loading install info from materialized package root")?;
        let entries = manager.resolve_env(&[Arc::new(info_via_root)], self.self_view).await?;

        // Step 6: Compose env (mirrors exec.rs).
        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
        process_env.apply_entries(&entries);
        // Block-tier: forward running ocx's resolution-affecting config to child.
        process_env.apply_ocx_config(context.config_view());
        // Windows PATHEXT: ensure launcher extension is discoverable.
        launcher::emplace_pathext(&mut process_env);

        // Step 7 (script branch): when --script is present, interpret the
        // Starlark script instead of exec'ing a trailing command. Returns
        // Ok(ExitCode) so main.rs::classify_error is bypassed (ADR Exit Code
        // Scheme). The non-script path below is byte-identical to before.
        if let Some(script_path) = &self.script {
            // PRECONDITION (Codex C7): run_script is sync and the ocx.run host
            // fn uses Handle::block_on inside block_in_place — correct ONLY on
            // Tokio's multi-thread runtime. main.rs uses the default
            // multi-thread #[tokio::main]; assert it so a future switch to
            // current_thread fails loudly in debug builds.
            debug_assert!(
                matches!(
                    tokio::runtime::Handle::current().runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::MultiThread
                ),
                "run_script requires the multi-thread Tokio runtime (Handle::block_on in block_in_place)"
            );

            // Read the script source. `-` reads the SOURCE from stdin (R1);
            // any other value is a filesystem path. A missing path file →
            // Usage/64; a stdin stream that errors → Io/74 (distinct: reading
            // a supplied stream that fails is I/O, not bad usage).
            //
            // Pre-engine host-setup failures (unreadable path / stdin stream
            // error) are NOT script outcomes — they are surfaced as typed
            // errors so `main.rs` logs them to stderr and `classify_error`
            // derives the exit code (subsystem-cli-api: "errors → typed
            // error, never eprintln; main.rs is the single boundary"). The
            // `Ok(ExitCode)` bypass (ADR) applies only to genuine *script*
            // outcomes, not to these pre-engine failures. Missing/unreadable
            // path → UsageError (64); stdin stream read failure → IoError (74,
            // distinct: reading a supplied stream that errors is I/O).
            let is_stdin = script_path == std::path::Path::new("-");
            let (source, label): (String, String) = if is_stdin {
                let mut buf = String::new();
                match tokio::io::AsyncReadExt::read_to_string(&mut tokio::io::stdin(), &mut buf).await {
                    // LDR-8: for `--script -`, a zero-byte stdin means the
                    // source was never delivered (closed/broken pipe) — that
                    // is an I/O failure (74), distinct from an explicitly
                    // empty script *file* (U11: empty file → Passed). Rust's
                    // `read_to_string` returns Ok(0) on EOF, so the broken-
                    // stream case is detected here, not via `Err`.
                    Ok(0) => {
                        drop(td_guard);
                        return Err(anyhow::Error::from(ocx_lib::Error::InternalFile(
                            std::path::PathBuf::from("<stdin>"),
                            std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "no script source provided on stdin (--script -)",
                            ),
                        )));
                    }
                    Ok(_) => (buf, "<stdin>".to_string()),
                    Err(e) => {
                        drop(td_guard);
                        return Err(anyhow::Error::from(ocx_lib::Error::InternalFile(
                            std::path::PathBuf::from("<stdin>"),
                            std::io::Error::new(e.kind(), format!("failed reading script source from stdin: {e}")),
                        )));
                    }
                }
            } else {
                match tokio::fs::read_to_string(script_path).await {
                    Ok(s) => (s, script_path.display().to_string()),
                    Err(e) => {
                        drop(td_guard);
                        return Err(anyhow::Error::from(UsageError::new(format!(
                            "cannot read script '{}': {e}",
                            script_path.display()
                        ))));
                    }
                }
            };

            // Scratch dir: sibling of the package root, same lifecycle (kept
            // with the package root under --keep/--output; removed with the
            // bare temp dir otherwise — see the explicit removal below).
            let scratch_root = provision_scratch_dir(&dest_path).await?;
            let cleanup_scratch = td_guard.is_some();

            if let Some(msg) = &keep_msg {
                eprintln!("{msg}");
            }

            let limits = ocx_lib::script::ScriptLimits {
                max_callstack_size: 50,
                wall_clock: std::time::Duration::from_secs(300),
            };

            // run_script is SYNC (Evaluator is !Send): invoke via
            // block_in_place, NOT .await / spawn_blocking (Fix#2).
            let outcome_res = tokio::task::block_in_place(|| {
                ocx_lib::script::run_script(
                    &source,
                    &label,
                    &dest_path,
                    &scratch_root,
                    &self.platform,
                    process_env,
                    limits,
                )
            });

            // Drop the tempdir guard now the engine finished (success or
            // failure) — same lifecycle as the non-script branch. The scratch
            // sibling is not inside the guarded tempdir, so remove it
            // explicitly in the bare (cleanup) case; --keep/--output leave it
            // for post-failure debugging.
            drop(td_guard);
            if cleanup_scratch {
                let _ = tokio::fs::remove_dir_all(&scratch_root).await;
            }

            let outcome = match outcome_res {
                Ok(o) => o,
                // Host setup/abort (unrecoverable) → Failure(1); never code 2.
                Err(e) => {
                    let report = crate::api::data::script_run::ScriptRunReport::new(
                        crate::api::data::script_run::ScriptStatus::Failed,
                        Some(crate::api::data::script_run::AssertionRecord {
                            // Pre/post-engine host abort: not an `expect.*`
                            // assertion — categorize as the non-assertion kind.
                            kind: "other".to_string(),
                            message: format!("script host failure: {e}"),
                        }),
                        None,
                    );
                    context.api().report(&report)?;
                    return Ok(cli::ExitCode::Failure.into());
                }
            };

            // R3: emit the structured envelope through the existing report
            // path; the exit code stays authoritative (both emitted).
            let run_summary = ocx_lib::script::last_run_summary();
            let report = crate::api::data::script_run::ScriptRunReport::from_outcome(&outcome, run_summary);
            context.api().report(&report)?;

            return Ok(map_outcome_to_exit_code(outcome).into());
        }

        // Step 7: Resolve command and exec.
        let (command, args) = self
            .command
            .split_first()
            .expect("clap required_unless_present=script guarantees a command in the non-script branch");

        let resolved = process_env.resolve_command(command);

        // Print keep message before the child runs — last output ocx produces.
        if let Some(msg) = &keep_msg {
            eprintln!("{msg}");
        }

        // Two distinct execution paths based on tempdir lifecycle:
        //
        // a) Bare invocation (no --keep, no --output): `td_guard` holds the
        //    tempdir. We MUST keep it alive while the child runs — the binary
        //    lives inside the materialized package, so deleting before exec
        //    causes ENOENT. Use spawn+wait so we can drop the guard AFTER the
        //    child exits, then propagate the exit code.
        //
        // b) --keep or --output: the directory is already persisted (either
        //    intentionally kept or written to a caller-owned path). Use execvp
        //    for the cleaner "no extra process" semantic. `td_guard` is None
        //    in this branch.
        if td_guard.is_some() {
            // Bare invocation: spawn child, await exit, drop tempdir, propagate.
            let status = child_process::spawn_and_wait(&resolved, args, process_env)
                .await
                .map_err(|e| anyhow::Error::from(e).context(format!("failed to run '{}'", resolved.display())))?;

            // Drop the tempdir guard now that the child has exited — this
            // deletes the materialized package directory (success or failure).
            drop(td_guard);

            Ok(child_process::propagate_exit_code(status))
        } else {
            // --keep or --output path: directory persists; use execvp which
            // diverges on Unix (Drop never runs, but that's fine here because
            // td_guard is None).
            let err = child_process::exec(&resolved, args, process_env);
            Err(anyhow::Error::from(err).context(format!("failed to run '{}'", resolved.display())))
        }
    }
}

/// Creates the script scratch directory as a sibling of the package root
/// inside the same temp/output root, following the same lifecycle as the
/// package root (kept when the package root is kept; deleted with the bare
/// path otherwise).
///
/// Returns the scratch root path (read-write sandbox for the engine).
async fn provision_scratch_dir(package_root: &std::path::Path) -> anyhow::Result<PathBuf> {
    let parent = package_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("package root has no parent directory"))?;
    let name = package_root.file_name().and_then(|n| n.to_str()).unwrap_or("test");
    let scratch = parent.join(format!("{name}-scratch"));
    // Async dir creation: this runs on the Tokio runtime BEFORE the
    // `block_in_place` engine call — blocking `std::fs` here would stall the
    // worker thread.
    tokio::fs::create_dir_all(&scratch).await.map_err(|e| {
        // Scratch creation failure → IoError (74) per C2; surface as the lib
        // file error so classify_error maps it (this is a pre-engine host
        // setup failure, not a script outcome).
        anyhow::Error::from(ocx_lib::error::file_error(&scratch, e))
    })?;
    Ok(scratch)
}

/// Maps an engine-neutral [`ScriptOutcome`] to the process exit code.
///
/// Returns `Ok(ExitCode)` so the command bypasses `main.rs::classify_error`
/// (ADR Exit Code Scheme). Engine-internal errors map to `Failure` (1) — no
/// code `2` is invented.
fn map_outcome_to_exit_code(outcome: ScriptOutcome) -> cli::ExitCode {
    match outcome.kind {
        ScriptOutcomeKind::Passed => cli::ExitCode::Success,
        ScriptOutcomeKind::Failed { .. } => cli::ExitCode::Failure,
        ScriptOutcomeKind::Usage { .. } => cli::ExitCode::UsageError,
        ScriptOutcomeKind::ScriptError { .. } => cli::ExitCode::DataError,
        ScriptOutcomeKind::Io { .. } => cli::ExitCode::IoError,
        ScriptOutcomeKind::Timeout => cli::ExitCode::Failure,
        // `ScriptOutcomeKind` is `#[non_exhaustive]` — unmapped kinds fall to
        // generic failure (locked here, never silently to a new code).
        _ => cli::ExitCode::Failure,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── map_outcome_to_exit_code — every C8 table row ────────────────────────
    //
    // Spec source: plan_package_test_scripting.md C8 exit-code mapping table +
    // Error Taxonomy. Each assertion quotes the canonical numeric exit value
    // from the contract, NOT derived by reading the match arm — if the mapping
    // drifts, the test catches it. The fn is pure; these lock the published
    // exit-code contract (the PRIMARY machine signal per R3).

    fn outcome(kind: ScriptOutcomeKind) -> ScriptOutcome {
        ScriptOutcome { kind }
    }

    #[test]
    fn passed_maps_to_success_0() {
        assert_eq!(map_outcome_to_exit_code(outcome(ScriptOutcomeKind::Passed)) as u8, 0);
    }

    #[test]
    fn failed_maps_to_failure_1() {
        let o = outcome(ScriptOutcomeKind::Failed {
            kind: Some(ocx_lib::script::AssertionKind::Ok),
            message: "assertion failed".into(),
        });
        assert_eq!(map_outcome_to_exit_code(o) as u8, 1);
    }

    #[test]
    fn usage_maps_to_usage_error_64() {
        let o = outcome(ScriptOutcomeKind::Usage {
            message: "unreadable script".into(),
        });
        assert_eq!(map_outcome_to_exit_code(o) as u8, 64);
    }

    #[test]
    fn script_error_maps_to_data_error_65() {
        let o = outcome(ScriptOutcomeKind::ScriptError {
            message: "syntax error".into(),
        });
        assert_eq!(map_outcome_to_exit_code(o) as u8, 65);
    }

    #[test]
    fn io_maps_to_io_error_74() {
        let o = outcome(ScriptOutcomeKind::Io {
            message: "scratch I/O failed".into(),
        });
        assert_eq!(map_outcome_to_exit_code(o) as u8, 74);
    }

    #[test]
    fn timeout_maps_to_failure_1() {
        // C8: Timeout → Failure (1), NOT a dedicated code.
        assert_eq!(map_outcome_to_exit_code(outcome(ScriptOutcomeKind::Timeout)) as u8, 1);
    }
}
