// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Launcher generator entry point.
//!
//! Generates the Unix `.sh` launcher and the Windows native `.exe` shim
//! (plus its `.shim` sidecar) at install time.
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

#[cfg(windows)]
use super::body::shim_sidecar_body;
use super::body::unix_launcher_body;
use super::safety::LauncherSafeString;

/// Generates Unix and Windows launchers for all declared entrypoints.
///
/// Writes `<dest>/<name>` (Unix, chmod 0755) and, on Windows, the native
/// `<dest>/<name>.exe` shim plus its `<dest>/<name>.shim` sidecar for each
/// entry in `entries`. If `entries` is empty, no files are written and `dest`
/// is not created.
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

        // Windows native shim (`<name>.exe`) + its `<name>.shim` sidecar.
        // The shim is the sole Windows launcher: `.EXE` is unconditionally in
        // the default Windows `PATHEXT`, so cmd.exe / PowerShell resolve
        // `<name>.exe` and Git-Bash resolves it directly — no `.cmd` is
        // emitted (it had no functional dependency and its `%*` re-parse was
        // the exact vector this shim closes). cfg-gated so the non-Windows
        // path is byte-for-byte unchanged.
        //
        // F-4: spawn order is NOT write order. The `.exe` MUST be on disk
        // before the `.shim` so the only recoverable partial state is
        // shim-present-without-sidecar (E1, recoverable by re-running
        // `generate()`) — never a `.shim` whose `.exe` is missing (the worse
        // state). The two writes are therefore SEQUENCED inside ONE per-entry
        // task (await `.exe`, then write `.shim`), still concurrent across
        // entries (ADR Contract 2 write-ordering postcondition).
        #[cfg(windows)]
        {
            let exe_path = dest.join(format!("{name}.exe"));
            let sidecar_body = shim_sidecar_body(&pkg_root_str);
            let sidecar_path = dest.join(format!("{name}.shim"));
            tasks.spawn(write_shim_exe_then_sidecar(exe_path, sidecar_path, sidecar_body));
        }
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

/// Writes the native Windows shim executable (`<exe_path>`) verbatim from
/// [`crate::shim::SHIM_BYTES`], then — only after the `.exe` write has fully
/// completed — its one-line `<sidecar_path>.shim` sidecar.
///
/// The two writes are sequenced (not two independent `JoinSet` tasks) so the
/// only recoverable partial state on a mid-generate fault is
/// `.exe`-present / `.shim`-absent (ADR E1, recoverable by re-running
/// `generate()`), never the reverse — a `.shim` without its `.exe` is the
/// worse state (ADR Contract 2 write-ordering postcondition, plan F-4).
///
/// `.exe` bytes are written verbatim (no transform) to preserve the
/// Authenticode verbatim-copy property.
#[cfg(windows)]
async fn write_shim_exe_then_sidecar(
    exe_path: PathBuf,
    sidecar_path: PathBuf,
    sidecar_body: String,
) -> Result<(), crate::Error> {
    tokio::fs::write(&exe_path, crate::shim::SHIM_BYTES)
        .await
        .map_err(|e| crate::error::file_error(&exe_path, e))?;
    write_launcher_file(sidecar_path, sidecar_body).await
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
        // Cutover: no `.cmd` is ever emitted on any platform. On Windows the
        // launcher is `<name>.exe` + `<name>.shim` (asserted by the
        // Windows-gated tests below); on non-Windows only `<name>` exists.
        assert!(
            !dest.join("cmake.cmd").exists(),
            "`.cmd` launcher must NOT be created (cutover to `.exe`-only)"
        );
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
            "no `.cmd` is ever emitted (cutover); also absent on rejection"
        );
        // The unsafe-char rejection at the `LauncherSafeString` boundary
        // (`generate()` entry) fires BEFORE any of the Windows writes are
        // spawned — so `.exe` and `.shim` must also be absent (the sidecar
        // reuses the same `LauncherSafeString`, no second validator; ADR
        // Contract 2 §Edge cases).
        assert!(
            !dest.join("cmake.exe").exists(),
            "no `.exe` shim must be written when pkg_root rejected at the entry boundary"
        );
        assert!(
            !dest.join("cmake.shim").exists(),
            "no `.shim` sidecar must be written when pkg_root rejected at the entry boundary"
        );
    }

    // ── Windows native shim emission (`.exe` + `.shim`, NO `.cmd`) ─────────
    //
    // Contract 2 postcondition (post-cutover): on Windows, each entry yields
    // THREE files — `<name>`, `<name>.exe`, `<name>.shim` — and explicitly
    // NO `<name>.cmd`. The `.exe` bytes must be BYTE-EQUAL to
    // `crate::shim::SHIM_BYTES` (the Authenticode verbatim-copy property —
    // not mere existence; a transformed copy would invalidate the signature).
    // Gated `#[cfg(windows)]` because the `.exe` and `.shim` writes are
    // themselves cfg-gated and `SHIM_BYTES` is only non-empty on a Windows
    // target.

    #[cfg(windows)]
    #[tokio::test]
    async fn generate_emits_three_windows_files_no_cmd() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);

        super::generate(&pkg_root, &entrypoints, &dest).await.unwrap();

        assert!(dest.join("cmake").exists(), "unix launcher must be created");
        assert!(
            dest.join("cmake.exe").exists(),
            "`.exe` shim must be created on Windows"
        );
        assert!(
            dest.join("cmake.shim").exists(),
            "`.shim` sidecar must be created on Windows"
        );
        assert!(
            !dest.join("cmake.cmd").exists(),
            "`.cmd` launcher must NOT be created (cutover to `.exe`-only — the \
             residual `%*` orphan is gone)"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn generate_exe_is_byte_equal_to_shim_bytes() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);

        super::generate(&pkg_root, &entrypoints, &dest).await.unwrap();

        let written = tokio::fs::read(dest.join("cmake.exe")).await.unwrap();
        assert_eq!(
            written.as_slice(),
            crate::shim::SHIM_BYTES,
            "`.exe` must be a verbatim copy of `crate::shim::SHIM_BYTES` — the \
             Authenticode verbatim-copy property (any transform invalidates the \
             signature). This is byte-equality, NOT mere existence."
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn generate_shim_sidecar_is_pkg_root_plus_lf() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);

        super::generate(&pkg_root, &entrypoints, &dest).await.unwrap();

        let sidecar = tokio::fs::read(dest.join("cmake.shim")).await.unwrap();
        let expected = format!("{}\n", pkg_root.to_string_lossy());
        assert_eq!(
            sidecar,
            expected.as_bytes(),
            "`.shim` must be exactly `format!(\"{{pkg_root}}\\n\")` (UTF-8, no BOM, single LF)"
        );
        assert!(
            !sidecar.starts_with(&[0xEF, 0xBB, 0xBF]),
            "`.shim` must not carry a UTF-8 BOM"
        );
    }

    /// F-4 (plan Progress Log): the `.exe`/`.shim` JoinSet spawn order is NOT
    /// the write order — Contract 2's write-ordering postcondition requires
    /// the implementation to SEQUENCE the writes so `.exe` lands before
    /// `.shim`. The recoverable partial state is therefore
    /// `.exe` present / `.shim` ABSENT (E1, recoverable by re-running
    /// `generate()`), never the reverse (a `.shim` without its `.exe` is the
    /// worse state). This test injects a failure on the SECOND shim write
    /// (the `.shim`) by pre-creating `cmake.shim` as a directory so the file
    /// write fails, then asserts the surviving state is `.exe` present,
    /// `.shim`-as-file absent — pinning the ordering as a real guarantee, not
    /// an accident of spawn order.
    #[cfg(windows)]
    #[tokio::test]
    async fn generate_half_state_is_exe_present_shim_absent() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        tokio::fs::create_dir_all(&dest).await.unwrap();
        // Pre-create `cmake.shim` as a DIRECTORY: the sidecar file write must
        // then fail, simulating a mid-generate fault on the 2nd shim write.
        tokio::fs::create_dir_all(dest.join("cmake.shim")).await.unwrap();
        let entrypoints = entries_for(&["cmake"]);

        let result = super::generate(&pkg_root, &entrypoints, &dest).await;
        assert!(
            result.is_err(),
            "generate() must surface the failed `.shim` write as an error"
        );

        // The recoverable half-state: the `.exe` shim is present (written
        // first), the `.shim` was NOT successfully written as a file (the
        // directory we pre-created is not a valid sidecar). This pins
        // `.exe`-before-`.shim` as a guarantee — never `.shim` without `.exe`.
        assert!(
            dest.join("cmake.exe").exists(),
            "`.exe` must be present after a failed `.shim` write (.exe written first)"
        );
        assert!(
            !dest.join("cmake.shim").is_file(),
            "the `.shim` must NOT exist as a valid file — recoverable state is \
             `.exe` present / `.shim` absent (E1), never the reverse"
        );
    }

    // ── non-Windows emits only the Unix `<name>` launcher ─────────────────
    //
    // On non-Windows the `.exe`/`.shim` emission is skipped via `cfg`, and
    // `crate::shim::SHIM_BYTES` is empty. Post-cutover, no `.cmd` is emitted
    // on any platform either — this pins that only `<name>` exists off
    // Windows, and `.cmd`/`.exe`/`.shim` are all absent.

    #[cfg(not(windows))]
    #[tokio::test]
    async fn generate_emits_only_unix_launcher_off_windows() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        tokio::fs::create_dir_all(pkg_root.join("content")).await.unwrap();
        let dest = pkg_root.join("entrypoints");
        let entrypoints = entries_for(&["cmake"]);

        super::generate(&pkg_root, &entrypoints, &dest).await.unwrap();

        assert!(dest.join("cmake").exists(), "unix launcher must still be created");
        assert!(
            !dest.join("cmake.cmd").exists(),
            "no `.cmd` launcher on any platform (cutover to `.exe`-only)"
        );
        assert!(
            !dest.join("cmake.exe").exists(),
            "no `.exe` shim off Windows (emission cfg-gated)"
        );
        assert!(
            !dest.join("cmake.shim").exists(),
            "no `.shim` sidecar off Windows (emission cfg-gated)"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn shim_bytes_empty_off_windows() {
        assert!(
            crate::shim::SHIM_BYTES.is_empty(),
            "`ocx` must carry zero shim weight off Windows (SHIM_BYTES empty)"
        );
    }
}
