// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus, Stdio};
use std::str::FromStr;

use clap::Parser;
use ocx_lib::cli::UsageError;
use ocx_lib::package::install_info::InstallInfo;
use ocx_lib::package::metadata::env::exporter::Entry as EnvEntry;
use ocx_lib::package_manager::PackageRef;
use ocx_lib::{env, oci};
use tokio::process::Command;

use crate::conventions::*;

/// Runs installed packages.
///
/// Each positional accepts an OCI identifier (`node:20`, `oci://node:20`) or a
/// `file://<absolute-package-root>` URI pointing at an already-installed
/// package directory. Identifier refs may be auto-installed; `file://` refs
/// must already exist on disk under `$OCX_HOME/packages/`.
///
/// **Contract stability**: the `file://` URI scheme + `ocx exec` argument
/// shape are load-bearing for all generated launchers. Changing either
/// invalidates every installed launcher; keep both stable across releases.
#[derive(Parser)]
pub struct Exec {
    /// Run in interactive mode, which will keep the environment variables set after the command finishes.
    ///
    /// This is useful for shells that support it, such as PowerShell and Elvish. For other shells, this flag will be ignored.
    #[clap(short = 'i', long = "interactive", default_value_t = false)]
    interactive: bool,

    /// Start with a clean environment containing only the package variables, instead of inheriting the current shell environment.
    #[clap(long = "clean", default_value_t = false)]
    clean: bool,

    /// Target platforms to consider when resolving packages. If not specified, only supported platforms will be considered.
    #[clap(short = 'p', long = "platform", value_delimiter = ',', value_name = "PLATFORM", num_args = 0..)]
    platforms: Vec<oci::Platform>,

    /// Package references to layer environment from.
    ///
    /// Each value is one of:
    /// - `<identifier>` (e.g. `node:20`) — bare OCI identifier, equivalent to
    ///   the explicit `oci://` form below.
    /// - `oci://<identifier>` — explicit OCI scheme, identifier is resolved
    ///   through the index and auto-installed when missing.
    /// - `file://<abs-package-root>` — absolute path to an already-installed
    ///   package root (e.g. `file:///home/u/.ocx/packages/...`). Skips
    ///   identifier resolution and the index entirely; used by generated
    ///   entry-point launchers.
    // Parsed inside `execute()` rather than via clap's `value_parser` so that
    // a malformed ref surfaces as `ExitCode::UsageError` (64) through our own
    // [`UsageError`] classification, not clap's universal exit-code 2.
    #[clap(required = true, num_args = 1.., value_terminator = "--", value_name = "REF")]
    refs: Vec<String>,

    /// Command to execute, with arguments. The command will be executed with the environment with the packages.
    #[clap(allow_hyphen_values = true, num_args = 1..)]
    command: Vec<String>,
}

impl Exec {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let manager = context.manager();
        let packages_root = context.file_structure().packages.root();
        let default_registry = context.default_registry();

        // Resolve refs to install infos in input order. `file://` refs short-circuit
        // to the on-disk package root; `oci://` and bare refs collect for a single
        // batched `find_or_install_all` call so OCI parallelism is preserved. Env
        // entries layer in the order refs appear on the command line.
        let mut ordered: Vec<Option<InstallInfo>> = (0..self.refs.len()).map(|_| None).collect();
        let mut oci_identifiers: Vec<oci::Identifier> = Vec::new();
        let mut oci_indexes: Vec<usize> = Vec::new();
        for (idx, raw) in self.refs.iter().enumerate() {
            let ref_ = PackageRef::from_str(raw).map_err(|e| UsageError::new(e.to_string()))?;
            match ref_ {
                PackageRef::Oci(_) => {
                    let id = ref_
                        .into_identifier(default_registry)?
                        .expect("Oci variant must produce Some identifier");
                    oci_identifiers.push(id);
                    oci_indexes.push(idx);
                }
                PackageRef::PackageRoot(path) => {
                    let validated = validate_package_root(&path, packages_root).await?;
                    ordered[idx] = Some(manager.install_info_from_package_root(&validated).await?);
                }
            }
        }

        if !oci_identifiers.is_empty() {
            let platforms = platforms_or_default(&self.platforms);
            let infos = manager.find_or_install_all(oci_identifiers, platforms).await?;
            for (idx, info) in oci_indexes.into_iter().zip(infos) {
                ordered[idx] = Some(info);
            }
        }

        let install_infos: Vec<std::sync::Arc<InstallInfo>> =
            ordered.into_iter().flatten().map(std::sync::Arc::new).collect();
        let entries = manager.resolve_env(&install_infos).await?;
        self.run_with_env(entries).await
    }

    /// Spawn the configured command with the given resolved environment and
    /// translate the child's exit status back into a process exit code.
    async fn run_with_env(&self, entries: Vec<EnvEntry>) -> anyhow::Result<ExitCode> {
        let mut process_env = if self.clean { env::Env::clean() } else { env::Env::new() };
        process_env.apply_entries(&entries);

        // On Windows, ensure `.CMD` is listed in PATHEXT so that the generated
        // `.cmd` entry-point launchers are found by the shell resolver. We
        // control the child environment here, so we inject silently — no
        // warning is appropriate for `exec`.
        #[cfg(target_os = "windows")]
        {
            let current_pathext = process_env
                .get("PATHEXT")
                .and_then(|v| v.to_str())
                .unwrap_or("")
                .to_string();
            if let Some(new_pathext) = pathext_for_child(&current_pathext) {
                process_env.set("PATHEXT", new_pathext);
            }
        }

        let Some((command, args)) = self.command.split_first() else {
            return Err(anyhow::anyhow!("No command provided to execute."));
        };

        let resolved = process_env.resolve_command(command);

        let mut child_process = Command::new(&resolved)
            .args(args)
            .stdin(if self.interactive {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .stderr(Stdio::inherit())
            .stdout(Stdio::inherit())
            .envs(process_env)
            .spawn()?;

        let status = child_process.wait().await?;
        Ok(child_exit_to_exit_code(status))
    }
}

/// Returns the `PATHEXT` value that should be set on the child env when
/// launching a binary, or `None` if the inherited value already lists the
/// `.cmd` launcher extension (case-insensitive). On Windows-only.
#[cfg(target_os = "windows")]
fn pathext_for_child(current: &str) -> Option<String> {
    if env::pathext_includes_launcher(current) {
        return None;
    }
    Some(if current.is_empty() {
        ".CMD;.EXE;.BAT;.COM".to_string()
    } else {
        format!(".CMD;{current}")
    })
}

/// Validate a `file://` package root: must be an absolute path that
/// canonicalizes to a location *inside* the OCX packages CAS root and contains
/// a `metadata.json` file. Returns the canonical path on success.
///
/// Both checks surface as [`UsageError`] so [`ocx_lib::cli::classify_error`]
/// maps the failure to [`ExitCode::UsageError`] (`64`) rather than the
/// default [`ExitCode::Failure`] (`1`).
async fn validate_package_root(dir: &Path, packages_root: &Path) -> Result<PathBuf, UsageError> {
    // Canonicalize both sides so symlinks and `..` components cannot smuggle
    // a path outside the packages root. `tokio::fs::canonicalize` keeps us in
    // the async runtime instead of blocking via `std::fs`.
    let canonical_dir = tokio::fs::canonicalize(dir)
        .await
        .map_err(|e| UsageError::new(format!("file:// path cannot be resolved ({}): {}", e, dir.display())))?;
    let canonical_root = tokio::fs::canonicalize(packages_root).await.map_err(|e| {
        UsageError::new(format!(
            "file:// validation failed: cannot resolve packages root ({}): {}",
            e,
            packages_root.display()
        ))
    })?;

    if !canonical_dir.starts_with(&canonical_root) {
        return Err(UsageError::new(format!(
            "file:// path must point inside {} (got {})",
            canonical_root.display(),
            canonical_dir.display()
        )));
    }

    // Quick existence check on metadata.json — the canonical signal that
    // `dir` is actually a package root and not, say, a registry slug dir.
    let metadata = canonical_dir.join("metadata.json");
    if !tokio::fs::try_exists(&metadata).await.unwrap_or(false) {
        return Err(UsageError::new(format!(
            "file:// path is not a package root (missing metadata.json): {}",
            canonical_dir.display()
        )));
    }

    Ok(canonical_dir)
}

/// Translate a child process [`ExitStatus`] into a typed [`ExitCode`].
///
/// Thin wrapper around [`exit_code_from_raw`] that pulls `code()` off the
/// status. Kept separate so the platform-independent mapping logic can be
/// unit-tested without constructing synthetic `ExitStatus` values.
fn child_exit_to_exit_code(status: ExitStatus) -> ExitCode {
    exit_code_from_raw(status.code())
}

/// Map a raw child exit code (`Option<i32>` as returned by
/// [`std::process::ExitStatus::code`]) to a typed [`ExitCode`].
///
/// `ExitCode` is a `u8`, so a child returning `>255` (e.g. Windows code
/// `300`) cannot be represented losslessly. Saturate to `u8::MAX` (`255`)
/// for any out-of-range positive code; map `None` (signal-killed on Unix)
/// or a negative code to [`ExitCode::FAILURE`].
///
/// Trade-off: scripts comparing on `==255` vs `==300` will see both as
/// `255`, but losing the precise number is preferable to the previous
/// `code as u8` truncation (300 → 44) which silently produced
/// shell-confusing values like `0` for any multiple of 256.
fn exit_code_from_raw(code: Option<i32>) -> ExitCode {
    match code {
        Some(code) if code >= 0 => ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX)),
        // Negative codes do not occur via `ExitStatus::code()` on either
        // supported platform; signal-killed (`code()` returns None on Unix)
        // maps here too.
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ExitCode` does not implement `PartialEq`, so we compare via its
    /// `Debug` output. `Debug` includes the underlying `u8` so the assertion
    /// is precise.
    fn assert_exit_code_eq(actual: ExitCode, expected: ExitCode) {
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    }

    #[test]
    fn child_exit_zero_is_success() {
        assert_exit_code_eq(exit_code_from_raw(Some(0)), ExitCode::from(0u8));
    }

    #[test]
    fn child_exit_typical_failure_propagated() {
        assert_exit_code_eq(exit_code_from_raw(Some(42)), ExitCode::from(42u8));
    }

    #[test]
    fn child_exit_above_255_saturates() {
        assert_exit_code_eq(exit_code_from_raw(Some(300)), ExitCode::from(255u8));
    }

    #[test]
    fn child_exit_negative_code_is_failure() {
        assert_exit_code_eq(exit_code_from_raw(Some(-1)), ExitCode::FAILURE);
    }

    #[test]
    fn child_exit_no_code_is_failure() {
        assert_exit_code_eq(exit_code_from_raw(None), ExitCode::FAILURE);
    }

    // ── validate_package_root ─────────────────────────────────────────────

    #[tokio::test]
    async fn validate_rejects_outside_packages_root() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let outside = tempfile::tempdir().expect("outside");
        let err = validate_package_root(outside.path(), packages_root.path())
            .await
            .expect_err("outside packages root must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("file://"), "msg should name scheme: {msg}");
    }

    #[tokio::test]
    async fn validate_rejects_inside_root_without_metadata_json() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let inside = packages_root.path().join("registry/algo/aa/rest");
        tokio::fs::create_dir_all(&inside).await.expect("mkdir");
        let err = validate_package_root(&inside, packages_root.path())
            .await
            .expect_err("missing metadata.json must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("metadata.json"), "msg should name metadata.json: {msg}");
    }

    #[tokio::test]
    async fn validate_accepts_path_under_packages_root_with_metadata() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let pkg_root = packages_root.path().join("registry/algo/aa/rest");
        tokio::fs::create_dir_all(&pkg_root).await.expect("mkdir");
        tokio::fs::write(pkg_root.join("metadata.json"), b"{}")
            .await
            .expect("write metadata");
        let canonical = validate_package_root(&pkg_root, packages_root.path())
            .await
            .expect("inside path with metadata.json should be accepted");
        assert!(canonical.starts_with(packages_root.path().canonicalize().expect("canon root")));
    }

    #[tokio::test]
    async fn validate_rejects_nonexistent_path_with_usage_error() {
        let packages_root = tempfile::tempdir().expect("packages root");
        let missing = packages_root.path().join("does-not-exist");
        let err = validate_package_root(&missing, packages_root.path())
            .await
            .expect_err("missing dir must be rejected");
        assert!(err.to_string().contains("file://"));
    }

    // ── pathext_for_child (Windows only) ─────────────────────────────────

    #[cfg(target_os = "windows")]
    mod pathext_for_child_tests {
        use super::super::pathext_for_child;

        #[test]
        fn returns_none_when_already_includes_cmd() {
            assert_eq!(pathext_for_child(".EXE;.CMD;.BAT"), None);
        }

        #[test]
        fn returns_none_for_lowercase_cmd() {
            assert_eq!(pathext_for_child(".exe;.cmd;.bat"), None);
        }

        #[test]
        fn prepends_cmd_to_existing_pathext() {
            assert_eq!(pathext_for_child(".EXE;.BAT").as_deref(), Some(".CMD;.EXE;.BAT"));
        }

        #[test]
        fn supplies_default_when_pathext_empty() {
            assert_eq!(pathext_for_child("").as_deref(), Some(".CMD;.EXE;.BAT;.COM"));
        }

        #[test]
        fn does_not_double_prepend_when_partial_match_present() {
            // ".cmdextra" is not ".cmd"; injection should still happen.
            assert_eq!(
                pathext_for_child(".cmdextra;.EXE").as_deref(),
                Some(".CMD;.cmdextra;.EXE")
            );
        }
    }
}
