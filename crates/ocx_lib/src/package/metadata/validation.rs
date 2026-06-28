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
//! - [`validate_entrypoint_args`] — checks that every `args` element in every
//!   entrypoint uses only permitted tokens (`${installPath}`); `${deps.*}` and
//!   unknown placeholders are rejected at publish time.
//!
//! Entrypoint uniqueness is enforced at construction time by
//! [`super::entrypoint::Entrypoints::new`] (also from the serde path), so no
//! publish-time entrypoint validation step is needed here.

use std::collections::HashMap;

use super::Metadata;
use super::dependency::{Dependencies, Dependency, DependencyName};
use super::slug::DEP_TOKEN_PATTERN;
use super::template::{TemplateError, disallowed_dep_token};

/// Matches any `${...}` token. After [`validate_env_tokens`] strips the two
/// recognized forms (`${installPath}` and `${deps.NAME.FIELD}`), anything still
/// matching here is an unknown placeholder and is rejected.
static UNKNOWN_TOKEN_RE: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"\$\{[^}]*\}").expect("valid regex"));

/// Returns the first `${...}` placeholder in `value` that is neither
/// `${installPath}` nor a valid `${deps.NAME.FIELD}` token, or `None` if every
/// placeholder is recognized. Shared by `validate_env_tokens` and
/// `validate_entrypoint_args` so the "unknown placeholder" rule has one source.
fn first_unknown_placeholder(value: &str) -> Option<String> {
    UNKNOWN_TOKEN_RE
        .find_iter(value)
        .map(|m| m.as_str())
        .find(|seg| *seg != "${installPath}" && !DEP_TOKEN_PATTERN.is_match(seg))
        .map(str::to_string)
}

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
    /// - An entrypoint `args` element contains a `${deps.*}` token or an unknown placeholder.
    fn try_from(metadata: Metadata) -> Result<Self, Self::Error> {
        validate_env_tokens(&metadata)?;
        validate_entrypoint_args(&metadata)?;
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
        if let Some(prev) = name_map.insert(name.clone(), dep) {
            // `prev` is the first dep with this name; store it so callers can
            // include both identifiers in the ambiguity error message.
            collision_map.insert(name, prev);
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

    let deps = metadata.dependencies();
    let (name_map, collision_map) = build_name_and_collision_maps(deps);

    if let Some(env) = metadata.env() {
        for var in env {
            let value = match var.value() {
                Some(v) => v,
                None => continue,
            };

            // Dep-token regex shared with `template::resolve_inner`
            // via `slug::DEP_TOKEN_PATTERN` so the sites cannot drift out of sync.
            for cap in DEP_TOKEN_PATTERN.captures_iter(value) {
                // Invariant: DEP_TOKEN_PATTERN defines exactly 2 capture groups; a successful
                // captures_iter match guarantees groups 1 and 2 are Some.
                let dep_name = cap
                    .get(1)
                    .expect("regex group 1 guaranteed by DEP_TOKEN_PATTERN")
                    .as_str();
                let field = cap
                    .get(2)
                    .expect("regex group 2 guaranteed by DEP_TOKEN_PATTERN")
                    .as_str();

                // The DEP_TOKEN_PATTERN regex only matches the slug pattern so dep_name is always
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

            // W1: reject any leftover `${...}` that is not `${installPath}` and was not
            // consumed by the DEP_TOKEN_PATTERN loop above (e.g. `${unknown}`,
            // `${installpath}`, `${deps.foo.install_path}`, `${deps.Python.installPath}`).
            // Routed through `first_unknown_placeholder` so the recognition logic is
            // shared with `validate_entrypoint_args` below.
            if let Some(placeholder) = first_unknown_placeholder(value) {
                return Err(Error::EnvVarInterpolation {
                    var_key: var.key.clone(),
                    source: TemplateError::UnknownPlaceholder { placeholder },
                }
                .into());
            }
        }
    }

    Ok(())
}

/// Validates entrypoint `args` elements at publish time.
///
/// Checks that every element in every entrypoint's `args` list uses only
/// permitted tokens (`${installPath}`). Rejects `${deps.*}` tokens with a
/// dedicated [`crate::package::error::Error::EntrypointArgInterpolation`]
/// error (not the misleading `UnknownDependencyRef`) and rejects unknown
/// placeholders such as `${installpath}` (wrong case) or `${foo}`.
///
/// Pure syntax check — no filesystem access.
///
/// # Errors
///
/// Returns an error if any arg element contains a disallowed or unknown token.
pub(super) fn validate_entrypoint_args(metadata: &Metadata) -> Result<(), crate::Error> {
    use super::super::error::Error;

    let Some(entrypoints) = metadata.entrypoints() else {
        return Ok(());
    };

    for (name, entry) in entrypoints.iter() {
        for arg in entry.args() {
            // Check for disallowed ${deps.*} token FIRST — the dep regex recognises
            // these tokens and would NOT cause first_unknown_placeholder to flag them,
            // so the disallowed-token check must precede the unknown-placeholder scan.
            if let Some(token) = disallowed_dep_token(arg) {
                return Err(Error::EntrypointArgInterpolation {
                    entrypoint: name.to_string(),
                    arg: arg.clone(),
                    source: TemplateError::DisallowedToken { token },
                }
                .into());
            }

            if let Some(placeholder) = first_unknown_placeholder(arg) {
                return Err(Error::EntrypointArgInterpolation {
                    entrypoint: name.to_string(),
                    arg: arg.clone(),
                    source: TemplateError::UnknownPlaceholder { placeholder },
                }
                .into());
            }
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::metadata::Metadata;

    fn hex(n: u8) -> String {
        // Build a 64-char fixture digest by repeating the single hex digit of `n`
        // (e.g. n=1 → "111…1", 64 chars). Callers pass small distinct n values
        // (1, 2, …) so each test gets a distinct, recognizable digest fixture.
        let digit = format!("{n:x}");
        digit.chars().cycle().take(64).collect()
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

    // 3.3 — valid: env ref matches declared dep name
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

    // 3.3 — error: collision — two same-basename deps (no name override) + token → AmbiguousDependencyRef
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

    // W1 — leftover ${...} rejection in env values

    // W1.1 — completely unknown placeholder is rejected
    #[test]
    fn unknown_placeholder_in_env_value_rejected() {
        let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", "${unknown}"));
        let err = ValidMetadata::try_from(meta).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("unknown") || msg.contains("${unknown}"),
            "expected unknown placeholder in error: {msg}"
        );
    }

    // W1.2 — wrong case: ${installpath} (lowercase) is rejected
    #[test]
    fn lowercase_install_path_placeholder_rejected() {
        let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", "${installpath}/bin"));
        let err = ValidMetadata::try_from(meta).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("installpath") || msg.contains("${installpath}"),
            "expected installpath placeholder in error: {msg}"
        );
    }

    // W1.3 — snake_case field: ${deps.foo.install_path} is rejected
    // The DEP_TOKEN regex only accepts [a-zA-Z]+ for the field segment, so
    // install_path (contains underscore) does not match and the whole token
    // is treated as an unknown placeholder by UNKNOWN_TOKEN_RE.
    #[test]
    fn snake_case_field_placeholder_rejected() {
        let meta = make_metadata(&dep_json("foo", None), &constant_env("X", "${deps.foo.install_path}"));
        let err = ValidMetadata::try_from(meta).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("install_path") || msg.contains("${deps.foo.install_path}"),
            "expected install_path placeholder in error: {msg}"
        );
    }

    // W1.4 — uppercase dep NAME: ${deps.Python.installPath} is rejected
    // The slug pattern [a-z0-9][a-z0-9_-]* forbids uppercase, so DEP_TOKEN does
    // not match and the token falls through to the UNKNOWN_TOKEN_RE check.
    #[test]
    fn uppercase_dep_name_placeholder_rejected() {
        let meta = make_metadata(
            &dep_json("python", None),
            &constant_env("X", "${deps.Python.installPath}"),
        );
        let err = ValidMetadata::try_from(meta).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Python") || msg.contains("${deps.Python.installPath}"),
            "expected Python placeholder in error: {msg}"
        );
    }

    // ── validate_entrypoint_args ──────────────────────────────────────────────

    /// Helper: parse a Metadata from an `entrypoints` JSON object string.
    /// No deps or env declared — entrypoint arg validation must not require them.
    fn make_metadata_with_entrypoints(entrypoints_json: &str) -> Metadata {
        let json = format!(r#"{{"type":"bundle","version":1,"entrypoints":{entrypoints_json}}}"#);
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("bad test JSON: {e}\n{json}"))
    }

    // Contract 8 — MUST FAIL against the current no-op validate_entrypoint_args stub.
    //
    // A `${deps.*}` token in any entrypoint arg must cause ValidMetadata::try_from to
    // return Err. The error message must name the entrypoint ("run"), carry the token
    // text ("deps.foo"), and state the policy ("is only valid in env values").
    //
    // Note: deps need NOT be declared in the metadata — the gate rejects ${deps.*}
    // in args unconditionally, distinct from env validation where undeclared refs
    // produce UnknownDependencyRef instead.
    #[test]
    fn entrypoint_arg_deps_token_rejected() {
        let meta =
            make_metadata_with_entrypoints(r#"{"run":{"command":"python","args":["${deps.foo.installPath}/x"]}}"#);
        let err =
            ValidMetadata::try_from(meta).expect_err("${deps.*} in entrypoint args must be rejected at publish time");
        let msg = format!("{err}");
        assert!(
            msg.contains("run"),
            "error must name the offending entrypoint 'run': {msg}"
        );
        assert!(
            msg.contains("deps.foo"),
            "error must contain the token text 'deps.foo': {msg}"
        );
        assert!(
            msg.contains("is only valid in env values"),
            "error must state that deps tokens are only valid in env values: {msg}"
        );
    }

    // Contract 9a — MUST FAIL against the current no-op stub.
    //
    // `${installpath}` (wrong case) in an arg is an unknown placeholder and must
    // be rejected. The error message must mention "installpath".
    #[test]
    fn entrypoint_arg_unknown_placeholder_wrong_case_rejected() {
        let meta = make_metadata_with_entrypoints(r#"{"run":{"args":["${installpath}/x"]}}"#);
        let err =
            ValidMetadata::try_from(meta).expect_err("${installpath} (wrong case) in entrypoint args must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("installpath"),
            "error must mention 'installpath' to help publisher spot the casing mistake: {msg}"
        );
    }

    // Contract 9b — MUST FAIL against the current no-op stub.
    //
    // A completely unknown placeholder `${foo}` in an arg must be rejected, and
    // the error message must mention "foo".
    #[test]
    fn entrypoint_arg_unknown_placeholder_rejected() {
        let meta = make_metadata_with_entrypoints(r#"{"run":{"args":["${foo}"]}}"#);
        let err = ValidMetadata::try_from(meta).expect_err("${foo} in entrypoint args must be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("foo"),
            "error must mention the unknown placeholder 'foo': {msg}"
        );
    }

    // Happy path — ${installPath} in entrypoint args must be accepted (Ok).
    #[test]
    fn entrypoint_arg_install_path_ok() {
        let meta =
            make_metadata_with_entrypoints(r#"{"run":{"command":"python","args":["${installPath}/app/main.py"]}}"#);
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "${{installPath}} in entrypoint args must be accepted at publish time"
        );
    }

    // Backward-compat: entrypoint with no args field must not trigger any error.
    #[test]
    fn entrypoint_arg_backward_compat_no_args() {
        let meta = make_metadata_with_entrypoints(r#"{"run":{}}"#);
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "entrypoint with no args field must be accepted (backward-compatible)"
        );
    }

    // Backward-compat: entrypoint with command but no args must not trigger any error.
    #[test]
    fn entrypoint_arg_backward_compat_command_only() {
        let meta = make_metadata_with_entrypoints(r#"{"run":{"command":"python"}}"#);
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "entrypoint with command but no args must be accepted (backward-compatible)"
        );
    }

    // Backward-compat: metadata with no entrypoints key at all must not trigger any error.
    #[test]
    fn entrypoint_arg_backward_compat_no_entrypoints() {
        let json = r#"{"type":"bundle","version":1}"#;
        let meta: Metadata = serde_json::from_str(json).unwrap();
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "metadata without any entrypoints key must be accepted"
        );
    }

    // No-FS invariant: arg validation is pure syntax — it never stats the filesystem.
    // An ${installPath} arg pointing to a deeply nested non-existent path must not
    // produce an error; publish-time validation is not a path-existence check.
    #[test]
    fn entrypoint_arg_validation_does_no_fs() {
        let meta =
            make_metadata_with_entrypoints(r#"{"run":{"args":["${installPath}/nonexistent/deeply/nested.xyz"]}}"#);
        assert!(
            ValidMetadata::try_from(meta).is_ok(),
            "${{installPath}} arg with non-existent path must be accepted (validation is pure syntax, no FS)"
        );
    }
}
