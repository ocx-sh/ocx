// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The contract lint: the [`Rule`]s over the output schemas and the command grammar.
//!
//! A run visits every root, `$def`, property, union arm, command and arg once and counts what it read, so a caller
//! can floor the counts against an independent reading of the same document.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::subset::{
    ALLOWLIST, ENUM_ENTRY_MEMBERS, VOCABULARY, VOCABULARY_FORMATS, X_ENUM, arm_tag, is_kebab_case, is_marked_unknown,
    is_opaque, is_snake_case, is_unknown_arm, ref_target,
};

/// Which document a lint run reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The `--format json` report contract.
    Reports,
    /// The error document.
    Errors,
    /// The command grammar, `cli.json`.
    Cli,
    /// An input schema (`metadata`, `patch`): only the key-spelling rule applies.
    Input,
}

/// One rule violation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The rule that fired.
    pub rule: Rule,
    /// Where: a JSON pointer into a schema document, or a command-line subject in `cli.json`.
    pub pointer: String,
    /// What is wrong.
    pub message: String,
}

/// What a run read; the caller floors these against independent counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Visit {
    pub roots: usize,
    pub defs: usize,
    pub properties: usize,
    pub keywords: usize,
    pub commands: usize,
    pub args: usize,
}

/// A run's findings and its reader counts.
#[derive(Debug, Default)]
pub struct Report {
    pub findings: Vec<Finding>,
    pub visit: Visit,
}

id_enum! {
    /// Every lint rule with its one-line title.
    Rule, "rule", {
        L01 "no anyOf",
        L02 "no type array",
        L03 "no null outside opaque leaves",
        L04 "scalar enums use the x-ocx-enum form",
        L05 "oneOf is a tagged union with one unknown arm; no payload property named type",
        L06 "snake_case property names",
        L07 "snake_case enum values",
        L08 "every property and $def has a description",
        L09 "every root is a schema_version wrapper over its payload",
        L10 "no root has a top-level error property",
        L11 "shared concepts use their vocabulary $def",
        L12 "no vocabulary pattern or format outside its $def",
        L13 "no top-level entries; a lone array payload is named items",
        L14 "a *Root $def is referenced only from the root list",
        L15 "no additionalProperties: false, no unevaluatedProperties",
        L16 "error.context properties are vocabulary $refs",
        L17 "keyword outside the allowlist",
        L18 "status is a scalar-enum $ref with no dry_run beside it",
        C01 "every non-hidden command and arg has help",
        C02 "a short letter has one meaning",
        C03 "long flags kebab-case, choice values snake_case",
        C04 "a flag name has one value type",
        C05 "env name is OCX_ + SCREAMING(long flag)",
        C06 "output destinations spell --output/-o",
        C07 "defaults identical under hermetic and populated env",
        C08 "secrets never travel on argv",
        C09 "every leaf declares an output mode naming a published root",
    }
}

impl Rule {
    /// Whether the rule applies to a schema document of `kind`.
    pub const fn applies(self, kind: Kind) -> bool {
        match kind {
            Kind::Input => matches!(self, Self::L06),
            Kind::Errors => !matches!(self, Self::L09 | Self::L10 | Self::L13 | Self::L14 | Self::L18),
            Kind::Reports => !matches!(self, Self::L16),
            Kind::Cli => false,
        }
    }
}

/// Keywords whose value is data, never a schema to descend into.
const DATA_KEYWORDS: &[&str] = &[
    "$schema",
    "$id",
    "$ref",
    "$comment",
    "title",
    "description",
    "type",
    "required",
    "const",
    "enum",
    "default",
    "examples",
    "format",
    "pattern",
    "minimum",
    "maximum",
    "maxLength",
    "uniqueItems",
    X_ENUM,
    "x-ocx-unknown-variant",
    "x-ocx-opaque",
];

/// Off-allowlist keywords a dedicated rule already reports, so the allowlist rule does not report them twice.
const DEDICATED: &[&str] = &["anyOf", "unevaluatedProperties"];

/// Lint one document.
pub fn run(doc: &Value, kind: Kind) -> Report {
    match kind {
        Kind::Cli => cli::run(doc),
        _ => SchemaLint::new(doc, kind).run(),
    }
}

/// The output-mode rule's cross-document half: every root a command's output names exists in `reports`.
pub fn cross(cli: &Value, reports: &Value) -> Vec<Finding> {
    cli::cross(cli, reports)
}

/// The defaults rule: the same grammar walked under a hermetic and a populated environment must carry identical defaults.
pub fn defaults_drift(hermetic: &Value, populated: &Value) -> Vec<Finding> {
    cli::defaults_drift(hermetic, populated)
}

pub(crate) fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn finding(rule: Rule, pointer: impl Into<String>, message: impl Into<String>) -> Finding {
    Finding {
        rule,
        pointer: pointer.into(),
        message: message.into(),
    }
}

/// Where the walk is: inside an opaque leaf, an unknown arm, its `not`, or a named `$def`.
#[derive(Clone, Copy, Default)]
struct Scope<'a> {
    opaque: bool,
    unknown_arm: bool,
    unknown_not: bool,
    def: Option<&'a str>,
}

struct SchemaLint<'a> {
    doc: &'a Value,
    kind: Kind,
    defs: Option<&'a Map<String, Value>>,
    vocabulary_patterns: BTreeSet<&'a str>,
    report: Report,
}

impl<'a> SchemaLint<'a> {
    fn new(doc: &'a Value, kind: Kind) -> Self {
        let defs = doc.get("$defs").and_then(Value::as_object);
        let vocabulary_patterns = defs
            .into_iter()
            .flatten()
            .filter(|(name, _)| VOCABULARY.contains(&name.as_str()))
            .filter_map(|(_, def)| def.get("pattern").and_then(Value::as_str))
            .collect();
        Self {
            doc,
            kind,
            defs,
            vocabulary_patterns,
            report: Report::default(),
        }
    }

    fn push(&mut self, rule: Rule, pointer: impl Into<String>, message: impl Into<String>) {
        if rule.applies(self.kind) {
            self.report.findings.push(finding(rule, pointer, message));
        }
    }

    fn def(&self, name: &str) -> Option<&'a Value> {
        self.defs.and_then(|defs| defs.get(name))
    }

    /// `node` itself, or the `$def` its `$ref` names, with the pointer of whichever was taken.
    fn resolve(&self, node: &'a Value, pointer: String) -> (&'a Value, String) {
        match ref_target(node).and_then(|name| self.def(name).map(|def| (name, def))) {
            Some((name, def)) => (def, format!("/$defs/{}", escape(name))),
            None => (node, pointer),
        }
    }

    fn run(mut self) -> Report {
        if self.kind != Kind::Reports {
            self.report.visit.roots = 1;
        }
        self.node(self.doc, String::new(), Scope::default());
        match self.kind {
            Kind::Reports => self.roots(),
            Kind::Errors => self.error_context(),
            Kind::Cli | Kind::Input => {}
        }
        self.report
    }

    fn node(&mut self, node: &'a Value, pointer: String, scope: Scope<'a>) {
        let Some(map) = node.as_object() else { return };
        let scope = Scope {
            opaque: scope.opaque || is_opaque(node),
            ..scope
        };
        self.node_rules(map, &pointer, scope);
        for (keyword, child) in map {
            self.report.visit.keywords += 1;
            let at = format!("{pointer}/{}", escape(keyword));
            let root_list = keyword == "reports" && pointer.is_empty() && self.kind == Kind::Reports;
            if !ALLOWLIST.contains(&keyword.as_str()) && !DEDICATED.contains(&keyword.as_str()) && !root_list {
                self.push(
                    Rule::L17,
                    at.clone(),
                    format!("`{keyword}` is outside the representation subset"),
                );
            }
            match keyword.as_str() {
                "properties" => self.properties(child, &at, scope),
                "$defs" => self.defs_map(child, &at),
                "reports" if root_list => {
                    for (name, entry) in child.as_object().into_iter().flatten() {
                        self.report.visit.roots += 1;
                        self.node(entry, format!("{at}/{}", escape(name)), scope);
                    }
                }
                "oneOf" => {
                    for (index, arm) in child.as_array().into_iter().flatten().enumerate() {
                        let arm_scope = Scope {
                            unknown_arm: is_marked_unknown(arm),
                            ..scope
                        };
                        self.node(arm, format!("{at}/{index}"), arm_scope);
                    }
                }
                "not" => {
                    let not_scope = Scope {
                        unknown_not: scope.unknown_arm,
                        ..scope
                    };
                    self.node(child, at, not_scope);
                }
                data if DATA_KEYWORDS.contains(&data) => {}
                _ => self.descend(child, at, scope),
            }
        }
    }

    /// Any other schema-valued keyword: an object is a schema, an array a list of schemas.
    fn descend(&mut self, child: &'a Value, at: String, scope: Scope<'a>) {
        match child {
            Value::Object(_) => self.node(child, at, scope),
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    self.node(item, format!("{at}/{index}"), scope);
                }
            }
            _ => {}
        }
    }

    fn defs_map(&mut self, child: &'a Value, at: &str) {
        for (name, def) in child.as_object().into_iter().flatten() {
            self.report.visit.defs += 1;
            let pointer = format!("{at}/{}", escape(name));
            if !has_description(def) {
                self.push(Rule::L08, pointer.clone(), format!("`$def` {name} has no description"));
            }
            self.node(
                def,
                pointer,
                Scope {
                    def: Some(name),
                    ..Scope::default()
                },
            );
        }
    }

    fn properties(&mut self, child: &'a Value, at: &str, scope: Scope<'a>) {
        let Some(properties) = child.as_object() else { return };
        for (name, schema) in properties {
            self.report.visit.properties += 1;
            let pointer = format!("{at}/{}", escape(name));
            if !scope.opaque {
                if !is_snake_case(name) {
                    self.push(
                        Rule::L06,
                        pointer.clone(),
                        format!("property `{name}` is not snake_case"),
                    );
                }
                if name == "type" && !is_tag(schema) {
                    self.push(
                        Rule::L05,
                        pointer.clone(),
                        "a payload property named `type` collides with the union tag",
                    );
                }
                self.vocabulary_by_name(name, schema, &pointer);
            }
            let union_tag = name == "type" && is_tag(schema);
            if !union_tag && !has_description(schema) {
                self.push(
                    Rule::L08,
                    pointer.clone(),
                    format!("property `{name}` has no description"),
                );
            }
            if name == "status" {
                let scalar_enum = ref_target(schema)
                    .and_then(|target| self.def(target))
                    .is_some_and(|def| def.get(X_ENUM).is_some());
                if !scalar_enum {
                    self.push(
                        Rule::L18,
                        pointer.clone(),
                        "`status` is not a `$ref` to a scalar-enum `$def`",
                    );
                }
                if properties.contains_key("dry_run") {
                    self.push(Rule::L18, format!("{at}/dry_run"), "a `dry_run` flag beside `status`");
                }
            }
            self.node(schema, pointer, scope);
        }
    }

    fn vocabulary_by_name(&mut self, name: &str, schema: &Value, pointer: &str) {
        let Some(expected) = vocabulary_for(name) else { return };
        let mut referenced = vec![ref_target(schema)];
        if let Some(items) = schema.get("items") {
            referenced.push(ref_target(items));
        }
        for composite in ["anyOf", "oneOf"] {
            for arm in schema.get(composite).and_then(Value::as_array).into_iter().flatten() {
                referenced.push(ref_target(arm));
            }
        }
        if !referenced
            .into_iter()
            .flatten()
            .any(|target| expected.contains(&target))
        {
            self.push(
                Rule::L11,
                pointer.to_owned(),
                format!("`{name}` is not a `$ref` to {}", expected.join(" or ")),
            );
        }
    }

    fn node_rules(&mut self, map: &'a Map<String, Value>, pointer: &str, scope: Scope<'a>) {
        let at = |suffix: &str| format!("{pointer}/{suffix}");
        match map.get("type") {
            Some(Value::Array(types)) => {
                self.push(Rule::L02, at("type"), "`type` is an array");
                if !scope.opaque && types.iter().any(|value| value == "null") {
                    self.push(Rule::L03, at("type"), "`type` admits null");
                }
            }
            Some(Value::String(value)) if value == "null" && !scope.opaque => {
                self.push(Rule::L03, at("type"), "`type` is null");
            }
            _ => {}
        }
        if map.contains_key("anyOf") {
            self.push(Rule::L01, at("anyOf"), "`anyOf` is outside the representation subset");
        }
        if map.get("additionalProperties") == Some(&Value::Bool(false)) {
            self.push(
                Rule::L15,
                at("additionalProperties"),
                "closed object: `additionalProperties: false`",
            );
        }
        if map.contains_key("unevaluatedProperties") {
            self.push(
                Rule::L15,
                at("unevaluatedProperties"),
                "closed object: `unevaluatedProperties`",
            );
        }
        if let Some(value) = map.get("const") {
            self.enum_value(value, &at("const"), scope);
        }
        if let Some(values) = map.get("enum").and_then(Value::as_array)
            && !scope.unknown_not
        {
            self.push(Rule::L04, at("enum"), "closed `enum`: use `x-ocx-enum`");
            for (index, value) in values.iter().enumerate() {
                self.enum_value(value, &format!("{pointer}/enum/{index}"), scope);
            }
        }
        if let Some(entries) = map.get(X_ENUM) {
            self.scalar_enum(map, entries, pointer, scope);
        }
        if let Some(arms) = map.get("oneOf").and_then(Value::as_array) {
            self.union(arms, pointer);
        }
        if let Some(target) = map
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| r.strip_prefix("#/$defs/"))
            && target.ends_with("Root")
            && scope.def.is_some()
        {
            self.push(
                Rule::L14,
                at("$ref"),
                format!("wrapper `{target}` referenced from a payload"),
            );
        }
        if !scope.def.is_some_and(|def| VOCABULARY.contains(&def)) {
            if let Some(pattern) = map.get("pattern").and_then(Value::as_str)
                && self.vocabulary_patterns.contains(pattern)
            {
                self.push(Rule::L12, at("pattern"), "a vocabulary pattern outside its `$def`");
            }
            if let Some(format) = map.get("format").and_then(Value::as_str)
                && let Some((_, owner)) = VOCABULARY_FORMATS.iter().find(|(name, _)| *name == format)
            {
                self.push(
                    Rule::L12,
                    at("format"),
                    format!("format `{format}` belongs to `{owner}`"),
                );
            }
        }
    }

    fn enum_value(&mut self, value: &Value, pointer: &str, scope: Scope<'a>) {
        if scope.opaque || scope.unknown_not {
            return;
        }
        match value {
            Value::Null => self.push(Rule::L03, pointer.to_owned(), "null enum value"),
            Value::String(text) if !is_snake_case(text) => {
                self.push(
                    Rule::L07,
                    pointer.to_owned(),
                    format!("enum value `{text}` is not snake_case"),
                );
            }
            _ => {}
        }
    }

    fn scalar_enum(&mut self, map: &Map<String, Value>, entries: &Value, pointer: &str, scope: Scope<'a>) {
        let at = format!("{pointer}/{X_ENUM}");
        if !matches!(map.get("type").and_then(Value::as_str), Some("string" | "integer")) {
            self.push(Rule::L04, at.clone(), "a scalar enum is a string or integer");
        }
        let Some(entries) = entries.as_array() else {
            self.push(Rule::L04, at, "`x-ocx-enum` is not a list");
            return;
        };
        for (index, entry) in entries.iter().enumerate() {
            let entry_at = format!("{at}/{index}");
            let Some(members) = entry.as_object() else {
                self.push(Rule::L04, entry_at, "an `x-ocx-enum` entry is not an object");
                continue;
            };
            let described = members
                .get("description")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.trim().is_empty());
            let unknown: Vec<&str> = members
                .keys()
                .map(String::as_str)
                .filter(|member| !ENUM_ENTRY_MEMBERS.contains(member))
                .collect();
            if !members.contains_key("value") || !described || !unknown.is_empty() {
                self.push(
                    Rule::L04,
                    entry_at.clone(),
                    format!("an entry needs `value` and `description` and nothing outside {ENUM_ENTRY_MEMBERS:?}"),
                );
            }
            if let Some(value) = members.get("value") {
                self.enum_value(value, &format!("{entry_at}/value"), scope);
            }
        }
    }

    /// An arm's tag: beside its `$ref` (a newtype variant) or inside the `$def` it names.
    fn tag_of(&self, arm: &'a Value) -> Option<&'a str> {
        arm_tag(arm).or_else(|| arm_tag(self.resolve(arm, String::new()).0))
    }

    fn union(&mut self, arms: &'a [Value], pointer: &str) {
        let at = format!("{pointer}/oneOf");
        if !arms.is_empty()
            && arms
                .iter()
                .all(|arm| arm.get("const").is_some() && arm.get("properties").is_none())
        {
            self.push(Rule::L04, at, "const-`oneOf` enum: use `x-ocx-enum`");
            return;
        }
        let known: BTreeSet<&str> = arms
            .iter()
            .filter(|arm| !is_marked_unknown(arm))
            .filter_map(|arm| self.tag_of(arm))
            .collect();
        let mut unknown = 0;
        for (index, arm) in arms.iter().enumerate() {
            let arm_at = format!("{at}/{index}");
            if is_marked_unknown(arm) {
                unknown += 1;
                if !is_unknown_arm(arm, &known) {
                    self.push(
                        Rule::L05,
                        arm_at,
                        "the unknown arm must require a string `type` excluding exactly the known tags",
                    );
                }
            } else if self.tag_of(arm).is_none() {
                self.push(Rule::L05, arm_at, "a union arm without a required `type` tag");
            }
        }
        if unknown != 1 {
            self.push(
                Rule::L05,
                at,
                format!("{unknown} unknown arms; a tagged union has exactly one"),
            );
        }
    }

    /// The root-shape rules over each entry of the root list.
    fn roots(&mut self) {
        let Some(roots) = self.doc.get("reports").and_then(Value::as_object) else {
            self.push(Rule::L09, "/reports", "no root list");
            return;
        };
        for (name, entry) in roots {
            let root_at = format!("/reports/{}", escape(name));
            let Some(target) = ref_target(entry) else {
                self.push(Rule::L09, root_at, "root is not a `$ref` to a `$def`");
                continue;
            };
            let Some(def) = self.def(target) else {
                self.push(Rule::L09, root_at, format!("root names missing `$def` {target}"));
                continue;
            };
            let payload_name = match target.strip_suffix("Root") {
                Some(payload) => {
                    self.wrapper(def, payload, &root_at);
                    payload
                }
                None => {
                    self.push(Rule::L09, root_at, format!("`{target}` is not a `*Root` wrapper"));
                    target
                }
            };
            let Some(payload) = self.def(payload_name) else {
                continue;
            };
            self.payload_shape(payload, &format!("/$defs/{}", escape(payload_name)));
        }
        for (name, def) in self.defs.into_iter().flatten() {
            if !name.ends_with("Root") && def.pointer("/properties/schema_version").is_some() {
                self.push(
                    Rule::L09,
                    format!("/$defs/{}/properties/schema_version", escape(name)),
                    "only a root wrapper carries `schema_version`",
                );
            }
        }
    }

    fn wrapper(&mut self, def: &Value, payload: &str, root_at: &str) {
        let leads = def.pointer("/required/0").and_then(Value::as_str) == Some("schema_version");
        let pinned = def.pointer("/properties/schema_version/const").is_some();
        let names = |schema: &Value| -> BTreeSet<String> {
            schema
                .get("properties")
                .and_then(Value::as_object)
                .map(|properties| properties.keys().cloned().collect())
                .unwrap_or_default()
        };
        let mut expected = self.def(payload).map(names);
        if let Some(expected) = expected.as_mut() {
            expected.insert("schema_version".to_owned());
        }
        let object = def.get("type") == Some(&Value::from("object"));
        if !object || !leads || !pinned || expected.as_ref() != Some(&names(def)) {
            self.push(
                Rule::L09,
                root_at.to_owned(),
                format!("wrapper must be an object led by a pinned `schema_version` over `{payload}`'s properties"),
            );
        }
    }

    fn payload_shape(&mut self, payload: &Value, pointer: &str) {
        if payload.get("type") == Some(&Value::from("array")) {
            self.push(Rule::L13, pointer.to_owned(), "a list root is an object with `items`");
        }
        let Some(properties) = payload.get("properties").and_then(Value::as_object) else {
            return;
        };
        if properties.contains_key("error") {
            self.push(
                Rule::L10,
                format!("{pointer}/properties/error"),
                "a root carries a top-level `error`",
            );
        }
        if properties.contains_key("entries") {
            self.push(
                Rule::L13,
                format!("{pointer}/properties/entries"),
                "`entries` is retired; use `items`",
            );
        }
        let payload_fields: Vec<(&String, &Value)> = properties
            .iter()
            .filter(|(name, _)| name.as_str() != "schema_version")
            .collect();
        if let [(name, schema)] = payload_fields.as_slice()
            && name.as_str() != "items"
            && name.as_str() != "entries"
            && schema.get("type") == Some(&Value::from("array"))
        {
            self.push(
                Rule::L13,
                format!("{pointer}/properties/{}", escape(name)),
                format!("a lone array payload is named `items`, not `{name}`"),
            );
        }
    }

    /// L16: every `error.context` property is a `$ref` to a vocabulary `$def`.
    fn error_context(&mut self) {
        let Some(error) = self.doc.pointer("/properties/error") else {
            return;
        };
        let (body, body_at) = self.resolve(error, "/properties/error".to_owned());
        let Some(context) = body.pointer("/properties/context") else {
            return;
        };
        let (context, context_at) = self.resolve(context, format!("{body_at}/properties/context"));
        for (name, schema) in context
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if !ref_target(schema).is_some_and(|target| VOCABULARY.contains(&target)) {
                self.push(
                    Rule::L16,
                    format!("{context_at}/properties/{}", escape(name)),
                    format!("context `{name}` is not a vocabulary `$ref`"),
                );
            }
        }
    }
}

fn has_description(schema: &Value) -> bool {
    schema
        .get("description")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
}

/// A `type` property that is a union tag (a string `const`) or the unknown arm's open tag (`not`).
fn is_tag(schema: &Value) -> bool {
    schema.get("const").is_some_and(Value::is_string) || schema.get("not").is_some()
}

/// The vocabulary `$def`s a property named `name` must reference.
fn vocabulary_for(name: &str) -> Option<&'static [&'static str]> {
    const IDENTIFIER: &[&str] = &["PackageRef", "PinnedPackageRef"];
    const PATH: &[&str] = &["AbsolutePath", "RelativePath"];
    let suffixed = |suffix: &str| name == suffix || name.ends_with(&format!("_{suffix}"));
    if suffixed("identifier") {
        Some(IDENTIFIER)
    } else if suffixed("digest") {
        Some(&["Digest"])
    } else if name.ends_with("_at") {
        Some(&["Timestamp"])
    } else if suffixed("size") {
        Some(&["ByteSize"])
    } else if name.ends_with("_path") || name.ends_with("_dir") || suffixed("home") {
        Some(PATH)
    } else if name == "platform" || name == "platforms" {
        Some(&["Platform"])
    } else if name == "registry" {
        Some(&["RegistryHost"])
    } else {
        None
    }
}

mod cli {
    use super::*;

    /// One arg and the command subject it belongs to.
    struct Arg<'a> {
        command: String,
        command_hidden: bool,
        arg: &'a Value,
    }

    impl Arg<'_> {
        fn text(&self, key: &str) -> Option<&str> {
            self.arg.get(key).and_then(Value::as_str)
        }

        fn flag(&self, key: &str) -> bool {
            self.arg.get(key) == Some(&Value::Bool(true))
        }

        fn hidden(&self) -> bool {
            self.command_hidden || self.flag("hidden")
        }

        fn value_type(&self) -> &str {
            self.arg
                .pointer("/value/type")
                .and_then(Value::as_str)
                .unwrap_or_default()
        }

        fn subject(&self) -> String {
            match self.text("long") {
                Some(long) => format!("{} --{long}", self.command),
                None => format!("{} <{}>", self.command, self.text("id").unwrap_or_default()),
            }
        }

        fn long_segments(&self) -> Vec<&str> {
            self.text("long")
                .map(|long| long.split('-').collect())
                .unwrap_or_default()
        }
    }

    fn subject(command: &Value) -> String {
        let path: Vec<&str> = command
            .get("path")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if path.is_empty() {
            "ocx".to_owned()
        } else {
            format!("ocx {}", path.join(" "))
        }
    }

    fn commands<'a>(command: &'a Value, hidden: bool, out: &mut Vec<(&'a Value, bool)>) {
        let hidden = hidden || command.get("hidden") == Some(&Value::Bool(true));
        out.push((command, hidden));
        for child in command.get("commands").and_then(Value::as_array).into_iter().flatten() {
            commands(child, hidden, out);
        }
    }

    fn all_commands(doc: &Value) -> Vec<(&Value, bool)> {
        let mut out = Vec::new();
        if let Some(root) = doc.get("root") {
            commands(root, false, &mut out);
        }
        out
    }

    fn all_args<'a>(listed: &[(&'a Value, bool)]) -> Vec<Arg<'a>> {
        listed
            .iter()
            .flat_map(|(command, hidden)| {
                command
                    .get("args")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|arg| Arg {
                        command: subject(command),
                        command_hidden: *hidden,
                        arg,
                    })
            })
            .collect()
    }

    pub(super) fn run(doc: &Value) -> Report {
        let mut report = Report::default();
        let listed = all_commands(doc);
        let args = all_args(&listed);
        report.visit.commands = listed.len();
        report.visit.args = args.len();
        let findings = &mut report.findings;

        for (command, hidden) in &listed {
            let at = subject(command);
            let summary = command.get("summary").and_then(Value::as_str).unwrap_or_default();
            if !hidden && summary.trim().is_empty() {
                findings.push(finding(Rule::C01, at.clone(), "command has no summary"));
            }
            let leaf = command
                .get("commands")
                .and_then(Value::as_array)
                .is_none_or(Vec::is_empty);
            let outputs = command
                .get("output")
                .and_then(Value::as_array)
                .is_some_and(|modes| !modes.is_empty());
            if !hidden && leaf && !outputs {
                findings.push(finding(Rule::C09, at, "leaf command declares no output mode"));
            }
        }

        let mut shorts: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
        let mut value_specs: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
        let mut env_links: BTreeMap<String, String> = BTreeMap::new();
        for arg in &args {
            let at = arg.subject();
            let secret_named = arg
                .long_segments()
                .iter()
                .any(|segment| matches!(*segment, "token" | "password" | "key" | "secret"));
            let off_argv = arg.flag("stdin_secret") || matches!(arg.value_type(), "path" | "switch" | "count");
            if secret_named && !off_argv {
                findings.push(finding(Rule::C08, at.clone(), "a secret-valued flag read from argv"));
            }
            if let (Some(env), Some(long)) = (arg.text("env"), arg.text("long")) {
                env_links.insert(env.to_owned(), long.to_owned());
            }
            if arg.hidden() {
                continue;
            }
            if arg.text("help").unwrap_or_default().trim().is_empty() {
                findings.push(finding(Rule::C01, at.clone(), "arg has no help"));
            }
            let meaning = arg.text("long").or(arg.text("id")).unwrap_or_default();
            if let Some(short) = arg.text("short") {
                shorts.entry(short).or_default().insert(meaning);
            }
            if let Some(long) = arg.text("long") {
                if !is_kebab_case(long) {
                    findings.push(finding(Rule::C03, at.clone(), format!("`--{long}` is not kebab-case")));
                }
                if let Some(value) = arg.arg.get("value") {
                    value_specs
                        .entry(long)
                        .or_default()
                        .insert(sorted_keys(value).to_string());
                }
                let output_like = arg
                    .long_segments()
                    .iter()
                    .any(|segment| matches!(*segment, "out" | "output"));
                let valued = matches!(arg.value_type(), "path" | "string");
                if (output_like && valued && long != "output")
                    || (long == "output" && arg.text("short").is_some_and(|short| short != "o"))
                    || (arg.text("short") == Some("o") && long != "output")
                {
                    findings.push(finding(
                        Rule::C06,
                        at.clone(),
                        "an output destination is spelled `--output`/`-o`",
                    ));
                }
            }
            for choice in arg
                .arg
                .pointer("/value/choices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let hidden_choice = choice.get("hidden") == Some(&Value::Bool(true));
                if let Some(value) = choice.get("value").and_then(Value::as_str)
                    && !hidden_choice
                    && !is_snake_case(value)
                {
                    findings.push(finding(
                        Rule::C03,
                        format!("{at}={value}"),
                        "choice value is not snake_case",
                    ));
                }
            }
        }
        for (short, meanings) in shorts {
            if meanings.len() > 1 {
                findings.push(finding(
                    Rule::C02,
                    format!("-{short} {}", set(meanings)),
                    format!("`-{short}` has more than one meaning"),
                ));
            }
        }
        for (long, specs) in value_specs {
            if specs.len() > 1 {
                findings.push(finding(
                    Rule::C04,
                    format!("--{long} {}", set(specs.iter().map(String::as_str))),
                    format!("`--{long}` carries more than one value spec"),
                ));
            }
        }

        for env in doc.get("env").and_then(Value::as_array).into_iter().flatten() {
            let Some(name) = env.get("name").and_then(Value::as_str) else {
                continue;
            };
            if let Some(flag) = env.get("flag").and_then(Value::as_str) {
                env_links.insert(name.to_owned(), flag.trim_start_matches('-').to_owned());
            }
            for value in env.get("choices").and_then(Value::as_array).into_iter().flatten() {
                if let Some(value) = value.as_str()
                    && !is_snake_case(value)
                {
                    findings.push(finding(
                        Rule::C03,
                        format!("{name}={value}"),
                        "env choice value is not snake_case",
                    ));
                }
            }
        }
        for (name, long) in env_links {
            let expected = format!("OCX_{}", long.to_uppercase().replace('-', "_"));
            if name != expected {
                findings.push(finding(
                    Rule::C05,
                    name,
                    format!("linked to `--{long}`, so named `{expected}`"),
                ));
            }
        }
        report
    }

    pub(super) fn cross(cli: &Value, reports: &Value) -> Vec<Finding> {
        let roots = reports.get("reports").and_then(Value::as_object);
        let mut findings = Vec::new();
        for (command, _) in all_commands(cli) {
            for mode in command.get("output").and_then(Value::as_array).into_iter().flatten() {
                let Some(root) = mode.get("root").and_then(Value::as_str) else {
                    continue;
                };
                if !roots.is_some_and(|roots| roots.contains_key(root)) {
                    findings.push(finding(
                        Rule::C09,
                        format!("{} -> {root}", subject(command)),
                        format!("output names `{root}`, which `reports` does not publish"),
                    ));
                }
            }
        }
        findings
    }

    /// `value` with every object's keys in sorted order, so its text does not depend on serde_json's
    /// `preserve_order` feature.
    fn sorted_keys(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: BTreeMap<&String, Value> =
                    map.iter().map(|(key, child)| (key, sorted_keys(child))).collect();
                Value::Object(sorted.into_iter().map(|(key, child)| (key.clone(), child)).collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted_keys).collect()),
            other => other.clone(),
        }
    }

    /// `{a,b}`: the set rides in the pointer, so an entry covering it reds when the set grows.
    fn set<'a>(members: impl IntoIterator<Item = &'a str>) -> String {
        format!("{{{}}}", members.into_iter().collect::<Vec<_>>().join(","))
    }

    fn defaults(doc: &Value) -> BTreeMap<String, Value> {
        all_args(&all_commands(doc))
            .iter()
            .map(|arg| (arg.subject(), arg.arg.get("default").cloned().unwrap_or(Value::Null)))
            .collect()
    }

    pub(super) fn defaults_drift(hermetic: &Value, populated: &Value) -> Vec<Finding> {
        let populated = defaults(populated);
        defaults(hermetic)
            .into_iter()
            .filter(|(at, default)| populated.get(at) != Some(default))
            .map(|(at, default)| {
                let other = populated.get(&at).cloned().unwrap_or(Value::Null);
                finding(
                    Rule::C07,
                    at,
                    format!("default {default} becomes {other} under a populated env"),
                )
            })
            .collect()
    }
}

/// A waiver or exemption: `{rule, document, pointer, reason}`, plus `adr` on an exemption.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub rule: Rule,
    pub document: String,
    pub pointer: String,
    pub reason: String,
    #[serde(default)]
    pub adr: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WaiverFile {
    #[serde(default)]
    waiver: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExemptionFile {
    #[serde(default)]
    exemption: Vec<Entry>,
}

/// Parse a `waivers/<area>.toml` file (`waiver = [ … ]`).
pub fn parse_waivers(text: &str) -> Result<Vec<Entry>, toml::de::Error> {
    toml::from_str::<WaiverFile>(text).map(|file| file.waiver)
}

/// Parse `exemptions.toml` (`[[exemption]]`).
pub fn parse_exemptions(text: &str) -> Result<Vec<Entry>, toml::de::Error> {
    toml::from_str::<ExemptionFile>(text).map(|file| file.exemption)
}

/// The outcome of subtracting exemptions and waivers from a set of findings; every list empty is green.
#[derive(Debug, Default)]
pub struct Reconciled {
    /// Findings no entry covers, with their document.
    pub unmatched: Vec<(String, Finding)>,
    /// Exemptions that matched nothing.
    pub stale_exemptions: Vec<Entry>,
    /// Waivers that matched nothing.
    pub stale_waivers: Vec<Entry>,
    /// Exemptions that cite no decision record.
    pub missing_adr: Vec<Entry>,
}

impl Reconciled {
    /// Whether nothing is left to report.
    pub fn is_clean(&self) -> bool {
        self.unmatched.is_empty()
            && self.stale_exemptions.is_empty()
            && self.stale_waivers.is_empty()
            && self.missing_adr.is_empty()
    }
}

/// Subtract `exemptions` and `waivers` from `findings` (each paired with its document name).
pub fn reconcile(findings: &[(&str, Finding)], exemptions: &[Entry], waivers: &[Entry]) -> Reconciled {
    let covers = |entry: &Entry, document: &str, finding: &Finding| {
        entry.document == document && entry.rule == finding.rule && entry.pointer == finding.pointer
    };
    let mut exemption_used = vec![false; exemptions.len()];
    let mut waiver_used = vec![false; waivers.len()];
    let mut reconciled = Reconciled::default();
    for (document, finding) in findings {
        if let Some(index) = exemptions.iter().position(|entry| covers(entry, document, finding)) {
            exemption_used[index] = true;
        } else if let Some(index) = waivers.iter().position(|entry| covers(entry, document, finding)) {
            waiver_used[index] = true;
        } else {
            reconciled.unmatched.push(((*document).to_owned(), finding.clone()));
        }
    }
    let unused = |entries: &[Entry], used: &[bool]| -> Vec<Entry> {
        entries
            .iter()
            .zip(used)
            .filter(|(_, used)| !**used)
            .map(|(entry, _)| entry.clone())
            .collect()
    };
    reconciled.stale_exemptions = unused(exemptions, &exemption_used);
    reconciled.stale_waivers = unused(waivers, &waiver_used);
    reconciled.missing_adr = exemptions
        .iter()
        .filter(|entry| entry.adr.as_deref().is_none_or(|adr| adr.trim().is_empty()))
        .cloned()
        .collect();
    reconciled
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn vocabulary_is_chosen_by_name() {
        assert_eq!(
            vocabulary_for("identifier"),
            Some(&["PackageRef", "PinnedPackageRef"][..])
        );
        assert_eq!(vocabulary_for("base_digest"), Some(&["Digest"][..]));
        assert_eq!(vocabulary_for("created_at"), Some(&["Timestamp"][..]));
        assert_eq!(
            vocabulary_for("install_dir"),
            Some(&["AbsolutePath", "RelativePath"][..])
        );
        for home in ["home", "ocx_home", "toolchain_home"] {
            assert_eq!(
                vocabulary_for(home),
                Some(&["AbsolutePath", "RelativePath"][..]),
                "{home}"
            );
        }
        // Homonyms whose type, not name, carries the meaning: a user's spelling, an index path, a map or a list.
        for free in [
            "digests",
            "format",
            "atlas",
            "sizes",
            "package",
            "repository",
            "packages",
            "homes",
        ] {
            assert_eq!(vocabulary_for(free), None, "{free}");
        }
    }

    #[test]
    fn a_tag_is_a_string_const_or_the_open_tag() {
        assert!(is_tag(&json!({"const": "known"})));
        assert!(is_tag(&json!({"type": "string", "not": {"enum": ["known"]}})));
        assert!(!is_tag(&json!({"const": 1})));
        assert!(!is_tag(&json!({"type": "string"})));
    }

    /// A `$ref` arm carries its tag beside the `$ref`; resolving the `$ref` alone would lose it.
    #[test]
    fn a_ref_arm_is_tagged_by_its_sibling_type() {
        let tagged = |name: &str| {
            json!({
                "$ref": format!("#/$defs/{name}"),
                "type": "object",
                "properties": {"type": {"type": "string", "const": name.to_lowercase()}},
                "required": ["type"],
            })
        };
        let unknown = json!({
            "x-ocx-unknown-variant": true,
            "type": "object",
            "required": ["type"],
            "properties": {"type": {"type": "string", "not": {"enum": ["one", "two"]}}},
        });
        let payload = json!({"type": "object", "description": "A payload.", "properties": {}});
        let doc = |arms: Value| {
            json!({"$defs": {
                "One": payload, "Two": payload,
                "Union": {"description": "A union.", "oneOf": arms},
            }})
        };
        let l05 = |doc: &Value| -> Vec<String> {
            run(doc, Kind::Errors)
                .findings
                .into_iter()
                .filter(|finding| finding.rule == Rule::L05)
                .map(|finding| format!("{} {}", finding.pointer, finding.message))
                .collect()
        };
        assert_eq!(
            l05(&doc(json!([tagged("One"), tagged("Two"), unknown]))),
            Vec::<String>::new()
        );
        assert_eq!(
            l05(&doc(json!([tagged("One"), tagged("Two")]))),
            vec!["/$defs/Union/oneOf 0 unknown arms; a tagged union has exactly one"]
        );
    }

    #[test]
    fn escape_follows_rfc_6901() {
        assert_eq!(escape("a/b~c"), "a~1b~0c");
    }
}
