// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Single source of truth for env-var template resolution.
//!
//! `EnvResolver` resolves one `Var` at a time into an optional `Entry`
//! (key, value, kind). It performs the template expansion and (for path
//! modifiers) the required-path validation that previously lived duplicated
//! across `Accumulator::resolve_var` and `Exporter::resolve_var`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::dep_context::DependencyContext;
use super::entry::Entry;
use super::modifier::ModifierKind;
use super::var::{Modifier, Var};
use crate::package::metadata::{dependency::DependencyName, template::TemplateResolver};

/// Resolves package metadata env-var templates against an install path and
/// a dependency context map.
///
/// Borrowing form (`&'a Path`, `&'a HashMap`) means callers do not pay a
/// `PathBuf` clone or a context map clone per resolution.
pub struct EnvResolver<'a> {
    install_path: &'a Path,
    dep_contexts: &'a HashMap<DependencyName, DependencyContext>,
}

impl<'a> EnvResolver<'a> {
    pub fn new(install_path: &'a Path, dep_contexts: &'a HashMap<DependencyName, DependencyContext>) -> Self {
        Self {
            install_path,
            dep_contexts,
        }
    }

    /// Resolves a single `Var` into an [`Entry`].
    ///
    /// Returns `Ok(None)` when the var carries no template value (rare —
    /// captures the existing semantics of `Var::value() -> Option<&str>`).
    /// For path modifiers, validates that a `required` path exists on disk.
    ///
    /// # Errors
    ///
    /// - [`crate::package::error::Error::EnvVarInterpolation`] on template
    ///   resolution failure (unknown dep ref, unknown field, dep not installed).
    /// - [`crate::package::error::Error::RequiredPathMissing`] when a
    ///   `required` path-modifier resolves to a path that does not exist.
    pub fn resolve(&self, var: &Var) -> crate::Result<Option<Entry>> {
        let Some(template) = var.value() else {
            return Ok(None);
        };

        let resolver = TemplateResolver::new(self.install_path, self.dep_contexts);
        let mut value =
            resolver
                .resolve(template)
                .map_err(|source| crate::package::error::Error::EnvVarInterpolation {
                    var_key: var.key.clone(),
                    source,
                })?;

        if let Modifier::Path(path_modifier) = &var.modifier {
            let mut path = PathBuf::from(&value);
            if path.is_relative() {
                path = self.install_path.join(path);
            }
            // Sync `path.exists()` is intentional: the entire env-resolution
            // chain is synchronous and called many times per command
            // invocation. A single `stat(2)` against an already-installed
            // package's local content tree is a fast filesystem probe;
            // wrapping in `block_in_place` per call would add scheduler
            // overhead that dominates the probe itself. Switch to
            // `tokio::fs::try_exists` only if the chain becomes async
            // end-to-end.
            //
            // Strip the Windows `\\?\` verbatim prefix before the existence
            // check and before writing the value into the child env.
            //
            // `install_path` (or a `self.install_path.join(relative)` result)
            // may carry the `\\?\` prefix when the path originated from
            // `tokio::fs::canonicalize` (which returns verbatim paths on
            // Windows).  Relative metadata values (`bin` without `${installPath}`)
            // reach this branch via the `join` above and inherit the prefix.
            // `dunce::simplified` converts `\\?\C:\foo` → `C:\foo` so both the
            // `path.exists()` probe and the exported string use the normal DOS
            // form, which Windows path APIs handle correctly in all contexts.
            // On POSIX and non-verbatim Windows paths the call is a no-op.
            let path = PathBuf::from(dunce::simplified(&path));
            if path_modifier.required && !path.exists() {
                return Err(crate::package::error::Error::RequiredPathMissing(path).into());
            }
            value = path.to_string_lossy().to_string();
        }

        Ok(Some(Entry {
            key: var.key.clone(),
            value,
            kind: ModifierKind::from(&var.modifier),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;
    use crate::package::error::Error as PackageError;
    use crate::package::metadata::dependency::DependencyName;
    use crate::package::metadata::env::var::Var;
    use crate::package::metadata::template::TemplateError;
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn pinned(repo: &str) -> oci::PinnedIdentifier {
        let hex = "a".repeat(64);
        let id: oci::Identifier = format!("ocx.sh/{repo}:1.0@sha256:{hex}").parse().unwrap();
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    fn ctx(dir: &TempDir, repo: &str) -> DependencyContext {
        DependencyContext::path_only(pinned(repo), dir.path().to_path_buf())
    }

    fn dep_name(s: &str) -> DependencyName {
        DependencyName::try_from(s).unwrap()
    }

    fn constant_var(key: &str, value: &str) -> Var {
        Var::new_constant(key, value)
    }

    /// Resolve a single var via [`EnvResolver::resolve`] and return its
    /// resolved value (`None` when the var has no template).
    fn resolve(
        dep_contexts: &HashMap<DependencyName, DependencyContext>,
        install_path: &std::path::Path,
        var: &Var,
    ) -> crate::Result<Option<String>> {
        let resolver = EnvResolver::new(install_path, dep_contexts);
        Ok(resolver.resolve(var)?.map(|entry| entry.value))
    }

    /// `${deps.NAME.installPath}` expands against the matching context.
    #[test]
    fn dep_install_path_expands() {
        let dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(dep_name("python"), ctx(&dir, "python"));

        let var = constant_var("MY_PYTHON", "${deps.python.installPath}/bin/python");
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(result, format!("{}/bin/python", dir.path().display()));
    }

    /// `${installPath}` and `${deps.NAME.installPath}` mix in one value.
    #[test]
    fn mixed_install_path_and_dep_install_path() {
        let dir = TempDir::new().unwrap();
        let cmake_dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("cmake"),
            DependencyContext::path_only(pinned("cmake"), cmake_dir.path().to_path_buf()),
        );

        let template = "${installPath}:${deps.cmake.installPath}/bin".to_string();
        let var = constant_var("MIXED", &template);
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(
            result,
            format!("{}:{}/bin", dir.path().display(), cmake_dir.path().display())
        );
    }

    /// Multiple `${deps.*}` tokens in one value.
    #[test]
    fn multiple_dep_tokens_in_one_value() {
        let dir = TempDir::new().unwrap();
        let cmake_dir = TempDir::new().unwrap();
        let python_dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("cmake"),
            DependencyContext::path_only(pinned("cmake"), cmake_dir.path().to_path_buf()),
        );
        ctxs.insert(
            dep_name("python"),
            DependencyContext::path_only(pinned("python"), python_dir.path().to_path_buf()),
        );

        let template = "${deps.cmake.installPath}/bin:${deps.python.installPath}/bin";
        let var = constant_var("PATH_BOTH", template);
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(
            result,
            format!("{}/bin:{}/bin", cmake_dir.path().display(), python_dir.path().display())
        );
    }

    /// Unknown NAME → `UnknownDependencyRef`.
    #[test]
    fn unknown_dep_name_returns_error() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();

        let var = constant_var("X", "${deps.nonexistent.installPath}");
        let err = resolve(&ctxs, dir.path(), &var).unwrap_err();
        assert!(
            matches!(&err, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UnknownDependencyRef { ref_name, .. }, ..
                } if ref_name.as_str() == "nonexistent"
            )),
            "unexpected error: {err}"
        );
    }

    /// Unsupported field → `UnknownDependencyField`.
    #[test]
    fn unsupported_field_returns_error() {
        let dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(dep_name("cmake"), ctx(&dir, "cmake"));

        let var = constant_var("X", "${deps.cmake.version}");
        let err = resolve(&ctxs, dir.path(), &var).unwrap_err();
        assert!(
            matches!(&err, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UnknownDependencyField { field, .. }, ..
                } if field == "version"
            )),
            "unexpected error: {err}"
        );
    }

    /// Dep context present but content path missing → `DependencyNotInstalled`.
    #[test]
    fn dep_not_installed_returns_error() {
        let dir = TempDir::new().unwrap();
        let missing_path = dir.path().join("not-there");
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("cmake"),
            DependencyContext::path_only(pinned("cmake"), missing_path),
        );

        let var = constant_var("X", "${deps.cmake.installPath}");
        let err = resolve(&ctxs, dir.path(), &var).unwrap_err();
        assert!(
            matches!(&err, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::DependencyNotInstalled { ref_name, .. }, ..
                } if ref_name.as_str() == "cmake"
            )),
            "unexpected error: {err}"
        );
    }

    /// Uppercase NAME is not matched by the lowercase-only regex — token
    /// passes through as literal text.
    #[test]
    fn uppercase_dep_name_not_matched() {
        let dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(dep_name("python"), ctx(&dir, "python"));

        let var = constant_var("X", "${deps.Python.installPath}");
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(result, "${deps.Python.installPath}");
    }

    /// Transitive dep absent from `dep_contexts` → `UnknownDependencyRef`.
    #[test]
    fn transitive_dep_not_in_contexts() {
        let dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(dep_name("cmake"), ctx(&dir, "cmake"));

        let var = constant_var("X", "${deps.transitive-tool.installPath}");
        let err = resolve(&ctxs, dir.path(), &var).unwrap_err();
        assert!(
            matches!(&err, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UnknownDependencyRef { ref_name, declared, .. }, ..
                } if ref_name.as_str() == "transitive-tool"
                    && declared.iter().any(|n| n.as_str() == "cmake")
            )),
            "unexpected error: {err}"
        );
    }

    // ── Regression: Windows verbatim prefix (`\\?\`) must not appear in
    //                composed path-modifier values ──────────────────────────
    //
    // Root cause: on Windows, `tokio::fs::canonicalize` returns paths with a
    // `\\?\` extended-length prefix.  When `install_path` carries that prefix
    // and the metadata template contains a forward-slash suffix
    // (e.g. `${installPath}/bin`), plain string substitution produced
    // `\\?\C:\…\content/bin`.  Windows disables all path normalization for
    // `\\?\`-prefixed paths, so the `/` was treated as a literal filename
    // character, making the path un-resolvable → `RequiredPathMissing` (os
    // error "required path does not exist").
    //
    // The fix uses `dunce::simplified` to strip `\\?\` before string
    // substitution.  On Linux this is a no-op, so the test documents and
    // proves the contract on every CI platform: the composed value must not
    // contain a `\\?\` prefix followed by a forward slash.

    /// Path-modifier resolution with a `\\?\`-style verbatim install path
    /// must strip the verbatim prefix.
    ///
    /// On Windows this reproduces the pre-fix bug where `${installPath}/bin`
    /// with a `\\?\C:\…` install_path produced `\\?\C:\…/bin` — a path that
    /// Windows path APIs cannot resolve because `\\?\` disables normalization.
    ///
    /// The test constructs a synthetic verbatim-style path string (works on
    /// Linux too: `dunce::simplified` is a no-op on non-verbatim paths, so the
    /// output just echoes the input — proving the positive case).  On Windows
    /// (CI leg), the pre-fix code would have failed the `path.exists()` check
    /// and raised `RequiredPathMissing`; post-fix, `dunce::simplified` converts
    /// the path to regular DOS form before any check.
    ///
    /// For the Linux-meaningful assertion: the resolved constant value must not
    /// contain a `\\?\` prefix with a `/` immediately after it — that pattern
    /// is always a mixed-separator bug regardless of platform.
    #[test]
    fn path_modifier_value_does_not_retain_verbatim_prefix_with_forward_slash() {
        // Use a real tempdir for the install_path so the path actually exists on
        // disk — this lets us exercise `required = false` without a false-positive
        // "required path missing" error on Linux, and lets Windows CI run `path.exists()`.
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();

        // Constant modifier: template expansion only (no required-path check).
        // This directly tests that the string produced by template substitution
        // does not carry a mixed `\\?\...<backslash>/bin` shape.
        let var = constant_var("MY_BIN", "${installPath}/bin");
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();

        // The composed value must not contain a verbatim prefix immediately
        // followed by a forward slash — that is the exact mix that breaks
        // Windows path APIs.
        assert!(
            !result.contains(r"\\?\") || !result.contains('/'),
            "composed path must not mix Windows verbatim prefix with forward slash; got: {result:?}\n\
             pre-fix regression: dunce::simplified must be called on install_path before string substitution"
        );

        // The resolved value must end with the correct platform path separator
        // followed by `bin` — not a forward slash on Windows.
        assert!(
            result.ends_with("bin"),
            "composed path must end with 'bin' (platform-native separator before it); got: {result:?}"
        );
    }

    /// Path-modifier (`required = false`) with a verbatim-style install path
    /// must not fail the `path.exists()` check due to mixed separators.
    ///
    /// This is the direct guard for the `required = true` production path:
    /// `required = false` lets us inspect the returned value without the
    /// existence check gating the test.  A separate test for `required = true`
    /// would need the directory to actually exist, which `bin/` inside a fresh
    /// TempDir does not.
    #[test]
    fn path_modifier_non_required_returns_composed_value_without_verbatim_prefix() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();

        let var = Var::new_path("BIN_PATH", "${installPath}/bin", /* required = */ false);
        let resolver = EnvResolver::new(dir.path(), &ctxs);
        let entry = resolver.resolve(&var).unwrap().unwrap();

        // The exported value must not start with `\\?\`.
        assert!(
            !entry.value.starts_with(r"\\?\"),
            "exported path-modifier value must not start with Windows verbatim prefix \\\\?\\; \
             got: {:?}\npre-fix regression: dunce::simplified must normalize the path before export",
            entry.value
        );

        // Must end with the 'bin' component.
        assert!(
            entry.value.ends_with("bin"),
            "exported path-modifier value must end with 'bin'; got: {:?}",
            entry.value
        );
    }
}
