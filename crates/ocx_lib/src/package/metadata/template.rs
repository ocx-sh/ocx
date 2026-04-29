// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Standalone template resolution for metadata string fields.
//!
//! Handles `${installPath}` and `${deps.NAME.FIELD}` substitution. Consumed by
//! `env::Accumulator` today and future metadata features (e.g., entry points).

use std::collections::HashMap;
use std::path::Path;

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci;

use super::env::accumulator::DependencyContext;
use super::slug::DEP_TOKEN_PATTERN;
use crate::package::metadata::dependency::DependencyName;

/// Resolves `${installPath}` and `${deps.NAME.FIELD}` tokens in template strings.
///
/// Build once with [`TemplateResolver::new`], call [`TemplateResolver::resolve`] for each
/// template string. Callers that iterate over many variables (e.g. `Exporter`) benefit from
/// building the resolver once and reusing it.
///
/// Two resolution modes are exposed:
/// - [`TemplateResolver::resolve`] — runtime mode. Substitutes real install paths and
///   verifies each dep's `install_path.exists()` on disk. Used by `env::Accumulator`
///   at install/exec time.
/// - [`TemplateResolver::resolve_for_validate`] — publish-time mode. Pure syntax +
///   reference-existence check, no filesystem access. Used by `validate_entrypoints`
///   so publish-time validation is platform-portable (no `Path::new("/")` sentinel).
pub struct TemplateResolver<'a> {
    install_path: &'a Path,
    dep_contexts: &'a HashMap<DependencyName, DependencyContext>,
}

// Every method on `TemplateResolver` returns `Result<_, TemplateError>`.
// `TemplateError` has large variants (Vec<DependencyName>, PinnedIdentifier);
// error paths are cold, so boxing the error to silence `result_large_err` would
// only add an allocation on the hot Ok-return path. Hoisted from per-fn allows
// to keep the rationale in one place — see Q15 in the entry-points review plan.
#[allow(clippy::result_large_err)]
impl<'a> TemplateResolver<'a> {
    pub fn new(install_path: &'a Path, dep_contexts: &'a HashMap<DependencyName, DependencyContext>) -> Self {
        Self {
            install_path,
            dep_contexts,
        }
    }

    /// Resolves `${installPath}` and `${deps.NAME.FIELD}` tokens in `template`.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError`] if a `${deps.*}` token references an unknown dependency name,
    /// an unsupported field, or a dependency that is not installed on disk.
    pub fn resolve(&self, template: &str) -> Result<String, TemplateError> {
        self.resolve_inner(template, /* check_exists = */ true)
    }

    /// Publish-time validation mode: substitutes tokens and checks references but does
    /// **not** consult the filesystem. Used by `validate_entrypoints` so validation
    /// is platform-portable — Windows publishers no longer rely on a `Path::new("/")`
    /// sentinel that may not exist on their host.
    ///
    /// `install_path` and the dep contexts' `install_path` fields are still substituted
    /// into the output (callers downstream of this method may inspect the resolved
    /// string for path-prefix containment), but no `exists()` probe is performed.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::UnknownDependencyRef`] / `UnknownDependencyField` when
    /// a `${deps.*}` token references something not declared. Never returns
    /// [`TemplateError::DependencyNotInstalled`].
    pub fn resolve_for_validate(&self, template: &str) -> Result<String, TemplateError> {
        self.resolve_inner(template, /* check_exists = */ false)
    }

    fn resolve_inner(&self, template: &str, check_exists: bool) -> Result<String, TemplateError> {
        // Step 1: substitute ${installPath}. Avoid the `String::replace` allocation
        // when the token isn't present — most templates contain only one of the two
        // forms (`${installPath}` xor `${deps.*}`) so the fast-path covers many calls.
        let value = if template.contains("${installPath}") {
            template.replace("${installPath}", &self.install_path.to_string_lossy())
        } else {
            // No substitution needed; defer the borrow→owned promotion until we know
            // step 2 won't short-circuit either.
            template.to_string()
        };

        // Step 2: substitute ${deps.NAME.FIELD} tokens. Regex shared with the
        // validator sites in `validation.rs` via `slug::DEP_TOKEN_PATTERN` so the
        // accepted character class stays in sync with DependencyName validation.
        if !value.contains("${deps.") {
            return Ok(value);
        }

        let mut result = String::with_capacity(value.len());
        let mut last = 0usize;

        for cap in DEP_TOKEN_PATTERN.captures_iter(&value) {
            // Invariant: DEP_TOKEN_PATTERN defines exactly 2 capture groups; a successful
            // captures_iter match guarantees groups 0–2 are Some.
            let m = cap.get(0).expect("regex group 0 is the full match, always present");
            result.push_str(&value[last..m.start()]);
            last = m.end();

            let dep_name = cap
                .get(1)
                .expect("regex group 1 guaranteed by DEP_TOKEN_PATTERN")
                .as_str();
            let field = cap
                .get(2)
                .expect("regex group 2 guaranteed by DEP_TOKEN_PATTERN")
                .as_str();

            // The regex only matches the slug pattern [a-z0-9][a-z0-9_-]* so every
            // captured dep_name is a structurally valid DependencyName by construction.
            let dep_name_typed =
                DependencyName::try_from(dep_name).expect("regex guarantees dep_name matches slug pattern");

            let ctx = self
                .dep_contexts
                .get(dep_name)
                .ok_or_else(|| TemplateError::UnknownDependencyRef {
                    ref_name: dep_name_typed.clone(),
                    declared: self.dep_contexts.keys().cloned().collect(),
                })?;

            let resolved = ctx
                .resolve_field(field)
                .ok_or_else(|| TemplateError::UnknownDependencyField {
                    ref_name: dep_name.to_string(),
                    field: field.to_string(),
                    supported_fields: vec!["installPath".to_string()],
                })?;

            // Sync `.exists()` is intentional: this method is the synchronous
            // template-resolution API consumed by `env::Accumulator` and the
            // entrypoint resolver (both call sites are themselves sync). The
            // probe is a single `stat(2)` against a path the caller has just
            // opened or is about to open, so its latency is bounded by the
            // local filesystem cache — not a candidate for `block_in_place`.
            if check_exists && !ctx.install_path().exists() {
                return Err(TemplateError::DependencyNotInstalled {
                    ref_name: dep_name_typed,
                    dep_identifier: ctx.identifier().clone(),
                });
            }

            result.push_str(&resolved);
        }

        result.push_str(&value[last..]);
        Ok(result)
    }
}

/// Errors produced during template string resolution.
///
/// These are template-level failures only — they carry no `var_key`. The wrapping error
/// variant [`crate::package::error::Error::EnvVarInterpolation`] adds the `var_key` context
/// for the env-variable layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TemplateError {
    /// A `${deps.NAME.*}` token names a dependency that is not declared.
    #[error(
        "references unknown dependency '{ref_name}'; declared: [{declared}]",
        declared = declared.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ")
    )]
    UnknownDependencyRef {
        ref_name: DependencyName,
        declared: Vec<DependencyName>,
    },

    /// A `${deps.NAME.FIELD}` token names a field that is not supported.
    #[error(
        "references unsupported field '{field}' on dependency '{ref_name}'; supported: [{supported}]",
        supported = supported_fields.join(", ")
    )]
    UnknownDependencyField {
        ref_name: String,
        field: String,
        supported_fields: Vec<String>,
    },

    /// Two direct dependencies share the same interpolation name (name field or basename) and
    /// the template references that name — the publisher must set `name` to disambiguate.
    ///
    /// Constructed only by `ValidMetadata::try_from` in `metadata.rs` (publish-time).
    /// `TemplateResolver::resolve` never constructs this variant — it receives a
    /// pre-disambiguated map.
    #[error(
        "references ambiguous dependency name '{ref_name}': \
         matches both {first} and {second}"
    )]
    AmbiguousDependencyRef {
        ref_name: DependencyName,
        first: oci::PinnedIdentifier,
        second: oci::PinnedIdentifier,
    },

    /// A `${deps.NAME.*}` token names a known dependency that is not installed on disk.
    #[error("references dependency '{ref_name}' ({dep_identifier}) which is not installed")]
    DependencyNotInstalled {
        ref_name: DependencyName,
        dep_identifier: oci::PinnedIdentifier,
    },

    /// A `${...}` placeholder remained after substitution — neither `${installPath}`
    /// nor `${deps.NAME.FIELD}` syntax. Rejected at publish time so unrecognized
    /// tokens are not baked into launcher targets as literals.
    #[error("contains unknown placeholder '{placeholder}'")]
    UnknownPlaceholder { placeholder: String },

    /// An entry point target resolves to a path that escapes its install root —
    /// either an absolute path outside `${installPath}` / `${deps.*.installPath}`,
    /// or a `..` segment that traverses outside the contained tree.
    #[error(
        "target '{target}' escapes the install root (must resolve under ${{installPath}} or ${{deps.NAME.installPath}}, no '..' traversal)"
    )]
    TargetEscapesInstallRoot { target: String },
}

impl ClassifyExitCode for TemplateError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::UnknownDependencyRef { .. }
            | Self::UnknownDependencyField { .. }
            | Self::AmbiguousDependencyRef { .. }
            | Self::UnknownPlaceholder { .. }
            | Self::TargetEscapesInstallRoot { .. } => ExitCode::DataError,
            Self::DependencyNotInstalled { .. } => ExitCode::NotFound,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci;
    use crate::package::metadata::dependency::DependencyName;
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::TempDir;

    fn pinned(repo: &str) -> oci::PinnedIdentifier {
        let hex = "a".repeat(64);
        let id: oci::Identifier = format!("ocx.sh/{repo}:1.0@sha256:{hex}").parse().unwrap();
        oci::PinnedIdentifier::try_from(id).unwrap()
    }

    fn ctx(path: &Path, repo: &str) -> DependencyContext {
        DependencyContext::path_only(pinned(repo), path.to_path_buf())
    }

    fn dep_name(s: &str) -> DependencyName {
        DependencyName::try_from(s).unwrap()
    }

    // 1. Literal strings (including empty) pass through unchanged.
    #[test]
    fn literal_passes_through() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        assert_eq!(resolver.resolve("plain text").unwrap(), "plain text");
        assert_eq!(resolver.resolve("").unwrap(), "");
    }

    // 2. ${installPath} is substituted with the resolver's install path.
    //    When it appears twice, both occurrences are replaced.
    #[test]
    fn install_path_substitution() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let install = dir.path().to_string_lossy();
        assert_eq!(
            resolver.resolve("${installPath}/bin").unwrap(),
            format!("{install}/bin"),
        );
        assert_eq!(
            resolver.resolve("${installPath}/bin:${installPath}/lib").unwrap(),
            format!("{install}/bin:{install}/lib"),
        );
    }

    // 3. ${deps.NAME.installPath} is substituted with the named dep's install path.
    #[test]
    fn dep_install_path_substitution() {
        let dep_dir = TempDir::new().unwrap();
        let self_dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("dep1"), ctx(dep_dir.path(), "dep1"));
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let dep_path = dep_dir.path().to_string_lossy();
        assert_eq!(resolver.resolve("${deps.dep1.installPath}").unwrap(), dep_path.as_ref(),);
    }

    // 4. Mixed ${installPath} and ${deps.NAME.installPath} in one template are both resolved.
    #[test]
    fn mixed_install_and_dep_tokens() {
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("dep1"), ctx(dep_dir.path(), "dep1"));
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let self_path = self_dir.path().to_string_lossy();
        let dep_path = dep_dir.path().to_string_lossy();
        assert_eq!(
            resolver.resolve("${installPath}:${deps.dep1.installPath}/bin").unwrap(),
            format!("{self_path}:{dep_path}/bin"),
        );
    }

    /// S2-prime: positional fidelity for mixed `${installPath}` and
    /// `${deps.NAME.installPath}` resolution. Pins that each substitution
    /// lands at the exact position where the corresponding token appeared
    /// in the template — not anywhere else, no token swap, no duplicated
    /// substitution. The template uses a `:` separator (PATH-style) and a
    /// `/share` suffix on the dep arm so a transposition would be visible
    /// in the assertion.
    #[test]
    fn mixed_install_path_and_deps_install_path_resolve_correctly() {
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("a"), ctx(dep_dir.path(), "a"));
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let self_path = self_dir.path().to_string_lossy();
        let dep_path = dep_dir.path().to_string_lossy();

        // PATH-shaped: <install>/bin:<dep_a>/share
        let template = "${installPath}/bin:${deps.a.installPath}/share";
        let resolved = resolver.resolve(template).unwrap();
        let expected = format!("{self_path}/bin:{dep_path}/share");
        assert_eq!(
            resolved, expected,
            "mixed ${{installPath}} + ${{deps.a.installPath}} must land at correct positions; \
             template={template:?} resolved={resolved:?} expected={expected:?}"
        );

        // Sanity: each substituted prefix appears exactly once in the output.
        assert_eq!(
            resolved.matches(self_path.as_ref()).count(),
            1,
            "${{installPath}} must be substituted exactly once: {resolved:?}"
        );
        assert_eq!(
            resolved.matches(dep_path.as_ref()).count(),
            1,
            "${{deps.a.installPath}} must be substituted exactly once: {resolved:?}"
        );
        // No leftover token markers.
        assert!(
            !resolved.contains("${"),
            "no template token markers may remain in resolved output: {resolved:?}"
        );
    }

    // 5. An unknown dep name returns UnknownDependencyRef with the missing name and empty declared list.
    #[test]
    fn unknown_dep_returns_error() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let err = resolver.resolve("${deps.missing.installPath}").unwrap_err();
        assert!(
            matches!(&err, TemplateError::UnknownDependencyRef { ref_name, declared }
                if ref_name.as_str() == "missing" && declared.is_empty()),
            "unexpected error: {err}"
        );
    }

    // 6. A known dep name with an unsupported field returns UnknownDependencyField.
    #[test]
    fn unsupported_field_returns_error() {
        let dep_dir = TempDir::new().unwrap();
        let self_dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("dep1"), ctx(dep_dir.path(), "dep1"));
        let resolver = TemplateResolver::new(self_dir.path(), &contexts);

        let err = resolver.resolve("${deps.dep1.version}").unwrap_err();
        assert!(
            matches!(&err, TemplateError::UnknownDependencyField { ref_name, field, .. }
                if ref_name == "dep1" && field == "version"),
            "unexpected error: {err}"
        );
    }

    // 7. A dep that is in dep_contexts but whose install_path does not exist on disk
    //    returns DependencyNotInstalled.
    #[test]
    fn dep_not_installed_returns_error() {
        let dir = TempDir::new().unwrap();
        let missing_path = dir.path().join("not-there");
        let mut contexts = HashMap::new();
        contexts.insert(
            dep_name("dep1"),
            DependencyContext::path_only(pinned("dep1"), missing_path),
        );
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let err = resolver.resolve("${deps.dep1.installPath}").unwrap_err();
        assert!(
            matches!(&err, TemplateError::DependencyNotInstalled { ref_name, .. }
                if ref_name.as_str() == "dep1"),
            "unexpected error: {err}"
        );
    }

    // 8a. resolve_for_validate substitutes tokens but does NOT consult the filesystem,
    //     so a dep whose install_path doesn't exist still resolves cleanly. This is
    //     the publish-time mode used by `validate_entrypoints` — Windows publishers
    //     can't rely on a `Path::new("/")` sentinel existing on their host.
    #[test]
    fn resolve_for_validate_does_not_check_exists() {
        let dir = TempDir::new().unwrap();
        let nonexistent = dir.path().join("definitely-not-there");
        let mut contexts = HashMap::new();
        contexts.insert(
            dep_name("dep1"),
            DependencyContext::path_only(pinned("dep1"), nonexistent),
        );
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        // Runtime mode: errors because install_path doesn't exist.
        assert!(matches!(
            resolver.resolve("${deps.dep1.installPath}").unwrap_err(),
            TemplateError::DependencyNotInstalled { .. }
        ));
        // Validate mode: succeeds because the filesystem is not consulted.
        assert!(resolver.resolve_for_validate("${deps.dep1.installPath}").is_ok());
    }

    // 8b. resolve_for_validate still rejects unknown deps and unsupported fields —
    //     it's the syntactic + referential check at publish time, not a no-op.
    #[test]
    fn resolve_for_validate_rejects_unknown_dep() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts);
        let err = resolver
            .resolve_for_validate("${deps.missing.installPath}")
            .unwrap_err();
        assert!(matches!(err, TemplateError::UnknownDependencyRef { .. }));
    }

    // 8. An uppercase dep name (e.g. ${deps.Python.installPath}) does not match the
    //    [a-z0-9][a-z0-9_-]* pattern, so the token is treated as a literal and returned
    //    as-is (mirrors accumulator::uppercase_dep_name_not_matched).
    #[test]
    fn uppercase_dep_name_not_matched() {
        let dir = TempDir::new().unwrap();
        let mut contexts = HashMap::new();
        contexts.insert(dep_name("python"), ctx(dir.path(), "python"));
        let resolver = TemplateResolver::new(dir.path(), &contexts);

        let result = resolver.resolve("${deps.Python.installPath}").unwrap();
        assert_eq!(result, "${deps.Python.installPath}");
    }
}
