// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Publish-time validation for package metadata.
//!
//! This module owns all logic that runs when a publisher calls
//! [`ValidMetadata::try_from`]:
//!
//! - [`validate_env_tokens`] — walks env var values and checks that every
//!   `${deps.NAME.FIELD}` token references a declared, non-ambiguous dep with
//!   a supported field name.
//! - [`validate_entrypoints`] — checks that every entrypoint `target` string
//!   resolves under a declared install root (no path-traversal, no unknown
//!   tokens, no unsafe characters).
//!
//! Both functions are `pub(super)` and called exclusively from
//! [`ValidMetadata::try_from`] in this module.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::LauncherSafeString;
use crate::utility::fs::path::lexical_normalize;

use super::Metadata;
use super::dependency::{Dependencies, Dependency, DependencyName};
use super::entrypoint::{EntrypointName, Entrypoints};
use super::env::accumulator::DependencyContext;
use super::slug;
use super::template::{TemplateError, TemplateResolver};

// ── Sentinel constants ────────────────────────────────────────────────────────

/// Sentinel string substituted for `${installPath}` at publish-time validation.
///
/// The launcher's resolved target string is compared against this prefix to
/// enforce ADR §4 containment: every `target` must resolve to a path under
/// `${installPath}` or a declared `${deps.NAME.installPath}`. Real install
/// paths are not known at publish time, so a recognizable sentinel stands in.
pub(super) const VALIDATE_INSTALL_PATH_SENTINEL: &str = "/__OCX_INSTALL_PATH_SENTINEL__";

/// Prefix for per-dep sentinel install paths during publish-time validation.
///
/// Combined with the dep's interpolation name to keep prefixes unique.
pub(super) const VALIDATE_DEP_SENTINEL_PREFIX: &str = "/__OCX_DEP_SENTINEL_";

/// Matches any `${...}` token. Used after `TemplateResolver::resolve_for_validate`
/// substitutes recognized tokens; anything still present is an unknown placeholder.
static UNKNOWN_TOKEN_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"\$\{[^}]*\}").expect("valid regex"));

// ── ValidMetadata ─────────────────────────────────────────────────────────────

/// Metadata that has passed publish-time validation.
///
/// Constructed exclusively via `TryFrom<Metadata>`, which verifies that all
/// `${deps.NAME.FIELD}` tokens in env var values reference declared direct
/// dependencies with known field names. This makes it impossible to publish
/// without running the check.
///
/// Derefs to [`Metadata`] for read access without unwrapping.
#[derive(Debug)]
pub struct ValidMetadata(Metadata);

impl TryFrom<Metadata> for ValidMetadata {
    type Error = crate::Error;

    /// # Errors
    ///
    /// Returns an error if:
    /// - A token references a name not matching any direct dependency (by name field or basename).
    /// - A token references an unsupported field (currently only `installPath` is supported).
    /// - Two direct dependencies share the same interpolation name (collision) and a token
    ///   uses that name — in this case `name` must be set to disambiguate.
    fn try_from(metadata: Metadata) -> Result<Self, Self::Error> {
        validate_env_tokens(&metadata)?;

        if let Some(entrypoints) = metadata.bundle_entrypoints() {
            // Build name_map here for entrypoint validation; validate_env_tokens builds its
            // own copy internally since it needs the collision_map too.
            let name_map = build_name_map(metadata.dependencies());
            validate_entrypoints(entrypoints, &name_map)?;
        }

        Ok(Self(metadata))
    }
}

impl From<ValidMetadata> for Metadata {
    fn from(v: ValidMetadata) -> Self {
        v.0
    }
}

impl std::ops::Deref for ValidMetadata {
    type Target = Metadata;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Builds a name → `&Dependency` map from the given `Dependencies`.
///
/// Keys are the interpolation names returned by `dep.name()`. When two deps
/// resolve to the same name, the second write wins in the `name_map` and the
/// colliding entry is recorded in `collision_map`. This function returns only
/// the primary map; callers that need collision detection use
/// [`build_name_and_collision_maps`].
fn build_name_map<'a>(deps: &'a Dependencies) -> HashMap<String, &'a Dependency> {
    let mut name_map: HashMap<String, &'a Dependency> = HashMap::new();
    for dep in deps {
        name_map.insert(dep.name().to_string(), dep);
    }
    name_map
}

/// Builds both the primary name map and the collision map from `Dependencies`.
///
/// The primary map maps dep names to the last dep with that name; the collision
/// map records the name when two or more deps share it. Callers should reject
/// any token whose name appears in the collision map.
fn build_name_and_collision_maps<'a>(
    deps: &'a Dependencies,
) -> (HashMap<String, &'a Dependency>, HashMap<String, &'a Dependency>) {
    let mut name_map: HashMap<String, &'a Dependency> = HashMap::new();
    let mut collision_map: HashMap<String, &'a Dependency> = HashMap::new();

    for dep in deps {
        let name = dep.name().to_string();
        if name_map.insert(name.clone(), dep).is_some() {
            collision_map.insert(name, dep);
        }
    }

    (name_map, collision_map)
}

/// Validates env var values: checks every `${deps.NAME.FIELD}` token for declared,
/// non-ambiguous deps and supported field names.
///
/// Does not consult the filesystem; pure syntax + reference check.
pub(super) fn validate_env_tokens(metadata: &Metadata) -> Result<(), crate::Error> {
    use super::super::error::Error;

    // Build the dep-token regex by embedding the slug body (without ^..$ anchors)
    // from the shared SLUG_PATTERN_STR constant so the pattern stays in sync.
    static DEP_TOKEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        // SLUG_PATTERN_STR is "^[a-z0-9][a-z0-9_-]*$"; strip anchors for embedding.
        let slug_body = slug::SLUG_PATTERN_STR.trim_start_matches('^').trim_end_matches('$');
        let pattern = format!(r"\$\{{deps\.({slug_body})\.([a-zA-Z]+)\}}");
        regex::Regex::new(&pattern).expect("valid dep-token regex")
    });

    let deps = metadata.dependencies();
    let (name_map, collision_map) = build_name_and_collision_maps(deps);

    if let Some(env) = metadata.env() {
        for var in env {
            let value = match var.value() {
                Some(v) => v,
                None => continue,
            };

            for cap in DEP_TOKEN.captures_iter(value) {
                // Invariant: DEP_TOKEN defines exactly 2 capture groups; a successful
                // captures_iter match guarantees groups 1 and 2 are Some.
                let dep_name = cap
                    .get(1)
                    .expect("regex group 1 guaranteed by DEP_TOKEN pattern")
                    .as_str();
                let field = cap
                    .get(2)
                    .expect("regex group 2 guaranteed by DEP_TOKEN pattern")
                    .as_str();

                // The DEP_TOKEN regex only matches the slug pattern so dep_name is always
                // a structurally valid DependencyName by construction.
                let dep_name_typed =
                    DependencyName::try_from(dep_name).expect("regex guarantees dep_name matches slug pattern");

                // Check for collision first.
                if collision_map.contains_key(dep_name) {
                    let first = name_map[dep_name].identifier.clone();
                    let second = collision_map[dep_name].identifier.clone();
                    return Err(Error::EnvVarInterpolation {
                        var_key: var.key.clone(),
                        source: TemplateError::AmbiguousDependencyRef {
                            ref_name: dep_name_typed,
                            first,
                            second,
                        },
                    }
                    .into());
                }

                // Check name is declared.
                if !name_map.contains_key(dep_name) {
                    return Err(Error::EnvVarInterpolation {
                        var_key: var.key.clone(),
                        source: TemplateError::UnknownDependencyRef {
                            ref_name: dep_name_typed,
                            declared: name_map
                                .keys()
                                .map(|s| {
                                    // Names in name_map come from dep.name() — always valid slugs.
                                    DependencyName::try_from(s.as_str()).expect("name_map key is a valid slug")
                                })
                                .collect(),
                        },
                    }
                    .into());
                }

                // Check field is supported.
                if field != "installPath" {
                    return Err(Error::EnvVarInterpolation {
                        var_key: var.key.clone(),
                        source: TemplateError::UnknownDependencyField {
                            ref_name: dep_name.to_string(),
                            field: field.to_string(),
                            supported_fields: vec!["installPath".to_string()],
                        },
                    }
                    .into());
                }
            }
        }
    }

    Ok(())
}

/// Builds the per-dep sentinel `DependencyContext` map for publish-time target resolution.
///
/// Each dep gets a unique sentinel install path so path-prefix containment can
/// distinguish which dep root a resolved target belongs to.
fn build_sentinel_contexts(name_map: &HashMap<String, &Dependency>) -> HashMap<DependencyName, DependencyContext> {
    name_map
        .iter()
        .map(|(name, dep)| {
            let sentinel = PathBuf::from(format!("{VALIDATE_DEP_SENTINEL_PREFIX}{name}__"));
            // SAFETY-invariant: names in name_map were produced by dep.name() which is always
            // a valid DependencyName (either an explicit validated name or a repository basename
            // guaranteed to satisfy the slug pattern by the OCI identifier parser).
            let dep_name = DependencyName::try_from(name.as_str()).expect("dep name from name_map is a valid slug");
            (dep_name, DependencyContext::path_only(dep.identifier.clone(), sentinel))
        })
        .collect()
}

/// Resolves an entrypoint `target` string using the validator's sentinel resolver.
///
/// Returns the resolved string, or `EntrypointTargetInvalid` if the resolver
/// rejects the token (unknown dep, unsupported field).
fn check_target_resolves(
    resolver: &TemplateResolver<'_>,
    target: &str,
    ep_name: &EntrypointName,
) -> Result<String, crate::Error> {
    use super::super::error::Error;

    resolver.resolve_for_validate(target).map_err(|source| {
        crate::Error::Package(Box::new(Error::EntrypointTargetInvalid {
            name: ep_name.to_string(),
            source,
        }))
    })
}

/// Checks that `resolved` starts with one of the `allowed_prefixes` (with a path boundary).
///
/// Collapses `..` traversals lexically before the check so that
/// `${installPath}/../..` cannot escape to `/`.
fn check_target_contained(
    resolved: &str,
    allowed_prefixes: &[String],
    ep_name: &EntrypointName,
    original_target: &str,
) -> Result<(), crate::Error> {
    use super::super::error::Error;

    let normalized = lexical_normalize(Path::new(resolved));
    let normalized_str = normalized.to_string_lossy();

    // Bare prefix string match alone would let `${installPath}_evil` slip through
    // (substring without separator), so require an exact match or a path-separator
    // boundary after the prefix.
    let in_allowed = allowed_prefixes.iter().any(|prefix| {
        if normalized_str == *prefix {
            return true;
        }
        if let Some(rest) = normalized_str.strip_prefix(prefix.as_str()) {
            return rest.starts_with('/');
        }
        false
    });

    if !in_allowed {
        return Err(crate::Error::Package(Box::new(Error::EntrypointTargetInvalid {
            name: ep_name.to_string(),
            source: TemplateError::TargetEscapesInstallRoot {
                target: original_target.to_string(),
            },
        })));
    }

    Ok(())
}

/// Checks that no `${...}` tokens remain in `resolved` after sentinel substitution.
///
/// Any remaining token is an unknown placeholder that was not recognized by
/// `resolve_for_validate`; baking it as a literal into a launcher is rejected.
fn check_no_unknown_tokens(resolved: &str, ep_name: &EntrypointName) -> Result<(), crate::Error> {
    use super::super::error::Error;

    if let Some(m) = UNKNOWN_TOKEN_RE.find(resolved) {
        return Err(crate::Error::Package(Box::new(Error::EntrypointTargetInvalid {
            name: ep_name.to_string(),
            source: TemplateError::UnknownPlaceholder {
                placeholder: m.as_str().to_string(),
            },
        })));
    }

    Ok(())
}

/// Checks that `resolved` contains no characters that would corrupt a generated launcher.
///
/// Defense-in-depth: surface unsafe-launcher characters at publish time so
/// publishers don't ship a package whose first install fails on every consumer.
fn check_launcher_safe(resolved: &str, _ep_name: &EntrypointName) -> Result<(), crate::Error> {
    // `LauncherSafeString::new` propagates directly to `crate::Error` via its `From` impls.
    // The entry point name context is not added here — the error already surfaces the
    // problematic character; adding ep_name would require wrapping `EntrypointTargetInvalid`
    // around the `LauncherSafeString` error, which is a behavior change, not a refactor.
    LauncherSafeString::new(resolved.to_string())?;
    Ok(())
}

/// Validates that all entrypoint targets reference only declared dependencies and
/// known fields, contain no path-traversal escapes, contain no unknown placeholders,
/// and contain no characters that would corrupt a generated launcher.
///
/// Uses sentinel strings for `${installPath}` and each declared dep's
/// `${...installPath}` so resolution happens without filesystem access — Windows
/// publishers no longer rely on the host's `/` directory existing.
pub(super) fn validate_entrypoints(
    entrypoints: &Entrypoints,
    name_map: &HashMap<String, &Dependency>,
) -> Result<(), crate::Error> {
    if entrypoints.is_empty() {
        return Ok(());
    }

    // Build dep_contexts keyed by name. Each dep gets a unique sentinel install
    // path so that path-prefix containment can distinguish which `${deps.NAME.*}`
    // a resolved target belongs to. No filesystem access happens at validate time;
    // the resolver's `resolve_for_validate` mode skips the `install_path.exists()`
    // check that the runtime mode performs.
    let install_sentinel = Path::new(VALIDATE_INSTALL_PATH_SENTINEL);
    let dep_contexts = build_sentinel_contexts(name_map);

    let resolver = TemplateResolver::new(install_sentinel, &dep_contexts);

    // Pre-compute the set of acceptable containment prefixes once.
    let mut allowed_prefixes: Vec<String> = Vec::with_capacity(name_map.len() + 1);
    allowed_prefixes.push(VALIDATE_INSTALL_PATH_SENTINEL.to_string());
    for name in name_map.keys() {
        allowed_prefixes.push(format!("{VALIDATE_DEP_SENTINEL_PREFIX}{name}__"));
    }

    for entry in entrypoints.iter() {
        // R1.1 / reference check: resolve tokens, reject unknown/unsupported refs.
        let resolved = check_target_resolves(&resolver, &entry.target, &entry.name)?;

        // R1.4: any `${...}` token still present after `resolve_for_validate` is an
        // unknown placeholder (`${installPath}` and `${deps.NAME.FIELD}` would have
        // been substituted). Reject so it can't be baked into a launcher as a literal.
        check_no_unknown_tokens(&resolved, &entry.name)?;

        // R1.2: containment check — `lexical_normalize` collapses `..` traversals
        // syntactically (no filesystem access). After normalization the path
        // must still start with one of the sentinel install-root prefixes; any
        // absolute target outside the sentinel set, or any `..`-traversal that
        // escapes its sentinel, fails this prefix match.
        check_target_contained(&resolved, &allowed_prefixes, &entry.name, &entry.target)?;

        // R1.3: defense-in-depth — surface unsafe-launcher characters at publish
        // time so publishers don't ship a package whose first install fails on
        // every consumer.
        check_launcher_safe(&resolved, &entry.name)?;
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::Metadata;

    fn hex(n: u8) -> String {
        format!("{:x}", n).repeat(64).chars().take(64).collect()
    }

    fn dep_json(repo: &str, name: Option<&str>) -> String {
        let h = hex(1);
        match name {
            Some(n) => format!(r#"{{"identifier":"ocx.sh/{repo}:1@sha256:{h}","name":"{n}"}}"#),
            None => format!(r#"{{"identifier":"ocx.sh/{repo}:1@sha256:{h}"}}"#),
        }
    }

    fn make_metadata(deps_json: &str, env_json: &str) -> Metadata {
        let json = format!(r#"{{"type":"bundle","version":1,"dependencies":[{deps_json}],"env":[{env_json}]}}"#);
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("bad test JSON: {e}\n{json}"))
    }

    fn constant_env(key: &str, value: &str) -> String {
        format!(r#"{{"key":"{key}","type":"constant","value":"{value}"}}"#)
    }

    fn make_metadata_with_entrypoints(entrypoints_json: &str) -> Metadata {
        let json = format!(r#"{{"type":"bundle","version":1,"entrypoints":[{entrypoints_json}]}}"#);
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("bad test JSON: {e}\n{json}"))
    }

    fn make_metadata_with_deps_and_entrypoints(deps_json: &str, entrypoints_json: &str) -> Metadata {
        let json = format!(
            r#"{{"type":"bundle","version":1,"dependencies":[{deps_json}],"entrypoints":[{entrypoints_json}]}}"#
        );
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("bad test JSON: {e}\n{json}"))
    }

    // ── validate_env_tokens ───────────────────────────────────────────────────

    // 3.3 — valid: env ref matches declared dep basename
    #[test]
    fn valid_known_dep_ref() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            &constant_env("X", "${deps.cmake.installPath}"),
        );
        assert!(ValidMetadata::try_from(meta).is_ok());
    }

    // 3.3 — valid: env ref matches declared dep alias
    #[test]
    fn valid_dep_ref_via_alias() {
        let meta = make_metadata(
            &dep_json("myorg/cmake", Some("my-cmake")),
            &constant_env("X", "${deps.my-cmake.installPath}"),
        );
        assert!(ValidMetadata::try_from(meta).is_ok());
    }

    // 3.3 — valid: no ${deps.*} tokens → no error
    #[test]
    fn no_dep_tokens_ok() {
        let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", "${installPath}/bin"));
        assert!(ValidMetadata::try_from(meta).is_ok());
    }

    // 3.3 — error: ref to undeclared dep name
    #[test]
    fn undeclared_dep_ref_errors() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            &constant_env("X", "${deps.ninja.installPath}"),
        );
        let err = ValidMetadata::try_from(meta).unwrap_err();
        assert!(format!("{err}").contains("ninja"), "expected ninja in error: {err}");
    }

    // 3.3 — error: unsupported field guards extensibility seam
    #[test]
    fn unsupported_field_errors() {
        let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", "${deps.cmake.version}"));
        let err = ValidMetadata::try_from(meta).unwrap_err();
        assert!(
            format!("{err}").contains("version"),
            "expected 'version' in error: {err}"
        );
    }

    // 3.3 — error: collision — two same-basename deps (no alias) + token → AmbiguousDependencyRef
    #[test]
    fn same_basename_collision_with_token_errors() {
        let h1 = hex(1);
        let h2 = hex(2);
        let deps =
            format!(r#"{{"identifier":"ocx.sh/cmake:1@sha256:{h1}"}},{{"identifier":"ghcr.io/cmake:1@sha256:{h2}"}}"#);
        let env = constant_env("X", "${deps.cmake.installPath}");
        let json = format!(r#"{{"type":"bundle","version":1,"dependencies":[{deps}],"env":[{env}]}}"#);
        let meta: Metadata = serde_json::from_str(&json).unwrap();
        let err = ValidMetadata::try_from(meta).unwrap_err();
        assert!(format!("{err}").contains("cmake"), "expected cmake in error: {err}");
    }

    // 3.3 — backward compat: same two same-basename deps without any ${deps.*} token → OK
    #[test]
    fn same_basename_without_token_ok() {
        let h1 = hex(1);
        let h2 = hex(2);
        let deps =
            format!(r#"{{"identifier":"ocx.sh/cmake:1@sha256:{h1}"}},{{"identifier":"ghcr.io/cmake:1@sha256:{h2}"}}"#);
        let env = constant_env("X", "${installPath}/bin");
        let json = format!(r#"{{"type":"bundle","version":1,"dependencies":[{deps}],"env":[{env}]}}"#);
        let meta: Metadata = serde_json::from_str(&json).unwrap();
        assert!(ValidMetadata::try_from(meta).is_ok());
    }

    // 3.3 — transitive scoping: R has direct dep D but tokens ref T (D's dep) → error
    #[test]
    fn transitive_dep_ref_errors() {
        let meta = make_metadata(
            &dep_json("direct-dep", None),
            &constant_env("X", "${deps.transitive-tool.installPath}"),
        );
        let err = ValidMetadata::try_from(meta).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("transitive-tool"),
            "expected transitive-tool in error: {msg}"
        );
        assert!(
            msg.contains("direct-dep"),
            "expected declared dep 'direct-dep' in error: {msg}"
        );
    }

    // ── validate_entrypoints ──────────────────────────────────────────────────

    /// Valid target resolving under ${installPath} — should pass.
    #[test]
    fn entrypoint_valid_install_path_target() {
        let meta = make_metadata_with_entrypoints(r#"{"name":"cmake","target":"${installPath}/bin/cmake"}"#);
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "valid ${{installPath}} target must pass validation"
        );
    }

    /// Valid target resolving under ${deps.foo.installPath} with foo in deps — should pass.
    #[test]
    fn entrypoint_valid_dep_install_path_target() {
        let h = hex(1);
        let dep = format!(r#"{{"identifier":"ocx.sh/foo:1@sha256:{h}"}}"#);
        let ep = r#"{"name":"foocmd","target":"${deps.foo.installPath}/bin/foocmd"}"#;
        let meta = make_metadata_with_deps_and_entrypoints(&dep, ep);
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "valid ${{deps.foo.installPath}} target with declared dep must pass validation"
        );
    }

    /// Target referencing undeclared dep — should yield EntrypointTargetInvalid.
    #[test]
    fn entrypoint_undeclared_dep_in_target_rejected() {
        let meta = make_metadata_with_entrypoints(r#"{"name":"cmake","target":"${deps.bar.installPath}/bin/cmake"}"#);
        let result = ValidMetadata::try_from(meta);
        assert!(result.is_err(), "undeclared dep in target must be rejected");
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cmake"), "error must name the entrypoint 'cmake': {msg}");
        assert!(msg.contains("bar"), "error must name the unknown dep 'bar': {msg}");
    }

    /// Absolute-path target outside ${installPath} must be rejected.
    #[test]
    fn validate_entrypoints_rejects_absolute_path_target() {
        let meta = make_metadata_with_entrypoints(r#"{"name":"evilcmd","target":"/etc/passwd"}"#);
        let result = ValidMetadata::try_from(meta);
        assert!(
            result.is_err(),
            "absolute-path target outside ${{installPath}} must be rejected; got: {:?}",
            result
        );
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("evilcmd") || msg.to_lowercase().contains("target"),
            "error must cite the offending entrypoint or 'target': {msg}"
        );
    }

    /// `${installPath}/../../etc/passwd` traversal target must be rejected.
    #[test]
    fn validate_entrypoints_rejects_parent_dir_traversal() {
        let meta = make_metadata_with_entrypoints(r#"{"name":"evilcmd","target":"${installPath}/../../etc/passwd"}"#);
        let result = ValidMetadata::try_from(meta);
        assert!(
            result.is_err(),
            "`${{installPath}}/../..` traversal target must be rejected; got: {:?}",
            result
        );
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("evilcmd")
                || msg.to_lowercase().contains("traversal")
                || msg.to_lowercase().contains("escape")
                || msg.to_lowercase().contains("outside"),
            "error must cite the entrypoint or describe the traversal: {msg}"
        );
    }

    /// Target with unsafe character (single quote) must be rejected at validate time.
    #[test]
    fn validate_entrypoints_rejects_target_with_unsafe_char() {
        let meta = make_metadata_with_entrypoints(r#"{"name":"evilcmd","target":"${installPath}/bin/it's-bad"}"#);
        let result = ValidMetadata::try_from(meta);
        assert!(
            result.is_err(),
            "target with unsafe character (single quote) must be rejected at validate time; got: {:?}",
            result
        );
    }

    /// Unknown `${unknownToken}` in target must be rejected.
    #[test]
    fn validate_entrypoints_rejects_unknown_token() {
        let meta = make_metadata_with_entrypoints(r#"{"name":"evilcmd","target":"${unknownToken}/bin/cmake"}"#);
        let result = ValidMetadata::try_from(meta);
        assert!(
            result.is_err(),
            "unknown ${{...}} token in target must be rejected; got: {:?}",
            result
        );
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("unknownToken")
                || msg.to_lowercase().contains("unknown")
                || msg.to_lowercase().contains("placeholder"),
            "error must name the unknown token or describe it: {msg}"
        );
    }

    // ── check_target_contained — per-concern unit tests ───────────────────────

    /// Happy path: exact match on allowed prefix.
    #[test]
    fn check_target_contained_exact_prefix_ok() {
        let ep_name = EntrypointName::try_from("cmd").unwrap();
        let prefixes = vec![VALIDATE_INSTALL_PATH_SENTINEL.to_string()];
        let result = check_target_contained(VALIDATE_INSTALL_PATH_SENTINEL, &prefixes, &ep_name, "target");
        assert!(result.is_ok(), "exact prefix match must succeed: {result:?}");
    }

    /// Happy path: path under allowed prefix.
    #[test]
    fn check_target_contained_subpath_ok() {
        let ep_name = EntrypointName::try_from("cmd").unwrap();
        let prefixes = vec![VALIDATE_INSTALL_PATH_SENTINEL.to_string()];
        let resolved = format!("{VALIDATE_INSTALL_PATH_SENTINEL}/bin/cmd");
        let result = check_target_contained(&resolved, &prefixes, &ep_name, "target");
        assert!(result.is_ok(), "subpath under prefix must succeed: {result:?}");
    }

    /// Error path: path outside all allowed prefixes.
    #[test]
    fn check_target_contained_outside_prefix_errors() {
        let ep_name = EntrypointName::try_from("cmd").unwrap();
        let prefixes = vec![VALIDATE_INSTALL_PATH_SENTINEL.to_string()];
        let result = check_target_contained("/etc/passwd", &prefixes, &ep_name, "/etc/passwd");
        assert!(result.is_err(), "path outside allowed prefixes must fail");
    }

    /// Error path: `_evil` suffix after prefix without separator must be rejected.
    #[test]
    fn check_target_contained_prefix_substring_without_separator_rejected() {
        let ep_name = EntrypointName::try_from("cmd").unwrap();
        let prefixes = vec![VALIDATE_INSTALL_PATH_SENTINEL.to_string()];
        let evil = format!("{VALIDATE_INSTALL_PATH_SENTINEL}_evil");
        let result = check_target_contained(&evil, &prefixes, &ep_name, &evil);
        assert!(
            result.is_err(),
            "prefix substring without path separator must fail: {result:?}"
        );
    }

    // ── check_no_unknown_tokens — per-concern unit tests ──────────────────────

    /// Happy path: no tokens remain after substitution.
    #[test]
    fn check_no_unknown_tokens_clean_string_ok() {
        let ep_name = EntrypointName::try_from("cmd").unwrap();
        let result = check_no_unknown_tokens("/some/path/cmd", &ep_name);
        assert!(result.is_ok());
    }

    /// Error path: unknown token present in resolved string.
    #[test]
    fn check_no_unknown_tokens_unknown_token_errors() {
        let ep_name = EntrypointName::try_from("cmd").unwrap();
        let result = check_no_unknown_tokens("${unknownToken}/bin/cmd", &ep_name);
        assert!(result.is_err(), "unknown token must cause error");
    }

    // ── sentinel constant values ──────────────────────────────────────────────

    /// Constants referenced by name in tests must not have been accidentally changed.
    #[test]
    fn sentinel_constants_have_expected_values() {
        assert_eq!(VALIDATE_INSTALL_PATH_SENTINEL, "/__OCX_INSTALL_PATH_SENTINEL__");
        assert_eq!(VALIDATE_DEP_SENTINEL_PREFIX, "/__OCX_DEP_SENTINEL_");
    }
}
