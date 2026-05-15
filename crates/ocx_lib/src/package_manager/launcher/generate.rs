// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Launcher generator entry point.
//!
//! Generates Unix `.sh` and Windows `.cmd` launcher scripts at install time.
//! Each launcher calls `ocx launcher exec '<baked-package-root>' -- "$@"`,
//! preserving OCX's clean-env execution guarantee. The baked path carries the
//! absolute package-root; `ocx launcher exec` reads `metadata.json` from that
//! root and resolves `argv0` against the composed `PATH` from the package's
//! `env` block at invocation time. Presentation flags and self-view selection
//! are hidden inside the `launcher exec` subcommand — they are no longer baked
//! into the launcher.

use std::path::{Path, PathBuf};

use tokio::task::JoinSet;

use crate::package::metadata::entrypoint::Entrypoints;

use super::body::{unix_launcher_body, windows_launcher_body};
use super::safety::LauncherSafeString;

/// Generates Unix and Windows launchers for all declared entrypoints.
///
/// Writes `<dest>/<name>` (Unix, chmod 0755) and `<dest>/<name>.cmd` (Windows)
/// for each entry in `entries`. If `entries` is empty, no files are written
/// and `dest` is not created.
///
/// `pkg_root` is the absolute package-root directory
/// (`packages/<registry>/<algo>/<2hex>/<30hex>/`). The launcher bakes
/// `<pkg_root>` and forwards `argv0` plus the user's args to
/// `ocx launcher exec`, which reads `metadata.json` from that root and
/// resolves `argv0` against the composed `PATH` at invocation time.
///
/// # Errors
///
/// Returns an error if:
/// - Creating the `dest` directory fails.
/// - Writing any launcher file fails.
/// - The `pkg_root` contains a character unsafe for either launcher
///   template (see [`LauncherSafeString`]).
pub async fn generate(pkg_root: &Path, entries: &Entrypoints, dest: &Path) -> Result<(), crate::Error> {
    if entries.is_empty() {
        return Ok(());
    }

    let pkg_root_str = LauncherSafeString::new(pkg_root.to_string_lossy().into_owned())?;

    tokio::fs::create_dir_all(dest)
        .await
        .map_err(|e| crate::error::file_error(dest, e))?;

    // Spawn one task per launcher file (Unix + Windows for every entry).
    // Each task is independent — there is no ordering constraint between
    // entries or between platforms — so concurrent writes amortise per-file
    // syscall latency over a large `entrypoints/` directory.
    let mut tasks: JoinSet<Result<(), crate::Error>> = JoinSet::new();

    for (name, _entry) in entries.iter() {
        let name = name.as_str();

        // Unix launcher: write body, then set the executable bit.
        let unix_body = unix_launcher_body(&pkg_root_str);
        let unix_path = dest.join(name);
        tasks.spawn(write_unix_launcher(unix_path, unix_body));

        // Windows launcher.
        let windows_body = windows_launcher_body(&pkg_root_str);
        let windows_path = dest.join(format!("{name}.cmd"));
        tasks.spawn(write_launcher_file(windows_path, windows_body));
    }

    // Drain results, surfacing the first error. Aborting the rest on failure
    // matches the previous serial behaviour: a single failed write halts the
    // generator and the partial `entrypoints/` directory is left for the
    // caller's atomic-move / cleanup contract.
    while let Some(join_result) = tasks.join_next().await {
        match join_result.expect(
            "BUG: write_unix_launcher / write_launcher_file are panic-free — only the JoinSet host task can fault here",
        ) {
            Ok(()) => {}
            Err(err) => {
                tasks.abort_all();
                return Err(err);
            }
        }
    }

    Ok(())
}

/// Writes a launcher file at `path` with the supplied body.
async fn write_launcher_file(path: PathBuf, body: String) -> Result<(), crate::Error> {
    tokio::fs::write(&path, body.as_bytes())
        .await
        .map_err(|e| crate::error::file_error(&path, e))
}

/// Writes the Unix launcher and sets its executable bit (no-op off Unix).
async fn write_unix_launcher(path: PathBuf, body: String) -> Result<(), crate::Error> {
    write_launcher_file(path.clone(), body).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(|e| crate::error::file_error(&path, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use std::collections::BTreeMap;

    use crate::package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};

    fn entries_for(names: &[&str]) -> Entrypoints {
        let map: BTreeMap<EntrypointName, Entrypoint> = names
            .iter()
            .map(|n| (EntrypointName::try_from(*n).unwrap(), Entrypoint::default()))
            .collect();
        Entrypoints::new(map)
    }

    // ── Empty Entrypoints — no files generated ────────────────────────────

    #[tokio::test]
    async fn generate_empty_entrypoints_creates_nothing() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&[]);

        super::generate(&pkg_root, &entrypoints, &dest).await.unwrap();
        assert!(
            !dest.exists(),
            "entrypoints/ dir must not be created for empty entrypoints"
        );
    }

    // ── Generator writes files for non-empty entrypoints ──────────────────

    #[tokio::test]
    async fn generate_writes_unix_and_windows_launchers() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);

        super::generate(&pkg_root, &entrypoints, &dest).await.unwrap();
        assert!(dest.join("cmake").exists(), "unix launcher must be created");
        assert!(dest.join("cmake.cmd").exists(), "windows launcher must be created");
    }

    // ── Unix file mode 0755 ───────────────────────────────────────────────

    #[cfg(unix)]
    #[tokio::test]
    async fn generate_sets_unix_mode_0755() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);

        super::generate(&pkg_root, &entrypoints, &dest).await.unwrap();
        let mode = std::fs::metadata(dest.join("cmake")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "unix launcher must be mode 0755");
    }

    // ── Path traversal defense-in-depth (EntrypointName) ─────────────────

    #[test]
    fn entrypoint_name_rejects_path_traversal_bytes() {
        // EntrypointName construction must reject names with / or \ or ..
        // (slug regex already excludes / and \; .. is caught by the regex too)
        assert!(EntrypointName::try_from("../evil").is_err());
        assert!(EntrypointName::try_from("sub/path").is_err());
        assert!(EntrypointName::try_from("back\\slash").is_err());
    }

    // ── Idempotent re-run (generating same entrypoints twice) ─────────────

    #[tokio::test]
    async fn generate_is_idempotent() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);

        // First call
        let _ = super::generate(&pkg_root, &entrypoints, &dest).await;
        // Second call — must not error due to already-existing files
        let _ = super::generate(&pkg_root, &entrypoints, &dest).await;
        // Both calls should return Ok(()).
    }

    // ── W1: pkg_root rejection on launcher-unsafe characters ──────────────
    //
    // `generate()` validates `pkg_root.to_string_lossy()` against
    // `LauncherSafeString::new` before any file is written. A path containing
    // any character in `LAUNCHER_UNSAFE_CHARS` (e.g. `'`, `%`, `"`, `\n`,
    // `\r`, `\0`) must surface the rejection and never produce a launcher with
    // an unsafe pkg_root baked in. This pins the contract that the unsafe-char
    // check fires once at the entry boundary in `generate()` rather than per
    // platform body.
    #[tokio::test]
    async fn generate_rejects_pkg_root_with_unsafe_character() {
        let tmp = tempdir().unwrap();
        // Single-quote `'` is in LAUNCHER_UNSAFE_CHARS — would break Unix
        // single-quoted shell literals if it slipped through.
        let pkg_root = tmp.path().join("pkg'with'quote");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);

        let err = super::generate(&pkg_root, &entrypoints, &dest)
            .await
            .expect_err("pkg_root with unsafe character must be rejected");
        match err {
            crate::Error::LauncherUnsafeCharacter { character, .. } => {
                assert_eq!(character, '\'', "must report the offending single quote");
            }
            other => panic!("expected LauncherUnsafeCharacter, got {other:?}"),
        }
        // No launcher files must have been written when validation fails at
        // the entry boundary.
        assert!(
            !dest.join("cmake").exists(),
            "no unix launcher must be written when pkg_root rejected"
        );
        assert!(
            !dest.join("cmake.cmd").exists(),
            "no windows launcher must be written when pkg_root rejected"
        );
    }
}
