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
use std::path::Path;

use crate::package::metadata::dependency::DependencyName;
use crate::package::metadata::entrypoint::Entrypoints;
use crate::package::metadata::env::accumulator::DependencyContext;
use crate::package::metadata::template::TemplateResolver;

/// Characters that cannot appear in any string baked into a generated launcher.
///
/// The set is the union of constraints from both supported platforms:
/// - `'` breaks Unix single-quoted shell literals (cannot be escaped inside one).
/// - `%` triggers `cmd.exe` variable expansion (`%X%`) inside the `.cmd` body.
/// - `"` would close the `SET "var=value"` statement on Windows.
/// - `\n`, `\r`, `\0` would inject newlines/control bytes into either script
///   body — a code-injection vector.
///
/// Both launchers are generated on every platform (cross-platform packages),
/// so the character set is unified rather than per-platform.
const LAUNCHER_UNSAFE_CHARS: &[char] = &['\'', '%', '"', '\n', '\r', '\0'];

/// A `String` proven free of characters that would corrupt a generated launcher.
///
/// Construction via [`LauncherSafeString::new`] is the only way to obtain one;
/// the launcher body functions accept `&LauncherSafeString` so the unsafe-char
/// check happens once, at the entry boundary, not per platform.
#[derive(Debug, Clone)]
pub(crate) struct LauncherSafeString(String);

impl LauncherSafeString {
    /// Validates `value` against [`LAUNCHER_UNSAFE_CHARS`] and wraps it.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::LauncherUnsafeCharacter`] if the input contains
    /// any of the unsafe characters listed above.
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, crate::Error> {
        let value = value.into();
        if let Some(c) = value.chars().find(|c| LAUNCHER_UNSAFE_CHARS.contains(c)) {
            return Err(crate::Error::LauncherUnsafeCharacter { value, character: c });
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

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

    for entry in entries.iter() {
        let name = entry.name.as_str();
        // Resolve the target template eagerly to fail fast on malformed
        // packages (unknown dep, bad field). Per ADR §6 the resolved value is
        // NOT baked into the launcher — `ocx exec` re-resolves it from
        // `metadata.json` at invocation time — so the result is discarded.
        resolver
            .resolve(&entry.target)
            .map_err(|e| crate::Error::EntrypointTargetInvalid {
                name: name.to_string(),
                source: Box::new(e),
            })?;

        // Write Unix launcher.
        let unix_body = unix_launcher_body(&pkg_root_str);
        let unix_path = dest.join(name);
        tokio::fs::write(&unix_path, unix_body.as_bytes())
            .await
            .map_err(|e| crate::error::file_error(&unix_path, e))?;
        // Set the executable bit (no-op on non-Unix platforms).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&unix_path, std::fs::Permissions::from_mode(0o755))
                .await
                .map_err(|e| crate::error::file_error(&unix_path, e))?;
        }

        // Write Windows launcher.
        let windows_body = windows_launcher_body(&pkg_root_str);
        let windows_path = dest.join(format!("{name}.cmd"));
        tokio::fs::write(&windows_path, windows_body.as_bytes())
            .await
            .map_err(|e| crate::error::file_error(&windows_path, e))?;
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
/// Inputs are pre-validated [`LauncherSafeString`]s — see [`unix_launcher_body`].
fn windows_launcher_body(pkg_root: &LauncherSafeString) -> String {
    let pkg_root = pkg_root.as_str();
    // `%~n0` expands to the script's filename without path or extension —
    // identical role to Unix `$(basename "$0")`. ADR §6 invariant preserved:
    // nothing from `target` is baked.
    format!(
        "@ECHO off\n\
         SETLOCAL\n\
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
        assert!(body.contains("SETLOCAL"), "must have SETLOCAL: {body}");
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

    // ── LauncherSafeString — unified character rejection ──────────────────

    #[test]
    fn launcher_safe_string_rejects_all_unsafe_characters() {
        for unsafe_char in ['\'', '%', '"', '\n', '\r', '\0'] {
            let input = format!("prefix{unsafe_char}suffix");
            let err = super::LauncherSafeString::new(input.clone())
                .expect_err(&format!("must reject {unsafe_char:?} (input {input:?})"));
            match err {
                crate::Error::LauncherUnsafeCharacter { character, value } => {
                    assert_eq!(character, unsafe_char, "must report the offending char");
                    assert_eq!(value, input, "must echo the rejected value");
                }
                other => panic!("expected LauncherUnsafeCharacter, got {other:?}"),
            }
        }
    }

    #[test]
    fn launcher_safe_string_rejects_double_quote_even_though_unix_tolerates_it() {
        // Both launchers are generated on every platform, so the constraint is
        // unified: `"` would break the Windows `SET "var=value"` form even on
        // a Linux install where only the `.sh` launcher is used.
        assert!(super::LauncherSafeString::new("/path/with\"dq").is_err());
    }

    #[test]
    fn launcher_safe_string_accepts_path_with_spaces() {
        let s = super::LauncherSafeString::new("/path with spaces/bin/tool").unwrap();
        assert_eq!(s.as_str(), "/path with spaces/bin/tool");
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
    /// forwards user args.
    #[test]
    fn windows_launcher_body_byte_exact_match_adr_form() {
        let body = super::windows_launcher_body(&safe("C:\\pkg\\root"));
        let expected = "@ECHO off\n\
                        SETLOCAL\n\
                        ocx exec \"file://C:\\pkg\\root\" -- \"%~n0\" %*\n";
        assert_eq!(
            body, expected,
            "Windows launcher must match the ADR §6 byte-exact form (file:// URI; no `_target` SET; uses %~n0)",
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
}
