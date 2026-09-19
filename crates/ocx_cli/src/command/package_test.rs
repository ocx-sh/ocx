// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_oci::layer_ref::LayerRef;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use ocx_config::env;
use ocx_package_manager::launch::{self, ExemptionReason, Launch};
use ocx_util::child_process;
use ocx_util::fs as ocx_fs;

use crate::api::data::script_run::{AssertionRecord, ScriptRunReport, ScriptStatus};
use crate::error::UsageError;
use crate::{conventions, options};
use ocx_package::metadata::env::apply::{ChildEnv, EnvEntriesExt, reconcile_list_separators};
use ocx_shell::shell::reconcile;

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
    ///
    /// This must be the compiled form `ocx package create --metadata` writes,
    /// with every dependency pinned to a digest; an authoring sidecar with
    /// tag-only dependencies is rejected. The build receipt is anchored to the
    /// bundle, so pointing this flag elsewhere does not move where an omitted
    /// `--platform` is read from.
    #[clap(short, long)]
    metadata: Option<PathBuf>,

    /// Target platform (e.g. `linux/amd64`). An explicit value is used as
    /// given. Omit it to take the platform the build receipt beside the bundle
    /// recorded; a usage error (exit 64) when neither names one. Parity with
    /// `package push`.
    #[clap(short, long)]
    platform: Option<ocx_oci::Platform>,

    /// Materialize into DIR instead of an auto-managed temp dir. DIR must not
    /// exist (created by ocx) or be empty. Implies keep - the dir is never
    /// deleted by ocx. Must reside on the same filesystem as
    /// `$OCX_HOME/layers/` - hardlink assembly does not fall back to copy.
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

    /// Strip ambient parent env before composing - only `OCX_*` config and
    /// composed package vars reach the child. Mirrors `ocx exec --clean`.
    #[clap(long, default_value_t = false)]
    clean: bool,

    #[clap(flatten)]
    env: options::EnvOverride,

    /// Identifier under which the package is materialized. Tag form
    /// (`repo:tag`) only; an explicit `@digest` is rejected (the digest is
    /// computed locally during this command and supplying one would conflict).
    #[clap(short = 'i', long = "identifier", required = true, value_terminator = "--")]
    identifier: options::Identifier,

    /// Layers, in order (base first, top last). Same syntax as `package push`:
    /// either a path to a `.tar.gz`/`.tar.xz`/`.tar.zst` archive, or
    /// `sha256:<hex>.<ext>`
    /// referring to a layer already present in the target registry. Digest
    /// refs are auto-pulled from the registry on demand; in `--offline`,
    /// missing digest blobs error with `PolicyBlocked`.
    ///
    /// A layer may carry an optional layout tail `:strip=N,prefix=P`
    /// (`strip=N` drops N leading path components; `prefix=P` relocates the
    /// layer under the relative subdirectory `P`), e.g.
    /// `./libs.tar.gz:strip=1,prefix=share`.
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

    /// Write a JUnit XML report for the scripted run to PATH.
    ///
    /// Requires `--script`; cannot be combined with a trailing command. Parent
    /// directories are created; an existing file is truncated, never merged -
    /// give each platform its own path and let CI collect them. The GitHub
    /// test-report actions merge the files they are handed into one report.
    /// GitLab's `artifacts:reports:junit` keeps results from different jobs in
    /// separate suites; a glob gathers only what one job wrote. Every per-leg
    /// entry carries the same package identifier as its label, so see the
    /// [one path per platform](https://ocx.sh/docs/authoring/testing#scripted-tests-junit-per-platform)
    /// documentation for the shape to design for.
    ///
    /// Written whenever the scripted run is reached, including a red run and an
    /// unreadable `--script` path; a failure resolving the package writes none.
    /// The JSON envelope on stdout is unchanged. The exit code is not: a report
    /// that cannot be written exits 74 after an otherwise green run, like every
    /// other operator-supplied output path.
    // The "requires --script" rule is enforced in `validate_junit`, not as a
    // clap `requires = "script"`. A clap `requires` demands both `--script`
    // AND the trailing `<COMMAND>` at once — an unsatisfiable pair, since the
    // two are mutually exclusive. Instead `command` lists `junit` in its
    // `required_unless_present_any`, so a bare `--junit` (no `--script`, no
    // command) parses through to `validate_junit`, which gives the clean
    // "--junit requires --script" message. `conflicts_with = "command"` still
    // catches `--junit` beside a command at parse time.
    #[clap(long, conflicts_with = "command", value_name = "PATH")]
    junit: Option<PathBuf>,

    /// Command to execute inside the composed env, with arguments. Required
    /// unless `--script` is given (exactly one of the two forms must be supplied).
    ///
    /// `last = true` (mirroring `toolchain_exec.rs`'s `argv`) makes clap parse everything
    /// before the mandatory `--` into `layers` and everything after into
    /// `command`. Without it, `command` is an ordinary positional sitting
    /// after the optional `layers` (index 1), which trips clap's debug-assert
    /// "non-required positional with a lower index than a required positional" -
    /// fatal in debug builds when the command tree is built (e.g. completion
    /// generation). Requires clap >= 4.5.57 (see `toolchain_exec.rs` NOTE).
    #[clap(allow_hyphen_values = true, last = true, required_unless_present_any = ["script", "junit"], num_args = 1..)]
    command: Vec<String>,
}

impl PackageTest {
    /// Rejects `--junit` given without `--script`.
    ///
    /// `--junit` reports a *scripted* run; without `--script` there is no
    /// scripted run to report. A bare `--junit` (no `--script`, no command)
    /// reaches here only because `command` lists `junit` in its
    /// `required_unless_present_any` — a `required_unless_present*` positional
    /// is NOT waived by a conflict with a present arg, so `conflicts_with`
    /// alone would leave clap demanding `<COMMAND>`. The
    /// `--junit`-beside-a-command shape is caught earlier by
    /// `conflicts_with = "command"`. See the field comment on `junit` for why
    /// this is not a clap `requires = "script"`.
    fn validate_junit(&self) -> anyhow::Result<()> {
        if self.junit.is_some() && self.script.is_none() {
            return Err(anyhow::Error::from(UsageError::new(
                "--junit requires --script; it reports a scripted run and has no report to write without one",
            )));
        }
        Ok(())
    }

    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        self.validate_junit()?;

        // Step 1: Resolve identifier. Reject @digest — the digest is computed locally.
        let identifier = self.identifier.with_domain(context.default_registry())?;
        if identifier.digest().is_some() {
            return Err(anyhow::Error::from(UsageError::new(
                "package test rejects @digest in identifier; the digest is computed locally from the supplied layers",
            )));
        }

        // Step 2: Load metadata.
        let metadata_path = conventions::resolve_metadata_path(&self.layers, self.metadata.as_deref())?;

        // Read the published sidecar `ocx package create` compiled — the same
        // bytes `ocx package push` would publish. The tested platform falls
        // back to the build receipt beside the bundle on exactly the contract
        // `ocx package push` uses, and the receipt is only opened when
        // `--platform` left the question open.
        let metadata = conventions::read_published_metadata(&metadata_path).await?;
        let receipt = match self.platform {
            Some(_) => None,
            None => crate::build_receipt::read_beside_bundle(&self.layers).await?,
        };
        let platform = crate::build_receipt::resolve_target_platform(self.platform.clone(), receipt.as_ref())?;
        let metadata = ocx_package::metadata::ValidMetadata::try_from(metadata)?;
        let info = ocx_package::info::Info {
            identifier: identifier.clone(),
            metadata: metadata.into(),
            platform: platform.clone(),
        };

        let manager = context.manager();
        let fs = context.file_structure();
        let temp_test_root = fs.temp.package_test_root();

        // Step 3: Decide destination + tempdir lifecycle.
        //
        // Three cases:
        // a) --output DIR: validate same-filesystem + empty; keep it (no delete).
        // b) --keep: auto temp dir, do not delete, print path to stderr.
        // c) default: auto temp dir; delete explicitly before exec (RAII cannot be
        //    used because `launch::exec` diverges on Unix — the process is
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
                    // attacker-controlled locations. `--output` is a fully user-supplied
                    // path, so walk the whole chain (no trusted boundary).
                    ocx_fs::refuse_if_symlink_in_path(out, None).await?;
                    // Validate same filesystem as $OCX_HOME/layers/.
                    let layers_root = fs.layers.root();
                    if !ocx_fs::same_filesystem(out, layers_root).await? {
                        return Err(anyhow::Error::from(ocx_util::error::FileError::new(
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
                        .map_err(|e| ocx_util::error::FileError::new(out, e))?;
                    (out.clone(), None, None)
                }

                (None, true) => {
                    tokio::fs::create_dir_all(&temp_test_root)
                        .await
                        .map_err(|e| ocx_util::error::FileError::new(&temp_test_root, e))?;
                    let td = tempfile::Builder::new()
                        .prefix("test-")
                        .tempdir_in(&temp_test_root)
                        .map_err(|e| ocx_util::error::FileError::new(&temp_test_root, e))?;
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
                        .map_err(|e| ocx_util::error::FileError::new(&temp_test_root, e))?;
                    let td = tempfile::Builder::new()
                        .prefix("test-")
                        .tempdir_in(&temp_test_root)
                        .map_err(|e| ocx_util::error::FileError::new(&temp_test_root, e))?;
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
        // Overrides are the caller's own contribution; the OCI tier reads no
        // `ocx.toml`, so they are the only thing this scope can carry.
        let cwd = std::env::current_dir()
            .map_err(|error| anyhow::Error::from(error).context("failed to read the current directory"))?;
        let mut env_overrides = self.env.entries(&cwd)?;
        let mut entries = manager
            .resolve_env(
                &[Arc::new(info_via_root)],
                self.self_view,
                // Cloned, not moved: the same overrides are ALSO the forwarded
                // slice below, and a handful of entries is cheaper than the
                // machinery to hand them back out of the scope.
                ocx_package_manager::EnvScope::Package {
                    env: env_overrides.clone(),
                },
                &platform,
            )
            .await?;
        // W-11: `entries` and `env_overrides` are disjoint `Vec`s holding
        // independent copies of the `--env` overrides (mirrors exec.rs) —
        // reconcile them together so a package-established `list` separator
        // reaches the forwarded copy.
        reconcile_list_separators(entries.iter_mut().chain(env_overrides.iter_mut()))?;

        // Step 6: Compose env (mirrors exec.rs). Composed entries + forwarded
        // ocx config + forwarded overrides, in the one order that is correct —
        // see `Env::apply_child_env`. This command materialises packages that
        // may declare entrypoints, so the launcher hop is the ordinary path
        // here, not a corner case.
        let mut process_env = if self.clean {
            env::Env::clean()
        } else {
            reconcile::inherited_env()
        };
        process_env.apply_child_env(
            ChildEnv {
                composed: &entries,
                forwarded: &env_overrides,
            },
            context.config_view(),
        );
        // No PATHEXT manipulation: the Windows launcher is now a native
        // `<name>.exe` shim resolved via the default Windows PATHEXT.

        // Step 7 (script branch): when --script is present, interpret the
        // Starlark script instead of exec'ing a trailing command. Returns
        // Ok(ExitCode) so main.rs::classify_error is bypassed (ADR Exit Code
        // Scheme). The non-script path below is byte-identical to before.
        if let Some(script_path) = &self.script {
            // Identity of the single `<testcase>` this invocation produces:
            // one identifier, one platform, one script (no fan-out).
            let junit_identifier = identifier.to_string();
            let junit_platform = platform.to_string();
            let junit = self.junit.as_ref().map(|path| crate::api::junit::Target {
                path,
                identifier: &junit_identifier,
                platform: &junit_platform,
            });

            // The script host spawns its own children through `ocx.run`, never
            // through `Launch`, so the exemption bound that `Launch::exempt`
            // applies to the trailing-command branch would not reach them.
            // Checked here, before the interpreter starts, so a fail-closed
            // policy refuses the preview outright rather than letting the
            // script run an arbitrary number of unrecorded children.
            launch::exemption_allowed(
                &context.records(ocx_package_manager::record::RecordsOptions::default())?,
                ExemptionReason::PackageTest,
            )?;
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
                        let message = "no script source provided on stdin (--script -)";
                        let fault = anyhow::Error::from(ocx_util::error::FileError::new(
                            std::path::PathBuf::from("<stdin>"),
                            std::io::Error::new(std::io::ErrorKind::UnexpectedEof, message),
                        ));
                        return Err(
                            argv_fault_with_report(junit.as_ref(), ScriptStatus::Io, "io", message, fault).await,
                        );
                    }
                    Ok(_) => (buf, "<stdin>".to_string()),
                    Err(e) => {
                        drop(td_guard);
                        let message = format!("failed reading script source from stdin: {e}");
                        let fault = anyhow::Error::from(ocx_util::error::FileError::new(
                            std::path::PathBuf::from("<stdin>"),
                            std::io::Error::new(e.kind(), message.clone()),
                        ));
                        return Err(
                            argv_fault_with_report(junit.as_ref(), ScriptStatus::Io, "io", &message, fault).await,
                        );
                    }
                }
            } else {
                match tokio::fs::read_to_string(script_path).await {
                    Ok(s) => (s, script_path.display().to_string()),
                    Err(e) => {
                        drop(td_guard);
                        let message = format!("cannot read script '{}': {e}", script_path.display());
                        let fault = anyhow::Error::from(UsageError::new(message.clone()));
                        return Err(argv_fault_with_report(
                            junit.as_ref(),
                            ScriptStatus::Usage,
                            "usage",
                            &message,
                            fault,
                        )
                        .await);
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

            // Delegate the engine run + structured report + exit-code mapping to
            // the shared runner (also used by `ocx patch test`). run_script is
            // SYNC (Evaluator is !Send); the helper invokes it via
            // block_in_place on the multi-thread runtime.
            let exit = crate::command::script_runner::run_script_in_env(
                &context,
                &source,
                &label,
                &dest_path,
                &scratch_root,
                &platform,
                process_env,
                junit.as_ref(),
            )
            .await;

            // Drop the tempdir guard now the engine finished (success or
            // failure) — same lifecycle as the non-script branch. The scratch
            // sibling is not inside the guarded tempdir, so remove it
            // explicitly in the bare (cleanup) case; --keep/--output leave it
            // for post-failure debugging.
            drop(td_guard);
            if cleanup_scratch {
                let _ = tokio::fs::remove_dir_all(&scratch_root).await;
            }

            return exit;
        }

        // Step 7: Resolve command and exec.
        //
        // The non-script branch needs a command. `required_unless_present_any =
        // ["script", "junit"]` plus the `validate_junit` guard at the top of
        // `execute` make an empty `command` here unreachable today — but a
        // `UsageError` rather than `.expect()` removes the dependency on that
        // ordering: a refactor that relaxes or reorders either guard yields a
        // clean exit 64, not a panic (exit 101).
        let Some((command, args)) = self.command.split_first() else {
            return Err(anyhow::Error::from(UsageError::new(
                "a command is required unless --script is given",
            )));
        };

        let resolved = process_env.resolve_test_command(command)?;

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
        // A maintainer preview over a locally materialized, unpublished package:
        // there is no registry identity and the digest is synthetic, so a record
        // would describe something that was never published. The exclusion is
        // declared rather than implicit — see `ExemptionReason`.
        //
        // It is also bounded by the operator's posture: `Launch::exempt` refuses
        // under `required = true`, so the policy is resolved here and handed to
        // it rather than the exemption being taken for granted.
        let policy = context.records(ocx_package_manager::record::RecordsOptions::default())?;
        if td_guard.is_some() {
            // Bare invocation: spawn child, await exit, drop tempdir, propagate.
            let launch = Launch::exempt(process_env, &resolved, args, ExemptionReason::PackageTest, &policy)?;
            let status = launch::spawn_and_wait(launch).await.map_err(anyhow::Error::from)?;

            // Drop the tempdir guard now that the child has exited — this
            // deletes the materialized package directory (success or failure).
            drop(td_guard);

            Ok(child_process::propagate_exit_code(status))
        } else {
            // --keep or --output path: directory persists; use execvp which
            // diverges on Unix (Drop never runs, but that's fine here because
            // td_guard is None).
            let launch = Launch::exempt(process_env, &resolved, args, ExemptionReason::PackageTest, &policy)?;
            Err(anyhow::Error::from(launch::exec(launch).await))
        }
    }
}

/// Writes a JUnit report for a fault that happened BEFORE the engine ran, and
/// returns `fault` — the error the process must exit on — unchanged.
///
/// The two argv faults (`--script` names an unreadable path; `--script -` gets
/// no source) return a typed error and emit no JSON envelope, but the issue
/// asks for a report file on *every* exit path — a CI job that saw the flag and
/// found no artifact cannot tell "ocx never ran" from "the tests all passed".
/// Both faults render as `<error>`, typed by the status they map to
/// (`usage` → 64, `io` → 74) rather than a single made-up token, and carry no
/// location: no Starlark error stands behind them.
///
/// Writing the report is **best effort**: the sidecar is a reporting channel,
/// not the diagnosis. An operator who typed an unreadable `--script` path needs
/// to be told *that*; answering with the *junit* path's write failure instead
/// (exit 74, and a message naming the wrong file) is a strictly worse
/// diagnosis. So a write failure is logged and `fault` still decides the exit
/// code. Returning the fault, rather than a `Result<()>` the caller would `?`,
/// is what makes the inverted precedence unexpressible at the call site.
///
/// No-op when `--junit` is absent.
async fn argv_fault_with_report(
    junit: Option<&crate::api::junit::Target<'_>>,
    status: ScriptStatus,
    kind: &str,
    message: &str,
    fault: anyhow::Error,
) -> anyhow::Error {
    let Some(junit) = junit else { return fault };
    let report = ScriptRunReport::new(
        status,
        Some(AssertionRecord {
            kind: kind.to_string(),
            message: message.to_string(),
            location: None,
        }),
        None,
    );
    if let Err(error) = crate::api::junit::write(junit, &report).await {
        log::warn!("could not write the JUnit report to {}: {error}", junit.path.display());
    }
    fault
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
        anyhow::Error::from(ocx_util::error::FileError::new(&scratch, e))
    })?;
    Ok(scratch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// `--junit` with neither `--script` nor a trailing command parses (clap
    /// waives the command positional because `--junit` conflicts with it), then
    /// `validate_junit` refuses it with a usage error naming both flags. The
    /// old `requires = "script"` produced an unsatisfiable "provide both
    /// --script and <COMMAND>" instead (H6).
    #[test]
    fn junit_without_script_is_a_usage_error_naming_both_flags() {
        let cmd = PackageTest::try_parse_from(["package-test", "-i", "example:1", "--junit", "out.xml"])
            .expect("`--junit` alone must parse — the command positional is waived by the conflict");
        let err = cmd
            .validate_junit()
            .expect_err("`--junit` without `--script` must be a usage error");
        let message = err.to_string();
        assert!(
            message.contains("--junit") && message.contains("--script"),
            "the usage error must name both flags: {message}"
        );
    }

    /// `--junit` beside a trailing command is caught by clap's `conflicts_with`
    /// at parse time — the check `validate_junit` cannot see it because the
    /// parse never succeeds.
    #[test]
    fn junit_with_a_trailing_command_is_a_clap_conflict() {
        // `PackageTest` has no `Debug`, so match rather than `expect_err`.
        let err = match PackageTest::try_parse_from([
            "package-test",
            "-i",
            "example:1",
            "--junit",
            "out.xml",
            "--",
            "echo",
        ]) {
            Ok(_) => panic!("`--junit` with a trailing command must fail to parse"),
            Err(err) => err,
        };
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "must be the conflict specifically, not an unrelated parse error: {err}"
        );
    }

    /// `--junit` with `--script` is the supported form: it parses and passes
    /// validation.
    #[test]
    fn junit_with_script_is_accepted() {
        let cmd = PackageTest::try_parse_from([
            "package-test",
            "-i",
            "example:1",
            "--script",
            "s.star",
            "--junit",
            "out.xml",
        ])
        .expect("`--junit --script` must parse");
        assert!(cmd.validate_junit().is_ok(), "`--junit --script` must validate");
    }

    /// An unwritable `--junit` path must not displace the argv fault that got
    /// us here. The operator typed a bad `--script` path; answering with the
    /// *junit* path's write failure hides the only message that names the
    /// script, and turns a usage error (64) into an I/O error (74).
    ///
    /// The unwritable target is a path *under a regular file*, so
    /// `create_dir_all` on the report's parent fails with ENOTDIR — the same
    /// failure `test_exit_codes.py::_package_test_junit` provokes, and the one
    /// that used to reach the caller through `?`.
    ///
    /// Mutation: make [`argv_fault_with_report`] return the write error when
    /// one occurs instead of `fault`. This reds — the returned message then
    /// names the junit path, not `smoke.star`.
    #[tokio::test]
    async fn a_junit_write_failure_does_not_displace_the_argv_fault() {
        let dir = tempfile::tempdir().expect("tempdir");
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").expect("write blocker file");
        let unwritable = blocker.join("junit.xml");

        let target = crate::api::junit::Target {
            path: &unwritable,
            identifier: "example:1",
            platform: "linux/amd64",
        };
        let fault = anyhow::Error::from(UsageError::new("cannot read script 'smoke.star'".to_string()));

        let returned = argv_fault_with_report(
            Some(&target),
            ScriptStatus::Usage,
            "usage",
            "cannot read script 'smoke.star'",
            fault,
        )
        .await;

        assert!(
            !unwritable.exists(),
            "the precondition is a report that could NOT be written; if it exists this test proves nothing"
        );
        assert!(
            returned.to_string().contains("smoke.star"),
            "the argv fault must survive the failed report write, got: {returned}"
        );
        assert!(
            returned.downcast_ref::<UsageError>().is_some(),
            "the fault must stay a UsageError (exit 64), not become the report's I/O error (74), got: {returned}"
        );
    }

    /// The same helper with no `--junit` is a pure pass-through — the fault is
    /// returned untouched and nothing is written.
    #[tokio::test]
    async fn without_junit_the_fault_passes_through_untouched() {
        let fault = anyhow::Error::from(UsageError::new("cannot read script 'smoke.star'".to_string()));
        let returned = argv_fault_with_report(
            None,
            ScriptStatus::Usage,
            "usage",
            "cannot read script 'smoke.star'",
            fault,
        )
        .await;
        assert!(
            returned.downcast_ref::<UsageError>().is_some() && returned.to_string().contains("smoke.star"),
            "got: {returned}"
        );
    }
}
