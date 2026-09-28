// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Path sandbox for script-supplied paths.
//!
//! The lexical resolvers alone do not stop symlink escapes: every path-consuming host fn, reads and
//! `ocx.run(cwd=…)` included, must also call [`verify_symlink_containment`].

use std::path::{Component, Path, PathBuf};

use ocx_util::fs::path::{escapes_root, lexical_normalize};

/// Reason a path was rejected by the sandbox guard.
#[derive(Debug, thiserror::Error)]
pub(super) enum GuardError {
    /// path escaped the sandbox lexically
    #[error("path escapes the sandbox: {0}")]
    LexicalEscape(String),
    /// path resolved through a symlink that leaves the sandbox
    #[error("path escapes the sandbox via a symlink: {0}")]
    SymlinkEscape(String),
}

/// Resolves a script-supplied write or `cwd` path inside the scratch root, lexically only.
pub(super) fn resolve_scratch(user_path: &str, scratch_root: &Path) -> Result<PathBuf, GuardError> {
    let raw = Path::new(user_path);

    // Absolute input, `C:\` prefixes included, would replace the scratch root on `join`.
    if raw.is_absolute()
        || raw
            .components()
            .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)))
    {
        return Err(GuardError::LexicalEscape(user_path.to_string()));
    }

    if escapes_root(raw) {
        return Err(GuardError::LexicalEscape(user_path.to_string()));
    }
    let normalized = lexical_normalize(raw);

    // Always scratch, or a write lands in the read-only package tree.
    let resolved = scratch_root.join(&normalized);

    // Defence in depth against a normalization edge the lexical pass missed.
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

/// Resolves a read-side path on scratch first, then on the read-only bundle content root.
pub(super) fn resolve_read(user_path: &str, scratch_root: &Path, content_root: &Path) -> Result<PathBuf, GuardError> {
    let scratch_candidate = resolve_scratch(user_path, scratch_root)?;
    if scratch_candidate.exists() {
        return Ok(scratch_candidate);
    }
    // Only after `resolve_scratch`, or a `../` path reads outside the bundle; the symlink re-check skips `..`.
    let normalized = lexical_normalize(Path::new(user_path));
    let content_candidate = content_root.join(&normalized);
    if content_candidate.exists() {
        return Ok(content_candidate);
    }
    Ok(scratch_candidate)
}

/// Checks that no symlink component of `resolved` leaves `root`.
///
/// Best effort: a check-to-open race by an adversarial in-sandbox process remains.
pub(super) fn verify_symlink_containment(root: &Path, resolved: &Path) -> Result<(), GuardError> {
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
            // Stop here, or `validate_target` miscounts the parent depth lexically and a planted chain escapes.
            break;
        }
        // `is_link`, not `is_symlink()`, or a Windows NTFS junction escapes the check.
        if ocx_util::fs::symlink::is_link(&current) {
            let target =
                std::fs::read_link(&current).map_err(|_| GuardError::SymlinkEscape(current.display().to_string()))?;
            ocx_util::fs::symlink::validate_target(root, &current, &target)
                .map_err(|_| GuardError::SymlinkEscape(current.display().to_string()))?;
        }
    }
    // A directory target (`cwd`, `mkdir`) is swept whole.
    if resolved.is_dir() && ocx_util::fs::path::validate_symlinks_in_dir(root, resolved).is_err() {
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

    /// `(scratch root, content root)` — the two bases `resolve_read` probes.
    fn roots() -> (tempfile::TempDir, tempfile::TempDir) {
        let scratch = tempfile::tempdir().unwrap();
        let content = tempfile::tempdir().unwrap();
        (scratch, content)
    }

    #[test]
    fn accepts_relative_path_inside_scratch() {
        // C7: a plain relative path resolves under the scratch root and the
        // returned path is contained in it.
        let (scratch, _content) = roots();
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
        let (scratch, _content) = roots();
        let err = resolve_scratch("../escape", scratch.path()).expect_err("a `..` escape must be rejected");
        assert!(matches!(err, GuardError::LexicalEscape(_)));
    }

    #[test]
    fn rejects_absolute_path() {
        // C4 Path rule: absolute paths rejected before normalization.
        let (scratch, _content) = roots();
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
        let (scratch, _content) = roots();
        let err = resolve_scratch("a/../../../outside", scratch.path())
            .expect_err("a multi-level `..` escape must be rejected");
        assert!(matches!(err, GuardError::LexicalEscape(_)));
    }

    #[test]
    fn resolve_scratch_is_strictly_scratch_anchored() {
        // W2: the previous test asserted `starts_with(scratch) || starts_with
        // (content)`, which is vacuously true because resolve_scratch ALWAYS
        // anchors on scratch. Assert the honest invariant: a relative path
        // resolves strictly inside the scratch root and NEVER under the content
        // root (write/cwd side has no read-only fallback).
        let (scratch, content) = roots();
        let resolved = resolve_scratch("bin/tool", scratch.path()).expect("an in-scope relative path must resolve");
        assert!(
            resolved.starts_with(scratch.path()),
            "resolve_scratch must anchor on the scratch root, got {resolved:?}"
        );
        assert!(
            !resolved.starts_with(content.path()),
            "resolve_scratch must never resolve into the content root, got {resolved:?}"
        );
    }

    #[test]
    fn resolve_read_falls_back_to_content_root_when_only_there() {
        // W2: a real read-fallback test. A file that exists ONLY under the
        // read-only content root (not in scratch) must resolve to the content
        // candidate — exercising the `resolve_read` fallback branch that the
        // old `read_only_access_*` test never actually covered.
        let (scratch, content) = roots();
        let nested = content.path().join("share");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("data.txt"), b"pkg").unwrap();

        let resolved = resolve_read("share/data.txt", scratch.path(), content.path())
            .expect("a content-root-only file must resolve via the read fallback");
        assert!(
            resolved.starts_with(content.path()) && !resolved.starts_with(scratch.path()),
            "resolve_read must land on the content candidate, got {resolved:?}"
        );
        assert!(resolved.exists(), "the resolved content candidate must exist");
    }

    #[test]
    fn resolve_read_prefers_scratch_when_present_in_both() {
        // resolve_read probes scratch first: a file present in scratch wins
        // over a same-named content-root file (scratch is the rw working area).
        let (scratch, content) = roots();
        std::fs::write(scratch.path().join("dup.txt"), b"scratch").unwrap();
        std::fs::write(content.path().join("dup.txt"), b"content").unwrap();

        let resolved = resolve_read("dup.txt", scratch.path(), content.path()).expect("present in scratch");
        assert!(
            resolved.starts_with(scratch.path()),
            "resolve_read must prefer the scratch candidate, got {resolved:?}"
        );
    }
}
