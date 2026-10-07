// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The published `--format json` report contract over every `Printable` root in [`ocx::api::data`].

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

/// Canonical published URL of the report contract.
pub const REPORTS_ID: &str = schema_id!("reports", reports_version!());

/// Version the six formerly wrapped roots publish at; the contract's own version is the same number.
pub const REPORTS_VERSION: u32 = reports_version!();

/// Marks a field serde omits instead of writing `null`; read by [`normalize`] for the execution record only.
const ABSENT_WHEN_NONE: &str = "x-ocx-absent-when-none";

/// Marks a publisher-supplied leaf; nothing below it is rewritten.
const OPAQUE: &str = "x-ocx-opaque";

/// Annotations a published document drops: none of them constrains a value.
const ANNOTATIONS: &[&str] = &["default", "examples", "$comment"];

/// Collects each registered root's name, payload `$def` and version while the generator records the payloads.
struct Roots<'a> {
    generator: &'a mut schemars::SchemaGenerator,
    roots: Vec<(&'static str, String, u32)>,
}

impl ocx::api::RootVisitor for Roots<'_> {
    fn root<T: ocx::api::Printable + schemars::JsonSchema>(&mut self) {
        let name = T::ROOT;
        let schema = serde_json::to_value(self.generator.subschema_for::<T>())
            .expect("a schemars Schema is always serializable");
        let payload = schema
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|target| target.strip_prefix("#/$defs/"))
            .unwrap_or_else(|| panic!("report root `{name}` is not a `$def`: {schema}"))
            .to_owned();
        self.roots.push((name, payload, T::SCHEMA_VERSION));
    }
}

/// The settings of a document ocx writes: `required` lists exactly the fields serde always emits.
pub fn settings() -> SchemaSettings {
    let mut settings = SchemaSettings::draft2020_12().for_serialize();
    settings.meta_schema = Some("https://json-schema.org/draft/2020-12/schema".into());
    settings
}

/// A published root: the payload's object schema with a pinned `schema_version` leading its properties.
///
/// # Panics
///
/// When `payload` is not an object schema with `properties`: `#[serde(flatten)]` could not emit it.
pub fn wrapper(name: &str, payload: &Value, version: u32) -> Value {
    let properties = payload
        .get("properties")
        .and_then(Value::as_object)
        .filter(|_| payload.get("type") == Some(&json!("object")))
        .unwrap_or_else(|| {
            panic!("report root `{name}` must be an object schema with `properties`; a union payload goes under a named property: {payload}")
        });
    let mut leading = Map::new();
    leading.insert(
        "schema_version".to_owned(),
        json!({
            "description": "The version of this root's shape; a breaking change increments it.",
            "type": "integer",
            "const": version
        }),
    );
    leading.extend(properties.clone());
    let mut required = vec![Value::from("schema_version")];
    required.extend(
        payload
            .get("required")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    );

    let mut wrapper = payload.clone();
    wrapper["properties"] = Value::Object(leading);
    wrapper["required"] = Value::Array(required);
    wrapper
}

/// Generate the report contract as pretty-printed JSON.
pub fn reports_schema() -> String {
    let mut generator = settings().into_generator();
    let mut visitor = Roots {
        generator: &mut generator,
        roots: Vec::new(),
    };
    ocx::api::visit_report_roots(&mut visitor);
    let registered = visitor.roots;

    let mut defs = generator.take_definitions(true);
    for def in defs.values_mut() {
        publish(def);
    }
    let mut roots = Map::new();
    for (name, payload, version) in registered {
        let root = format!("{payload}Root");
        let schema = wrapper(name, &defs[&payload], version);
        assert!(
            defs.insert(root.clone(), schema).is_none(),
            "two report roots share the payload `{payload}`"
        );
        roots.insert(name.to_owned(), json!({ "$ref": format!("#/$defs/{root}") }));
    }

    let mut document = Map::new();
    document.insert(
        "$schema".to_owned(),
        Value::String("https://json-schema.org/draft/2020-12/schema".to_owned()),
    );
    document.insert("$id".to_owned(), Value::String(REPORTS_ID.to_owned()));
    document.insert("reports".to_owned(), Value::Object(roots));
    document.insert("$defs".to_owned(), Value::Object(defs));

    serde_json::to_string_pretty(&Value::Object(document))
        .expect("a serde_json::Value is always serializable to a JSON string")
}

/// The name of every published report root, in registry order.
pub fn root_names() -> Vec<String> {
    let document: Value = serde_json::from_str(&reports_schema()).expect("the report contract is JSON");
    document
        .get("reports")
        .and_then(Value::as_object)
        .map(|roots| roots.keys().cloned().collect())
        .unwrap_or_default()
}

/// Rewrite one generated schema into its published form, in place.
///
/// Drops the annotation keywords; strips `null` from every property serde can omit (absent from `required`), so a
/// `null` left standing is one serde writes; lists a documented `const` set as an open `x-ocx-enum`; and gives a
/// union tagged by `type` its unknown arm. A node marked `x-ocx-opaque` and everything below it pass through
/// untouched.
pub fn publish(schema: &mut Value) {
    let Value::Object(map) = schema else { return };
    if map.get(OPAQUE) == Some(&Value::Bool(true)) {
        return;
    }
    for annotation in ANNOTATIONS {
        map.remove(*annotation);
    }
    for (keyword, child) in map.iter_mut() {
        match keyword.as_str() {
            "properties" | "$defs" => child
                .as_object_mut()
                .into_iter()
                .flat_map(Map::values_mut)
                .for_each(publish),
            "oneOf" | "anyOf" | "allOf" => child.as_array_mut().into_iter().flatten().for_each(publish),
            "items" | "additionalProperties" | "not" | "propertyNames" => publish(child),
            _ => {}
        }
    }
    let required: Vec<String> = map
        .get("required")
        .and_then(Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if let Some(Value::Object(properties)) = map.get_mut("properties") {
        for (name, property) in properties.iter_mut() {
            if !required.contains(name) {
                strip_null(property);
            }
        }
    }
    open_enum(map);
    open_union(map);
}

/// A `oneOf` of documented string or integer `const`s becomes `{"type": …, "x-ocx-enum": [{value, description}]}`.
fn open_enum(map: &mut Map<String, Value>) {
    let Some(Value::Array(arms)) = map.get("oneOf") else {
        return;
    };
    let scalar = |value: &Value| match value {
        Value::String(_) => Some("string"),
        Value::Number(number) if number.is_i64() || number.is_u64() => Some("integer"),
        _ => None,
    };
    let Some(kind) = arms.first().and_then(|arm| arm.get("const")).and_then(scalar) else {
        return;
    };
    let entries: Option<Vec<Value>> = arms
        .iter()
        .map(|arm| {
            let arm = arm.as_object()?;
            let value = arm.get("const").filter(|value| scalar(value) == Some(kind))?;
            let description = arm.get("description").and_then(Value::as_str)?;
            let plain = arm.get("type") == Some(&Value::from(kind)) && arm.len() == 3;
            plain.then(|| json!({"value": value, "description": description}))
        })
        .collect();
    let Some(entries) = entries.filter(|entries| !entries.is_empty()) else {
        return;
    };
    map.remove("oneOf");
    map.insert("type".to_owned(), Value::from(kind));
    map.insert("x-ocx-enum".to_owned(), Value::Array(entries));
}

/// A `oneOf` whose every arm is tagged by a required `type` `const` gains the unknown arm.
///
/// A newtype variant's arm is a `$ref` with the tag beside it; under 2020-12 the siblings apply too.
fn open_union(map: &mut Map<String, Value>) {
    let Some(Value::Array(arms)) = map.get_mut("oneOf") else {
        return;
    };
    let tags: Option<Vec<Value>> = arms
        .iter()
        .map(|arm| {
            let tag = arm.pointer("/properties/type/const").filter(|tag| tag.is_string())?;
            let tagged = arm
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|names| names.iter().any(|name| name == "type"));
            tagged.then(|| tag.clone())
        })
        .collect();
    let Some(tags) = tags.filter(|tags| !tags.is_empty()) else {
        return;
    };
    arms.push(json!({
        "x-ocx-unknown-variant": true,
        "type": "object",
        "required": ["type"],
        "properties": {"type": {"type": "string", "not": {"enum": tags}}}
    }));
}

/// Rewrite `required` and nullability to match what serde emits: a plain `Option<T>` (written as `null`)
/// becomes required, an [`ABSENT_WHEN_NONE`] field becomes optional and non-null.
///
/// For the execution record, generated under the deserialize contract; the report contract uses [`publish`].
pub fn normalize(node: &mut Value) {
    match node {
        Value::Array(items) => {
            for item in items {
                normalize(item);
            }
        }
        Value::Object(map) => {
            if map.contains_key("properties") {
                correct_required(map);
            }
            for value in map.values_mut() {
                normalize(value);
            }
        }
        _ => {}
    }
}

/// Apply the two corrections to one object schema's `required` list.
fn correct_required(map: &mut Map<String, Value>) {
    let mut required: Vec<String> = map
        .get("required")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();

    let Some(Value::Object(properties)) = map.get_mut("properties") else {
        return;
    };
    let order: Vec<String> = properties.keys().cloned().collect();

    for (key, property) in properties.iter_mut() {
        let absent_when_none = property
            .get(ABSENT_WHEN_NONE)
            .and_then(Value::as_bool)
            .unwrap_or_default();
        if absent_when_none {
            if let Value::Object(object) = property {
                object.remove(ABSENT_WHEN_NONE);
            }
            strip_null(property);
            required.retain(|name| name != key);
        } else if is_nullable(property) && !required.iter().any(|name| name == key) {
            required.push(key.clone());
        }
    }

    required.sort_by_key(|name| order.iter().position(|field| field == name).unwrap_or(usize::MAX));
    if required.is_empty() {
        map.remove("required");
    } else {
        map.insert(
            "required".to_owned(),
            Value::Array(required.into_iter().map(Value::String).collect()),
        );
    }
}

/// Report whether a property schema admits `null`.
fn is_nullable(property: &Value) -> bool {
    if let Some(Value::Array(types)) = property.get("type") {
        return types.iter().any(|value| value.as_str() == Some("null"));
    }
    ["anyOf", "oneOf"].iter().any(|key| {
        property
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|branches| branches.iter().any(is_null_branch))
    })
}

/// Remove the `null` alternative a property schema admits, if any.
fn strip_null(property: &mut Value) {
    let Value::Object(map) = property else {
        return;
    };
    if let Some(Value::Array(types)) = map.get_mut("type") {
        types.retain(|value| value.as_str() != Some("null"));
        if let [only] = types.as_slice() {
            let only = only.clone();
            map.insert("type".to_owned(), only);
        }
    }
    for key in ["anyOf", "oneOf"] {
        let Some(Value::Array(branches)) = map.get_mut(key) else {
            continue;
        };
        branches.retain(|branch| !is_null_branch(branch));
        if let [only] = branches.as_slice() {
            let only = only.clone();
            map.remove(key);
            if let Value::Object(fields) = only {
                for (name, value) in fields {
                    map.insert(name, value);
                }
            }
        }
    }
}

/// Report whether a schema branch is the bare `null` type.
fn is_null_branch(branch: &Value) -> bool {
    branch.get("type").and_then(Value::as_str) == Some("null")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `api::data`, resolved from this crate's manifest directory.
    fn data_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../ocx_cli/src/api/data")
    }

    /// Every `.rs` under `api::data`, with its `#[cfg(test)]` half removed.
    fn data_sources() -> Vec<(String, String)> {
        let mut files: Vec<_> = std::fs::read_dir(data_dir())
            .expect("api::data is a directory in this workspace")
            .filter_map(|entry| {
                let path = entry.expect("a readable directory entry").path();
                (path.extension()? == "rs").then(|| {
                    let name = path.file_name()?.to_string_lossy().into_owned();
                    let body = std::fs::read_to_string(&path).ok()?;
                    Some((name, body.split("#[cfg(test)]").next()?.to_owned()))
                })?
            })
            .collect();
        files.sort();
        files
    }

    /// A root that is printable but unlisted is a command whose JSON nobody
    /// published — the failure mode is silent, so it is asserted rather than
    /// trusted to review.
    #[test]
    fn every_printable_root_is_published() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        let published: Vec<&str> = document["reports"]
            .as_object()
            .expect("`reports` is an object")
            .keys()
            .map(String::as_str)
            .collect();

        let mut missing = Vec::new();
        for (file, body) in data_sources() {
            for line in body.lines() {
                // `impl<R: Serialize> Printable for SweepReport<R>` counts too:
                // a generic root is still a printed document, and matching only
                // the plain form is how it went unpublished the first time.
                let Some((_, tail)) = line.split_once("Printable for ") else {
                    continue;
                };
                let name = tail.split(['<', ' ', '{']).next().unwrap_or_default().trim();
                if name.is_empty() {
                    continue;
                }
                // A generic root is published once per instantiation, under a
                // name that starts with the base type.
                if !published
                    .iter()
                    .any(|root| *root == name || root.starts_with(&format!("{name}<")))
                {
                    missing.push(format!("{name} ({file})"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "printable report roots missing from `visit_report_roots`: {missing:?}"
        );
    }

    /// Every `$ref` target in `node`, with the pointer it sits at.
    fn refs<'a>(node: &'a Value, pointer: String, out: &mut Vec<(String, &'a str)>) {
        match node {
            Value::Object(map) => {
                for (key, child) in map {
                    if key == "$ref"
                        && let Some(target) = child.as_str()
                    {
                        out.push((pointer.clone(), target));
                    }
                    refs(child, format!("{pointer}/{key}"), out);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    refs(item, format!("{pointer}/{index}"), out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn every_root_is_a_top_level_wrapper_over_its_payload() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        let versions = ocx::api::report_versions();
        let roots = document["reports"].as_object().expect("`reports` is an object");
        assert_eq!(roots.len(), 56);
        assert_eq!(roots.len(), versions.len());
        for (name, entry) in roots {
            let target = entry["$ref"].as_str().expect("a root is a `$ref`");
            let wrapper_name = target.strip_prefix("#/$defs/").expect("a `$defs` pointer");
            let payload_name = wrapper_name.strip_suffix("Root").expect("a `*Root` wrapper");
            let wrapper = &document["$defs"][wrapper_name];
            let payload = &document["$defs"][payload_name];
            let keys = |schema: &Value| -> Vec<String> {
                schema["properties"]
                    .as_object()
                    .expect("an object schema")
                    .keys()
                    .cloned()
                    .collect()
            };
            let mut expected = vec!["schema_version".to_owned()];
            expected.extend(keys(payload));
            assert_eq!(keys(wrapper), expected, "{name}");
            assert_eq!(wrapper["required"][0], "schema_version", "{name}");
            assert_eq!(
                wrapper["properties"]["schema_version"]["const"],
                versions[name.as_str()],
                "{name}"
            );
            assert!(payload["properties"].get("schema_version").is_none(), "{name}");
        }

        let mut found = Vec::new();
        refs(&document["$defs"], "/$defs".to_owned(), &mut found);
        let nested: Vec<_> = found.iter().filter(|(_, target)| target.ends_with("Root")).collect();
        assert!(nested.is_empty(), "a wrapper referenced from a payload: {nested:?}");
        // A root type nested in another report reaches the payload, so the scan above judged real nesting.
        assert!(
            found
                .iter()
                .any(|(at, target)| at.starts_with("/$defs/SignatureReportSweptTag")
                    && *target == "#/$defs/SignatureReport"),
            "the sweep row no longer nests `SignatureReport`: {found:?}"
        );
    }

    #[test]
    fn roots_start_at_one_and_the_six_formerly_wrapped_at_two() {
        let at_two: Vec<&str> = ocx::api::report_versions()
            .into_iter()
            .filter(|(_, version)| *version != 1)
            .map(|(name, version)| {
                assert_eq!(version, REPORTS_VERSION, "{name}");
                name
            })
            .collect();
        assert_eq!(
            at_two,
            [
                "AttestationReport",
                "SbomListingReport",
                "SignatureReport",
                "SweepReport<AttestationReport>",
                "SweepReport<SignatureReport>",
                "VerificationReport",
            ]
        );
    }

    #[test]
    #[should_panic(expected = "must be an object schema with `properties`")]
    fn a_payload_that_is_not_an_object_has_no_wrapper() {
        wrapper(
            "Listing",
            &json!({"description": "A list.", "type": "array", "items": {"type": "string"}}),
            1,
        );
    }

    #[test]
    fn the_report_contract_is_published_as_v2() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        assert_eq!(
            document["$id"],
            format!("https://ocx.sh/schemas/reports/v{REPORTS_VERSION}.json")
        );
    }

    /// Unset optionals are omitted, pinned on real fields rather than on a fixture.
    ///
    /// `DryRunEntry::path` and `EnvEntry::source` are both omitted when unset, never
    /// `null`, so neither is required; the SDK's `pull --dry-run` parser once broke on
    /// a `path` that was written as `null`. The written-`null` shape is pinned on a
    /// fixture by `null_is_stripped_only_from_properties_serde_can_omit`.
    #[test]
    fn unset_optionals_are_omitted_from_real_report_fields() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        let defs = &document["$defs"];

        for (def, field) in [("DryRunEntry", "path"), ("EnvEntry", "source")] {
            let entry = &defs[def];
            assert!(
                !entry["required"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{def} has required fields"))
                    .iter()
                    .any(|name| name == field),
                "{def}.{field} is absent when unset, so it is not required"
            );
            let property = &entry["properties"][field];
            assert!(property.is_object(), "{def}.{field} is published: {entry}");
            assert!(
                property.get("anyOf").is_none() && !property["type"].is_array(),
                "{def}.{field} carries no null alternative: {property}"
            );
        }
    }

    /// The marker is an internal handshake with `normalize`, which the report
    /// contract no longer runs; publishing it would invite consumers to depend on it.
    #[test]
    fn the_marker_never_reaches_the_published_document() {
        assert!(!reports_schema().contains(ABSENT_WHEN_NONE));
    }

    /// One fixture type through the report pipeline: generated, then published.
    fn published<T: schemars::JsonSchema>() -> Value {
        let mut schema =
            serde_json::to_value(settings().into_generator().into_root_schema_for::<T>()).expect("schema serialises");
        publish(&mut schema);
        for def in schema
            .get_mut("$defs")
            .and_then(Value::as_object_mut)
            .into_iter()
            .flat_map(Map::values_mut)
        {
            publish(def);
        }
        schema
    }

    fn required(schema: &Value) -> Vec<&str> {
        schema["required"]
            .as_array()
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    }

    /// A `null` survives exactly where serde writes one, so the contract lint's no-null rule
    /// reds on an `Option` that lost its `skip_serializing_if`.
    #[test]
    fn null_is_stripped_only_from_properties_serde_can_omit() {
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct Inner {
            /// A field.
            x: u8,
        }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct Fixture {
            #[serde(skip_serializing_if = "Option::is_none")]
            omitted: Option<String>,
            written: Option<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            omitted_ref: Option<Inner>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            omitted_list: Vec<String>,
        }
        let schema = published::<Fixture>();
        assert_eq!(required(&schema), ["written"], "{schema}");
        assert_eq!(schema["properties"]["omitted"], json!({"type": "string"}));
        assert_eq!(schema["properties"]["written"]["type"], json!(["string", "null"]));
        assert_eq!(schema["properties"]["omitted_ref"], json!({"$ref": "#/$defs/Inner"}));
    }

    /// Paired with its control: the same subtree without the marker is
    /// rewritten, so an unchanged opaque leaf is the guard holding, not a no-op.
    #[test]
    fn an_opaque_leaf_is_never_rewritten() {
        let leaf = json!({
            "description": "Publisher payload.",
            "properties": {"camelKey": {"type": ["string", "null"], "default": null}},
            "default": {"Kebab-Key": null}
        });
        let mut opaque = leaf.clone();
        opaque[OPAQUE] = json!(true);
        let wrap = |payload: &Value| json!({"type": "object", "properties": {"payload": payload}});

        let mut published_opaque = wrap(&opaque);
        publish(&mut published_opaque);
        assert_eq!(published_opaque, wrap(&opaque));

        let mut published_plain = wrap(&leaf);
        publish(&mut published_plain);
        assert_ne!(published_plain, wrap(&leaf), "control: a plain subtree is rewritten");
    }

    #[test]
    fn annotations_are_dropped_but_properties_named_like_them_stay() {
        let mut schema = json!({
            "$comment": "c",
            "type": "object",
            "properties": {
                "default": {"type": "string", "description": "d", "default": "x"},
                "examples": {"type": "string", "description": "e", "examples": ["y"]}
            },
            "required": ["default", "examples"]
        });
        publish(&mut schema);
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": {
                    "default": {"type": "string", "description": "d"},
                    "examples": {"type": "string", "description": "e"}
                },
                "required": ["default", "examples"]
            })
        );
    }

    #[test]
    fn a_documented_const_set_publishes_as_an_open_enum() {
        #[derive(serde::Serialize, schemars::JsonSchema)]
        #[serde(rename_all = "snake_case")]
        /// A documented set.
        #[expect(dead_code, reason = "a schema fixture, never constructed")]
        enum Documented {
            /// The first.
            First,
            /// The second.
            Second,
        }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct Holder {
            value: Documented,
        }
        let schema = published::<Holder>();
        assert_eq!(
            schema["$defs"]["Documented"],
            json!({
                "description": "A documented set.",
                "type": "string",
                "x-ocx-enum": [
                    {"value": "first", "description": "The first."},
                    {"value": "second", "description": "The second."}
                ]
            })
        );
    }

    #[test]
    fn a_documented_integer_const_set_publishes_as_an_open_integer_enum() {
        let mut schema = json!({
            "description": "A version.",
            "oneOf": [{"type": "integer", "const": 1, "description": "The first."}]
        });
        publish(&mut schema);
        assert_eq!(
            schema,
            json!({
                "description": "A version.",
                "type": "integer",
                "x-ocx-enum": [{"value": 1, "description": "The first."}]
            })
        );
    }

    #[test]
    fn a_union_tagged_by_type_gains_one_unknown_arm_and_a_kind_union_does_not() {
        #[derive(serde::Serialize, schemars::JsonSchema)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[expect(dead_code, reason = "a schema fixture, never constructed")]
        enum Tagged {
            One { x: u8 },
            Two,
        }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        #[expect(dead_code, reason = "a schema fixture, never constructed")]
        enum Kinded {
            One { x: u8 },
            Two,
        }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct Inner {
            /// A field.
            x: u8,
        }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[expect(dead_code, reason = "a schema fixture, never constructed")]
        enum Wrapped {
            One(Inner),
        }
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct Holder {
            tagged: Tagged,
            kinded: Kinded,
            wrapped: Wrapped,
        }
        let schema = published::<Holder>();
        let arms = schema["$defs"]["Tagged"]["oneOf"].as_array().expect("a union");
        assert_eq!(arms.len(), 3);
        assert_eq!(
            arms[2],
            json!({
                "x-ocx-unknown-variant": true,
                "type": "object",
                "required": ["type"],
                "properties": {"type": {"type": "string", "not": {"enum": ["one", "two"]}}}
            })
        );
        let kinded = schema["$defs"]["Kinded"]["oneOf"].as_array().expect("a union");
        assert_eq!(kinded.len(), 2, "a `kind` tag is not a contract union: {kinded:?}");
        let wrapped = schema["$defs"]["Wrapped"]["oneOf"].as_array().expect("a union");
        assert_eq!(
            wrapped.get(1),
            Some(&json!({
                "x-ocx-unknown-variant": true,
                "type": "object",
                "required": ["type"],
                "properties": {"type": {"type": "string", "not": {"enum": ["one"]}}}
            })),
            "a `$ref` arm's tag sits beside the `$ref`: {wrapped:?}"
        );
    }

    /// Walks schema positions only: a key of `properties` or `$defs` is a name.
    fn annotation_pointers(node: &Value, pointer: &str, out: &mut Vec<String>) {
        let Value::Object(map) = node else {
            if let Value::Array(items) = node {
                for (index, item) in items.iter().enumerate() {
                    annotation_pointers(item, &format!("{pointer}/{index}"), out);
                }
            }
            return;
        };
        for (key, child) in map {
            let at = format!("{pointer}/{key}");
            if ANNOTATIONS.contains(&key.as_str()) {
                out.push(at.clone());
            }
            if matches!(key.as_str(), "properties" | "$defs" | "reports") {
                for (name, schema) in child.as_object().into_iter().flatten() {
                    annotation_pointers(schema, &format!("{at}/{name}"), out);
                }
            } else {
                annotation_pointers(child, &at, out);
            }
        }
    }

    #[test]
    fn the_report_contract_carries_no_annotation() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        let mut found = Vec::new();
        annotation_pointers(&document, "", &mut found);
        assert!(found.is_empty(), "annotations in the report contract: {found:?}");
    }

    /// schemars appends a counter when two types share a `$def` name; the
    /// second one's name then depends on registration order.
    #[test]
    fn no_def_name_is_a_collision_counter() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        let defs = document["$defs"].as_object().expect("`$defs` is an object");
        let collided: Vec<&String> = defs
            .keys()
            .filter(|name| {
                let base = name.trim_end_matches(|c: char| c.is_ascii_digit());
                base.len() < name.len() && defs.contains_key(base)
            })
            .collect();
        assert!(!defs.is_empty());
        assert!(collided.is_empty(), "`$def` names schemars disambiguated: {collided:?}");
    }

    /// Fields serde skips when empty are not `required`, wherever the type lives.
    #[test]
    fn serde_optional_fields_are_not_required() {
        let document: Value = serde_json::from_str(&reports_schema()).expect("the generated contract is valid JSON");
        let defs = &document["$defs"];
        let written = defs["WriteOutcome"]["oneOf"]
            .as_array()
            .expect("WriteOutcome is a union")
            .iter()
            .find(|arm| arm["properties"].get("dropped").is_some())
            .expect("an arm carries `dropped`");
        for (schema, field) in [
            (&defs["Bundle"], "strip_components"),
            (&defs["Dependency"], "name"),
            (written, "dropped"),
        ] {
            assert!(schema["properties"].get(field).is_some(), "`{field}` exists: {schema}");
            assert!(!required(schema).contains(&field), "`{field}` is required: {schema}");
        }
    }

    /// Every `.rs` under `crates/`, with its `#[cfg(test)]` half removed.
    fn crate_sources() -> Vec<(String, String)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).expect("a readable directory under crates/") {
                let path = entry.expect("a readable directory entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let body = std::fs::read_to_string(&path).expect("a readable source file");
                    let body = body.split("#[cfg(test)]").next().unwrap_or_default().to_owned();
                    out.push((path.to_string_lossy().into_owned(), body));
                }
            }
        }
        let mut files = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".."), &mut files);
        files.sort();
        files
    }

    /// schemars keys `$defs` by `schema_id`, which a hand-written impl leaves
    /// at its `schema_name` — so two manual impls returning the same name
    /// silently overwrite each other in the published document, while derives
    /// self-disambiguate. `package::version::Version` and
    /// `metadata::bundle::Version` shipped that way once: `SlotRow.source`
    /// published as the bundle format's `integer enum [1]`.
    #[test]
    fn manual_schema_names_are_unique_across_the_workspace() {
        let mut owners: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
        for (file, body) in crate_sources() {
            for (at, _) in body.match_indices("fn schema_name()") {
                let tail = &body[at..];
                let Some(start) = tail.find('"') else { continue };
                let literal = &tail[start + 1..];
                let Some(end) = literal.find('"') else { continue };
                owners.entry(literal[..end].to_owned()).or_default().push(file.clone());
            }
        }
        assert!(
            !owners.is_empty(),
            "the walk found no manual `schema_name` impls — wrong root?"
        );
        let collisions: Vec<String> = owners
            .iter()
            .filter(|(_, files)| files.len() > 1)
            .map(|(name, files)| format!("{name}: {}", files.join(", ")))
            .collect();
        assert!(
            collisions.is_empty(),
            "manual JsonSchema impls share a schema_name — the later one overwrites the earlier in `$defs`:\n{}",
            collisions.join("\n")
        );
    }
}
