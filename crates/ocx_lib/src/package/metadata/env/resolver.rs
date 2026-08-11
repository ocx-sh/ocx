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
use super::list;
use super::modifier::ModifierKind;
use super::var::{Modifier, Var};
use crate::package::metadata::{
    dependency::DependencyName,
    template::{SelfEnvScope, TemplateResolver},
};

/// Whether the package being resolved has a materialized content tree on disk.
///
/// The only thing this decides is whether a `required` path modifier fires its
/// existence probe. That probe validates an **installed** tree; a deferred tool
/// (plan contract C-013, [#302](https://github.com/ocx-sh/ocx/issues/302)) has
/// none by construction, so its premise does not hold and it is suppressed —
/// otherwise a package declaring `required: true` would fail compose on a cold
/// store and succeed on a warm one, which is exactly the content-cache
/// dependence C-013 and S-005 assert is absent. The check is not lost: the
/// first invocation materializes through the ordinary install path and composes
/// again with `content/` present.
///
/// A named type rather than a `bool` parameter deliberately — the two states
/// are a domain fact, and `EnvResolver::with_content_state(false)` would say
/// nothing at a call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentState {
    /// The package directory exists; a `required` path is genuinely required.
    Materialized,
    /// The package is composed from ref-linked config blobs and will
    /// materialize on first invocation; nothing under `content/` exists yet.
    Deferred,
}

/// Resolves package metadata env-var templates against an install path and
/// a dependency context map.
///
/// Borrowing form (`&'a Path`, `&'a HashMap`) means callers do not pay a
/// `PathBuf` clone or a context map clone per resolution.
pub struct EnvResolver<'a> {
    install_path: &'a Path,
    dep_contexts: &'a HashMap<DependencyName, DependencyContext>,
    content_state: ContentState,
}

impl<'a> EnvResolver<'a> {
    pub fn new(install_path: &'a Path, dep_contexts: &'a HashMap<DependencyName, DependencyContext>) -> Self {
        Self {
            install_path,
            dep_contexts,
            content_state: ContentState::Materialized,
        }
    }

    /// Declares whether the package's content tree exists, returning `self` for
    /// chaining after [`new`](Self::new).
    ///
    /// Only the lazy compose path passes [`ContentState::Deferred`]; every
    /// other caller composes an installed package and keeps the default.
    #[must_use]
    pub fn with_content_state(mut self, content_state: ContentState) -> Self {
        self.content_state = content_state;
        self
    }

    /// Resolves a single `Var` into an [`Entry`] **that the caller is going to
    /// emit**, so every filesystem and shape assertion runs.
    ///
    /// `self_env` is the scope `${self.env.KEY}` resolves against: the entries
    /// this package's strictly-earlier vars already resolved to, in declaration
    /// order (D6.1). Pass an empty [`SelfEnvScope`] where none exists.
    ///
    /// Returns `Ok(None)` when the var carries no template value (rare —
    /// captures the existing semantics of `Var::value() -> Option<&str>`).
    /// For path modifiers, validates that a `required` path exists on disk.
    ///
    /// # Errors
    ///
    /// - [`crate::package::error::Error::EnvVarInterpolation`] on template
    ///   resolution failure (unknown dep ref, unknown field, dep not installed,
    ///   undefined or ambiguous `${self.env.*}` reference).
    /// - [`crate::package::error::Error::RequiredPathMissing`] when a
    ///   `required` path-modifier resolves to a path that does not exist —
    ///   suppressed under [`ContentState::Deferred`], whose premise is that no
    ///   content tree exists yet.
    /// - [`crate::package::error::Error::SeparatorEdgedListValue`] when a
    ///   list-modifier value *resolves* to one edged by its own separator.
    pub fn resolve(&self, var: &Var, self_env: &SelfEnvScope<Entry>) -> crate::Result<Option<Entry>> {
        self.resolve_inner(var, self_env, /* emit_assertions = */ true)
    }

    /// The one resolution path; `emit_assertions` is D8's split.
    ///
    /// Both public entry points route through here, so the only thing that can
    /// differ between them is which assertions run. The *value* a var resolves
    /// to is the same either way — which is what lets `${self.env.KEY}` see
    /// identical bytes whether or not the var it names crosses the surface.
    fn resolve_inner(
        &self,
        var: &Var,
        self_env: &SelfEnvScope<Entry>,
        emit_assertions: bool,
    ) -> crate::Result<Option<Entry>> {
        let Some(template) = var.value() else {
            return Ok(None);
        };

        let resolver = TemplateResolver::new(self.install_path, self.dep_contexts).with_self_env(self_env);
        let resolved = if emit_assertions {
            resolver.resolve(template)
        } else {
            resolver.resolve_without_existence_checks(template)
        };
        let mut value = resolved.map_err(|source| crate::package::error::Error::EnvVarInterpolation {
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
            // Also suppressed for a deferred package: see [`ContentState`].
            if emit_assertions
                && self.content_state == ContentState::Materialized
                && path_modifier.required
                && !path.exists()
            {
                return Err(crate::package::error::Error::RequiredPathMissing(path).into());
            }
            value = path.to_string_lossy().to_string();
        }

        // A second separator-edge check, on the resolved bytes. The parse
        // boundaries see the authored template, and `${installPath}` with
        // separator `/` resolves to a `/`-edged value none of them could have
        // seen — one that makes the fold's flank match ambiguous.
        let separator = if let Modifier::List(list_modifier) = &var.modifier {
            // `None` is refused for package metadata by `ValidMetadata`, which
            // runs on every load path; the fallback is what the human-facing
            // surfaces authored through.
            let separator = list_modifier.separator.as_deref().unwrap_or(list::DEFAULT_SEPARATOR);
            if emit_assertions && list::is_separator_edged(&value, separator) {
                return Err(crate::package::error::Error::SeparatorEdgedListValue {
                    key: var.key.clone(),
                    separator: separator.to_string(),
                    value,
                }
                .into());
            }
            list_modifier.separator.clone()
        } else {
            None
        };

        Ok(Some(Entry {
            key: var.key.clone(),
            value,
            // `Var::value()` above returns `None` for an unknown modifier type,
            // so this point is unreachable for one; `ValidMetadata` has also
            // already refused it on every load path.
            kind: ModifierKind::try_from(&var.modifier)
                .expect("a var with a resolvable value template names a known modifier kind"),
            separator,
        }))
    }

    /// Resolves a single `Var` the caller is **not** going to emit — the value
    /// only, with every filesystem and shape assertion suppressed.
    ///
    /// The composer resolves a package's whole `env` array so that a crossing
    /// var can reference a non-crossing earlier one (D8), which means vars
    /// nobody emits now resolve too. The split is stated as a rule, not a list:
    /// *value resolution always; every filesystem and shape assertion on emit
    /// only.* Concretely, relative to [`EnvResolver::resolve`] this suppresses
    /// [`crate::package::error::Error::RequiredPathMissing`] (C-026),
    /// [`crate::package::error::Error::SeparatorEdgedListValue`] — both
    /// assertions about a contribution that never joins a fold — and, through
    /// [`TemplateResolver::resolve_without_existence_checks`],
    /// [`crate::package::metadata::template::TemplateError::DependencyNotInstalled`]
    /// (C-027), which would otherwise turn a working install into exit 79 over a
    /// value nobody reads.
    ///
    /// A template *fault* is not suppressed: an unrecognised token or an unknown
    /// dep reference still fails here, because a package whose own metadata
    /// cannot resolve is broken regardless of who is looking.
    ///
    /// # Errors
    ///
    /// [`crate::package::error::Error::EnvVarInterpolation`] on template
    /// resolution failure.
    pub fn resolve_without_emit_assertions(
        &self,
        var: &Var,
        self_env: &SelfEnvScope<Entry>,
    ) -> crate::Result<Option<Entry>> {
        self.resolve_inner(var, self_env, /* emit_assertions = */ false)
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

    /// The `${self.env.KEY}` scope, built the way the composer builds it: the
    /// entries earlier vars already resolved to, in declaration order.
    fn scope_of<const N: usize>(declared: [Entry; N]) -> SelfEnvScope<Entry> {
        declared.into_iter().collect()
    }

    fn constant_var(key: &str, value: &str) -> Var {
        Var::new_constant(key, value)
    }

    fn list_var(key: &str, value: &str, separator: &str) -> Var {
        Var {
            key: key.to_string(),
            modifier: Modifier::List(list::List {
                separator: Some(separator.to_string()),
                value: value.to_string(),
            }),
            visibility: crate::package::metadata::visibility::Visibility::PRIVATE,
        }
    }

    /// Resolve a single var via [`EnvResolver::resolve`] and return its
    /// resolved value (`None` when the var has no template).
    fn resolve(
        dep_contexts: &HashMap<DependencyName, DependencyContext>,
        install_path: &std::path::Path,
        var: &Var,
    ) -> crate::Result<Option<String>> {
        let resolver = EnvResolver::new(install_path, dep_contexts);
        Ok(resolver.resolve(var, &SelfEnvScope::new())?.map(|entry| entry.value))
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

    /// Unsupported field under a recognised namespace → `UnknownField`, naming
    /// the namespace as well as the leaf (D12: one variant for every namespace,
    /// not one per namespace).
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
                    source: TemplateError::UnknownField { namespace, field, .. }, ..
                } if field == "version" && namespace == "deps.cmake"
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

    /// Uppercase NAME fails the anchored `NAME` grammar, and OCX claims every
    /// `${…}` (D3) — so the token is refused, not emitted as literal text.
    ///
    /// Inverted from the pre-grammar behaviour, where an unmatched token
    /// silently reached the consuming tool as the eight characters
    /// `${deps.P…}` and failed there instead.
    #[test]
    fn uppercase_dep_name_is_refused_not_passed_through() {
        let dir = TempDir::new().unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(dep_name("python"), ctx(&dir, "python"));

        let var = constant_var("X", "${deps.Python.installPath}");
        let err = resolve(&ctxs, dir.path(), &var).unwrap_err();
        assert!(
            matches!(&err, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UnknownToken { token, .. }, ..
                } if token == "${deps.Python.installPath}"
            )),
            "unexpected error: {err}"
        );
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

    /// C-006(b) / S-023 — the escape defends against OCX's scanner, not against
    /// the layers above it. A `path` var whose escaped value resolves to
    /// literal bytes is still a *relative* path, so the resolver joins it under
    /// the install path exactly as it would any other relative value.
    ///
    /// The sibling leg is C-006(a) in `template.rs`: the same value in a
    /// `constant` var is byte-identical end to end. Together they pin that what
    /// the escape produces is ordinary resolved bytes, with no pass-through
    /// promise attached.
    #[test]
    fn an_escaped_foreign_token_in_a_path_var_is_joined_under_the_install_path() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();

        let var = Var::new_path("TOOL_DIR", "$${workspaceFolder}/x", false);
        let resolved = resolve(&ctxs, dir.path(), &var).unwrap().unwrap();
        assert_eq!(
            resolved,
            dir.path().join("${workspaceFolder}/x").to_string_lossy(),
            "an escaped token resolves to a relative value, which a path var joins under the install path"
        );
    }

    // ── W-4: list values are re-checked after template resolution ─────────

    /// The resolved entry carries the separator the fold needs.
    #[test]
    fn a_list_var_resolves_into_an_entry_carrying_its_separator() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();

        let resolver = EnvResolver::new(dir.path(), &ctxs);
        let entry = resolver
            .resolve(&list_var("GODEBUG", "gctrace=1", ","), &SelfEnvScope::new())
            .unwrap()
            .expect("a list var resolves to an entry");
        assert_eq!(entry.kind, ModifierKind::List);
        assert_eq!(entry.separator.as_deref(), Some(","));
        assert_eq!(entry.value, "gctrace=1");
    }

    /// The parse gates only ever see the authored template, which here carries
    /// no comma at all — the comma arrives from the dependency's install path.
    /// A gate reading the authored bytes cannot catch that, so the resolver
    /// checks again on the resolved ones.
    #[test]
    fn a_value_resolving_to_a_separator_edged_one_is_refused() {
        let dir = TempDir::new().unwrap();
        let edged = dir.path().join("opts,");
        std::fs::create_dir(&edged).unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("tool"),
            DependencyContext::path_only(pinned("tool"), edged.clone()),
        );

        let resolver = EnvResolver::new(dir.path(), &ctxs);
        let error = resolver
            .resolve(
                &list_var("PARTS", "${deps.tool.installPath}", ","),
                &SelfEnvScope::new(),
            )
            .expect_err("a resolved value edged by its separator must be refused");
        let message = error.to_string();
        assert!(message.contains("PARTS"), "must name the var: {message}");
        assert!(
            message.contains(&edged.to_string_lossy().to_string()),
            "must show the resolved value, which is what the parse gate could not see: {message}"
        );
    }

    /// The same template with a separator that does not edge the resolved
    /// value passes — the check keys on the separator, not on the path shape.
    #[test]
    fn a_resolved_value_not_edged_by_its_separator_passes() {
        let dir = TempDir::new().unwrap();
        let edged = dir.path().join("opts,");
        std::fs::create_dir(&edged).unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("tool"),
            DependencyContext::path_only(pinned("tool"), edged.clone()),
        );

        let resolver = EnvResolver::new(dir.path(), &ctxs);
        let entry = resolver
            .resolve(
                &list_var("PARTS", "${deps.tool.installPath}", " "),
                &SelfEnvScope::new(),
            )
            .unwrap()
            .expect("a space separator does not edge this value");
        assert_eq!(entry.value, edged.to_string_lossy());
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
        let entry = resolver.resolve(&var, &SelfEnvScope::new()).unwrap().unwrap();

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

    // ── `${self.env.KEY}` reads the referenced var's resolved Entry (D6.2) ────

    /// C-022 — a token-bearing `path` var referenced through `${self.env.*}`
    /// yields its own single resolved contribution. Never a folded `PATH`:
    /// folding happens later, against the ambient environment, and would make
    /// a published artifact's resolution machine-dependent.
    #[test]
    fn a_self_env_reference_to_a_token_bearing_path_var_yields_one_contribution() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = EnvResolver::new(dir.path(), &ctxs);

        let contribution = resolver
            .resolve(&Var::new_path("P", "${installPath}/bin", false), &SelfEnvScope::new())
            .unwrap()
            .expect("a path var resolves to an entry");
        let referencing = resolver
            .resolve(&constant_var("Q", "${self.env.P}"), &scope_of([contribution.clone()]))
            .unwrap()
            .expect("a constant var resolves to an entry");

        assert_eq!(
            referencing.value, contribution.value,
            "the reference must yield the referenced var's own resolved contribution"
        );
        assert!(
            contribution.value.starts_with(&*dir.path().to_string_lossy()) && contribution.value.ends_with("bin"),
            "the contribution must be the install-rooted bin directory: {:?}",
            contribution.value
        );
    }

    /// C-023 — the referenced var's declared **type** decides the bytes, and
    /// the value alone cannot show it: a bare-relative `path` var is joined
    /// under the install path before the value is taken, a `constant` with the
    /// identical value is not.
    ///
    /// The two legs are one check: either alone is satisfied by an
    /// implementation that ignores the type.
    #[test]
    fn a_self_env_reference_carries_the_referenced_vars_type_not_just_its_value() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = EnvResolver::new(dir.path(), &ctxs);

        let path_contribution = resolver
            .resolve(&Var::new_path("P", "bin", false), &SelfEnvScope::new())
            .unwrap()
            .expect("a path var resolves to an entry");
        let via_path = resolver
            .resolve(
                &constant_var("VIA_PATH", "${self.env.P}"),
                &scope_of([path_contribution.clone()]),
            )
            .unwrap()
            .expect("a constant var resolves to an entry");

        let constant_contribution = resolver
            .resolve(&constant_var("C", "bin"), &SelfEnvScope::new())
            .unwrap()
            .expect("a constant var resolves to an entry");
        let via_constant = resolver
            .resolve(
                &constant_var("VIA_CONSTANT", "${self.env.C}"),
                &scope_of([constant_contribution.clone()]),
            )
            .unwrap()
            .expect("a constant var resolves to an entry");

        assert_eq!(
            via_constant.value, "bin",
            "a constant contributes its value verbatim, so the reference does too"
        );
        assert_eq!(
            via_path.value,
            dir.path().join("bin").to_string_lossy(),
            "a bare-relative path var contributes an install-rooted path, so the reference does too"
        );
        assert_ne!(
            via_path.value, via_constant.value,
            "the same authored value under two types must not resolve to the same bytes"
        );
    }

    // ── D8's split at the resolver: assertions on emit only ───────────────────

    /// C-026 (resolver leg) — the emitting entry point asserts a `required`
    /// path exists; the non-emitting one resolves the identical var without
    /// the assertion.
    ///
    /// Both legs are required. The suppressing leg alone passes if the
    /// existence check is deleted outright; the asserting leg alone says
    /// nothing about the split.
    #[test]
    fn a_missing_required_path_is_asserted_only_where_the_value_is_emitted() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = EnvResolver::new(dir.path(), &ctxs);
        let var = Var::new_path("TOOL_DIR", "${installPath}/absent", /* required = */ true);

        let error = resolver
            .resolve(&var, &SelfEnvScope::new())
            .expect_err("an emitted required path must be asserted to exist");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(), PackageError::RequiredPathMissing(_))),
            "unexpected error: {error}"
        );

        let entry = resolver
            .resolve_without_emit_assertions(&var, &SelfEnvScope::new())
            .expect("a value nobody emits must resolve without the existence assertion")
            .expect("a path var resolves to an entry");
        assert!(
            entry.value.ends_with("absent"),
            "the value must still resolve, only the assertion is suppressed: {:?}",
            entry.value
        );
    }

    /// C-027 (resolver leg) — a declared-but-uninstalled dependency must not
    /// turn a working install into exit 79 over a value nobody reads. The
    /// asserting sibling is `dep_not_installed_returns_error` above, over the
    /// same fixture shape.
    #[test]
    fn an_uninstalled_dependency_is_tolerated_where_the_value_is_not_emitted() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("not-there");
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("cmake"),
            DependencyContext::path_only(pinned("cmake"), missing.clone()),
        );
        let resolver = EnvResolver::new(dir.path(), &ctxs);

        let entry = resolver
            .resolve_without_emit_assertions(
                &constant_var("X", "${deps.cmake.installPath}/bin"),
                &SelfEnvScope::new(),
            )
            .expect("an uninstalled dependency must not fail a value nobody emits")
            .expect("a constant var resolves to an entry");
        assert_eq!(entry.value, format!("{}/bin", missing.to_string_lossy()));
    }

    /// D8 / OQ-3 — the separator edge is a shape assertion about a
    /// contribution that joins a fold, and a var nobody emits joins none. The
    /// asserting sibling is
    /// `a_value_resolving_to_a_separator_edged_one_is_refused` above, over the
    /// same fixture.
    #[test]
    fn a_separator_edged_resolved_value_is_refused_only_where_it_is_emitted() {
        let dir = TempDir::new().unwrap();
        let edged = dir.path().join("opts,");
        std::fs::create_dir(&edged).unwrap();
        let mut ctxs = HashMap::new();
        ctxs.insert(
            dep_name("tool"),
            DependencyContext::path_only(pinned("tool"), edged.clone()),
        );
        let resolver = EnvResolver::new(dir.path(), &ctxs);

        let entry = resolver
            .resolve_without_emit_assertions(
                &list_var("PARTS", "${deps.tool.installPath}", ","),
                &SelfEnvScope::new(),
            )
            .expect("a value nobody folds must not be checked against the fold's flank rule")
            .expect("a list var resolves to an entry");
        assert_eq!(entry.value, edged.to_string_lossy());
    }

    /// D8 — the split suppresses **assertions**, never **faults**. A package
    /// whose own metadata cannot resolve is broken regardless of who is
    /// looking, so an unknown dependency reference still fails here.
    ///
    /// Without this leg the three suppression tests above are satisfied by a
    /// `resolve_without_emit_assertions` that returns `Ok` unconditionally.
    #[test]
    fn resolving_without_emit_assertions_still_refuses_a_template_fault() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = EnvResolver::new(dir.path(), &ctxs);

        let error = resolver
            .resolve_without_emit_assertions(
                &constant_var("X", "${deps.nonexistent.installPath}"),
                &SelfEnvScope::new(),
            )
            .expect_err("a template fault is not an emit-time assertion");
        assert!(
            matches!(&error, crate::Error::Package(e) if matches!(e.as_ref(),
                PackageError::EnvVarInterpolation {
                    source: TemplateError::UnknownDependencyRef { ref_name, .. }, ..
                } if ref_name.as_str() == "nonexistent"
            )),
            "unexpected error: {error}"
        );
    }

    /// The self-env scope reaches the template resolver through the
    /// non-emitting entry point too — a non-crossing var may itself reference
    /// an earlier one (D8 resolves the package's *whole* env array).
    #[test]
    fn the_self_env_scope_reaches_the_non_emitting_entry_point_as_well() {
        let dir = TempDir::new().unwrap();
        let ctxs: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = EnvResolver::new(dir.path(), &ctxs);

        let scope = scope_of([Entry {
            key: "A".to_string(),
            value: "alpha".to_string(),
            kind: ModifierKind::Constant,
            separator: None,
        }]);
        let entry = resolver
            .resolve_without_emit_assertions(&constant_var("B", "${self.env.A}/x"), &scope)
            .unwrap()
            .expect("a constant var resolves to an entry");
        assert_eq!(entry.value, "alpha/x");
    }
}
