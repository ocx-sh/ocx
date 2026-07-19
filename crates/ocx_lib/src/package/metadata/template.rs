// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Standalone template resolution for metadata string fields.
//!
//! Handles `${installPath}` and `${deps.NAME.FIELD}` substitution. Consumed by
//! `env::resolver::EnvResolver` today and future metadata features (e.g., entry points).

use std::collections::HashMap;
use std::path::Path;

use crate::cli::{ClassifyExitCode, ExitCode};
use crate::oci;

use super::env::dep_context::DependencyContext;
use super::slug::DEP_TOKEN_PATTERN;
use crate::package::metadata::dependency::DependencyName;
use crate::utility::fs::path::RelativePath;

/// Caller-facing intent: what surface is interpolation serving?
///
/// Maps into [`AllowedTokens`] via `From<Usage>`. Use at call sites so the engine
/// sees a capability set, not a consumer identity (SRP: engine policy ≠ consumer
/// identity).
#[derive(Debug, Clone, Copy)]
pub enum Usage {
    /// Interpolating an environment-variable value. Both `${installPath}` and
    /// `${deps.NAME.*}` tokens are permitted.
    Environment,
    /// Interpolating an entrypoint `args` element. Only `${installPath}` is
    /// permitted; `${deps.*}` tokens are rejected with
    /// [`TemplateError::DisallowedToken`].
    EntryPointArgs,
}

/// Engine-facing capability set: which token classes the resolver may substitute.
///
/// Constructed from [`Usage`] via `From<Usage>` or built directly for tests. The
/// engine gates on this struct, never on consumer identity — callers set intent via
/// [`Usage`], the engine sees only what is allowed.
///
/// `installPath` is always permitted regardless of the capability set.
#[derive(Debug, Clone, Copy)]
pub struct AllowedTokens {
    /// Whether `${deps.NAME.*}` tokens are permitted.
    pub deps: bool,
}

impl From<Usage> for AllowedTokens {
    fn from(usage: Usage) -> Self {
        match usage {
            Usage::Environment => AllowedTokens { deps: true },
            Usage::EntryPointArgs => AllowedTokens { deps: false },
        }
    }
}

/// Returns the first `${deps...}` substring in `s` if `s` contains a `${deps.`
/// prefix, or `None` otherwise.
///
/// Used by both the runtime capability gate in [`TemplateResolver::resolve_inner`]
/// and the publish-time validation in `validation::validate_entrypoint_args` so
/// "dep token present" has exactly one definition.
pub(super) fn disallowed_dep_token(s: &str) -> Option<String> {
    let idx = s.find("${deps.")?;
    let end = s[idx..].find('}').map(|e| idx + e + 1).unwrap_or(s.len());
    Some(s[idx..end].to_string())
}

/// Classifies a `Path`-modifier env var's raw value template as an
/// `${installPath}`-rooted `PATH` directory, extracting the relative
/// directory under the install root.
///
/// Returns `Some(rel)` only when `value` is *exactly* `${installPath}/<rel>`
/// — no `${deps.*}` combination, no extra literal segments before or after
/// the token. Any other shape (a bare `${installPath}`, a combined-path var,
/// a literal path) returns `None`: best-effort scan scope, not an error.
/// `rel` is parsed through [`RelativePath::parse`] so a malformed or
/// escaping `<rel>` is excluded the same way, not surfaced as an error.
///
/// Consumed by `package::bin_scan::scan_interface_binaries` (create-time
/// executable auto-scan) to identify which declared `Path` vars are scan
/// targets. Does not itself check `modifier`/`visibility` — those live on
/// the `Var` struct, not the raw value string, and are the caller's
/// responsibility. See `adr_declared_binaries_metadata.md` §2 steps 1-2.
pub fn classify_install_path_rooted_dir(value: &str) -> Option<RelativePath> {
    const INSTALL_PATH_DIR_PREFIX: &str = "${installPath}/";
    let rel = value.strip_prefix(INSTALL_PATH_DIR_PREFIX)?;
    // A second `${` marker means the value combines `${installPath}` with
    // another token (typically `${deps.*}` in a `:`-joined PATH value) —
    // not the exact single-token shape this classifier targets. Best-effort
    // exclusion from scan scope, not an error (ADR §2 step 1).
    if rel.contains("${") {
        return None;
    }
    RelativePath::parse(rel).ok()
}

/// Resolves `${installPath}` and `${deps.NAME.FIELD}` tokens in template strings.
///
/// Build once with [`TemplateResolver::new`], call [`TemplateResolver::resolve`] for each
/// template string. Callers that iterate over many variables (e.g. `Exporter`) benefit from
/// building the resolver once and reusing it.
///
/// Runtime mode only: [`TemplateResolver::resolve`] substitutes real install paths and
/// verifies each dep's `install_path.exists()` on disk. Used by `env::resolver::EnvResolver`
/// at install/exec time.
///
/// Use [`TemplateResolver::usage`] to restrict which token classes are allowed (e.g.,
/// `Usage::EntryPointArgs` forbids `${deps.*}` tokens).
pub struct TemplateResolver<'a> {
    install_path: &'a Path,
    dep_contexts: &'a HashMap<DependencyName, DependencyContext>,
    allowed: AllowedTokens,
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
            // Default: Environment caps — both ${installPath} and ${deps.*} permitted.
            // Preserves today's behavior for every existing caller that does not call .usage().
            allowed: Usage::Environment.into(),
        }
    }

    /// Sets the interpolation usage, restricting which token classes are permitted.
    ///
    /// Pass a [`Usage`] variant or an [`AllowedTokens`] value directly. The default
    /// is [`Usage::Environment`] (all tokens permitted), matching the existing behavior
    /// for env-value interpolation callers.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let resolver = TemplateResolver::new(path, &deps).usage(Usage::EntryPointArgs);
    /// ```
    #[must_use]
    pub fn usage(mut self, usage: impl Into<AllowedTokens>) -> Self {
        self.allowed = usage.into();
        self
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

    fn resolve_inner(&self, template: &str, check_exists: bool) -> Result<String, TemplateError> {
        // Capability gate (ADR D6): when ${deps.*} tokens are not permitted by the
        // active AllowedTokens set, reject before any substitution so a real
        // dep_contexts map can never substitute a dep path into the output.
        if !self.allowed.deps
            && let Some(token) = disallowed_dep_token(template)
        {
            return Err(TemplateError::DisallowedToken { token });
        }

        // Step 1: substitute ${installPath}. Avoid the `String::replace` allocation
        // when the token isn't present — most templates contain only one of the two
        // forms (`${installPath}` xor `${deps.*}`) so the fast-path covers many calls.
        let value = if template.contains("${installPath}") {
            // Strip the Windows extended-length `\\?\` verbatim prefix via
            // `dunce::simplified` before converting to a string.
            //
            // Background: on Windows, `tokio::fs::canonicalize` returns a
            // `\\?\`-prefixed verbatim path.  When `install_path` carries that
            // prefix and the publisher-authored template contains a forward-slash
            // suffix (e.g. `${installPath}/bin`), plain string substitution
            // produces `\\?\C:\…\content/bin` — a mixed-separator string whose
            // leading `\\?\` disables all Windows path normalization.  Windows
            // then treats `/bin` as a *literal filename component* (the entire
            // string `content/bin` is one path element), so the path never
            // resolves on disk → `RequiredPathMissing` error.
            //
            // `dunce::simplified` converts `\\?\C:\foo` → `C:\foo` (a no-op on
            // paths that are not verbatim, and a no-op on all POSIX paths).
            // After simplification the separator in the substituted string is the
            // native backslash, so a subsequent `PathBuf::from(value)` or OS-level
            // path lookup works correctly on every platform.
            let simplified = dunce::simplified(self.install_path);
            let install_lossy = simplified.to_string_lossy();
            // Defense-in-depth: if the install path itself contains a `${` sequence it
            // would inject a syntactically-valid-looking (but semantically wrong) token
            // into the substituted string, causing the dep-token regex in step 2 to
            // re-process bytes that originated from the filesystem, not from the
            // publisher's template. Reject eagerly so the dep-token loop never sees them.
            if let Some(idx) = install_lossy.find("${") {
                // Surface the first injected token for a diagnostic-friendly error.
                let end = install_lossy[idx..]
                    .find('}')
                    .map(|e| idx + e + 1)
                    .unwrap_or(install_lossy.len());
                let placeholder = install_lossy[idx..end].to_string();
                return Err(TemplateError::UnknownPlaceholder { placeholder });
            }
            template.replace("${installPath}", &install_lossy)
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
            // template-resolution API consumed by `env::resolver::EnvResolver` and the
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
    /// tokens are not baked into env values as literals.
    #[error("contains unknown placeholder '{placeholder}'")]
    UnknownPlaceholder { placeholder: String },

    /// A recognized token class is present but not permitted by the current
    /// [`AllowedTokens`] capability set.
    ///
    /// Raised as the **first** step in resolution when `!allowed.deps` and the
    /// template contains `${deps.` — before the dep regex fires — so a real
    /// `dep_contexts` map can never silently substitute a dep path (ADR D6
    /// gate-before-regex correctness claim).
    #[error("token '{token}' is not permitted here; '${{deps.*}}' is only valid in env values")]
    DisallowedToken { token: String },
}

impl ClassifyExitCode for TemplateError {
    fn classify(&self) -> Option<ExitCode> {
        Some(match self {
            Self::UnknownDependencyRef { .. }
            | Self::UnknownDependencyField { .. }
            | Self::AmbiguousDependencyRef { .. }
            | Self::UnknownPlaceholder { .. }
            | Self::DisallowedToken { .. } => ExitCode::DataError,
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

    // 9. Defense-in-depth: if the install_path itself contains a `${` sequence,
    //    resolve() returns UnknownPlaceholder rather than letting the injected
    //    bytes reach the dep-token regex in step 2.
    #[test]
    fn install_path_containing_token_fragment_is_rejected() {
        use std::path::PathBuf;

        // Construct a path that looks like it contains a dep token fragment.
        // On real filesystems such a path is unusual but not impossible (user-chosen
        // home directory, unusual mount point, etc.).
        let suspicious_path = PathBuf::from("/opt/${deps.foo}/bin");
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(&suspicious_path, &contexts);

        let err = resolver.resolve("${installPath}/tool").unwrap_err();
        assert!(
            matches!(&err, TemplateError::UnknownPlaceholder { placeholder }
                if placeholder.starts_with("${")),
            "expected UnknownPlaceholder when install_path contains a token fragment, got: {err}"
        );
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

    // ── Contract 10: Usage / AllowedTokens mapping ────────────────────────────

    // Contract 10 (From<Usage> mapping): Usage::Environment maps to deps=true;
    // Usage::EntryPointArgs maps to deps=false.
    #[test]
    fn usage_maps_to_allowed_tokens() {
        let env_caps = AllowedTokens::from(Usage::Environment);
        assert!(
            env_caps.deps,
            "Usage::Environment must map to AllowedTokens {{ deps: true }}"
        );

        let args_caps = AllowedTokens::from(Usage::EntryPointArgs);
        assert!(
            !args_caps.deps,
            "Usage::EntryPointArgs must map to AllowedTokens {{ deps: false }}"
        );
    }

    // Contract 10: resolver with Usage::EntryPointArgs resolves ${installPath} to the
    // install path — the only token class allowed in entrypoint args.
    #[test]
    fn entrypoint_args_usage_resolves_install_path() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let install = dir.path().to_string_lossy();
        let result = resolver.resolve("${installPath}/app.py").unwrap();
        assert_eq!(
            result,
            format!("{install}/app.py"),
            "Usage::EntryPointArgs must resolve ${{installPath}} to the install path"
        );
    }

    // Contract 10c: a bare "${installPath}" (no prefix or suffix) resolves to exactly
    // the install path string under Usage::EntryPointArgs. Pins that the resolver
    // performs a full string replacement, not just a prefix substitution.
    #[test]
    fn entrypoint_args_bare_install_path_resolves_to_exact_path() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let install = dir.path().to_string_lossy();
        let result = resolver.resolve("${installPath}").unwrap();
        assert_eq!(
            result,
            install.as_ref(),
            "bare '${{installPath}}' must resolve to exactly the install path; got {result:?}"
        );
    }

    // Contract 10b — gate-before-regex correctness proof (ADR D6).
    //
    // A resolver built with *real, on-disk* dep_contexts and Usage::EntryPointArgs must
    // reject a ${deps.*} template with DisallowedToken BEFORE the dep regex fires.
    // The dep is declared AND its path exists on disk — the gate must fire regardless,
    // proving that the capability gate (not an empty dep_contexts map) is the safety
    // mechanism. A real dep_contexts must never substitute a dep path into an
    // EntryPointArgs template.
    #[test]
    fn entrypoint_args_usage_rejects_deps_token_before_regex() {
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap(); // dep path EXISTS on disk

        let mut contexts = HashMap::new();
        // Non-empty dep_contexts with a real, on-disk dep — the dep regex would
        // resolve this successfully if the capability gate were absent.
        contexts.insert(dep_name("uv"), ctx(dep_dir.path(), "uv"));

        let resolver = TemplateResolver::new(self_dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let err = resolver.resolve("${deps.uv.installPath}/x").unwrap_err();
        assert!(
            matches!(&err, TemplateError::DisallowedToken { token } if token.contains("deps.uv")),
            "expected DisallowedToken with token containing 'deps.uv'; \
             rejected even with a real, on-disk dep context (proves gate fires before the \
             dep regex — no substitution); got: {err}"
        );
    }

    // Contract 10: Usage::Environment still resolves ${deps.*} tokens correctly —
    // the env interpolation path is unaffected by the new capability model.
    #[test]
    fn environment_usage_still_resolves_deps() {
        let self_dir = TempDir::new().unwrap();
        let dep_dir = TempDir::new().unwrap();

        let mut contexts = HashMap::new();
        contexts.insert(dep_name("dep1"), ctx(dep_dir.path(), "dep1"));

        let resolver = TemplateResolver::new(self_dir.path(), &contexts).usage(Usage::Environment);

        let dep_path = dep_dir.path().to_string_lossy();
        let result = resolver.resolve("${deps.dep1.installPath}").unwrap();
        assert_eq!(
            result,
            dep_path.as_ref(),
            "Usage::Environment must resolve ${{deps.*}} tokens (env path unchanged)"
        );
    }

    // ── Contract 7: within-element repetition + multi-element independent resolve ──

    /// Contract 7: `${installPath}` may appear more than once within a single arg
    /// element — both occurrences must be substituted. Also verifies that multiple
    /// arg elements each resolve independently (two-element case).
    ///
    /// Uses `Usage::EntryPointArgs` as the caller intent, matching the runtime
    /// resolution path for baked entrypoint args.
    #[test]
    fn entrypoint_args_install_path_repeats_in_one_element() {
        let dir = TempDir::new().unwrap();
        let contexts: HashMap<DependencyName, DependencyContext> = HashMap::new();
        let resolver = TemplateResolver::new(dir.path(), &contexts).usage(Usage::EntryPointArgs);

        let install = dir.path().to_string_lossy();

        // Within-element case: ${installPath} appears twice in one arg element.
        // Both occurrences must be substituted; no ${...} markers may remain.
        let template = "--prefix=${installPath}:${installPath}/bin";
        let result = resolver.resolve(template).unwrap();
        assert_eq!(
            result.matches(install.as_ref()).count(),
            2,
            "both occurrences of ${{installPath}} in a single element must be substituted; \
             template={template:?} resolved={result:?}"
        );
        assert!(
            !result.contains("${"),
            "no template markers may remain after resolution; got: {result:?}"
        );

        // Two-element case: each element must resolve independently.
        let result_root = resolver.resolve("--root=${installPath}").unwrap();
        assert_eq!(
            result_root,
            format!("--root={install}"),
            "--root=${{installPath}} must resolve to --root=<content_path>"
        );

        let result_x = resolver.resolve("${installPath}/x").unwrap();
        assert_eq!(
            result_x,
            format!("{install}/x"),
            "${{installPath}}/x must resolve to <content_path>/x"
        );
    }

    // ── classify_install_path_rooted_dir ──────────────────────────────────────

    /// A bare `${installPath}` with no trailing slash is not the
    /// `${installPath}/<rel>` shape — no `<rel>` to classify.
    #[test]
    fn classify_install_path_rooted_dir_bare_token_returns_none() {
        assert_eq!(classify_install_path_rooted_dir("${installPath}"), None);
    }

    /// `${installPath}/` (trailing slash, nothing after) yields an empty,
    /// non-escaping relative path — the containment root itself.
    #[test]
    fn classify_install_path_rooted_dir_trailing_slash_yields_empty_rel() {
        let rel = classify_install_path_rooted_dir("${installPath}/").expect("empty rel must parse");
        assert_eq!(rel.as_path(), Path::new(""));
    }

    /// The token must lead the value; a prefix before `${installPath}` means
    /// this is not the exact single-token shape the classifier targets.
    #[test]
    fn classify_install_path_rooted_dir_prefix_mid_string_returns_none() {
        assert_eq!(classify_install_path_rooted_dir("foo/${installPath}/bin"), None);
    }

    /// A `<rel>` that escapes the containment root via `..` is rejected —
    /// best-effort scan-scope exclusion, not an error.
    #[test]
    fn classify_install_path_rooted_dir_escaping_rel_returns_none() {
        assert_eq!(classify_install_path_rooted_dir("${installPath}/../etc"), None);
    }

    /// Happy path: `${installPath}/bin` classifies to the relative dir `bin`.
    #[test]
    fn classify_install_path_rooted_dir_happy_path_returns_rel() {
        let rel = classify_install_path_rooted_dir("${installPath}/bin").expect("bin must parse");
        assert_eq!(rel.as_path(), Path::new("bin"));
    }
}
