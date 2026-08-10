// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Structural and publish-time validation for package metadata.
//!
//! The two are deliberately different layers (D14 —
//! `adr_interpolation_token_grammar.md`). [`ValidMetadata::try_from`] runs on
//! **every** ingress path, so it may only assert that a document is
//! *structurally readable*:
//!
//! - `validate_env_modifier_types` — refuses an env var whose modifier `type`
//!   this binary does not know, so a package built for a newer ocx fails closed
//!   with a version remedy instead of running with a silently wrong environment.
//! - `validate_env_list_entries` — enforces the wire contract for `list`
//!   entries: a separator is present, foldable, and does not edge its value.
//!
//! [`validate_for_publish`] is the strict gate on top of it, run by
//! `ocx package create` / `ocx package push` where a publisher is present:
//!
//! - `validate_env_tokens` — scans every env var value and checks that each
//!   `${deps.NAME.installPath}` token references a declared, non-ambiguous dep,
//!   and that each `${self.env.KEY}` names exactly one var declared strictly
//!   earlier. An unsupported field never reaches the reference check: the scan
//!   itself refuses `${deps.NAME.version}`.
//! - `validate_entrypoint_args` — scans every `args` element in every
//!   entrypoint and refuses the token classes `Usage::EntryPointArgs` does not
//!   permit.
//!
//! Refusing an unrecognised token on a *read* path is what D14 removes: an ocx
//! meeting a token it does not know still shows the package, and refuses only
//! when something asks for the value. Compose and execute need no gate of their
//! own — `TemplateResolver::resolve` cannot produce bytes for a token it does
//! not recognise.
//!
//! Entrypoint uniqueness is enforced at construction time by
//! [`super::entrypoint::Entrypoints::new`] (also from the serde path), so no
//! publish-time entrypoint validation step is needed here.

use std::collections::HashMap;

use super::Metadata;
use super::dependency::{Dependencies, Dependency};
use super::template::scanner::{self, Segment, Token};
use super::template::{AllowedTokens, TemplateError, Usage, first_disallowed_token};

// ── ValidMetadata ─────────────────────────────────────────────────────────────

/// Metadata this binary can structurally read.
///
/// Constructed exclusively via `TryFrom<Metadata>`, which verifies that every
/// env var declares a modifier type this binary knows and that every `list`
/// entry satisfies the separator contract — statements about the document's
/// *grammar*, not about whether its values resolve.
///
/// It deliberately does **not** promise that the document's interpolation
/// tokens are recognised or that its `${deps.*}` references resolve: that is
/// [`validate_for_publish`]'s job at publish time, and
/// `TemplateResolver::resolve`'s at compose time (D14).
///
/// Derefs to [`Metadata`] for read access without unwrapping.
#[derive(Debug)]
pub struct ValidMetadata(Metadata);

impl TryFrom<Metadata> for ValidMetadata {
    type Error = crate::Error;

    /// # Errors
    ///
    /// Returns an error if an env var declares an unknown modifier `type`, or
    /// if a `list` entry is missing its separator, declares an unusable one, or
    /// carries a value that separator edges.
    fn try_from(metadata: Metadata) -> Result<Self, Self::Error> {
        // Runs first: an unknown modifier type means the reader is too old for
        // this package, which is the answer the user needs — reporting a
        // complaint about a var whose grammar we cannot read would send them to
        // fix the wrong thing.
        validate_env_modifier_types(&metadata)?;
        validate_env_list_entries(&metadata)?;
        Ok(Self(metadata))
    }
}

/// The publish gate: structural readability **plus** every token check.
///
/// `ocx package create` and `ocx package push` call this instead of
/// [`ValidMetadata::try_from`]. It is the one explicit enforcement point D14
/// keeps — the publisher is present, and a typo must not reach a registry.
/// Every other refusal is the resolver's own, at the operation that needs the
/// value.
///
/// # Errors
///
/// Everything [`ValidMetadata::try_from`] returns, plus an unrecognised
/// `${…}`, a `${deps.*}` token naming an undeclared or ambiguous dependency or
/// an unsupported field, and a token class an entrypoint `args` element may
/// not carry.
pub fn validate_for_publish(metadata: Metadata) -> Result<ValidMetadata, crate::Error> {
    let valid = ValidMetadata::try_from(metadata)?;
    validate_env_tokens(&valid)?;
    validate_entrypoint_args(&valid)?;
    Ok(valid)
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

/// Rejects the first env var whose modifier `type` this binary does not know.
///
/// Parsing keeps such a var (as `Modifier::Unknown`) precisely so this gate can
/// name it. Skipping it instead would run the package in an environment its
/// publisher never published — the failure would surface downstream, in the
/// tool, with nothing pointing back at ocx's version.
///
/// # Errors
///
/// [`crate::package::error::Error::UnknownEnvModifier`] naming the var key, the
/// unrecognized type, and the remedy. Declaration order decides which var is
/// named when several are unreadable.
pub(super) fn validate_env_modifier_types(metadata: &Metadata) -> Result<(), crate::Error> {
    use super::super::error::Error;
    use super::env::modifier::Modifier;

    let Some(env) = metadata.env() else {
        return Ok(());
    };

    for var in env {
        if let Modifier::Unknown { type_name } = &var.modifier {
            return Err(Error::UnknownEnvModifier {
                key: var.key.clone(),
                type_name: type_name.clone(),
            }
            .into());
        }
    }

    Ok(())
}

/// Enforces the `list` wire contract on every list-typed env var.
///
/// The separator is **required** in package metadata, where no human is present
/// to be told which one was assumed and a wrong guess fails silently in the
/// consuming tool. Refusing it here rather than as a serde missing-field keeps
/// the message on the variable the publisher has to go fix.
///
/// # Errors
///
/// [`crate::package::error::Error::MissingListSeparator`],
/// [`crate::package::error::Error::InvalidListSeparator`], or
/// [`crate::package::error::Error::SeparatorEdgedListValue`] for the first
/// offending var in declaration order.
pub(super) fn validate_env_list_entries(metadata: &Metadata) -> Result<(), crate::Error> {
    use super::super::error::Error;
    use super::env::list;
    use super::env::modifier::Modifier;

    let Some(env) = metadata.env() else {
        return Ok(());
    };

    for var in env {
        let Modifier::List(declared) = &var.modifier else {
            continue;
        };
        let Some(separator) = declared.separator.as_deref() else {
            return Err(Error::MissingListSeparator { key: var.key.clone() }.into());
        };
        if !list::separator_is_valid(separator) {
            return Err(Error::InvalidListSeparator {
                key: var.key.clone(),
                separator: separator.to_string(),
            }
            .into());
        }
        // The authored bytes only — a value that resolves to an edged one is
        // caught again by `EnvResolver`, which is the first place the resolved
        // form exists.
        if list::is_separator_edged(&declared.value, separator) {
            return Err(Error::SeparatorEdgedListValue {
                key: var.key.clone(),
                separator: separator.to_string(),
                value: declared.value.clone(),
            }
            .into());
        }
    }

    Ok(())
}

/// Validates env var values: every `${…}` must be one of the four recognised
/// tokens, every `${deps.NAME.installPath}` must name a declared, non-ambiguous
/// dep, and every `${self.env.KEY}` must name exactly one var declared strictly
/// earlier in this same document.
///
/// Recognition is [`scanner::scan`]'s — the one recogniser (D10) — so a token
/// this gate refuses is exactly a token the resolver could not have rendered.
/// Does not consult the filesystem; pure syntax + reference check.
///
/// **Why `${self.env.*}` is refused here and not only at compose time.** The
/// reference is decidable from the document alone — no filesystem, no dep
/// contexts, no install — which is the class the publish gate already handles
/// for `${deps.*}`. Left to the composer, a forward or ambiguous reference
/// publishes cleanly and then exits 65 on every consumer: a publishable artifact
/// nobody can use. The direction is the safe one, too — the accept set may only
/// grow, so refusing now and accepting later stays available.
pub(super) fn validate_env_tokens(metadata: &Metadata) -> Result<(), crate::Error> {
    use super::super::error::Error;
    use super::template::SelfEnvScope;
    use super::template::scanner::TokenShape;

    let (name_map, collision_map) = build_name_and_collision_maps(metadata.dependencies());

    let Some(env) = metadata.env() else {
        return Ok(());
    };

    // The scope a `${self.env.KEY}` in the var being walked may name: the keys
    // declared strictly earlier, in declaration order. The same prefix the
    // composer's accumulator holds, without the values this gate has no way to
    // resolve — so the two agree on which references are legal.
    let mut declared_before: SelfEnvScope<&str> = SelfEnvScope::new();

    for var in env {
        let Some(value) = var.value() else {
            // No value template, no contribution: `Var::value()` is `None` only
            // for a modifier type this binary cannot read, and the composer's
            // accumulator skips such a var for the same reason.
            continue;
        };

        let segments = scanner::scan(value).map_err(|source| Error::EnvVarInterpolation {
            var_key: var.key.clone(),
            source,
        })?;

        for segment in segments {
            let Segment::Token(token) = segment else {
                continue;
            };
            check_token_reference(&token, &name_map, &collision_map).map_err(|source| Error::EnvVarInterpolation {
                var_key: var.key.clone(),
                source,
            })?;
            if let TokenShape::SelfEnv { key } = &token.shape {
                declared_before
                    .lookup(key)
                    .map_err(|source| Error::EnvVarInterpolation {
                        var_key: var.key.clone(),
                        source,
                    })?;
            }
        }

        declared_before.push(var.key.as_str());
    }

    Ok(())
}

/// Checks one recognised token's `${deps.*}` reference against the declared
/// direct dependencies.
///
/// Only the dependency reference lives here — shape recognition already happened
/// in [`scanner::scan`], `${installPath}` always has a referent, and
/// `${self.env.KEY}` is checked against the declaration prefix its caller
/// carries, which this function has no view of.
///
/// The field is not re-checked either. `TokenShape::Dep` carries no field:
/// `installPath` is the only leaf, so the scan refuses `${deps.cmake.version}`
/// as [`TemplateError::UnknownField`] and a `Dep` token with an unsupported
/// field cannot be constructed. A branch for it here would be a green that
/// could never go red.
fn check_token_reference(
    token: &Token<'_>,
    name_map: &HashMap<String, &Dependency>,
    collision_map: &HashMap<String, &Dependency>,
) -> Result<(), TemplateError> {
    use super::template::scanner::TokenShape;

    let TokenShape::Dep { name } = &token.shape else {
        return Ok(());
    };

    // Ambiguity first: a name two deps answer to is refused before the
    // declared-name check can pick one of them.
    if let (Some(first), Some(second)) = (name_map.get(name.as_str()), collision_map.get(name.as_str())) {
        return Err(TemplateError::AmbiguousDependencyRef {
            ref_name: name.clone(),
            first: Box::new(first.identifier.clone()),
            second: Box::new(second.identifier.clone()),
        });
    }

    if !name_map.contains_key(name.as_str()) {
        // Read back off the `Dependency` rather than reparsing the map key, so
        // the declared list needs no fallible step and no unreachable arm.
        // Sorted, because the map's own order is hash noise in a message.
        let mut declared: Vec<_> = name_map.values().map(|dependency| dependency.name()).collect();
        declared.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        return Err(TemplateError::UnknownDependencyRef {
            ref_name: name.clone(),
            declared,
        });
    }

    Ok(())
}

/// Validates entrypoint `args` elements at publish time.
///
/// Every `${…}` in every `args` element must be recognised, and must belong to
/// a token class `Usage::EntryPointArgs` permits — `${installPath}` and its
/// `${self.installPath}` alias, nothing else. `${deps.*}` and `${self.env.*}`
/// are refused as [`TemplateError::DisallowedToken`], never the misleading
/// `UnknownDependencyRef`.
///
/// Pure syntax check — no filesystem access.
///
/// # Errors
///
/// [`crate::package::error::Error::EntrypointArgInterpolation`] for the first
/// arg element carrying an unrecognised or disallowed token.
pub(super) fn validate_entrypoint_args(metadata: &Metadata) -> Result<(), crate::Error> {
    use super::super::error::Error;

    let Some(entrypoints) = metadata.entrypoints() else {
        return Ok(());
    };
    let allowed = AllowedTokens::from(Usage::EntryPointArgs);

    for (name, entry) in entrypoints.iter() {
        for arg in entry.args() {
            let segments = scanner::scan(arg).map_err(|source| Error::EntrypointArgInterpolation {
                entrypoint: name.to_string(),
                arg: arg.clone(),
                source,
            })?;

            if let Some(token) = first_disallowed_token(&segments, allowed) {
                return Err(Error::EntrypointArgInterpolation {
                    entrypoint: name.to_string(),
                    arg: arg.clone(),
                    source: TemplateError::DisallowedToken {
                        token: token.to_owned(),
                    },
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
        assert!(validate_for_publish(meta).is_ok());
    }

    // 3.3 — valid: env ref matches declared dep name
    #[test]
    fn valid_dep_ref_via_alias() {
        let meta = make_metadata(
            &dep_json("myorg/cmake", Some("my-cmake")),
            &constant_env("X", "${deps.my-cmake.installPath}"),
        );
        assert!(validate_for_publish(meta).is_ok());
    }

    // 3.3 — valid: no ${deps.*} tokens → no error
    #[test]
    fn no_dep_tokens_ok() {
        let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", "${installPath}/bin"));
        assert!(validate_for_publish(meta).is_ok());
    }

    // 3.3 — error: ref to undeclared dep name
    #[test]
    fn undeclared_dep_ref_errors() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            &constant_env("X", "${deps.ninja.installPath}"),
        );
        let err = validate_for_publish(meta).unwrap_err();
        assert!(format!("{err}").contains("ninja"), "expected ninja in error: {err}");
    }

    // 3.3 — error: unsupported field guards extensibility seam
    #[test]
    fn unsupported_field_errors() {
        let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", "${deps.cmake.version}"));
        let err = validate_for_publish(meta).unwrap_err();
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
        let err = validate_for_publish(meta).unwrap_err();
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
        assert!(validate_for_publish(meta).is_ok());
    }

    // 3.3 — transitive scoping: R has direct dep D but tokens ref T (D's dep) → error
    #[test]
    fn transitive_dep_ref_errors() {
        let meta = make_metadata(
            &dep_json("direct-dep", None),
            &constant_env("X", "${deps.transitive-tool.installPath}"),
        );
        let err = validate_for_publish(meta).unwrap_err();
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
        let err = validate_for_publish(meta).unwrap_err();
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
        let err = validate_for_publish(meta).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("installpath") || msg.contains("${installpath}"),
            "expected installpath placeholder in error: {msg}"
        );
    }

    // W1.3 — snake_case field: ${deps.foo.install_path} is rejected
    // `deps.NAME` is a recognised namespace, so the scanner reports the unknown
    // leaf by name rather than calling the whole token unrecognisable.
    #[test]
    fn snake_case_field_placeholder_rejected() {
        let meta = make_metadata(&dep_json("foo", None), &constant_env("X", "${deps.foo.install_path}"));
        let err = validate_for_publish(meta).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("install_path") || msg.contains("${deps.foo.install_path}"),
            "expected install_path placeholder in error: {msg}"
        );
    }

    // W1.4 — uppercase dep NAME: ${deps.Python.installPath} is rejected
    // The scanner validates NAME by `DependencyName::try_from`, whose pattern
    // forbids uppercase, so the body fails the anchored grammar.
    #[test]
    fn uppercase_dep_name_placeholder_rejected() {
        let meta = make_metadata(
            &dep_json("python", None),
            &constant_env("X", "${deps.Python.installPath}"),
        );
        let err = validate_for_publish(meta).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("Python") || msg.contains("${deps.Python.installPath}"),
            "expected Python placeholder in error: {msg}"
        );
    }

    // ── R-4: validate_env_modifier_types ──────────────────────────────────────

    /// An env var declaring a modifier type this binary does not know fails
    /// closed, naming the key, the type, and the remedy. Running the package
    /// with that var silently dropped would put it in a state its publisher
    /// never published.
    #[test]
    fn unknown_env_modifier_type_is_rejected_with_key_type_and_remedy() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            r#"{"key":"GODEBUG","type":"frobnicate","separator":",","value":"gctrace=1"}"#,
        );
        let message = ValidMetadata::try_from(meta)
            .expect_err("an unknown modifier type must fail closed")
            .to_string();
        assert!(message.contains("GODEBUG"), "must name the var key: {message}");
        assert!(
            message.contains("frobnicate"),
            "must name the unrecognized type: {message}"
        );
        assert!(
            message.contains("upgrade ocx"),
            "must state the remedy, not just the fault: {message}"
        );
    }

    /// The rejection carries `DataError` (65) — malformed-for-this-reader input,
    /// the same class as every other metadata refusal.
    #[test]
    fn unknown_env_modifier_type_exits_with_data_error() {
        use crate::cli::{ClassifyExitCode, ExitCode};

        let meta = make_metadata(
            &dep_json("cmake", None),
            r#"{"key":"X","type":"frobnicate","value":"v"}"#,
        );
        let error = ValidMetadata::try_from(meta).expect_err("unknown type must fail");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    /// With several unreadable vars, declaration order decides which is named —
    /// so the message is reproducible across runs rather than map-order noise.
    #[test]
    fn first_unknown_env_modifier_in_declaration_order_is_named() {
        let env = concat!(
            r#"{"key":"FIRST","type":"frobnicate","value":"a"},"#,
            r#"{"key":"SECOND","type":"map","value":"b"}"#
        );
        let meta = make_metadata(&dep_json("cmake", None), env);
        let message = ValidMetadata::try_from(meta)
            .expect_err("unknown types must fail")
            .to_string();
        assert!(message.contains("FIRST"), "the first offender must be named: {message}");
        assert!(!message.contains("SECOND"), "only one offender is reported: {message}");
    }

    /// The type gate runs before template validation: a var whose grammar this
    /// binary cannot read must not be reported as a broken template, which
    /// would send the reader to edit a value that is not the problem.
    #[test]
    fn unknown_modifier_type_is_reported_before_a_template_fault() {
        let env = concat!(
            r#"{"key":"NEWTYPE","type":"frobnicate","value":"a"},"#,
            r#"{"key":"BROKEN","type":"constant","value":"${nonsense}"}"#
        );
        let meta = make_metadata(&dep_json("cmake", None), env);
        let message = validate_for_publish(meta)
            .expect_err("both faults are fatal")
            .to_string();
        assert!(
            message.contains("upgrade ocx"),
            "the version gap outranks the template fault: {message}"
        );
    }

    /// Metadata using only known types is unaffected by the new gate.
    #[test]
    fn known_env_modifier_types_pass_the_gate() {
        let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", "${installPath}/bin"));
        assert!(ValidMetadata::try_from(meta).is_ok());
    }

    // ── W-4: the `list` wire contract ─────────────────────────────────────────

    /// The separator is required on the wire. The refusal names the variable
    /// and the field, because a serde missing-field error would name neither.
    #[test]
    fn a_list_without_a_separator_is_refused_naming_the_var_and_the_field() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            r#"{"key":"JDK_JAVA_OPTIONS","type":"list","value":"-ea"}"#,
        );
        let message = ValidMetadata::try_from(meta)
            .expect_err("the wire requires an explicit separator")
            .to_string();
        assert!(message.contains("JDK_JAVA_OPTIONS"), "must name the var: {message}");
        assert!(message.contains("separator"), "must name the field: {message}");
    }

    /// Same class as every other metadata refusal: malformed input, 65.
    #[test]
    fn a_list_without_a_separator_exits_with_data_error() {
        use crate::cli::{ClassifyExitCode, ExitCode};

        let meta = make_metadata(&dep_json("cmake", None), r#"{"key":"X","type":"list","value":"v"}"#);
        let error = ValidMetadata::try_from(meta).expect_err("missing separator must fail");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    /// An empty separator would degrade the fold's flank match to a bare
    /// substring scan, deleting text out of the middle of unrelated elements.
    #[test]
    fn an_empty_list_separator_is_refused() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            r#"{"key":"OPTS","type":"list","separator":"","value":"-ea"}"#,
        );
        let message = ValidMetadata::try_from(meta)
            .expect_err("an empty separator is unfoldable")
            .to_string();
        assert!(message.contains("OPTS"), "must name the var: {message}");
    }

    /// `=` is the `--env KEY:list:SEP=VALUE` delimiter; a separator the flag
    /// grammar cannot express must not be publishable either.
    #[test]
    fn a_list_separator_carrying_an_equals_is_refused() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            r#"{"key":"OPTS","type":"list","separator":"=","value":"-ea"}"#,
        );
        assert!(
            ValidMetadata::try_from(meta).is_err(),
            "a separator containing '=' must be refused"
        );
    }

    /// A value edged by its own separator makes the fold's flank match
    /// ambiguous — its own separator fuses with the wrapper.
    #[test]
    fn a_separator_edged_list_value_is_refused() {
        for value in [",gctrace=1", "gctrace=1,"] {
            let meta = make_metadata(
                &dep_json("cmake", None),
                &format!(r#"{{"key":"GODEBUG","type":"list","separator":",","value":"{value}"}}"#),
            );
            let message = ValidMetadata::try_from(meta)
                .expect_err("a separator-edged value must be refused")
                .to_string();
            assert!(message.contains("GODEBUG"), "must name the var: {message}");
        }
    }

    /// The happy path, including a value that legitimately carries the
    /// separator in its interior (one opaque contribution, never tokenized).
    #[test]
    fn a_well_formed_list_entry_passes_the_gate() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            r#"{"key":"GODEBUG","type":"list","separator":",","value":"gctrace=1,madvdontneed=1"}"#,
        );
        assert!(ValidMetadata::try_from(meta).is_ok());
    }

    /// A list value still goes through template validation — the list gate
    /// runs first but does not shadow it.
    #[test]
    fn a_list_value_with_an_unknown_placeholder_is_still_refused() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            r#"{"key":"OPTS","type":"list","separator":" ","value":"${nonsense}"}"#,
        );
        let message = validate_for_publish(meta)
            .expect_err("template faults still apply to list values")
            .to_string();
        assert!(message.contains("nonsense"), "must name the placeholder: {message}");
    }

    // ── D14: classification always, refusal on resolve only ───────────────────

    /// The one document C-037 and C-038's unit legs are both asserted over. A
    /// read-only leg with no failing sibling proves nothing, and a check that
    /// only ever fails is indistinguishable from one that always fails — so the
    /// two verdicts have to be about the same bytes.
    ///
    /// [`Metadata`] is not `Clone`, so each leg parses this fixture again; the
    /// document is one source either way.
    fn unrecognised_token_document() -> Metadata {
        make_metadata(&dep_json("cmake", None), &constant_env("X", "${workspaceFolder}/x"))
    }

    /// C-037 / C-038 (unit legs) / S-026 / S-027 — ingress accepts a token this
    /// binary does not recognise, and the publish gate refuses it.
    ///
    /// The ingress leg is what makes the permissive read path *reachable*: `pull`
    /// and `install` route through `ValidMetadata::try_from`, so a refusal there
    /// would mean `inspect` / `info` / `describe` have nothing left to read. The
    /// publish leg is the one explicit enforcement point D14 keeps — the
    /// publisher is present, and a typo must not reach a registry.
    #[test]
    fn an_unrecognised_token_passes_ingress_and_is_refused_at_publish() {
        assert!(
            ValidMetadata::try_from(unrecognised_token_document()).is_ok(),
            "ingress must accept an unrecognised token, or the read-only surfaces have nothing to read"
        );

        let message = validate_for_publish(unrecognised_token_document())
            .expect_err("the publish gate must refuse an unrecognised token")
            .to_string();
        assert!(
            message.contains("${workspaceFolder}"),
            "the refusal must name the token verbatim: {message}"
        );
    }

    /// The publish refusal is malformed input (65), the same class as every
    /// other metadata refusal.
    #[test]
    fn an_unrecognised_token_exits_with_data_error_at_publish() {
        use crate::cli::{ClassifyExitCode, ExitCode};

        let error = validate_for_publish(unrecognised_token_document()).expect_err("an unrecognised token must fail");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    /// C-006(a) / S-022 — the escape is the publisher's exit from the claimed
    /// space, so the escaped form of the same payload publishes. Without this
    /// leg the refusal above would be indistinguishable from "OCX refuses every
    /// value containing the bytes `workspaceFolder`".
    #[test]
    fn an_escaped_foreign_token_publishes() {
        let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", "$${workspaceFolder}/x"));
        assert!(
            validate_for_publish(meta).is_ok(),
            "$${{…}} is the only way to publish a literal ${{…}}, so it must publish"
        );
    }

    /// C-005 / S-025 — a `${` with no `}` is literal text, not a token, so it
    /// publishes. This is the one shape that is legal today and must stay legal:
    /// it is why the publish accept-set only ever grows.
    #[test]
    fn an_unterminated_open_delimiter_publishes() {
        for value in ["${installPath", "prefix ${", "${self.installPath"] {
            let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", value));
            assert!(
                validate_for_publish(meta).is_ok(),
                "{value:?} carries no token, so it must publish unchanged"
            );
        }
    }

    /// S-001 / S-010 — the alias and both render modifiers move from rejected
    /// to accepted. Nothing moves the other way.
    #[test]
    fn the_self_alias_and_its_render_modifiers_publish() {
        for value in [
            "${self.installPath}/bin",
            "${self.installPath:posix}",
            "${installPath:native}",
            "${deps.cmake.installPath:posix}/bin",
        ] {
            let meta = make_metadata(&dep_json("cmake", None), &constant_env("X", value));
            assert!(validate_for_publish(meta).is_ok(), "{value:?} must publish");
        }
    }

    /// S-012 — a modifier outside the closed set is refused at publish, and the
    /// message lists what is supported. The vocabulary is the whole enum, so a
    /// publisher who guessed gets the complete answer.
    #[test]
    fn an_unknown_render_modifier_is_refused_at_publish() {
        let meta = make_metadata(
            &dep_json("cmake", None),
            &constant_env("X", "${self.installPath:frobnicate}"),
        );
        let message = validate_for_publish(meta)
            .expect_err("a modifier outside the closed set must be refused")
            .to_string();
        assert!(
            message.contains("frobnicate"),
            "must name the offending modifier: {message}"
        );
        assert!(
            message.contains("native") && message.contains("posix"),
            "must list the supported modifiers: {message}"
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
    // A `${deps.*}` token in any entrypoint arg must cause validate_for_publish to
    // return Err, carrying `TemplateError::DisallowedToken` under the entrypoint
    // that declared the arg.
    //
    // Asserted on the variant, not on the sentence. `DisallowedToken` is what
    // separates "refused because this token class is not permitted here" from
    // `UnknownDependencyRef` ("permitted, but names nothing") — the distinction the
    // gate exists to make. A substring assertion on the remedy wording cannot tell
    // those two apart and goes green for the wrong error the moment either message
    // is reworded (C-028); only the token text itself is asserted as text.
    //
    // Note: deps need NOT be declared in the metadata — the gate rejects ${deps.*}
    // in args unconditionally, distinct from env validation where undeclared refs
    // produce UnknownDependencyRef instead.
    #[test]
    fn entrypoint_arg_deps_token_rejected() {
        use crate::package::error::Error as PackageError;

        let meta =
            make_metadata_with_entrypoints(r#"{"run":{"command":"python","args":["${deps.foo.installPath}/x"]}}"#);
        let err =
            validate_for_publish(meta).expect_err("${deps.*} in entrypoint args must be rejected at publish time");

        let crate::Error::Package(package_error) = &err else {
            panic!("expected a package error, got: {err}");
        };
        let PackageError::EntrypointArgInterpolation { entrypoint, source, .. } = package_error.as_ref() else {
            panic!("expected EntrypointArgInterpolation, got: {package_error}");
        };
        assert_eq!(entrypoint.as_str(), "run", "error must name the offending entrypoint");
        let TemplateError::DisallowedToken { token } = source else {
            panic!("expected TemplateError::DisallowedToken, got: {source:?}");
        };
        assert!(
            token.contains("deps.foo"),
            "the refused token must be named verbatim: {token}"
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
            validate_for_publish(meta).expect_err("${installpath} (wrong case) in entrypoint args must be rejected");
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
        let err = validate_for_publish(meta).expect_err("${foo} in entrypoint args must be rejected");
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
            validate_for_publish(meta).is_ok(),
            "${{installPath}} in entrypoint args must be accepted at publish time"
        );
    }

    // Backward-compat: entrypoint with no args field must not trigger any error.
    #[test]
    fn entrypoint_arg_backward_compat_no_args() {
        let meta = make_metadata_with_entrypoints(r#"{"run":{}}"#);
        assert!(
            validate_for_publish(meta).is_ok(),
            "entrypoint with no args field must be accepted (backward-compatible)"
        );
    }

    // Backward-compat: entrypoint with command but no args must not trigger any error.
    #[test]
    fn entrypoint_arg_backward_compat_command_only() {
        let meta = make_metadata_with_entrypoints(r#"{"run":{"command":"python"}}"#);
        assert!(
            validate_for_publish(meta).is_ok(),
            "entrypoint with command but no args must be accepted (backward-compatible)"
        );
    }

    // Backward-compat: metadata with no entrypoints key at all must not trigger any error.
    #[test]
    fn entrypoint_arg_backward_compat_no_entrypoints() {
        let json = r#"{"type":"bundle","version":1}"#;
        let meta: Metadata = serde_json::from_str(json).unwrap();
        assert!(
            validate_for_publish(meta).is_ok(),
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
            validate_for_publish(meta).is_ok(),
            "${{installPath}} arg with non-existent path must be accepted (validation is pure syntax, no FS)"
        );
    }

    // ── A forward `${self.env.KEY}` reference is refused at publish ────────────
    //
    // The reference is statically decidable from the document alone — no
    // filesystem, no dep contexts, no install — which is the class the publish
    // gate already handles for `${deps.*}`. Left to the composer it would
    // publish cleanly and then exit 65 on every consumer: a publishable
    // artifact nobody can use.

    /// Two vars, `A = "alpha"` and `B = "${self.env.A}"`, in the given order.
    /// The two fixtures below are byte-identical modulo the array order, which
    /// is what makes the accept/refuse pair a check rather than two unrelated
    /// documents.
    fn self_env_pair(first_is_the_reference: bool) -> Metadata {
        let reference = constant_env("B", "${self.env.A}");
        let target = constant_env("A", "alpha");
        let env = if first_is_the_reference {
            format!("{reference},{target}")
        } else {
            format!("{target},{reference}")
        };
        make_metadata(&dep_json("cmake", None), &env)
    }

    /// A reference to a var declared **later** is refused at publish, naming
    /// the referencing var and the key it could not see.
    #[test]
    fn a_forward_self_env_reference_is_refused_at_publish() {
        use crate::package::error::Error as PackageError;

        let error =
            validate_for_publish(self_env_pair(true)).expect_err("a forward reference must not reach a registry");

        let crate::Error::Package(package_error) = &error else {
            panic!("expected a package error, got: {error}");
        };
        let PackageError::EnvVarInterpolation { var_key, source } = package_error.as_ref() else {
            panic!("expected EnvVarInterpolation, got: {package_error}");
        };
        assert_eq!(var_key, "B", "the refusal must name the var that carries the reference");
        let TemplateError::UndefinedSelfEnvRef { key, .. } = source else {
            panic!("expected UndefinedSelfEnvRef, got: {source:?}");
        };
        assert_eq!(key, "A", "the refusal must name the key that was not in scope");
    }

    /// The same two vars in declaration order publish. Without this leg the
    /// refusal above is indistinguishable from refusing every
    /// `${self.env.*}` token.
    #[test]
    fn a_backward_self_env_reference_publishes() {
        assert!(
            validate_for_publish(self_env_pair(false)).is_ok(),
            "a reference to an earlier-declared var is the feature, and must publish"
        );
    }

    /// C-021 — a key declared twice before the reference is refused at publish
    /// too, and as the *ambiguous* fault rather than the undefined one.
    ///
    /// The gate and the composer share one rule (`SelfEnvScope::lookup`), so a
    /// document that publishes is one every consumer can compose; without this
    /// leg the gate's half of D7 is unpinned, and a gate that only counted "at
    /// least one" would let the ambiguity through to every consumer instead.
    #[test]
    fn a_doubly_declared_self_env_target_is_refused_at_publish() {
        use crate::package::error::Error as PackageError;

        let env = format!(
            "{},{},{}",
            constant_env("A", "first"),
            constant_env("A", "second"),
            constant_env("B", "${self.env.A}")
        );
        let error = validate_for_publish(make_metadata(&dep_json("cmake", None), &env))
            .expect_err("an ambiguous reference must not reach a registry");

        let crate::Error::Package(package_error) = &error else {
            panic!("expected a package error, got: {error}");
        };
        let PackageError::EnvVarInterpolation { var_key, source } = package_error.as_ref() else {
            panic!("expected EnvVarInterpolation, got: {package_error}");
        };
        assert_eq!(var_key, "B", "the refusal must name the var that carries the reference");
        let TemplateError::AmbiguousSelfEnvRef { key } = source else {
            panic!("expected AmbiguousSelfEnvRef, got: {source:?}");
        };
        assert_eq!(key, "A", "the refusal must name the key that was declared twice");
    }

    /// The refusal is malformed input (65), the same class as every other
    /// metadata refusal.
    #[test]
    fn a_forward_self_env_reference_exits_with_data_error() {
        use crate::cli::{ClassifyExitCode, ExitCode};

        let error = validate_for_publish(self_env_pair(true)).expect_err("a forward reference must fail");
        assert_eq!(error.classify(), Some(ExitCode::DataError));
    }

    /// D14 — the refusal belongs to the publish gate, not to ingress. An older
    /// ocx meeting the same document must still be able to *look* at it.
    #[test]
    fn a_forward_self_env_reference_still_passes_ingress() {
        assert!(
            ValidMetadata::try_from(self_env_pair(true)).is_ok(),
            "token references are a publish-time concern; ingress only asserts structural readability"
        );
    }

    /// C-028 (publish leg) — `${self.env.*}` is legal in env values only, so an
    /// entrypoint arg refuses it as `DisallowedToken`.
    ///
    /// Asserted on the variant, which is what makes the leg falsifiable:
    /// delete the D9 gate and publish *accepts* the document, because args
    /// carry no self-env scope for a reference check to fail against. The
    /// runtime leg lives at
    /// `template::tests::entrypoint_args_reject_a_self_env_token_as_disallowed`.
    #[test]
    fn a_self_env_token_in_an_entrypoint_arg_is_refused_as_disallowed() {
        use crate::package::error::Error as PackageError;

        let meta = make_metadata_with_entrypoints(r#"{"run":{"args":["${self.env.TOOL_HOME}/x"]}}"#);
        let error = validate_for_publish(meta).expect_err("${self.env.*} in entrypoint args must be refused");

        let crate::Error::Package(package_error) = &error else {
            panic!("expected a package error, got: {error}");
        };
        let PackageError::EntrypointArgInterpolation { entrypoint, source, .. } = package_error.as_ref() else {
            panic!("expected EntrypointArgInterpolation, got: {package_error}");
        };
        assert_eq!(entrypoint.as_str(), "run");
        let TemplateError::DisallowedToken { token } = source else {
            panic!("expected TemplateError::DisallowedToken, got: {source:?}");
        };
        assert_eq!(
            token, "${self.env.TOOL_HOME}",
            "the refused token must be named verbatim"
        );
    }
}
