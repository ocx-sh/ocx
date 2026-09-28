// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use serde::{Deserialize, Serialize};

use super::slug::{SLUG_MAX_LEN, SLUG_PATTERN, SLUG_PATTERN_STR};
use super::visibility::Visibility;

/// An entrypoint name matching `^[a-z0-9][a-z0-9_-]*$`, at most [`EntrypointName::MAX_LEN`] bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct EntrypointName(String);

impl EntrypointName {
    /// Maximum byte length, keeping launcher filenames under Windows `MAX_PATH`.
    pub const MAX_LEN: usize = SLUG_MAX_LEN;

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for EntrypointName {
    type Error = EntrypointError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
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
/// The map key in `entrypoints` supplies the *invocable name* — the filename
/// of the generated launcher; this object holds the per-entry value. The
/// launcher re-enters via `ocx launcher exec '<package-root>' -- <name> [args...]`,
/// preserving clean-env execution semantics, and resolves the *dispatch
/// command* against the composed `PATH` from the package's `env` block:
/// `command` when set, otherwise the invocable name itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Entrypoint {
    /// Dispatch target resolved on the composed `PATH`, when it differs from
    /// the invocable name. Absent means the entrypoint name *is* the command
    /// (the common case): a package may expose `hello` while dispatching a
    /// differently named binary such as `hello-bin`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<EntrypointName>,

    /// Fixed leading arguments the generated launcher prepends before the user's
    /// own arguments. Each element may carry `${installPath}` — or its alias
    /// `${self.installPath}` — optionally suffixed `:native` or `:posix`; `${deps.*}`
    /// and `${self.env.*}` are NOT permitted here, and every other `${...}` is rejected
    /// (write `$${` for a literal `${`). Absent/empty serializes to nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
}

impl Entrypoint {
    /// The dispatch command, or `None` when it is the invocable name; see
    /// [`Entrypoints::dispatch_command`].
    pub fn command(&self) -> Option<&EntrypointName> {
        self.command.as_ref()
    }

    /// Fixed leading argv tokens the launcher prepends before the user's arguments.
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// Map of entrypoint names to entrypoint definitions for a package.
///
/// Deserialization rejects duplicate keys ([`EntrypointError::DuplicateName`])
/// that `serde_json` would silently resolve last-wins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entrypoints {
    entries: BTreeMap<EntrypointName, Entrypoint>,
}

impl Entrypoints {
    /// The visibility entry points carry as a surface carrier: consumers invoke
    /// them, while the package's own runtime calls `bin/` directly.
    pub const IMPLICIT_VISIBILITY: Visibility = Visibility::INTERFACE;

    pub fn new(entries: BTreeMap<EntrypointName, Entrypoint>) -> Self {
        Self { entries }
    }

    /// Test constructor from slug-valid name literals; panics on an invalid name.
    #[cfg(any(test, feature = "__testing"))]
    pub fn from_names<I, S>(names: I) -> Self
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

    pub fn iter(&self) -> impl Iterator<Item = (&EntrypointName, &Entrypoint)> + use<'_> {
        self.entries.iter()
    }

    pub fn names(&self) -> impl Iterator<Item = &EntrypointName> + use<'_> {
        self.entries.keys()
    }

    pub fn get(&self, name: &str) -> Option<&Entrypoint> {
        self.entries.get(name)
    }

    /// The dispatch command for `name`: its [`Entrypoint::command`] when set,
    /// otherwise `name` itself, also when `name` is not declared.
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
            "description": "Map of entrypoint names to entrypoint definitions. Each key is the user-invokable command name; the value object carries an optional `command` field naming the binary the generated launcher dispatches to when it differs from the invokable name (omit it and the name is dispatched directly). An optional `args` array supplies fixed leading arguments the generated launcher prepends before user args; each element may carry `${installPath}` (or its alias `${self.installPath}`), optionally suffixed `:native` or `:posix`, while `${deps.*}` and `${self.env.*}` are not permitted in args and every other `${...}` is rejected — write `$${` for a literal `${`.",
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
    /// An entrypoint name fails the slug regex or exceeds [`EntrypointName::MAX_LEN`].
    // The literal `64` must track `EntrypointName::MAX_LEN`; `#[error]` cannot interpolate a const.
    #[error("invalid entrypoint name '{name}': must match ^[a-z0-9][a-z0-9_-]*$ (max 64 chars)")]
    InvalidName { name: String },
    /// A JSON object contains the same entrypoint name twice.
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
        let bundle: crate::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
        assert!(bundle.entrypoints.is_empty());
        let serialized = serde_json::to_string(&bundle).unwrap();
        assert!(!serialized.contains("entrypoints"));
    }

    #[test]
    fn bundle_with_empty_entrypoints_skip_serialized() {
        let json = r#"{"version":1,"entrypoints":{}}"#;
        let bundle: crate::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
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
        let bundle: crate::metadata::bundle::Bundle = serde_json::from_str(json).unwrap();
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
