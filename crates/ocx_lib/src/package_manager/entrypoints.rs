// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Launcher generator for package entry points.
//!
//! Generates Unix `.sh` and Windows `.cmd` launcher scripts at install time.
//! Each launcher calls `ocx exec 'file://<baked-package-root>' -- "$@"`,
//! preserving OCX's clean-env execution guarantee. The baked URI carries the
//! absolute package-root path; `ocx exec` reads `metadata.json` from that
//! root and resolves the binary target via env interpolation at invocation
//! time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokio::task::JoinSet;

use crate::package::metadata::dependency::DependencyName;
use crate::package::metadata::entrypoint::Entrypoints;
use crate::package::metadata::env::accumulator::DependencyContext;
use crate::package::metadata::template::TemplateResolver;
use crate::package_manager::launcher_safe::LauncherSafeString;

/// Generates Unix and Windows launchers for all declared entrypoints.
///
/// Writes `<dest>/<name>` (Unix, chmod 0755) and `<dest>/<name>.cmd` (Windows)
/// for each entry in `entries`. If `entries` is empty, no files are written
/// and `dest` is not created.
///
/// `pkg_root` is the absolute package-root directory
/// (`packages/<registry>/<algo>/<2hex>/<30hex>/`). The launcher bakes
/// `file://<pkg_root>` and forwards the user's args to `ocx exec`, which reads
/// `metadata.json` from that root and resolves each entry's `target` via env
/// interpolation at invocation time. Each entry's `target` is still resolved
/// here through [`TemplateResolver`] (against `<pkg_root>/content`) as a
/// defense-in-depth check that the templates parse — an unresolvable target
/// means the package is malformed and install should fail before any launcher
/// is written.
///
/// # Errors
///
/// Returns an error if:
/// - Creating the `dest` directory fails.
/// - Writing any launcher file fails.
/// - The `pkg_root` contains a character unsafe for either launcher
///   template (see [`LAUNCHER_UNSAFE_CHARS`]).
/// - An entry's `target` references an unknown dependency or field.
pub async fn generate(
    pkg_root: &Path,
    entries: &Entrypoints,
    dep_contexts: &HashMap<DependencyName, DependencyContext>,
    dest: &Path,
) -> Result<(), crate::Error> {
    if entries.is_empty() {
        return Ok(());
    }

    let pkg_root_str = LauncherSafeString::new(pkg_root.to_string_lossy().into_owned())?;
    // `${installPath}` resolves to the content tree, not the package root.
    let content_path = pkg_root.join("content");
    let resolver = TemplateResolver::new(&content_path, dep_contexts);

    tokio::fs::create_dir_all(dest)
        .await
        .map_err(|e| crate::error::file_error(dest, e))?;

    // Spawn one task per launcher file (Unix + Windows for every entry).
    // Each task is independent — there is no ordering constraint between
    // entries or between platforms — so concurrent writes amortise per-file
    // syscall latency over a large `entrypoints/` directory.
    let mut tasks: JoinSet<Result<(), crate::Error>> = JoinSet::new();

    for entry in entries.iter() {
        let name = entry.name.as_str();
        // Resolve the target template eagerly to fail fast on malformed
        // packages (unknown dep, bad field). Per ADR §6 the resolved value is
        // NOT baked into the launcher — `ocx exec` re-resolves it from
        // `metadata.json` at invocation time — so the result is discarded.
        resolver
            .resolve(&entry.target)
            .map_err(|e| crate::Error::EntrypointInstallFailed {
                name: name.to_string(),
                source: Box::new(e),
            })?;

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
        match join_result.expect("entrypoint launcher task panicked") {
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

/// Produces the body of a Unix `.sh` launcher.
///
/// Per ADR `adr_package_entry_points.md` §6, the launcher does NOT bake the
/// resolved `target` — `target` is a publish-time existence assertion only.
/// The launcher hands `'file://<pkg_root>'` and the user's args to `ocx exec`,
/// which reads `metadata.json` from that root and resolves the target via env
/// interpolation at invocation time.
///
/// Inputs are pre-validated [`LauncherSafeString`]s so this function is total
/// (no `Result`); the unsafe-char check fires once, at the [`generate`] entry
/// boundary, instead of being repeated per platform.
fn unix_launcher_body(pkg_root: &LauncherSafeString) -> String {
    let pkg_root = pkg_root.as_str();
    // `$(basename "$0")` injects the launcher's own filename as first positional
    // after `--`. ADR §6 invariant ("target field is not the launcher file") is
    // preserved: nothing from `target` is baked. `ocx exec` receives the entry-
    // point name via `$0` and resolves the binary through the package's PATH env
    // (set up from metadata). The URI's three slashes are intentional —
    // `file://` (empty host) plus the absolute path's leading `/`.
    format!(
        "#!/bin/sh\n\
         # Generated by ocx at install time. Do not edit.\n\
         exec ocx exec 'file://{pkg_root}' -- \"$(basename \"$0\")\" \"$@\"\n"
    )
}

/// Produces the body of a Windows `.cmd` launcher.
///
/// Per ADR `adr_package_entry_points.md` §6, the launcher does NOT bake the
/// resolved `target` — `target` is a publish-time existence assertion only.
/// The launcher forwards `%*` to `ocx exec "file://<pkg_root>"`, which reads
/// `metadata.json` from that root and resolves the target via env
/// interpolation at invocation time.
///
/// `SETLOCAL DisableDelayedExpansion` closes the registry-level `!VAR!`
/// expansion vector (BatBadBut interim mitigation, ADR
/// `adr_windows_cmd_argv_injection.md`). Residual risk: caller-supplied
/// arguments forwarded via `%*` are still re-parsed by `cmd.exe`; callers
/// passing user-controlled argument strings must shell-quote them.
///
/// Inputs are pre-validated [`LauncherSafeString`]s — see [`unix_launcher_body`].
fn windows_launcher_body(pkg_root: &LauncherSafeString) -> String {
    let pkg_root = pkg_root.as_str();
    // `%~n0` expands to the script's filename without path or extension —
    // identical role to Unix `$(basename "$0")`. ADR §6 invariant preserved:
    // nothing from `target` is baked.
    // `DisableDelayedExpansion`: closes the `!VAR!` expansion vector that is
    // active when the Windows registry key
    // `HKCU\Software\Microsoft\Command Processor\DelayedExpansion` is set to
    // `1`. See ADR `adr_windows_cmd_argv_injection.md` for threat model.
    format!(
        "@ECHO off\n\
         SETLOCAL DisableDelayedExpansion\n\
         ocx exec \"file://{pkg_root}\" -- \"%~n0\" %*\n"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use tempfile::tempdir;

    use crate::package::metadata::entrypoint::{Entrypoint, EntrypointName, Entrypoints};

    // ── helpers ────────────────────────────────────────────────────────────

    fn make_entrypoint(name: &str, target: &str) -> Entrypoint {
        Entrypoint {
            name: EntrypointName::try_from(name).unwrap(),
            target: target.to_string(),
        }
    }

    fn make_entrypoints(entries: Vec<Entrypoint>) -> Entrypoints {
        Entrypoints::new(entries).unwrap()
    }

    fn safe(s: &str) -> super::LauncherSafeString {
        super::LauncherSafeString::new(s).unwrap()
    }

    // ── Unix launcher body — golden match ─────────────────────────────────

    #[test]
    fn unix_launcher_body_golden_match() {
        let body = super::unix_launcher_body(&safe("/some/pkg/root"));
        assert!(body.starts_with("#!/bin/sh"), "must start with shebang: {body}");
        assert!(
            body.contains("'file:///some/pkg/root'"),
            "must inline single-quoted file:// URI for the package root: {body}"
        );
        assert!(
            body.contains("exec ocx exec 'file://"),
            "must contain exec call: {body}"
        );
        assert!(
            body.contains("\"$(basename \"$0\")\""),
            "must inject launcher's own filename via $(basename \"$0\"): {body}"
        );
        assert!(body.contains("\"$@\""), "must forward all args via \"$@\": {body}");
        assert!(
            !body.contains("_target"),
            "ADR §6: launcher must not bake `_target` (target resolved by `ocx exec` at invocation time): {body}"
        );
        assert!(
            !body.contains("--install-dir"),
            "post-flatten launcher must use file:// URI, not --install-dir: {body}"
        );
    }

    #[test]
    fn windows_launcher_body_golden_match() {
        let body = super::windows_launcher_body(&safe("C:\\pkg\\root"));
        assert!(
            body.contains("@ECHO off") || body.contains("@echo off"),
            "must have ECHO off: {body}"
        );
        assert!(
            body.contains("SETLOCAL DisableDelayedExpansion"),
            "must have SETLOCAL DisableDelayedExpansion (BatBadBut interim mitigation): {body}"
        );
        assert!(
            body.contains("ocx exec \"file://C:\\pkg\\root\""),
            "must inline file:// URI for the package root: {body}"
        );
        assert!(
            body.contains("\"%~n0\""),
            "must inject launcher's own filename via %~n0: {body}"
        );
        assert!(body.contains("%*"), "must forward all args via %*: {body}");
        assert!(
            !body.contains("_target"),
            "ADR §6: launcher must not bake `_target` (target resolved by `ocx exec` at invocation time): {body}"
        );
        assert!(
            !body.contains("--install-dir"),
            "post-flatten launcher must use file:// URI, not --install-dir: {body}"
        );
    }

    // ── Path with spaces is handled correctly ─────────────────────────────

    #[test]
    fn unix_launcher_body_handles_path_with_spaces() {
        // Paths with spaces must be preserved verbatim inside single quotes.
        let body = super::unix_launcher_body(&safe("/path with spaces"));
        assert!(body.contains("spaces"), "space must appear in body: {body}");
    }

    // ── Empty Entrypoints — no files generated ────────────────────────────

    #[tokio::test]
    async fn generate_empty_entrypoints_creates_nothing() {
        let tmp = tempdir().unwrap();
        let pkg_root = tmp.path().join("pkg");
        let dest = pkg_root.join("entrypoints");
        let entrypoints = make_entrypoints(vec![]);
        let dep_contexts = HashMap::new();

        super::generate(&pkg_root, &entrypoints, &dep_contexts, &dest)
            .await
            .unwrap();
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
        let entrypoints = make_entrypoints(vec![make_entrypoint("cmake", "${installPath}/bin/cmake")]);
        let dep_contexts = HashMap::new();

        super::generate(&pkg_root, &entrypoints, &dep_contexts, &dest)
            .await
            .unwrap();
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
        let entrypoints = make_entrypoints(vec![make_entrypoint("cmake", "${installPath}/bin/cmake")]);
        let dep_contexts = HashMap::new();

        super::generate(&pkg_root, &entrypoints, &dep_contexts, &dest)
            .await
            .unwrap();
        let mode = std::fs::metadata(dest.join("cmake")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "unix launcher must be mode 0755");
    }

    // ── plan_review_findings_pr64 §3.11 + §3.15: ADR §6 launcher contract ──

    /// ADR §6 invariant ("target field is not the launcher file") — no
    /// `_target` baked. The launcher passes its OWN filename via
    /// `$(basename "$0")` as first positional after `--`, then forwards
    /// `"$@"` for user args. `ocx exec` resolves the binary via the package's
    /// PATH env (set up from metadata).
    #[test]
    fn unix_launcher_body_byte_exact_match_adr_form() {
        let body = super::unix_launcher_body(&safe("/some/pkg/root"));
        let expected = "#!/bin/sh\n\
                        # Generated by ocx at install time. Do not edit.\n\
                        exec ocx exec 'file:///some/pkg/root' -- \"$(basename \"$0\")\" \"$@\"\n";
        assert_eq!(
            body, expected,
            "Unix launcher must match the ADR §6 byte-exact form (file:// URI; no `_target` baking; uses $(basename $0))",
        );
    }

    /// Windows launcher mirrors ADR §6 — no `_target` SET. `%~n0` injects
    /// the script's name without extension as first positional, then `%*`
    /// forwards user args. `DisableDelayedExpansion` closes the `!VAR!`
    /// vector (BatBadBut interim mitigation — ADR `adr_windows_cmd_argv_injection.md`).
    #[test]
    fn windows_launcher_body_byte_exact_match_adr_form() {
        let body = super::windows_launcher_body(&safe("C:\\pkg\\root"));
        let expected = "@ECHO off\n\
                        SETLOCAL DisableDelayedExpansion\n\
                        ocx exec \"file://C:\\pkg\\root\" -- \"%~n0\" %*\n";
        assert_eq!(
            body, expected,
            "Windows launcher must match the ADR §6 byte-exact form (file:// URI; no `_target` SET; uses %~n0)",
        );
        assert!(
            body.contains("SETLOCAL DisableDelayedExpansion"),
            "launcher must use SETLOCAL DisableDelayedExpansion (BatBadBut interim mitigation): {body}"
        );
        assert!(
            !body.contains("EnableDelayedExpansion"),
            "template must not enable delayed expansion: {body}"
        );
    }

    /// `.cmd` quoting property test — the Windows launcher body must not
    /// expand any cmd.exe metachars (`& ^ < > ! ( ) |`) outside double-quoted
    /// regions. The launcher's `LauncherSafeString` rejects unsafe characters
    /// at the entry boundary (via `LAUNCHER_UNSAFE_CHARS`), and the ADR §6
    /// form removes the resolved-target SET surface entirely — there must be
    /// no `_target=` SET line at all.
    #[test]
    fn windows_launcher_body_no_unescaped_cmd_metachars() {
        let body = super::windows_launcher_body(&safe("C:\\pkg\\root"));
        // ADR §6 form: launcher must not bake _target at all (no SET _target line).
        assert!(
            !body.contains("_target"),
            "ADR §6 launcher must not bake `_target` SET — present in:\n{body}"
        );
        // Defensive check: nothing outside double-quoted regions should contain
        // cmd.exe metachars. We scan the body line-by-line and assert that each
        // metachar instance, if present, appears between a `"` pair.
        for (lineno, line) in body.lines().enumerate() {
            for &meta in &['&', '^', '<', '>', '!', '(', ')', '|'] {
                if let Some(idx) = line.find(meta) {
                    let before = &line[..idx];
                    let after = &line[idx + 1..];
                    let opens_before = before.matches('"').count();
                    let closes_after = after.matches('"').count();
                    let is_quoted = opens_before % 2 == 1 && closes_after >= 1;
                    assert!(
                        is_quoted,
                        "line {lineno}: unquoted cmd.exe metachar {meta:?} in launcher body:\n{line}",
                    );
                }
            }
        }
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
        let entrypoints = make_entrypoints(vec![make_entrypoint("cmake", "${installPath}/bin/cmake")]);
        let dep_contexts = HashMap::new();

        // First call
        let _ = super::generate(&pkg_root, &entrypoints, &dep_contexts, &dest).await;
        // Second call — must not error due to already-existing files
        let _ = super::generate(&pkg_root, &entrypoints, &dep_contexts, &dest).await;
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
        let entrypoints = make_entrypoints(vec![make_entrypoint("cmake", "${installPath}/bin/cmake")]);
        let dep_contexts = HashMap::new();

        let err = super::generate(&pkg_root, &entrypoints, &dep_contexts, &dest)
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
