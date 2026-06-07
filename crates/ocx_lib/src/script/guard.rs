// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Path sandbox for script-supplied paths.
//!
//! Two layers, both required:
//!
//! 1. LEXICAL — [`crate::utility::fs::path::lexical_normalize`] +
//!    [`crate::utility::fs::path::escapes_root`]. Absolute input is rejected
//!    before normalization; the result is joined onto the chosen root and
//!    re-checked. This alone does NOT defeat symlink or TOCTOU escapes.
//! 2. SYMLINK + TOCTOU (Codex C1) — after the lexical check, EVERY
//!    path-consuming host fn (read-side `read_file`/`exists` included, plus
//!    `write_file`/`mkdir` and `ocx.run(cwd=…)`) re-validates symlink
//!    containment via the EXISTING `crate::symlink::validate_target` /
//!    `crate::utility::fs::path::validate_symlinks_in_dir` utilities (no
//!    hand-rolled symlink walk) and, where feasible, re-checks containment
//!    post-canonicalize immediately before the syscall to shrink the TOCTOU
//!    window. The residual check-to-open window cannot be fully eliminated
//!    against an adversarial in-sandbox process that spawns binaries — a
//!    documented best-effort bound flagged for `/security-auditor`.

use std::path::{Component, Path, PathBuf};

use crate::utility::fs::path::{escapes_root, lexical_normalize};

/// Reason a path was rejected by the sandbox guard.
#[derive(Debug, thiserror::Error)]
pub(super) enum GuardError {
    /// path escaped the sandbox lexically (`..`, absolute, or outside root)
    #[error("path escapes the sandbox: {0}")]
    LexicalEscape(String),
    /// path resolved through a symlink that leaves the sandbox
    #[error("path escapes the sandbox via a symlink: {0}")]
    SymlinkEscape(String),
}

/// Resolves a script-supplied path, anchoring it on the scratch root.
///
/// This is the WRITE/`cwd` side: the result is always inside the read-write
/// scratch root. (Read-side fallback to the package root is `resolve_read`.)
/// Applies the lexical layer only; the caller must additionally apply the
/// symlink re-check (Codex C1) before opening/spawning. Returns the resolved
/// absolute path on success.
pub(super) fn resolve_scratch(user_path: &str, scratch_root: &Path) -> Result<PathBuf, GuardError> {
    let raw = Path::new(user_path);

    // Reject absolute input (incl. Windows prefixes like `C:\`) before any
    // normalization — an absolute path can never be sandbox-relative.
    if raw.is_absolute()
        || raw
            .components()
            .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)))
    {
        return Err(GuardError::LexicalEscape(user_path.to_string()));
    }

    // Lexical layer: a `..` that escapes the logical root is rejected. The
    // shared helper treats `\` as a separator on every platform so test
    // scripts stay portable.
    if escapes_root(raw) {
        return Err(GuardError::LexicalEscape(user_path.to_string()));
    }
    let normalized = lexical_normalize(raw);

    // The scratch root is the read-write sandbox. Package-root reads are
    // handled separately by `resolve_read` (which probes the package root as
    // an explicit fallback base); this fn always anchors on scratch so a write
    // can never land in the read-only package tree.
    let resolved = scratch_root.join(&normalized);

    // Defence in depth: re-check the joined path cannot escape the scratch
    // root (guards against any normalization edge the lexical pass missed).
    if let Ok(rel) = resolved.strip_prefix(scratch_root)
        && escapes_root(rel)
    {
        return Err(GuardError::LexicalEscape(user_path.to_string()));
    }
    if !resolved.starts_with(scratch_root) {
        return Err(GuardError::LexicalEscape(user_path.to_string()));
    }
    Ok(resolved)
}

/// Resolves a read-side path, allowing the package root as a fallback base.
///
/// `read_file` / `exists` accept `{scratch (rw), package (ro)}`. This anchors
/// the lexically-validated relative path on the scratch root first; if it does
/// not exist there it retries against the read-only package root. Both
/// candidates are inside their respective roots (the lexical layer already
/// rejected `..`/absolute escapes).
pub(super) fn resolve_read(user_path: &str, scratch_root: &Path, package_root: &Path) -> Result<PathBuf, GuardError> {
    let scratch_candidate = resolve_scratch(user_path, scratch_root)?;
    if scratch_candidate.exists() {
        return Ok(scratch_candidate);
    }
    let normalized = lexical_normalize(Path::new(user_path));
    let package_candidate = package_root.join(&normalized);
    if package_candidate.exists() {
        return Ok(package_candidate);
    }
    // Neither exists yet — default to the scratch candidate (the host fn maps
    // a missing file to its own error / `false`).
    Ok(scratch_candidate)
}

/// Codex C1 symlink + best-effort TOCTOU re-check.
///
/// After the lexical [`resolve_scratch`] / [`resolve_read`], every path-consuming host fn
/// re-validates that no symlink component leaves `root`. Reuses the existing
/// [`crate::symlink`] validators (no hand-rolled symlink walk). The residual
/// check-to-open window against an adversarial in-sandbox process cannot be
/// fully eliminated — documented best-effort bound (flagged for
/// `/security-auditor`).
pub(super) fn verify_symlink_containment(root: &Path, resolved: &Path) -> Result<(), GuardError> {
    // Walk each existing ancestor component; if it is a symlink, validate its
    // target stays within `root` via the existing single-component validator.
    let mut current = root.to_path_buf();
    let Ok(rel) = resolved.strip_prefix(root) else {
        return Err(GuardError::SymlinkEscape(resolved.display().to_string()));
    };
    for comp in rel.components() {
        let Component::Normal(name) = comp else {
            continue;
        };
        current.push(name);
        if std::fs::symlink_metadata(&current).is_err() {
            // Does not exist yet (e.g. a not-yet-written file) — nothing to
            // traverse; later components cannot exist either.
            break;
        }
        // Junction-aware: `Metadata::file_type().is_symlink()` misses Windows
        // NTFS junctions, so a junction-based escape would bypass the
        // containment re-check. `crate::symlink::is_link` reports junctions on
        // Windows and is identical to `is_symlink()` on Unix.
        if crate::symlink::is_link(&current) {
            let target =
                std::fs::read_link(&current).map_err(|_| GuardError::SymlinkEscape(current.display().to_string()))?;
            crate::symlink::validate_target(root, &current, &target)
                .map_err(|_| GuardError::SymlinkEscape(current.display().to_string()))?;
        }
    }
    // If the resolved path itself is a directory subtree, sweep it too
    // (post-canonicalize defence for `cwd`/`mkdir`).
    if resolved.is_dir() && crate::utility::fs::path::validate_symlinks_in_dir(root, resolved).is_err() {
        return Err(GuardError::SymlinkEscape(resolved.display().to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_scratch — lexical sandbox layer (C7) ─────────────────────────
    //
    // Spec source: plan_package_test_scripting.md C7 + Component Contract C4
    // "Path rule" + Error Taxonomy "Sandbox escape — lexical". These tests
    // encode the lexical layer ONLY (no symlink — that is the C1 re-check,
    // exercised at acceptance level U7/U8/U13/U14).

    fn roots() -> (tempfile::TempDir, tempfile::TempDir) {
        let scratch = tempfile::tempdir().unwrap();
        let package = tempfile::tempdir().unwrap();
        (scratch, package)
    }

    #[test]
    fn accepts_relative_path_inside_scratch() {
        // C7: a plain relative path resolves under the scratch root and the
        // returned path is contained in it.
        let (scratch, _package) = roots();
        let resolved =
            resolve_scratch("out/result.txt", scratch.path()).expect("a relative in-scratch path must be accepted");
        assert!(
            resolved.starts_with(scratch.path()),
            "resolved path must be inside the scratch root, got {resolved:?}"
        );
    }

    #[test]
    fn rejects_parent_dir_escape() {
        // Error Taxonomy: `..` escape → guard rejection.
        let (scratch, _package) = roots();
        let err = resolve_scratch("../escape", scratch.path()).expect_err("a `..` escape must be rejected");
        assert!(matches!(err, GuardError::LexicalEscape(_)));
    }

    #[test]
    fn rejects_absolute_path() {
        // C4 Path rule: absolute paths rejected before normalization.
        let (scratch, _package) = roots();
        let abs = if cfg!(windows) {
            "C:\\etc\\passwd"
        } else {
            "/etc/passwd"
        };
        let err = resolve_scratch(abs, scratch.path()).expect_err("an absolute path must be rejected");
        assert!(matches!(err, GuardError::LexicalEscape(_)));
    }

    #[test]
    fn rejects_deep_parent_dir_escape() {
        // Error Taxonomy: multi-level `..` traversal out of scratch.
        let (scratch, _package) = roots();
        let err = resolve_scratch("a/../../../outside", scratch.path())
            .expect_err("a multi-level `..` escape must be rejected");
        assert!(matches!(err, GuardError::LexicalEscape(_)));
    }

    #[test]
    fn resolve_scratch_is_strictly_scratch_anchored() {
        // W2: the previous test asserted `starts_with(scratch) || starts_with
        // (package)`, which is vacuously true because resolve_scratch ALWAYS
        // anchors on scratch. Assert the honest invariant: a relative path
        // resolves strictly inside the scratch root and NEVER under the package
        // root (write/cwd side has no package fallback).
        let (scratch, package) = roots();
        let resolved = resolve_scratch("bin/tool", scratch.path()).expect("an in-scope relative path must resolve");
        assert!(
            resolved.starts_with(scratch.path()),
            "resolve_scratch must anchor on the scratch root, got {resolved:?}"
        );
        assert!(
            !resolved.starts_with(package.path()),
            "resolve_scratch must never resolve into the package root, got {resolved:?}"
        );
    }

    #[test]
    fn resolve_read_falls_back_to_package_root_when_only_there() {
        // W2: a real read-fallback test. A file that exists ONLY under the
        // read-only package root (not in scratch) must resolve to the package
        // candidate — exercising the `resolve_read` fallback branch that the
        // old `read_only_access_*` test never actually covered.
        let (scratch, package) = roots();
        let nested = package.path().join("share");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("data.txt"), b"pkg").unwrap();

        let resolved = resolve_read("share/data.txt", scratch.path(), package.path())
            .expect("a package-root-only file must resolve via the read fallback");
        assert!(
            resolved.starts_with(package.path()) && !resolved.starts_with(scratch.path()),
            "resolve_read must land on the package candidate, got {resolved:?}"
        );
        assert!(resolved.exists(), "the resolved package candidate must exist");
    }

    #[test]
    fn resolve_read_prefers_scratch_when_present_in_both() {
        // resolve_read probes scratch first: a file present in scratch wins
        // over a same-named package-root file (scratch is the rw working area).
        let (scratch, package) = roots();
        std::fs::write(scratch.path().join("dup.txt"), b"scratch").unwrap();
        std::fs::write(package.path().join("dup.txt"), b"package").unwrap();

        let resolved = resolve_read("dup.txt", scratch.path(), package.path()).expect("present in scratch");
        assert!(
            resolved.starts_with(scratch.path()),
            "resolve_read must prefer the scratch candidate, got {resolved:?}"
        );
    }
}
