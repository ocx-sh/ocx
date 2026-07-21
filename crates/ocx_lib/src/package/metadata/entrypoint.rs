// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use serde::{Deserialize, Serialize};

use super::slug::{SLUG_MAX_LEN, SLUG_PATTERN, SLUG_PATTERN_STR};
use super::visibility::Visibility;

/// A validated entrypoint name.
///
/// Must match `^[a-z0-9][a-z0-9_-]*$` and be at most
/// [`EntrypointName::MAX_LEN`] bytes. Enforced at construction and
/// deserialization.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EntrypointName(String);

impl EntrypointName {
    /// Maximum byte length of an entrypoint name.
    ///
    /// Caps publisher-supplied names so generated launcher filenames stay
    /// well under platform path limits (Windows `MAX_PATH = 260`, including
    /// the `.exe`/`.shim` suffix and the surrounding install directory). 64
    /// chars is generous for human-readable command names while leaving headroom.
    ///
    /// Mirrors [`slug::SLUG_MAX_LEN`] — both newtypes share the same upper bound.
    pub const MAX_LEN: usize = SLUG_MAX_LEN;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EntrypointName {
    type Error = EntrypointError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        // Length checked first to avoid running the regex on pathologically long input.
        if value.len() > Self::MAX_LEN {
            return Err(EntrypointError::InvalidName { name: value });
        }
        if !SLUG_PATTERN.is_match(&value) {
            return Err(EntrypointError::InvalidName { name: value });
        }
        Ok(EntrypointName(value))
    }
}

impl TryFrom<&str> for EntrypointName {
    type Error = EntrypointError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        EntrypointName::try_from(value.to_string())
    }
}

impl std::str::FromStr for EntrypointName {
    type Err = EntrypointError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EntrypointName::try_from(s.to_string())
    }
}

impl std::fmt::Display for EntrypointName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::borrow::Borrow<str> for EntrypointName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for EntrypointName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        EntrypointName::try_from(s).map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for EntrypointName {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("EntrypointName")
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "Entrypoint name for invocation by users. Must match ^[a-z0-9][a-z0-9_-]*$ and be at most 64 characters.",
            "pattern": SLUG_PATTERN_STR,
            "maxLength": SLUG_MAX_LEN
        })
    }
}

/// A single named entrypoint for a package.
///
/// The map key in [`Entrypoints`] supplies the *invocable name* — the
/// filename of the generated launcher. This struct holds the per-entry
/// value.
///
/// The launcher generated for each entry re-enters via
/// `ocx launcher exec '<package-root>' -- <name> [args...]`, preserving
/// clean-env execution semantics. `ocx launcher exec` resolves the
/// *dispatch command* against the composed `PATH` from the package's `env`
/// block: [`Entrypoint::command`] when set, otherwise the invocable name
/// itself (the common case where they coincide).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Entrypoint {
    /// Dispatch target resolved on the composed `PATH`, when it differs from
    /// the invocable name. Absent means the entrypoint name *is* the command
    /// (the common case): a package may expose `hello` while dispatching a
    /// differently named binary such as `hello-bin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<EntrypointName>,

    /// Fixed leading arguments the generated launcher prepends before the user's
    /// own arguments. Each element supports `${installPath}` interpolation (the
    /// package content directory); `${deps.*}` is NOT permitted here. Absent/empty
    /// serializes to nothing (wire-compatible with the pre-`args` shape).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
}

impl Entrypoint {
    /// The dispatch command, or `None` when it coincides with the invocable
    /// name. Callers wanting the effective command should fall back to the
    /// entrypoint's map key — see [`Entrypoints::dispatch_command`].
    pub fn command(&self) -> Option<&EntrypointName> {
        self.command.as_ref()
    }

    /// Fixed leading arguments prepended before user-supplied arguments when
    /// the generated launcher dispatches this entrypoint. Each element is one
    /// argv token; `${installPath}` is interpolated to the package's content
    /// directory at runtime. Returns an empty slice when no baked args are
    /// declared.
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// Map of entrypoint names to entrypoint definitions for a package.
///
/// Serializes as a JSON object keyed by entrypoint name (e.g.
/// `{"cmake": {}, "ctest": {}}`). The map shape mirrors the Cargo
/// `[dependencies.X]`, Compose `services:`, and GitHub Actions `jobs:`
/// idioms — uniqueness within a package is given by JSON object key
/// semantics.
///
/// `#[serde(default)]` on the containing field means an absent
/// `entrypoints` field deserializes to an empty map;
/// `skip_serializing_if = "Entrypoints::is_empty"` means an empty map is
/// omitted on serialization (additive-optional, forward-compat).
///
/// Deserialization uses a custom `MapAccess` visitor that rejects duplicate
/// keys with [`EntrypointError::DuplicateName`]. The `serde_json` default of
/// silently last-wins on duplicate keys is unsafe for a registry where
/// duplicate names indicate publisher error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entrypoints {
    entries: BTreeMap<EntrypointName, Entrypoint>,
}

impl Entrypoints {
    /// The visibility entry points carry as a surface carrier.
    ///
    /// Entry points have no publisher-declared visibility field; they are
    /// launchers a *consumer* invokes the package through, while the
    /// package's own runtime bypasses them and calls `bin/` directly. That
    /// is exactly [`Visibility::INTERFACE`]: consumer axis yes, self axis
    /// no. The composer's surface algebra
    /// (`package_manager::composer::carrier_crosses`) derives everything
    /// else from this one constant — a root's launchers appear on its
    /// interface surface only, and a dependency's launchers cross the edge
    /// like any interface-side carrier (they are how the parent invokes it).
    pub const IMPLICIT_VISIBILITY: Visibility = Visibility::INTERFACE;

    /// Constructs an `Entrypoints` from a name-keyed map.
    ///
    /// Uniqueness is given by `BTreeMap` key semantics; this constructor is
    /// infallible. The custom `Deserialize` impl is the only path that can
    /// observe duplicate keys (raw JSON), and it surfaces them as
    /// [`EntrypointError::DuplicateName`].
    pub fn new(entries: BTreeMap<EntrypointName, Entrypoint>) -> Self {
        Self { entries }
    }

    /// Convenience constructor for tests that pass known-valid name literals.
    ///
    /// Each name is validated by [`EntrypointName::try_from`]. Panics on an
    /// invalid name — callers must pass slug-valid literals. This is acceptable
    /// here because all callers are test helpers constructing compile-time
    /// constants; invalid input is a programming error, not a runtime condition.
    #[cfg(test)]
    pub(crate) fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let entries = names
            .into_iter()
            .map(|s| {
                let name =
                    EntrypointName::try_from(s.as_ref()).expect("from_names: caller must pass a valid slug name");
                (name, Entrypoint::default())
            })
            .collect();
        Self { entries }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Iterates `(name, entry)` pairs in name-sorted order.
    pub fn iter(&self) -> impl Iterator<Item = (&EntrypointName, &Entrypoint)> + use<'_> {
        self.entries.iter()
    }

    /// Iterates declared entrypoint names in name-sorted order.
    pub fn names(&self) -> impl Iterator<Item = &EntrypointName> + use<'_> {
        self.entries.keys()
    }

    /// Returns the [`Entrypoint`] registered under `name`, or `None` if no
    /// entry with that name exists.
    pub fn get(&self, name: &str) -> Option<&Entrypoint> {
        self.entries.get(name)
    }

    /// Resolves the dispatch command for an invocable entrypoint `name`.
    ///
    /// Returns the entry's [`Entrypoint::command`] when set, otherwise `name`
    /// itself. `name` is returned verbatim when it is not a declared
    /// entrypoint, so callers that already validated the name (e.g.
    /// `ocx launcher exec`, where the launcher filename is the name) keep
    /// today's "resolve the name on PATH" behaviour with no special-casing.
    pub fn dispatch_command<'a>(&'a self, name: &'a str) -> &'a str {
        self.get(name)
            .and_then(Entrypoint::command)
            .map_or(name, EntrypointName::as_str)
    }
}

impl Serialize for Entrypoints {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.entries.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Entrypoints {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct MapVisitor;

        impl<'de> serde::de::Visitor<'de> for MapVisitor {
            type Value = Entrypoints;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a map of entrypoint name to entrypoint definition")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Entrypoints, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut entries: BTreeMap<EntrypointName, Entrypoint> = BTreeMap::new();
                while let Some(key) = map.next_key::<EntrypointName>()? {
                    let value: Entrypoint = map.next_value()?;
                    // serde_json's default behaviour silently last-wins on
                    // duplicate keys. Reject them so publishers see the
                    // mistake rather than a silently dropped entry.
                    match entries.entry(key) {
                        Entry::Occupied(occ) => {
                            return Err(serde::de::Error::custom(EntrypointError::DuplicateName {
                                name: occ.key().0.clone(),
                            }));
                        }
                        Entry::Vacant(vac) => {
                            vac.insert(value);
                        }
                    }
                }
                Ok(Entrypoints { entries })
            }
        }

        deserializer.deserialize_map(MapVisitor)
    }
}

impl schemars::JsonSchema for Entrypoints {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("Entrypoints")
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let value_schema = generator.subschema_for::<Entrypoint>();
        schemars::json_schema!({
            "type": "object",
            "description": "Map of entrypoint names to entrypoint definitions. Each key is the user-invokable command name; the value object carries an optional `command` field naming the binary the generated launcher dispatches to when it differs from the invokable name (omit it and the name is dispatched directly). An optional `args` array supplies fixed leading arguments the generated launcher prepends before user args; each element supports `${installPath}` interpolation (`${deps.*}` is not permitted in args).",
            "additionalProperties": value_schema,
            "propertyNames": {
                "pattern": SLUG_PATTERN_STR,
                "maxLength": SLUG_MAX_LEN
            }
        })
    }
}

/// Errors that can occur when validating entrypoint metadata.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EntrypointError {
    /// An entrypoint name fails the slug regex `^[a-z0-9][a-z0-9_-]*$`
    /// or exceeds [`EntrypointName::MAX_LEN`].
    // The literal `64` must stay in sync with `EntrypointName::MAX_LEN` (= slug::SLUG_MAX_LEN);
    // serde/thiserror `#[error]` attributes cannot interpolate a const at compile time.
    #[error("invalid entrypoint name '{name}': must match ^[a-z0-9][a-z0-9_-]*$ (max 64 chars)")]
    InvalidName { name: String },
    /// A JSON object contains the same entrypoint name twice. Surfaced by
    /// the custom [`Entrypoints`] deserializer.
    #[error("duplicate entrypoint name '{name}'")]
    DuplicateName { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep_name(s: &str) -> EntrypointName {
        EntrypointName::try_from(s).unwrap()
    }

    fn map_of(names: &[&str]) -> BTreeMap<EntrypointName, Entrypoint> {
        names.iter().map(|n| (ep_name(n), Entrypoint::default())).collect()
    }

    // ── EntrypointName slug validation ────────────────────────────────────

    #[test]
    fn name_accepts_simple_lowercase() {
        assert!(EntrypointName::try_from("cmake").is_ok());
    }

    #[test]
    fn name_accepts_alphanumeric_with_dash_underscore() {
        assert!(EntrypointName::try_from("a1_2-3").is_ok());
        assert!(EntrypointName::try_from("ctest-2").is_ok());
        assert!(EntrypointName::try_from("gcc12").is_ok());
    }

    #[test]
    fn name_rejects_empty() {
        let err = EntrypointName::try_from("").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_uppercase() {
        let err = EntrypointName::try_from("Cmake").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
        let err = EntrypointName::try_from("CMAKE").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_accepts_leading_digit() {
        // Slug pattern ^[a-z0-9][a-z0-9_-]*$ allows a leading digit.
        assert!(EntrypointName::try_from("1abc").is_ok());
    }

    #[test]
    fn name_rejects_leading_underscore() {
        let err = EntrypointName::try_from("_cmake").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_leading_dash() {
        let err = EntrypointName::try_from("-cmake").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_path_traversal() {
        let err = EntrypointName::try_from("../../bin/sh").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_slash() {
        let err = EntrypointName::try_from("bin/cmake").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_rejects_unicode() {
        let err = EntrypointName::try_from("cmaké").unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    #[test]
    fn name_accepts_64_char_slug() {
        let at_cap: String = "a".repeat(EntrypointName::MAX_LEN);
        assert!(EntrypointName::try_from(at_cap.as_str()).is_ok());
    }

    #[test]
    fn name_rejects_65_char_slug() {
        let over_cap: String = "a".repeat(EntrypointName::MAX_LEN + 1);
        let err = EntrypointName::try_from(over_cap.as_str()).unwrap_err();
        assert!(matches!(err, EntrypointError::InvalidName { .. }));
    }

    // ── Entrypoints constructor ───────────────────────────────────────────

    #[test]
    fn entrypoints_new_accepts_unique_names() {
        let eps = Entrypoints::new(map_of(&["cmake", "ctest"]));
        assert_eq!(eps.len(), 2);
        assert!(!eps.is_empty());
    }

    #[test]
    fn entrypoints_new_accepts_empty() {
        let eps = Entrypoints::new(BTreeMap::new());
        assert!(eps.is_empty());
    }

    #[test]
    fn entrypoints_iter_in_sorted_order() {
        let eps = Entrypoints::new(map_of(&["ctest", "cmake", "cpack"]));
        let names: Vec<&str> = eps.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["cmake", "cpack", "ctest"]);
    }

    // ── Entrypoint serde ──────────────────────────────────────────────────

    #[test]
    fn entrypoint_value_is_empty_object() {
        let ep = Entrypoint::default();
        let json = serde_json::to_string(&ep).unwrap();
        assert_eq!(json, "{}");
        let back: Entrypoint = serde_json::from_str(&json).unwrap();
        assert_eq!(ep, back);
    }

    // ── Entrypoints serde — map shape ─────────────────────────────────────

    #[test]
    fn entrypoints_round_trip_via_serde() {
        let json = r#"{"cmake":{},"ctest":{}}"#;
        let eps: Entrypoints = serde_json::from_str(json).unwrap();
        assert_eq!(eps.len(), 2);
        let back = serde_json::to_string(&eps).unwrap();
        assert_eq!(back, json);
    }

    #[test]
    fn entrypoints_empty_map_round_trips() {
        let json = "{}";
        let eps: Entrypoints = serde_json::from_str(json).unwrap();
        assert!(eps.is_empty());
        let back = serde_json::to_string(&eps).unwrap();
        assert_eq!(back, "{}");
    }

    #[test]
    fn entrypoints_rejects_invalid_key() {
        let json = r#"{"":{}}"#;
        let err = serde_json::from_str::<Entrypoints>(json).unwrap_err();
        assert!(err.to_string().contains("name"), "expected name error: {err}");
    }

    #[test]
    fn entrypoints_rejects_uppercase_key() {
        let json = r#"{"Cmake":{}}"#;
        let err = serde_json::from_str::<Entrypoints>(json).unwrap_err();
        assert!(err.to_string().contains("name"), "expected name error: {err}");
    }

    #[test]
    fn entrypoints_rejects_array_shape() {
        let json = r#"[{"name":"cmake"}]"#;
        let err = serde_json::from_str::<Entrypoints>(json).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("a map of entrypoint name"),
            "expected map-shape error citing visitor expecting text: {msg}"
        );
    }

    /// Pin the contract: serde_json's default last-wins behaviour for
    /// duplicate object keys is a publisher footgun. The custom MapVisitor
    /// must reject duplicates with the typed `EntrypointError::DuplicateName`
    /// diagnostic so the offending name surfaces verbatim.
    #[test]
    fn entrypoints_rejects_duplicate_keys() {
        let json = r#"{"cmake":{},"cmake":{}}"#;
        let err = serde_json::from_str::<Entrypoints>(json)
            .expect_err("duplicate entrypoint keys must be rejected during deserialization");
        let msg = err.to_string();
        // Match the typed DuplicateName Display: `duplicate entrypoint name 'cmake'`
        assert!(msg.contains("duplicate"), "error must cite duplication: {msg}");
        assert!(
            msg.contains("entrypoint name"),
            "error must say 'entrypoint name': {msg}"
        );
        assert!(
            msg.contains("'cmake'"),
            "error must cite the offending key 'cmake': {msg}"
        );
    }

    /// W6: pin that EntrypointName deserialization errors carry enough
    /// diagnostic content for users to fix the metadata. The slug regex
    /// pattern hint must appear so publishers see *what* shape is expected.
    #[test]
    fn entrypoint_name_deserialize_error_message_contains_pattern_hint() {
        let json = r#""Foo Bar""#;
        let err =
            serde_json::from_str::<EntrypointName>(json).expect_err("invalid entrypoint name must fail to deserialize");
        let msg = err.to_string();
        assert!(msg.contains("Foo Bar"), "error must echo 'Foo Bar': {msg}");
        assert!(
            msg.contains("[a-z0-9]") || msg.contains("must match"),
            "error must hint at the slug pattern: {msg}"
        );
    }

    // ── Bundle round-trips with new map shape ─────────────────────────────

    #[test]
    fn bundle_without_entrypoints_round_trips() {
        let json = r#"{"version":1}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(bundle.entrypoints.is_empty());
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(!serialized.contains("entrypoints"));
    }

    #[test]
    fn bundle_with_empty_entrypoints_skip_serialized() {
        let json = r#"{"version":1,"entrypoints":{}}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(bundle.entrypoints.is_empty());
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(
            !serialized.contains("entrypoints"),
            "empty entrypoints should be skipped: {serialized}"
        );
    }

    #[test]
    fn bundle_with_populated_entrypoints_round_trips() {
        let json = r#"{"version":1,"entrypoints":{"cmake":{}}}"#;
        let bundle: crate::package::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(!bundle.entrypoints.is_empty());
        assert_eq!(bundle.entrypoints.names().next().unwrap().as_str(), "cmake");
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(serialized.contains("entrypoints"));
        assert!(serialized.contains("cmake"));
    }

    #[test]
    fn entry_without_command_deserializes_and_omits_on_serialize() {
        let entry: Entrypoint = serde_json::from_str("{}").unwrap();
        assert!(entry.command().is_none());
        // skip_serializing_if keeps the wire format byte-identical with the
        // pre-`command` shape for the common case.
        assert_eq!(serde_json::to_string(&entry).unwrap(), "{}");
    }

    #[test]
    fn entry_with_command_round_trips() {
        let entry: Entrypoint = serde_json::from_str(r#"{"command":"hello-bin"}"#).unwrap();
        assert_eq!(entry.command().unwrap().as_str(), "hello-bin");
        assert_eq!(serde_json::to_string(&entry).unwrap(), r#"{"command":"hello-bin"}"#);
    }

    #[test]
    fn entry_command_rejects_invalid_slug() {
        // `command` reuses EntrypointName validation — a path traversal
        // attempt is rejected at deserialization, not silently dispatched.
        let err = serde_json::from_str::<Entrypoint>(r#"{"command":"../evil"}"#).unwrap_err();
        assert!(err.to_string().contains("invalid"), "{err}");
    }

    #[test]
    fn dispatch_command_falls_back_to_name_without_command() {
        let eps = Entrypoints::from_names(["hello"]);
        assert_eq!(eps.dispatch_command("hello"), "hello");
    }

    #[test]
    fn dispatch_command_returns_declared_command() {
        let json = r#"{"hello":{"command":"hello-bin"},"plain":{}}"#;
        let eps: Entrypoints = serde_json::from_str(json).unwrap();
        assert_eq!(eps.dispatch_command("hello"), "hello-bin");
        assert_eq!(eps.dispatch_command("plain"), "plain");
    }

    #[test]
    fn dispatch_command_returns_name_verbatim_when_undeclared() {
        // `ocx launcher exec` relies on this: an unknown name (should not
        // happen — the launcher filename is always declared) degrades to
        // today's resolve-name-on-PATH behaviour, never panics.
        let eps = Entrypoints::from_names(["hello"]);
        assert_eq!(eps.dispatch_command("ghost"), "ghost");
    }

    // ── Contract 1: Entrypoint deserialization with args and command ──────────

    /// Contract 1: `{"command":"python","args":["run","x"]}` deserializes to
    /// an Entrypoint with command == Some("python") and args == ["run", "x"].
    #[test]
    fn entrypoint_deser_with_args_and_command() {
        let entry: Entrypoint = serde_json::from_str(r#"{"command":"python","args":["run","x"]}"#).unwrap();
        assert_eq!(
            entry.command().unwrap().as_str(),
            "python",
            "command must be Some(\"python\")"
        );
        assert_eq!(entry.args(), &["run", "x"], "args must be [\"run\", \"x\"]");
    }

    // ── Contract 2: Round-trip byte-identity ─────────────────────────────────

    /// Contract 2 (first case): `{"command":"python","args":["run","x"]}` serializes
    /// back to byte-identical JSON after deserialization.
    #[test]
    fn entrypoint_args_round_trip_byte_identical() {
        let json = r#"{"command":"python","args":["run","x"]}"#;
        let entry: Entrypoint = serde_json::from_str(json).unwrap();
        let back = serde_json::to_string(&entry).unwrap();
        assert_eq!(back, json, "round-trip must produce byte-identical JSON");
    }

    /// Contract 2 (second case): `{"args":["--flag"]}` (no command) deserializes
    /// to command==None, args==["--flag"], and serializes back byte-identically.
    #[test]
    fn entrypoint_args_without_command_round_trip() {
        let json = r#"{"args":["--flag"]}"#;
        let entry: Entrypoint = serde_json::from_str(json).unwrap();
        assert!(entry.command().is_none(), "command must be None when absent from JSON");
        assert_eq!(entry.args(), &["--flag"]);
        let back = serde_json::to_string(&entry).unwrap();
        assert_eq!(back, json, "round-trip without command must be byte-identical");
    }

    // ── Contract 3: args() accessor ───────────────────────────────────────────

    /// Contract 3a: `args()` returns the populated slice for a deserialized entry.
    #[test]
    fn args_accessor_returns_slice_for_populated_entry() {
        let entry: Entrypoint = serde_json::from_str(r#"{"args":["a","b","c"]}"#).unwrap();
        assert_eq!(entry.args(), &["a", "b", "c"]);
    }

    /// Contract 3b: `args()` returns an empty slice for `Entrypoint::default()`.
    #[test]
    fn args_accessor_returns_empty_slice_for_default() {
        let entry = Entrypoint::default();
        let empty: &[String] = &[];
        assert_eq!(entry.args(), empty, "default Entrypoint must have empty args slice");
    }

    // ── Contract 4: args without command inside Entrypoints map ──────────────

    /// Contract 4: `{"tool":{"args":["--flag"]}}` parsed as an Entrypoints map.
    /// - `dispatch_command("tool")` == "tool" (no command field → name used)
    /// - `get("tool").unwrap().args()` == ["--flag"]
    /// - `get("absent")` is None
    #[test]
    fn entrypoints_map_with_args_no_command() {
        let json = r#"{"tool":{"args":["--flag"]}}"#;
        let eps: Entrypoints = serde_json::from_str(json).unwrap();

        // No command field → dispatch_command returns the entrypoint name itself.
        assert_eq!(
            eps.dispatch_command("tool"),
            "tool",
            "absent command must cause dispatch_command to return the name"
        );

        // get() returns the entry and args() surfaces the baked args.
        let entry = eps.get("tool").expect("entry 'tool' must exist");
        assert_eq!(entry.args(), &["--flag"]);

        // get() returns None for an undeclared name.
        assert!(
            eps.get("absent").is_none(),
            "get() must return None for an undeclared name"
        );
    }
}
