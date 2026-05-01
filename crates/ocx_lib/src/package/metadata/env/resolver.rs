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
}
