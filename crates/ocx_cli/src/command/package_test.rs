// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use ocx_lib::utility::child_process;
use ocx_lib::utility::fs as ocx_fs;
use ocx_lib::{cli::UsageError, env, oci, package, prelude::*, publisher::LayerRef};

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

    /// Target platform (e.g. `linux/amd64`). Required - parity with `package push`.
    #[clap(short, long, required = true)]
    platform: oci::Platform,

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

    /// Command to execute inside the composed env, with arguments. Required.
    ///
    /// `last = true` (mirroring `run.rs`'s `argv`) makes clap parse everything
    /// before the mandatory `--` into `layers` and everything after into
    /// `command`. Without it, `command` is an ordinary required positional sitting
    /// after the optional `layers` (index 1), which trips clap's debug-assert
    /// "non-required positional with a lower index than a required positional" -
    /// fatal in debug builds when the command tree is built (e.g. completion
    /// generation). Requires clap >= 4.5.57 (see `run.rs` NOTE).
    #[clap(allow_hyphen_values = true, last = true, required = true, num_args = 1..)]
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
        // No PATHEXT manipulation: the Windows launcher is now a native
        // `<name>.exe` shim resolved via the default Windows PATHEXT.

        // Step 7: Resolve command and exec.
        let (command, args) = self
            .command
            .split_first()
            .expect("clap required=true guarantees at least one command element");

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
