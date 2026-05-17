// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Hidden `ocx launcher exec` subcommand — stable entry-point from generated launchers.
//!
//! Generated launcher scripts call:
//!   `ocx launcher exec '<pkg-root>' -- "$(basename "$0")" "$@"`
//!
//! This subcommand is the sole path from an installed launcher into the OCX runtime.
//! It hides all presentation flags, self-view selection, and binary pinning behind
//! the stable `launcher exec` name pair, reducing the launcher ABI surface from
//! 8 wire commitments to 2 (the `launcher` + `exec` subcommand names and positional shape).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use ocx_lib::cli::UsageError;
use ocx_lib::package::metadata::env::entry::Entry as EnvEntry;
use ocx_lib::package_manager::launcher;
use ocx_lib::utility::child_process;
use ocx_lib::{env, env::OcxConfigView};

/// Entry point from generated launchers. Validates the package root, then
/// executes the resolved entrypoint with forced self-view and silent presentation.
#[derive(Parser)]
pub struct LauncherExec {
    /// Absolute path to the installed package root (the directory containing
    /// `metadata.json`). Baked into the launcher at install time.
    pkg_root: PathBuf,

    /// The launcher's own filename (argv0 passed after `--`), used to
    /// identify which entrypoint to dispatch.
    #[clap(last = true, required = true, num_args = 1..)]
    argv: Vec<String>,
}

impl LauncherExec {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        let fs = context.file_structure();
        let packages_root = fs.packages.root();
        // Also allow launchers materialised under the package-test temp root
        // ($OCX_HOME/temp/test/) — `ocx package test` places packages there and
        // the launchers bake that path as their pkg-root.
        let temp_test_root = fs.temp.root().join("test");
        let manager = context.manager();

        // Validate: pkg_root must be absolute, under $OCX_HOME/packages/ OR
        // $OCX_HOME/temp/test/ (package-test materialization path), and contain
        // metadata.json. Errors surface as UsageError (exit 64).
        let validated = validate_launcher_pkg_root(&self.pkg_root, packages_root, Some(&temp_test_root)).await?;

        // Resolve env with self_view=true — the launcher always runs in the
        // package's own env (public + private surface). This is equivalent to
        // the former `--self` flag that was baked into every launcher template.
        let info = manager.install_info_from_package_root(&validated).await?;
        let entries = manager.resolve_env(&[std::sync::Arc::new(info)], true).await?;

        // argv[0] is the launcher's own filename — the entrypoint name.
        // argv[1..] are the user args.
        let (argv0, args) = self
            .argv
            .split_first()
            .expect("clap required=true guarantees at least one argv element");

        self.run_with_env(entries, args, argv0, context.config_view()).await
    }

    /// Run the resolved entrypoint with the given env.
    ///
    /// Presentation flags are forced here (not baked in the launcher template):
    /// - log_level=off, color=never, format=plain were previously baked into the
    ///   launcher script; now they are applied on the *inner* ocx invocation from
    ///   within this subcommand (i.e. if this subcommand itself spawns child ocx,
    ///   which it does not — it execs the entrypoint binary directly).
    ///
    /// `child_process::exec` diverges on success on every platform — Unix
    /// `execvp(2)`s, Windows spawns + waits + `process::exit`s — so this
    /// function only returns when start-up itself fails.
    async fn run_with_env(
        &self,
        entries: Vec<EnvEntry>,
        args: &[String],
        command: &str,
        config_view: &OcxConfigView,
    ) -> anyhow::Result<ExitCode> {
        let mut process_env = env::Env::new();
        process_env.apply_entries(&entries);
        // Forward resolution-affecting OCX config to any grandchild ocx processes.
        process_env.apply_ocx_config(config_view);
        // Ensure the child PATHEXT lists the OCX launcher extension on Windows.
        launcher::emplace_pathext(&mut process_env);

        let resolved = process_env.resolve_command(command);

        let err = child_process::exec(&resolved, args, process_env);
        Err(anyhow::Error::from(err).context(format!("failed to run '{}'", resolved.display())))
    }
}

/// Validate a package root path for use from a launcher.
///
/// The path must:
/// - Be absolute
/// - Canonicalize to a location inside `packages_root` OR `extra_root` (when `Some`)
/// - Contain `metadata.json`
///
/// `extra_root` is supplied for the `package test` materialization path
/// (`$OCX_HOME/temp/test/`): launchers baked into a test-materialized package
/// carry the temp path as their pkg-root, which is equally OCX-controlled and
/// equally safe to allow.
///
/// This mirrors the former `validate_package_root` from `options/package_ref.rs`,
/// now inlined here (its only remaining caller) with error messages updated to
/// reference `launcher exec` instead of `file://`.
async fn validate_launcher_pkg_root(
    dir: &std::path::Path,
    packages_root: &std::path::Path,
    extra_root: Option<&std::path::Path>,
) -> Result<PathBuf, UsageError> {
    if !dir.is_absolute() {
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root must be absolute, got '{}'",
            dir.display()
        )));
    }

    // Canonicalize both sides so symlinks and `..` components cannot smuggle
    // a path outside the allowed roots.
    let canonical_dir = tokio::fs::canonicalize(dir).await.map_err(|e| {
        UsageError::new(format!(
            "launcher exec: pkg-root '{}' cannot be resolved: {e}",
            dir.display()
        ))
    })?;

    // Use `.ok()` for packages_root so that a non-existent store (fresh
    // OCX_HOME with no packages/ dir yet, as in `ocx package test` on a clean
    // host) does not hard-fail here. When packages_root is absent it simply
    // cannot match as a prefix — the extra_root check below covers the
    // package-test case. Security boundary unchanged: an absent root matches
    // nothing.
    let canonical_root = tokio::fs::canonicalize(packages_root).await.ok();

    // Canonicalize extra_root when present; `.ok()` so that a non-existent
    // extra root (temp/test/ created lazily) simply yields None and cannot
    // match as a prefix of canonical_dir.
    let canonical_extra = if let Some(extra) = extra_root {
        tokio::fs::canonicalize(extra).await.ok()
    } else {
        None
    };

    let under_packages = canonical_root.as_ref().is_some_and(|r| canonical_dir.starts_with(r));
    let under_extra = canonical_extra.as_ref().is_some_and(|r| canonical_dir.starts_with(r));

    if !under_packages && !under_extra {
        // Build a display path for the error. When packages_root does not
        // exist yet (fresh OCX_HOME), fall back to the raw path so the error
        // message still names a useful location.
        let root_display = canonical_root.as_deref().unwrap_or(packages_root).display();
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root must point inside {} (got {})",
            root_display,
            canonical_dir.display()
        )));
    }

    // Existence check on metadata.json — canonical signal that this is a package root.
    let metadata = canonical_dir.join("metadata.json");
    if !tokio::fs::try_exists(&metadata).await.unwrap_or(false) {
        return Err(UsageError::new(format!(
            "launcher exec: pkg-root is not a package root (missing metadata.json): {}",
            canonical_dir.display()
        )));
    }

    Ok(canonical_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a directory tree `base/subdir/` with a `metadata.json`
    /// inside `subdir/` and return `(base, subdir)`.
    fn make_pkg_tree(tmp: &std::path::Path, base: &str, pkg: &str) -> (PathBuf, PathBuf) {
        let base_dir = tmp.join(base);
        let pkg_dir = base_dir.join(pkg);
        std::fs::create_dir_all(&pkg_dir).unwrap();
        std::fs::write(pkg_dir.join("metadata.json"), b"{}").unwrap();
        (base_dir, pkg_dir)
    }

    // ── validate_launcher_pkg_root — key contract rows ───────────────────────

    /// Pkg root inside packages_root → accepted (normal install path).
    #[tokio::test]
    async fn accepts_path_under_packages_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (packages_root, pkg_dir) = make_pkg_tree(tmp.path(), "packages", "abc123");
        let result = validate_launcher_pkg_root(&pkg_dir, &packages_root, None).await;
        assert!(result.is_ok(), "expected Ok; got {result:?}");
    }

    /// Pkg root inside extra_root (temp/test/) → accepted (package-test path).
    #[tokio::test]
    async fn accepts_path_under_extra_root() {
        let tmp = tempfile::tempdir().unwrap();
        let (temp_test_root, pkg_dir) = make_pkg_tree(tmp.path(), "temp/test", "test-XXXXX");
        // packages_root is a separate sibling that does NOT contain pkg_dir.
        let packages_root = tmp.path().join("packages");
        std::fs::create_dir_all(&packages_root).unwrap();
        let result = validate_launcher_pkg_root(&pkg_dir, &packages_root, Some(&temp_test_root)).await;
        assert!(result.is_ok(), "expected Ok for temp/test path; got {result:?}");
    }

    /// Regression: pkg root inside extra_root when packages_root does NOT EXIST
    /// (fresh OCX_HOME, no packages ever installed). This was the actual bug:
    /// `canonicalize(packages_root)` hard-failed with ENOENT before the
    /// extra_root check was reached, causing exit 64 on every `ocx package test`
    /// invocation on a clean host for packages with entrypoint launchers.
    #[tokio::test]
    async fn accepts_path_under_extra_root_when_packages_root_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let (temp_test_root, pkg_dir) = make_pkg_tree(tmp.path(), "temp/test", "test-ABCDE");
        // packages_root intentionally NOT created — simulates fresh OCX_HOME
        // where no `ocx install` has ever been run.
        let packages_root = tmp.path().join("packages");
        assert!(!packages_root.exists(), "test setup: packages_root must be absent");
        let result = validate_launcher_pkg_root(&pkg_dir, &packages_root, Some(&temp_test_root)).await;
        assert!(
            result.is_ok(),
            "expected Ok for temp/test path with absent packages_root; got {result:?}"
        );
    }

    /// Security boundary: path outside both allowed roots is rejected even when
    /// packages_root does not exist (absent packages_root must not widen the
    /// accepted set to "anything").
    #[tokio::test]
    async fn rejects_outside_path_when_packages_root_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let temp_test_root = tmp.path().join("temp/test");
        std::fs::create_dir_all(&temp_test_root).unwrap();
        // packages_root intentionally absent.
        let packages_root = tmp.path().join("packages");

        let outsider_dir = tmp.path().join("outsider/pkg");
        std::fs::create_dir_all(&outsider_dir).unwrap();
        std::fs::write(outsider_dir.join("metadata.json"), b"{}").unwrap();

        let result = validate_launcher_pkg_root(&outsider_dir, &packages_root, Some(&temp_test_root)).await;
        assert!(
            result.is_err(),
            "expected Err for outsider with absent packages_root; got Ok"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("pkg-root must point inside"),
            "unexpected error message: {msg}"
        );
    }

    /// Pkg root outside both allowed roots → rejected.
    #[tokio::test]
    async fn rejects_path_outside_both_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let packages_root = tmp.path().join("packages");
        std::fs::create_dir_all(&packages_root).unwrap();
        let temp_test_root = tmp.path().join("temp/test");
        std::fs::create_dir_all(&temp_test_root).unwrap();

        // outsider is a sibling of both allowed roots.
        let outsider_dir = tmp.path().join("outsider/pkg");
        std::fs::create_dir_all(&outsider_dir).unwrap();
        std::fs::write(outsider_dir.join("metadata.json"), b"{}").unwrap();

        let result = validate_launcher_pkg_root(&outsider_dir, &packages_root, Some(&temp_test_root)).await;
        assert!(result.is_err(), "expected Err for outsider path; got Ok");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("pkg-root must point inside"),
            "unexpected error message: {msg}"
        );
    }

    /// Non-absolute path → rejected with appropriate message.
    #[tokio::test]
    async fn rejects_relative_path() {
        let result = validate_launcher_pkg_root(
            std::path::Path::new("relative/path"),
            std::path::Path::new("/packages"),
            None,
        )
        .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be absolute"));
    }

    /// Path pointing to a directory that exists but has no metadata.json → rejected.
    #[tokio::test]
    async fn rejects_missing_metadata_json() {
        let tmp = tempfile::tempdir().unwrap();
        let packages_root = tmp.path().join("packages");
        let pkg_dir = packages_root.join("no-meta");
        std::fs::create_dir_all(&pkg_dir).unwrap();
        // metadata.json intentionally absent.
        let result = validate_launcher_pkg_root(&pkg_dir, &packages_root, None).await;
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("missing metadata.json"),
            "wrong rejection reason"
        );
    }
}
