// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Project-tier environment declarations (`[env]` and `[group.<name>.env]`).
//!
//! A project declares environment variables the same way it declares tools:
//! in `ocx.toml`, checked in beside the code. The value grammar is a union —
//! a bare string is a constant, a table names its modifier explicitly:
//!
//! ```toml
//! [env]
//! CI = "1"                                            # constant
//! JAVA_OPTS = { type = "constant", value = "-Xmx2g" } # same, explicit
//! PATH = { type = "path", value = "node_modules/.bin" }
//! ```
//!
//! `constant` replaces, `path` prepends. A relative `path` value resolves
//! against the project root (the directory holding `ocx.toml`); an absolute
//! one passes through. That is what makes deferring interpolation viable —
//! `PATH` prepending, the one case that genuinely needs a dynamic value, is
//! expressible with no template engine and no `${projectRoot}` token.
//!
//! Deliberately **not** [`crate::package::metadata::env::var::Var`]: `Var`
//! carries a `visibility` axis and `${installPath}` templating, and neither
//! has project-tier meaning. A project is never a dependency of anything, so
//! there is no edge to gate — `--self` has no effect on these entries.
//!
//! Values are literal in v1. No interpolation of any kind.

use std::collections::BTreeMap;
use std::collections::btree_map;
use std::path::Path;

use super::error::ProjectErrorKind;
use crate::package::metadata::env::entry::Entry;
use crate::package::metadata::env::modifier::ModifierKind;

/// TOML table path of the default group's env, used as the `scope` in every
/// [`ProjectErrorKind`] this module raises. Named groups build theirs as
/// `group.<name>.env`.
pub const DEFAULT_ENV_SCOPE: &str = "env";

/// One declared project-tier environment value: a modifier and a literal.
///
/// Normalized — the string shorthand and the `{ type, value }` table both
/// land here, so nothing downstream branches on which spelling was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvValue {
    /// How the value combines with any existing binding for the key:
    /// [`ModifierKind::Constant`] replaces, [`ModifierKind::Path`] prepends.
    pub kind: ModifierKind,
    /// The literal value, verbatim as written. Never interpolated.
    pub value: String,
}

impl EnvValue {
    /// A [`ModifierKind::Constant`] value — the bare-string shorthand's
    /// meaning, and the overwhelmingly common case.
    pub fn constant(value: impl Into<String>) -> Self {
        Self {
            kind: ModifierKind::Constant,
            value: value.into(),
        }
    }

    /// A [`ModifierKind::Path`] value, prepended to the key's existing value.
    pub fn path(value: impl Into<String>) -> Self {
        Self {
            kind: ModifierKind::Path,
            value: value.into(),
        }
    }
}

/// The `[env]` / `[group.<name>.env]` table: declared keys in sorted order.
///
/// Sorted rather than insertion-ordered so a serializer round-trip and the
/// resolved [`Entry`] order are both deterministic regardless of how the
/// author laid the file out.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProjectEnv {
    entries: BTreeMap<String, EnvValue>,
}

impl ProjectEnv {
    /// `true` when nothing is declared. Drives `skip_serializing_if` so an
    /// untouched config never grows an empty `[env]` table on write-back.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The declared value for `key`, if any.
    pub fn get(&self, key: &str) -> Option<&EnvValue> {
        self.entries.get(key)
    }

    /// Declared `(key, value)` pairs in sorted key order.
    pub fn iter(&self) -> btree_map::Iter<'_, String, EnvValue> {
        self.entries.iter()
    }

    /// Parse a raw `[env]` table, validating both the key policy and the
    /// value grammar.
    ///
    /// `scope` is the TOML table path (`"env"`, `"group.ci.env"`) and appears
    /// verbatim in every error, so a diagnostic names the table the user has
    /// to go edit. This is the canonical parse entry point — the
    /// [`serde::Deserialize`] impl below exists only to satisfy derives.
    ///
    /// Key validation must delegate to [`crate::env::is_valid_env_key`], the
    /// single shared validator already gating both shell emitters and both CI
    /// flavors; do not write a second one. The value grammar must be factored
    /// so this method and the `Deserialize` impl share one branch, per the
    /// [`crate::config::mirror::parse_mirror_value`] precedent — two copies
    /// of a union grammar drift.
    ///
    /// # Errors
    ///
    /// - [`ProjectErrorKind::EnvReservedKey`] — an `OCX_*` / `__OCX_*` key.
    /// - [`ProjectErrorKind::EnvInvalidKey`] — a key outside the POSIX
    ///   environment-name grammar.
    /// - [`ProjectErrorKind::EnvUnknownModifier`] — a table `type` that is
    ///   neither `path` nor `constant`.
    /// - [`ProjectErrorKind::EnvInvalidValue`] — a value that is neither a
    ///   string nor a well-formed `{ type, value }` table.
    pub fn from_table(scope: &str, raw: &toml::Table) -> Result<Self, ProjectErrorKind> {
        let mut entries = BTreeMap::new();
        for (key, value) in raw {
            // Reserved before grammar: an `OCX_*` key that also breaks the
            // POSIX grammar is reserved either way, and saying so first spares
            // the user a fix-then-fail-again round.
            if crate::env::is_reserved_ocx_key(key) {
                return Err(ProjectErrorKind::EnvReservedKey {
                    scope: scope.to_string(),
                    key: key.clone(),
                });
            }
            if !crate::env::is_valid_env_key(key) {
                return Err(ProjectErrorKind::EnvInvalidKey {
                    scope: scope.to_string(),
                    key: key.clone(),
                });
            }
            entries.insert(key.clone(), parse_env_value(scope, key, value)?);
        }
        Ok(Self { entries })
    }

    /// Materialize the declared entries as resolved [`Entry`] values, ready
    /// to append to the vector `resolve_env*` returns.
    ///
    /// Relative [`ModifierKind::Path`] values resolve against `project_root`
    /// (the directory holding `ocx.toml`), never the current directory —
    /// `ocx run` from a subdirectory must see the same `PATH` as from the
    /// root. Absolute values pass through untouched.
    ///
    /// Infallible: the key policy and the value grammar were already enforced
    /// by [`Self::from_table`], and joining a declared value onto the root is
    /// the whole of the remaining work.
    pub fn to_entries(&self, project_root: &Path) -> Vec<Entry> {
        self.entries
            .iter()
            .map(|(key, declared)| Entry {
                key: key.clone(),
                value: match declared.kind {
                    ModifierKind::Constant => declared.value.clone(),
                    // `join` is the whole resolution: an absolute value
                    // replaces the root outright, a relative one lands under
                    // it. On Windows a driveless-but-rooted value (`/opt/bin`,
                    // written by a POSIX author) is not `is_absolute()`, and
                    // joining is what anchors it to the project's drive
                    // instead of leaving it dependent on the process's current
                    // drive — which is the cwd-independence this method owes
                    // `ocx run` from a subdirectory.
                    ModifierKind::Path => project_root.join(&declared.value).to_string_lossy().into_owned(),
                },
                kind: declared.kind.clone(),
            })
            .collect()
    }
}

/// The one value-grammar branch: a bare string is a
/// [`ModifierKind::Constant`], a `{ type, value }` table names its modifier.
///
/// Shared by [`ProjectEnv::from_table`] and — through it — the
/// [`serde::Deserialize`] impl, per the [`crate::config::mirror::parse_mirror_value`]
/// precedent. Two copies of a union grammar drift.
///
/// # Errors
///
/// [`ProjectErrorKind::EnvUnknownModifier`] for a `type` that is neither
/// `path` nor `constant`; [`ProjectErrorKind::EnvInvalidValue`] for any other
/// shape, carrying the TOML type name of the offending value.
fn parse_env_value(scope: &str, key: &str, value: &toml::Value) -> Result<EnvValue, ProjectErrorKind> {
    if let Some(constant) = value.as_str() {
        return Ok(EnvValue::constant(constant));
    }

    let invalid_value = || ProjectErrorKind::EnvInvalidValue {
        scope: scope.to_string(),
        key: key.to_string(),
        found: value.type_str().to_string(),
    };

    let Some(table) = value.as_table() else {
        return Err(invalid_value());
    };
    let Some(declared_type) = table.get("type").and_then(toml::Value::as_str) else {
        return Err(invalid_value());
    };
    // Through `ModifierKind`'s own `FromStr` rather than a local match: the
    // same grammar is parsed by `ocx run --env KEY:TYPE=VALUE`, and two
    // hand-rolled copies of one union drift. The error only supplies `found` —
    // the scope/key context that makes the message actionable is added here.
    let kind = declared_type
        .parse::<ModifierKind>()
        .map_err(|error| ProjectErrorKind::EnvUnknownModifier {
            scope: scope.to_string(),
            key: key.to_string(),
            found: error.found,
        })?;
    let Some(literal) = table.get("value").and_then(toml::Value::as_str) else {
        return Err(invalid_value());
    };

    // Reject anything beyond `type` and `value`, matching the generated
    // schema's `additionalProperties: false` and `Group`'s
    // `deny_unknown_fields`. Accepting-and-ignoring would let a file authored
    // against a newer ocx run here with materially different semantics and no
    // signal — `required` on a path entry is a deferred ADR feature, so
    // `{ type = "path", value = "bin", required = true }` would silently drop
    // the caller's fail-if-absent intent. It would also leave the editor
    // (which validates against the schema) disagreeing with the CLI.
    if let Some(unknown) = table
        .keys()
        .find(|name| name.as_str() != "type" && name.as_str() != "value")
    {
        return Err(ProjectErrorKind::EnvUnknownValueField {
            scope: scope.to_string(),
            key: key.to_string(),
            field: unknown.clone(),
        });
    }

    Ok(EnvValue {
        kind,
        value: literal.to_string(),
    })
}

impl serde::Serialize for ProjectEnv {
    /// Emits the shorthand where it applies: a [`ModifierKind::Constant`]
    /// serializes as a bare string, a [`ModifierKind::Path`] as the
    /// `{ type, value }` table. One shape in, the same shape out.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;

        /// The table arm. `kind` serializes through [`ModifierKind`]'s own
        /// derive, so the `path`/`constant` spelling has one source.
        #[derive(serde::Serialize)]
        struct TableForm<'a> {
            #[serde(rename = "type")]
            kind: &'a ModifierKind,
            value: &'a str,
        }

        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, declared) in &self.entries {
            match declared.kind {
                ModifierKind::Constant => map.serialize_entry(key, &declared.value)?,
                ModifierKind::Path => map.serialize_entry(
                    key,
                    &TableForm {
                        kind: &declared.kind,
                        value: &declared.value,
                    },
                )?,
            }
        }
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for ProjectEnv {
    /// NOT the canonical parse path — [`ProjectEnv::from_table`] is, and it
    /// is what `ocx.toml` loading actually calls. This impl exists so
    /// `ProjectConfig` and `Group` can keep their `Deserialize` derives; it
    /// has no scope context, so it reports every fault against
    /// [`DEFAULT_ENV_SCOPE`] and flattens the typed error through
    /// `de::Error::custom`.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = <toml::Table as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_table(DEFAULT_ENV_SCOPE, &raw).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for ProjectEnv {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("ProjectEnv")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // Hand-written because schemars reads the field's Rust TYPE and so
        // cannot see the string-or-table union the hand-rolled Deserialize
        // accepts. A derive would publish the table arm only — and the
        // string arm is the common case (`CI = "1"`), so nearly every
        // correct `[env]` block would be red-underlined in any taplo-enabled
        // editor. `taplo.toml` binds this schema live to every `ocx.toml`.
        schemars::json_schema!({
            "type": "object",
            "description": "Environment variables applied to tools run from this project. Keys matching OCX_* or __OCX_* are rejected: a checked-in file must not be able to reconfigure how ocx itself resolves. Values are literal — no interpolation.",
            "additionalProperties": crate::utility::schema::string_or_table(
                "Shorthand for a constant: replaces any existing value of the variable.",
                serde_json::json!({
                    "type": "object",
                    "properties": {
                        // ponytail: enum spelled out rather than $ref'd at
                        // ModifierKind (which derives no JsonSchema today).
                        // Two variants, closed internal enum — a third is an
                        // ADR-level event. Specify phase pins the pairing.
                        "type": {
                            "type": "string",
                            "enum": ["path", "constant"],
                            "description": "`constant` replaces the variable's value; `path` prepends to it."
                        },
                        "value": {
                            "type": "string",
                            "description": "The literal value. For `path`, a relative value resolves against the project root; an absolute one passes through."
                        }
                    },
                    "required": ["type", "value"],
                    "additionalProperties": false
                })
            )
        })
    }
}

#[cfg(test)]
mod tests {
    //! Contract-first tests for [`ProjectEnv`], encoding the `ProjectEnv`
    //! component contract in `plan_project_env_declaration.md` and the S1-S9b
    //! decisions in `adr_project_env_declaration.md`. Tests call
    //! [`ProjectEnv::from_table`] directly — the canonical parse entry point
    //! — never the [`serde::Deserialize`] impl, which exists only so
    //! `ProjectConfig`/`Group` can keep their derives.
    use std::path::PathBuf;

    use super::*;
    use crate::project::error::ProjectErrorKind;

    // ── value grammar (S5) ──────────────────────────────────────────────

    #[test]
    fn bare_string_is_constant() {
        let table: toml::Table = toml::from_str(r#"CI = "1""#).expect("fixture TOML must parse");
        let env = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect("bare string must parse");
        assert_eq!(env.get("CI"), Some(&EnvValue::constant("1")));
    }

    #[test]
    fn explicit_constant_table_form() {
        let table: toml::Table =
            toml::from_str(r#"X = { type = "constant", value = "v" }"#).expect("fixture TOML must parse");
        let env = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect("explicit constant table must parse");
        assert_eq!(env.get("X"), Some(&EnvValue::constant("v")));
    }

    #[test]
    fn path_relative_resolves_against_project_root() {
        let table: toml::Table = toml::from_str(r#"PATH = { type = "path", value = "node_modules/.bin" }"#)
            .expect("fixture TOML must parse");
        let env = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect("path table must parse");

        // Assert the join, not a hardcoded literal (quality-rust.md
        // "Cross-Platform Path Handling"): a relative `type = "path"` value
        // must resolve against project_root regardless of platform
        // separator conventions.
        let project_root = std::env::temp_dir().join("ocx-project-env-test-root");
        let entries = env.to_entries(&project_root);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "PATH");
        assert_eq!(entries[0].kind, ModifierKind::Path);
        assert_eq!(
            PathBuf::from(&entries[0].value),
            project_root.join("node_modules/.bin"),
            "relative path value must resolve against project_root, not CWD"
        );
    }

    #[test]
    fn path_absolute_passes_through() {
        // Built from a genuinely platform-absolute path (`std::env::temp_dir()`)
        // rather than a POSIX literal like "/abs/bin": quality-rust.md notes
        // that literal is NOT absolute on Windows and would be drive-joined
        // onto project_root instead of passing through unchanged.
        let absolute = std::env::temp_dir().join("ocx-project-env-test-abs-bin");
        let absolute_str = absolute
            .to_str()
            .expect("temp_dir path must be valid UTF-8")
            .to_string();

        let mut value_table = toml::Table::new();
        value_table.insert("type".to_string(), toml::Value::String("path".to_string()));
        value_table.insert("value".to_string(), toml::Value::String(absolute_str));
        let mut table = toml::Table::new();
        table.insert("PATH".to_string(), toml::Value::Table(value_table));

        let env = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect("absolute path table must parse");
        let project_root = std::env::temp_dir().join("ocx-project-env-test-root-2");
        let entries = env.to_entries(&project_root);
        assert_eq!(
            PathBuf::from(&entries[0].value),
            absolute,
            "absolute path value must pass through unchanged, not be joined onto project_root"
        );
    }

    // ── reserved-key rejection (X1) ──────────────────────────────────────

    #[test]
    fn reserved_ocx_prefixed_key_rejected() {
        let table: toml::Table = toml::from_str(r#"OCX_FOO = "1""#).expect("fixture TOML must parse");
        let err = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect_err("OCX_* key must be rejected");
        let ProjectErrorKind::EnvReservedKey { scope, key } = &err else {
            panic!("expected EnvReservedKey, got {err:?}");
        };
        assert_eq!(scope, DEFAULT_ENV_SCOPE);
        assert_eq!(key, "OCX_FOO");
    }

    #[test]
    fn reserved_dunder_ocx_prefixed_key_rejected() {
        let table: toml::Table = toml::from_str(r#"__OCX_FOO = "1""#).expect("fixture TOML must parse");
        let err = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect_err("__OCX_* key must be rejected");
        let ProjectErrorKind::EnvReservedKey { scope, key } = &err else {
            panic!("expected EnvReservedKey, got {err:?}");
        };
        assert_eq!(scope, DEFAULT_ENV_SCOPE);
        assert_eq!(key, "__OCX_FOO");
    }

    #[test]
    fn reserved_key_rejected_in_group_scope() {
        // Same X1 policy applies inside `[group.<name>.env]` — `scope` is
        // the mechanism `from_table` uses to report it, so exercise that
        // argument directly rather than routing through
        // `ProjectConfig::from_toml_str`'s (still unimplemented) group body
        // parsing.
        let table: toml::Table = toml::from_str(r#"OCX_FOO = "1""#).expect("fixture TOML must parse");
        let err =
            ProjectEnv::from_table("group.ci.env", &table).expect_err("OCX_* key must be rejected in group scope too");
        let ProjectErrorKind::EnvReservedKey { scope, key } = &err else {
            panic!("expected EnvReservedKey, got {err:?}");
        };
        assert_eq!(scope, "group.ci.env");
        assert_eq!(key, "OCX_FOO");
    }

    // ── invalid-key rejection (X2) — parity with the shared validator ────

    #[test]
    fn invalid_key_rejected_through_shared_validator() {
        // Each case is chosen so a hand-rolled "second validator" would
        // plausibly diverge from `env::is_valid_env_key` (leading digit,
        // internal dash, embedded space, embedded dot): `from_table` must
        // reject exactly what the shared validator rejects — a quoted TOML
        // key sidesteps bare-key/dotted-key syntax so the fixture's literal
        // key content matches what reaches the validator.
        for key in ["1FOO", "FOO-BAR", "FOO BAR", "FOO.BAR"] {
            assert!(
                !crate::env::is_valid_env_key(key),
                "test bug: fixture key {key:?} must itself be invalid per the shared validator"
            );
            let source = format!("{key:?} = \"1\"");
            let table: toml::Table = toml::from_str(&source).expect("quoted-key fixture TOML must parse");
            let err = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table)
                .expect_err(&format!("invalid key {key:?} must be rejected"));
            let ProjectErrorKind::EnvInvalidKey {
                scope,
                key: rejected_key,
            } = &err
            else {
                panic!("expected EnvInvalidKey for {key:?}, got {err:?}");
            };
            assert_eq!(scope, DEFAULT_ENV_SCOPE);
            assert_eq!(rejected_key, key);
        }
    }

    #[test]
    fn valid_key_forms_accepted() {
        // Bounds the grammar from the other side: keys the shared validator
        // accepts must not be rejected by a stricter grammar of its own.
        for key in ["FOO", "_x", "A1", "_OCX_INTERNAL"] {
            assert!(
                crate::env::is_valid_env_key(key),
                "test bug: fixture key {key:?} must itself be valid per the shared validator"
            );
            let source = format!("{key:?} = \"1\"");
            let table: toml::Table = toml::from_str(&source).expect("quoted-key fixture TOML must parse");
            let env = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table)
                .unwrap_or_else(|e| panic!("valid key {key:?} must parse; got {e:?}"));
            assert_eq!(env.get(key), Some(&EnvValue::constant("1")));
        }
    }

    // ── malformed values ──────────────────────────────────────────────

    #[test]
    fn unknown_modifier_type_rejected() {
        let table: toml::Table =
            toml::from_str(r#"X = { type = "bogus", value = "v" }"#).expect("fixture TOML must parse");
        let err =
            ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect_err("unknown modifier type must be rejected");
        let ProjectErrorKind::EnvUnknownModifier { scope, key, found } = &err else {
            panic!("expected EnvUnknownModifier, got {err:?}");
        };
        assert_eq!(scope, DEFAULT_ENV_SCOPE);
        assert_eq!(key, "X");
        assert_eq!(found, "bogus");
    }

    /// A value table carrying anything beyond `type` and `value` is rejected,
    /// not accepted-and-ignored.
    ///
    /// `required` is the concrete motivating case: the ADR defers it, so a
    /// file authored against a future ocx that honours it must not run here
    /// with the fail-if-absent intent silently dropped. Rejecting also keeps
    /// the runtime in step with the generated schema, which declares
    /// `additionalProperties: false` — otherwise the editor accepts what the
    /// CLI rejects, or worse, the reverse.
    #[test]
    fn unknown_value_table_field_rejected() {
        let table: toml::Table = toml::from_str(r#"PATH = { type = "path", value = "bin", required = true }"#)
            .expect("fixture TOML must parse");
        let err = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table)
            .expect_err("an unknown field in a value table must be rejected");
        let ProjectErrorKind::EnvUnknownValueField { scope, key, field } = &err else {
            panic!("expected EnvUnknownValueField, got {err:?}");
        };
        assert_eq!(scope, DEFAULT_ENV_SCOPE);
        assert_eq!(key, "PATH");
        assert_eq!(field, "required");
    }

    #[test]
    fn integer_value_rejected() {
        let table: toml::Table = toml::from_str("X = 42").expect("fixture TOML must parse");
        let err = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect_err("integer value must be rejected");
        let ProjectErrorKind::EnvInvalidValue { scope, key, found } = &err else {
            panic!("expected EnvInvalidValue, got {err:?}");
        };
        assert_eq!(scope, DEFAULT_ENV_SCOPE);
        assert_eq!(key, "X");
        assert_eq!(found, "integer");
    }

    #[test]
    fn boolean_value_rejected() {
        let table: toml::Table = toml::from_str("X = true").expect("fixture TOML must parse");
        let err = ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect_err("boolean value must be rejected");
        let ProjectErrorKind::EnvInvalidValue { scope, key, found } = &err else {
            panic!("expected EnvInvalidValue, got {err:?}");
        };
        assert_eq!(scope, DEFAULT_ENV_SCOPE);
        assert_eq!(key, "X");
        assert_eq!(found, "boolean");
    }

    #[test]
    fn table_missing_value_field_rejected() {
        // Documented on `EnvValue::from_table`: a table value must be a
        // "well-formed { type, value } table" — `type` alone is not.
        let table: toml::Table = toml::from_str(r#"X = { type = "path" }"#).expect("fixture TOML must parse");
        let err =
            ProjectEnv::from_table(DEFAULT_ENV_SCOPE, &table).expect_err("table missing 'value' must be rejected");
        let ProjectErrorKind::EnvInvalidValue { scope, key, .. } = &err else {
            panic!("expected EnvInvalidValue, got {err:?}");
        };
        assert_eq!(scope, DEFAULT_ENV_SCOPE);
        assert_eq!(key, "X");
    }
}
