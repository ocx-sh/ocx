// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::package::install_info::InstallInfo;
use crate::package::metadata::dependency::DependencyName;
use crate::package::metadata::template::TemplateResolver;
use crate::{Result, env, oci};

use super::var;

/// Resolved context for a single direct dependency, available during env interpolation.
///
/// Keyed in `Accumulator::dep_contexts` by `dep.name()` (the explicit name or repository basename).
/// Backed by `Arc<InstallInfo>` so that future fields like `${deps.NAME.version}` and
/// `${deps.NAME.digest}` become additive match arms in `resolve_field` without changing
/// any upstream signatures.
///
/// # Reserved
///
/// A `resolved_env` field for per-dep resolved environment data is reserved for a future
/// extension and is not present yet.
#[derive(Debug, Clone)]
pub struct DependencyContext {
    install_info: Arc<InstallInfo>,
}

impl DependencyContext {
    /// Constructs a `DependencyContext` wrapping real install info.
    pub fn new(install_info: Arc<InstallInfo>) -> Self {
        Self { install_info }
    }

    /// Returns the underlying `Arc<InstallInfo>`.
    pub fn install_info(&self) -> &Arc<InstallInfo> {
        &self.install_info
    }

    /// Returns the absolute content path for this dependency (`packages/.../content/`).
    pub fn install_path(&self) -> &Path {
        &self.install_info.content
    }

    /// Returns the full pinned OCI identifier for this dependency.
    pub fn identifier(&self) -> &oci::PinnedIdentifier {
        &self.install_info.identifier
    }

    /// Resolves a named field to a string value.
    ///
    /// Currently supports `"installPath"` only. Future fields (`"version"`, `"digest"`)
    /// will become additional match arms here without signature churn in callers.
    pub fn resolve_field(&self, field: &str) -> Option<String> {
        match field {
            "installPath" => Some(self.install_path().to_string_lossy().into_owned()),
            _ => None,
        }
    }

    /// Constructs a context from an identifier and a content path only.
    ///
    /// Wraps a synthetic `InstallInfo` whose `content` is the supplied path and
    /// whose other fields use minimal placeholders. Used by call sites that have
    /// no full `InstallInfo` available — `validate_entrypoints` (publish-time
    /// sentinels) and `resolve.rs` (runtime, where only `${...installPath}`
    /// resolution is required). Future fields beyond `installPath` will require
    /// callers to supply a real `InstallInfo` via [`DependencyContext::new`].
    pub fn sentinel(identifier: oci::PinnedIdentifier, sentinel_path: PathBuf) -> Self {
        let info = InstallInfo::new_for_sentinel(identifier, sentinel_path);
        Self {
            install_info: Arc::new(info),
        }
    }
}

pub struct Accumulator<'a> {
    install_path: std::path::PathBuf,
    dep_contexts: &'a HashMap<DependencyName, DependencyContext>,
    env: &'a mut env::Env,
}

impl<'a> Accumulator<'a> {
    pub fn new(
        install_path: impl AsRef<std::path::Path>,
        dep_contexts: &'a HashMap<DependencyName, DependencyContext>,
        env: &'a mut crate::env::Env,
    ) -> Self {
        Accumulator {
            install_path: install_path.as_ref().to_path_buf(),
            dep_contexts,
            env,
        }
    }

    pub fn add(&mut self, var: &var::Var) -> Result<()> {
        let key = &var.key;
        let value = match self.resolve_var(var)? {
            Some(value) => value,
            None => return Ok(()),
        };

        match var.modifier {
            var::Modifier::Path(_) => {
                self.env.add_path(key, &value);
            }
            var::Modifier::Constant(_) => {
                self.env.set(key, value);
            }
        };
        Ok(())
    }

    pub fn resolve_var(&self, var: &var::Var) -> Result<Option<String>> {
        let value = match var.value() {
            Some(v) => v,
            None => return Ok(None),
        };

        let resolver = TemplateResolver::new(&self.install_path, self.dep_contexts);
        let mut value =
            resolver
                .resolve(value)
                .map_err(|source| crate::package::error::Error::EnvVarInterpolation {
                    var_key: var.key.clone(),
                    source,
                })?;

        if let var::Modifier::Path(path_modifier) = &var.modifier {
            let mut path = std::path::PathBuf::from(&value);
            if path.is_relative() {
                path = self.install_path.join(path);
            }
            if path_modifier.required && !path.exists() {
                return Err(crate::package::error::Error::RequiredPathMissing(path).into());
            }
            value = path.to_string_lossy().to_string();
        }

        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;
    use crate::package::error::Error as PackageError;
    use crate::package::metadata::template::TemplateError;
    use tempfile::TempDir;

    fn pinned(repo: &str) -> oci::PinnedIdentifier {
        let hex = "a".repeat(64);
        let id: oci::Identifier = format!("ocx.sh/{repo}:1.0@sha256:{hex}").parse().unwrap();
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    fn ctx(dir: &TempDir, repo: &str) -> DependencyContext {
        DependencyContext::sentinel(pinned(repo), dir.path().to_path_buf())
    }

    fn dep_name(s: &str) -> DependencyName {
        DependencyName::try_from(s).unwrap()
    }

    fn constant_var(key: &str, value: &str) -> crate::package::metadata::env::var::Var {
        crate::package::metadata::env::var::Var::new_constant(key, value)
    }

    fn resolve(
        dep_contexts: &HashMap<DependencyName, DependencyContext>,
        install_path: &std::path::Path,
        var: &crate::package::metadata::env::var::Var,
    ) -> crate::Result<Option<String>> {
        let mut env = crate::env::Env::clean();
        let acc = Accumulator::new(install_path, dep_contexts, &mut env);
        acc.resolve_var(var)
    }

    // 3.1 — ${deps.python.installPath}/bin/python expands correctly
    #[test]
    fn dep_install_path_expands() {
        let dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(dep_name("python"), ctx(&dir, "python"));

        let var = constant_var("MY_PYTHON", "${deps.python.installPath}/bin/python");
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(result, format!("{}/bin/python", dir.path().display()));
    }

    // 3.1 — ${installPath} and ${deps.cmake.installPath} in same value
    #[test]
    fn mixed_install_path_and_dep_install_path() {
        let dir = TempDir::new().unwrap();
        let cmake_dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("cmake"),
            DependencyContext::sentinel(pinned("cmake"), cmake_dir.path().to_path_buf()),
        );

        let template = "${installPath}:${deps.cmake.installPath}/bin".to_string();
        let var = constant_var("MIXED", &template);
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(
            result,
            format!("{}:{}/bin", dir.path().display(), cmake_dir.path().display())
        );
    }

    // 3.1 — multiple ${deps.*} tokens in one value
    #[test]
    fn multiple_dep_tokens_in_one_value() {
        let dir = TempDir::new().unwrap();
        let cmake_dir = TempDir::new().unwrap();
        let python_dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("cmake"),
            DependencyContext::sentinel(pinned("cmake"), cmake_dir.path().to_path_buf()),
        );
        ctxs.insert(
            dep_name("python"),
            DependencyContext::sentinel(pinned("python"), python_dir.path().to_path_buf()),
        );

        let template = "${deps.cmake.installPath}/bin:${deps.python.installPath}/bin";
        let var = constant_var("PATH_BOTH", template);
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(
            result,
            format!("{}/bin:{}/bin", cmake_dir.path().display(), python_dir.path().display())
        );
    }

    // 3.1 — unknown NAME → UnknownDependencyRef
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

    // 3.1 — unsupported field → UnknownDependencyField
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

    // 3.1 — dep_paths entry present but directory missing → DependencyNotInstalled
    #[test]
    fn dep_not_installed_returns_error() {
        let dir = TempDir::new().unwrap();
        let missing_path = dir.path().join("not-there");
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("cmake"),
            DependencyContext::sentinel(pinned("cmake"), missing_path),
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

    // 3.1 — uppercase NAME → error (basenames are lowercase, regex won't match)
    #[test]
    fn uppercase_dep_name_not_matched() {
        let dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(dep_name("python"), ctx(&dir, "python"));

        // ${deps.Python.installPath} — uppercase P doesn't match the pattern [a-z0-9...]
        let var = constant_var("X", "${deps.Python.installPath}");
        // Token is not matched by regex → treated as literal text, returned as-is.
        let result = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(result, "${deps.Python.installPath}");
    }

    // 3.3b — transitive dep not in dep_contexts → UnknownDependencyRef
    #[test]
    fn transitive_dep_not_in_contexts() {
        let dir = TempDir::new().unwrap();
        // dep_contexts contains only direct dep D; transitive dep T is not included
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

    // DependencyContext::sentinel — install_path() returns the supplied sentinel path.
    #[test]
    fn dependency_context_sentinel_resolves_install_path() {
        let sentinel_path = PathBuf::from("/__OCX_SENTINEL__");
        let ctx = DependencyContext::sentinel(pinned("cmake"), sentinel_path.clone());
        assert_eq!(ctx.install_path(), sentinel_path.as_path());
        assert_eq!(ctx.resolve_field("installPath").as_deref(), Some("/__OCX_SENTINEL__"),);
    }

    // DependencyContext::resolve_field — unknown fields return None.
    #[test]
    fn dependency_context_resolve_field_unknown_returns_none() {
        let dir = TempDir::new().unwrap();
        let ctx = DependencyContext::sentinel(pinned("cmake"), dir.path().to_path_buf());
        assert!(ctx.resolve_field("version").is_none());
        assert!(ctx.resolve_field("digest").is_none());
        assert!(ctx.resolve_field("").is_none());
    }
}
