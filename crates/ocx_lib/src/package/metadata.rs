// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::{Deserialize, Serialize};

pub mod bundle;
pub mod dependency;
pub mod entry_point;
pub mod env;
pub mod template;
pub mod visibility;

// Re-export entry_point public API so callers can use `metadata::EntryPoint` etc.
pub use entry_point::{EntryPoint, EntryPointError, EntryPointName, EntryPoints};

/// OCX package metadata.
///
/// Declares a package's type, version, extraction options, and environment variables.
/// Currently only the `bundle` type is supported.
#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Metadata {
    Bundle(bundle::Bundle),
}

impl Metadata {
    pub fn env(&self) -> Option<&env::Env> {
        match self {
            Metadata::Bundle(bundle) => Some(&bundle.env),
        }
    }

    pub fn dependencies(&self) -> &dependency::Dependencies {
        match self {
            Metadata::Bundle(bundle) => &bundle.dependencies,
        }
    }

    /// Returns the `entry_points` field for bundle metadata, or `None` for non-bundle variants.
    ///
    /// Mirrors the pattern of `bundle_dependencies()` — returns `None` for
    /// future non-bundle metadata types so callers can opt into entry-point
    /// behavior without checking the variant themselves.
    pub fn bundle_entry_points(&self) -> Option<&entry_point::EntryPoints> {
        match self {
            Metadata::Bundle(bundle) => Some(&bundle.entry_points),
        }
    }
}

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
    /// - A token references a name not matching any direct dependency (by alias or basename).
    /// - A token references an unsupported field (currently only `installPath` is supported).
    /// - Two direct dependencies share the same interpolation name (collision) and a token
    ///   uses that name — in this case `alias` must be set to disambiguate.
    fn try_from(metadata: Metadata) -> Result<Self, Self::Error> {
        use self::template::TemplateError;
        use super::error::Error;
        use std::collections::HashMap;

        static DEP_TOKEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
            regex::Regex::new(r"\$\{deps\.([a-z0-9][a-z0-9_-]*)\.([a-zA-Z]+)\}").expect("valid regex")
        });

        let deps = metadata.dependencies();

        // Build name → identifier map; detect basename collisions.
        let mut name_map: HashMap<&str, &dependency::Dependency> = HashMap::new();
        let mut collision_map: HashMap<&str, &dependency::Dependency> = HashMap::new();

        for dep in deps {
            let name = dep.name();
            if name_map.insert(name, dep).is_some() {
                // Second dep with the same name — store in collision map.
                collision_map.insert(name, dep);
            }
        }

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

                    // Check for collision first.
                    if collision_map.contains_key(dep_name) {
                        let first = name_map[dep_name].identifier.clone();
                        let second = collision_map[dep_name].identifier.clone();
                        return Err(Error::EnvVarInterpolation {
                            var_key: var.key.clone(),
                            source: TemplateError::AmbiguousDependencyRef {
                                ref_name: dep_name.to_string(),
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
                                ref_name: dep_name.to_string(),
                                declared: name_map.keys().map(|s| s.to_string()).collect(),
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

        // Entry point target validation.
        if let Some(entry_points) = metadata.bundle_entry_points() {
            validate_entry_points(entry_points, &name_map)?;
        }

        Ok(Self(metadata))
    }
}

/// Sentinel string substituted for `${installPath}` at publish-time validation.
///
/// The launcher's resolved target string is compared against this prefix to
/// enforce ADR §4 containment: every `target` must resolve to a path under
/// `${installPath}` or a declared `${deps.NAME.installPath}`. Real install
/// paths are not known at publish time, so a recognizable sentinel stands in.
const VALIDATE_INSTALL_PATH_SENTINEL: &str = "/__OCX_INSTALL_PATH_SENTINEL__";

/// Prefix for per-dep sentinel install paths during publish-time validation.
/// Combined with the dep's interpolation name to keep prefixes unique.
const VALIDATE_DEP_SENTINEL_PREFIX: &str = "/__OCX_DEP_SENTINEL_";

/// Matches any `${...}` token. Used after `TemplateResolver::resolve_for_validate`
/// substitutes recognized tokens; anything still present is an unknown placeholder.
static UNKNOWN_TOKEN_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"\$\{[^}]*\}").expect("valid regex"));

/// Validates that all entry point targets reference only declared dependencies and
/// known fields, contain no path-traversal escapes, contain no unknown placeholders,
/// and contain no characters that would corrupt a generated launcher.
///
/// Uses sentinel strings for `${installPath}` and each declared dep's
/// `${...installPath}` so resolution happens without filesystem access — Windows
/// publishers no longer rely on the host's `/` directory existing.
fn validate_entry_points(
    entry_points: &entry_point::EntryPoints,
    name_map: &std::collections::HashMap<&str, &dependency::Dependency>,
) -> Result<(), crate::Error> {
    use self::env::accumulator::DependencyContext;
    use self::template::TemplateResolver;
    use super::error::Error;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    use crate::package_manager::entry_points::LauncherSafeString;
    use crate::utility::fs::path::lexical_normalize;

    if entry_points.is_empty() {
        return Ok(());
    }

    // Build dep_contexts keyed by name. Each dep gets a unique sentinel install
    // path so that path-prefix containment can distinguish which `${deps.NAME.*}`
    // a resolved target belongs to. No filesystem access happens at validate time;
    // the resolver's `resolve_for_validate` mode skips the `install_path.exists()`
    // check that the runtime mode performs.
    let install_sentinel = Path::new(VALIDATE_INSTALL_PATH_SENTINEL);
    let dep_contexts: HashMap<String, DependencyContext> = name_map
        .iter()
        .map(|(name, dep)| {
            let sentinel = PathBuf::from(format!("{VALIDATE_DEP_SENTINEL_PREFIX}{name}__"));
            (
                name.to_string(),
                DependencyContext::new(dep.identifier.clone(), sentinel),
            )
        })
        .collect();

    let resolver = TemplateResolver::new(install_sentinel, &dep_contexts);

    // Pre-compute the set of acceptable containment prefixes once.
    let mut allowed_prefixes: Vec<String> = Vec::with_capacity(name_map.len() + 1);
    allowed_prefixes.push(VALIDATE_INSTALL_PATH_SENTINEL.to_string());
    for name in name_map.keys() {
        allowed_prefixes.push(format!("{VALIDATE_DEP_SENTINEL_PREFIX}{name}__"));
    }

    for entry in entry_points.iter() {
        let resolved =
            resolver
                .resolve_for_validate(&entry.target)
                .map_err(|source| Error::EntryPointTargetInvalid {
                    name: entry.name.to_string(),
                    source,
                })?;

        // R1.4: any `${...}` token still present after `resolve_for_validate` is an
        // unknown placeholder (`${installPath}` and `${deps.NAME.FIELD}` would have
        // been substituted). Reject so it can't be baked into a launcher as a
        // literal.
        if let Some(m) = UNKNOWN_TOKEN_RE.find(&resolved) {
            return Err(Error::EntryPointTargetInvalid {
                name: entry.name.to_string(),
                source: self::template::TemplateError::UnknownPlaceholder {
                    placeholder: m.as_str().to_string(),
                },
            }
            .into());
        }

        // R1.2: containment check — `lexical_normalize` collapses `..` traversals
        // syntactically (no filesystem access). After normalization the path
        // must still start with one of the sentinel install-root prefixes; any
        // absolute target outside the sentinel set, or any `..`-traversal that
        // escapes its sentinel, fails this prefix match.
        //
        // Bare prefix string match alone would let `${installPath}_evil` slip
        // through (substring without separator), so require an exact match or
        // a path-separator boundary after the prefix.
        let normalized = lexical_normalize(Path::new(&resolved));
        let normalized_str = normalized.to_string_lossy();
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
            return Err(Error::EntryPointTargetInvalid {
                name: entry.name.to_string(),
                source: self::template::TemplateError::TargetEscapesInstallRoot {
                    target: entry.target.clone(),
                },
            }
            .into());
        }

        // R1.3: defense-in-depth — surface unsafe-launcher characters at publish
        // time so publishers don't ship a package whose first install fails on
        // every consumer.
        LauncherSafeString::new(resolved.clone())?;
    }

    Ok(())
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

#[cfg(test)]
mod valid_metadata_tests {
    use super::*;

    fn make_metadata(deps_json: &str, env_json: &str) -> Metadata {
        let json = format!(r#"{{"type":"bundle","version":1,"dependencies":[{deps_json}],"env":[{env_json}]}}"#);
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("bad test JSON: {e}\n{json}"))
    }

    fn hex(n: u8) -> String {
        format!("{:x}", n).repeat(64).chars().take(64).collect()
    }

    fn dep_json(repo: &str, alias: Option<&str>) -> String {
        let h = hex(1);
        match alias {
            Some(a) => format!(r#"{{"identifier":"ocx.sh/{repo}:1@sha256:{h}","alias":"{a}"}}"#),
            None => format!(r#"{{"identifier":"ocx.sh/{repo}:1@sha256:{h}"}}"#),
        }
    }

    fn constant_env(key: &str, value: &str) -> String {
        format!(r#"{{"key":"{key}","type":"constant","value":"{value}"}}"#)
    }

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

    // ── 3.2 Entry point target validation ─────────────────────────────────

    fn make_metadata_with_entry_points(entry_points_json: &str) -> Metadata {
        let json = format!(r#"{{"type":"bundle","version":1,"entry_points":[{entry_points_json}]}}"#);
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("bad test JSON: {e}\n{json}"))
    }

    fn make_metadata_with_deps_and_entry_points(deps_json: &str, entry_points_json: &str) -> Metadata {
        let json = format!(
            r#"{{"type":"bundle","version":1,"dependencies":[{deps_json}],"entry_points":[{entry_points_json}]}}"#
        );
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("bad test JSON: {e}\n{json}"))
    }

    /// Valid target resolving under ${installPath} — should pass.
    #[test]
    fn entry_point_valid_install_path_target() {
        let meta = make_metadata_with_entry_points(r#"{"name":"cmake","target":"${installPath}/bin/cmake"}"#);
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "valid ${{installPath}} target must pass validation"
        );
    }

    /// Valid target resolving under ${deps.foo.installPath} with foo in deps — should pass.
    #[test]
    fn entry_point_valid_dep_install_path_target() {
        let h = hex(1);
        let dep = format!(r#"{{"identifier":"ocx.sh/foo:1@sha256:{h}"}}"#);
        let ep = r#"{"name":"foocmd","target":"${deps.foo.installPath}/bin/foocmd"}"#;
        let meta = make_metadata_with_deps_and_entry_points(&dep, ep);
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "valid ${{deps.foo.installPath}} target with declared dep must pass validation"
        );
    }

    /// Target referencing undeclared dep — should yield EntryPointTargetInvalid.
    #[test]
    fn entry_point_undeclared_dep_in_target_rejected() {
        // No dependencies declared, but target references ${deps.bar.installPath}.
        let meta = make_metadata_with_entry_points(r#"{"name":"cmake","target":"${deps.bar.installPath}/bin/cmake"}"#);
        let result = ValidMetadata::try_from(meta);
        assert!(result.is_err(), "undeclared dep in target must be rejected");
        let err = result.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("cmake"), "error must name the entry point 'cmake': {msg}");
        assert!(msg.contains("bar"), "error must name the unknown dep 'bar': {msg}");
    }

    // ── 3.2 backward compat: same two same-basename deps without any ${deps.*} token → OK
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

    // ── plan_review_findings_pr64 §3.2–3.5: validation boundary at consumption ──

    /// Plan §3.2 (R1.2): an entry-point `target` set to an absolute filesystem path
    /// outside `${installPath}` and any declared `${deps.*.installPath}` must be
    /// rejected by `validate_entry_points`. Today the validator only walks `${...}`
    /// substitutions, so a fully-literal absolute path slips through.
    #[test]
    fn validate_entry_points_rejects_absolute_path_target() {
        let meta = make_metadata_with_entry_points(r#"{"name":"evilcmd","target":"/etc/passwd"}"#);
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
            "error must cite the offending entry-point or 'target': {msg}"
        );
    }

    /// Plan §3.3 (R1.2): a `${installPath}/../../etc/passwd` style traversal target must
    /// be rejected by `validate_entry_points` — resolving it under a sentinel install
    /// path should fail the path-prefix containment check after R1.2 lands.
    #[test]
    fn validate_entry_points_rejects_parent_dir_traversal() {
        let meta = make_metadata_with_entry_points(r#"{"name":"evilcmd","target":"${installPath}/../../etc/passwd"}"#);
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
            "error must cite the entry-point or describe the traversal: {msg}"
        );
    }

    /// Plan §3.4 (R1.3): a target containing a `LAUNCHER_UNSAFE_CHARS` byte (here a
    /// single quote that would break the Unix launcher's single-quoted string) must
    /// be rejected at validate time as defense-in-depth. Today validation only checks
    /// dep references; the unsafe-char check fires only at install time.
    #[test]
    fn validate_entry_points_rejects_target_with_unsafe_char() {
        let meta = make_metadata_with_entry_points(r#"{"name":"evilcmd","target":"${installPath}/bin/it's-bad"}"#);
        let result = ValidMetadata::try_from(meta);
        assert!(
            result.is_err(),
            "target with unsafe character (single quote) must be rejected at validate time; got: {:?}",
            result
        );
    }

    /// Plan §3.5 (R1.4): a target containing an unrecognized `${unknownToken}` must be
    /// rejected by `validate_entry_points`. Today unknown tokens pass through as
    /// literals (see `entry_point_unknown_placeholder_rejected` above) — that test
    /// documents current behavior; this one encodes the post-fix contract.
    #[test]
    fn validate_entry_points_rejects_unknown_token() {
        let meta = make_metadata_with_entry_points(r#"{"name":"evilcmd","target":"${unknownToken}/bin/cmake"}"#);
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

    // 3.3 — transitive scoping: R has direct dep D but tokens ref T (D's dep) → error
    #[test]
    fn transitive_dep_ref_errors() {
        // R's metadata only declares D, not T
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
}
