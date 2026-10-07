// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The Rust backend: emits the `ocx-sdk` crate's sources from the IR.
//!
//! Owned emission, not a schema-to-Rust tool: structs with `Option` fields omitted when unset, open scalar
//! enums and tagged unions with an `Unknown` arm, and a union decoder that reads `type` first, so a known
//! tag with a malformed payload is an error rather than an unknown variant. Beside them: one function per
//! command, the exit registry with fail-closed success predicates, the env manifest, the version table and
//! the runtime. The runtime is fixed text (`templates/rust/*.rs`) copied verbatim. Generated files name each other
//! as `super::module`, so the crate root can be a module of another crate; only `serde` and `serde_json`
//! are needed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::ir::{
    Arg, Command, EnumEntry, EnvVar, Field, Ir, Output, Scalar, Shape, TypeDef, TypeRef, ValueKind, Variant,
    variant_name,
};

/// The crate root inside the output directory; every other file is a module it declares.
pub const CRATE_ROOT: &str = "lib.rs";

/// The oldest `ocx` release that publishes the contract; named in `Error::Unsupported`.
const MINIMUM_OCX: &str = "0.6.4";

/// Global options the SDK owns: it always asks for JSON and needs the report on stdout.
const OWNED_GLOBALS: &[&str] = &["format", "json", "quiet"];

/// Status enums whose known success values the SDK vouches for; every other value, known or not, is a failure.
const SUCCESS_STATUSES: &[(&str, &[&str])] = &[
    ("AttestationStatus", &["succeeded"]),
    ("CapabilityStatus", &["passed"]),
    ("ListingStatus", &["success"]),
    ("ScriptStatus", &["passed"]),
    ("SlotStatus", &["ok"]),
    ("SweptStatus", &["completed"]),
];

/// Names a generated type must not take: they would shadow what the emitted code refers to unqualified.
const RESERVED_TYPES: &[&str] = &[
    "Argv",
    "BTreeMap",
    "Box",
    "Deserialize",
    "Deserializer",
    "Error",
    "Fields",
    "Option",
    "Result",
    "Self",
    "Serialize",
    "Serializer",
    "String",
    "Value",
    "Vec",
];

/// Rust keywords; a wire name that is one becomes a raw identifier, unless a raw one is not allowed.
const KEYWORDS: &[&str] = &[
    "Self", "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate", "do", "dyn",
    "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "macro",
    "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return", "self", "static", "struct", "super",
    "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
];

/// Keywords that cannot be raw identifiers.
const NO_RAW: &[&str] = &["Self", "crate", "self", "super"];

/// Methods of `Ocx` a command function must not take the name of.
const OCX_METHODS: &[&str] = &[
    "argv",
    "binary",
    "call",
    "call_empty",
    "call_outcome",
    "call_raw",
    "discover",
    "handshake",
    "new",
    "with_cancel",
    "with_env",
    "with_globals",
    "with_limits",
];

const LICENSE_HEADER: &str = "// SPDX-License-Identifier: Apache-2.0\n// Copyright 2026 The OCX Authors\n\n";

/// `is_secret` and its matcher as emitted into `env.rs`.
const IS_SECRET: &str = r#"/// Whether `name` is a public variable whose value must stay out of logs and `Debug`. A `{...}` in a declared
/// name stands for one or more characters, and names compare ignoring ASCII case, as the environment does on Windows.
pub fn is_secret(name: &str) -> bool {
    PUBLIC.iter().any(|var| var.secret && matches_name(var.name, name))
}

fn matches_name(pattern: &str, name: &str) -> bool {
    let Some((literal, rest)) = pattern.split_once('{') else {
        return pattern.eq_ignore_ascii_case(name);
    };
    let Some((_, tail)) = rest.split_once('}') else {
        return false;
    };
    let Some(head) = name.get(..literal.len()) else {
        return false;
    };
    let after = &name[literal.len()..];
    if !head.eq_ignore_ascii_case(literal) || after.is_empty() {
        return false;
    }
    after
        .char_indices()
        .skip(1)
        .map(|(index, _)| index)
        .chain(std::iter::once(after.len()))
        .any(|start| matches_name(tail, &after[start..]))
}
"#;

const WIRE: &str = include_str!("../templates/rust/wire.rs");
const SPAWN: &str = include_str!("../templates/rust/spawn.rs");
const RUNTIME: &str = include_str!("../templates/rust/runtime.rs");

/// The widest a generated line may be before rustfmt would break it.
const LINE_WIDTH: usize = 120;

/// One emitted source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedFile {
    /// Relative to the output directory.
    pub path: PathBuf,
    pub contents: String,
}

/// Emits every source file, sorted by path. Two runs over the same IR are byte-identical.
pub fn generate(ir: &Ir) -> Vec<GeneratedFile> {
    let generator = Generator::new(ir);
    let mut files = vec![
        file("commands.rs", generator.commands()),
        file("contract.rs", generator.contract()),
        file("env.rs", generator.env()),
        file("exit.rs", generator.exit()),
        file(CRATE_ROOT, generator.root()),
        file("runtime.rs", RUNTIME.to_owned()),
        file("spawn.rs", SPAWN.to_owned()),
        file("types.rs", generator.types()),
        file("wire.rs", WIRE.to_owned()),
    ];
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

fn file(path: &str, contents: String) -> GeneratedFile {
    GeneratedFile {
        path: PathBuf::from(path),
        contents,
    }
}

struct Generator<'a> {
    ir: &'a Ir,
    /// Fields whose type closes a cycle through the type that holds them, and so need a `Box`.
    boxed: BTreeSet<BoxedField>,
}

/// `(type, union variant tag or empty, wire field name)`.
type BoxedField = (String, String, String);

impl<'a> Generator<'a> {
    fn new(ir: &'a Ir) -> Self {
        Self {
            ir,
            boxed: boxed_fields(ir),
        }
    }

    fn type_def(&self, definition: &TypeDef) -> String {
        match &definition.shape {
            Shape::Struct(fields) => self.struct_type(definition, fields),
            Shape::ScalarEnum { base, entries } => scalar_enum(definition, *base, entries),
            Shape::Union(variants) => self.union_type(definition, variants),
            Shape::Alias(target) => format!(
                "{}pub type {} = {};\n",
                doc(&definition.description, 0),
                type_name(&definition.name),
                rust_type(target)
            ),
        }
    }

    fn types(&self) -> String {
        let body = self
            .ir
            .types
            .iter()
            .map(|definition| self.type_def(definition))
            .collect::<Vec<_>>()
            .join("\n");
        let mut standard = Vec::new();
        if body.contains("BTreeMap<") {
            standard.push("use std::collections::BTreeMap;".to_owned());
        }
        let mut external = Vec::new();
        if body.contains("serialize_map") {
            external.push("use serde::de::Error as _;".to_owned());
            external.push("use serde::ser::SerializeMap;".to_owned());
        }
        let serde: Vec<&str> = ["Deserialize", "Deserializer", "Serialize", "Serializer"]
            .into_iter()
            .filter(|name| match *name {
                "Deserialize" => body.contains("Deserialize"),
                "Deserializer" => body.contains("Deserializer<"),
                "Serialize" => body.contains("Serialize"),
                _ => body.contains("Serializer>"),
            })
            .collect();
        if !serde.is_empty() {
            external.push(use_statement("serde", &serde));
        }
        if self.ir.types.iter().any(|definition| mentions_value(&definition.shape)) {
            external.push("use serde_json::Value;".to_owned());
        }
        let wire: Vec<&str> = ["Unknowns", "field", "payload"]
            .into_iter()
            .filter(|name| match *name {
                "field" => body.contains("field(pointer, out"),
                "payload" => body.contains("payload::<"),
                _ => true,
            })
            .collect();
        let local = vec![use_statement("super::wire", &wire)];
        source(
            "The types of the machine documents, decoded as `ocx` prints them.",
            &[standard, external, local],
            &body,
        )
    }

    fn struct_type(&self, definition: &TypeDef, fields: &[Field]) -> String {
        let name = type_name(&definition.name);
        let mut out = doc(&definition.description, 0);
        out.push_str("#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]\n");
        let _ = writeln!(out, "pub struct {name} {{");
        for field in fields {
            out.push_str(&doc(&field.description, 4));
            out.push_str(&serde_attribute(field, 4));
            let ty = self.field_type(&definition.name, "", field);
            let _ = writeln!(out, "    pub {}: {ty},", ident(&field.name));
        }
        out.push_str("}\n\n");
        out.push_str(&unknowns_impl(&name, &struct_walk(fields)));
        out
    }

    fn union_type(&self, definition: &TypeDef, variants: &[Variant]) -> String {
        let name = type_name(&definition.name);
        let mut out = doc(&definition.description, 0);
        out.push_str("#[derive(Clone, Debug, PartialEq)]\n#[non_exhaustive]\n");
        let _ = writeln!(out, "pub enum {name} {{");
        for variant in variants {
            out.push_str(&doc(&variant.description, 4));
            let arm = variant_name(&variant.tag);
            if variant.fields.is_empty() {
                let _ = writeln!(out, "    {arm},");
                continue;
            }
            let _ = writeln!(out, "    {arm} {{");
            for field in &variant.fields {
                out.push_str(&doc(&field.description, 8));
                let ty = self.field_type(&definition.name, &variant.tag, field);
                let _ = writeln!(out, "        {}: {ty},", ident(&field.name));
            }
            out.push_str("    },\n");
        }
        out.push_str("    /// A variant this SDK was not generated for: the whole object, as sent.\n");
        out.push_str("    Unknown(Value),\n}\n\n");
        out.push_str(&self.union_serialize(&name, definition, variants));
        out.push_str(&self.union_deserialize(&name, definition, variants));
        out.push_str(&union_unknowns(&name, variants));
        out
    }

    fn union_serialize(&self, name: &str, _definition: &TypeDef, variants: &[Variant]) -> String {
        let mut out = format!("impl Serialize for {name} {{\n");
        out.push_str("    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {\n");
        out.push_str("        match self {\n");
        for variant in variants {
            let arm = variant_name(&variant.tag);
            if variant.fields.is_empty() {
                let _ = writeln!(out, "            Self::{arm} => {{");
            } else {
                let _ = writeln!(
                    out,
                    "{}",
                    braced(12, &format!("Self::{arm}"), &bindings(&variant.fields), " => {")
                );
            }
            out.push_str("                let mut object = serializer.serialize_map(None)?;\n");
            let _ = writeln!(
                out,
                "                object.serialize_entry(\"type\", {:?})?;",
                variant.tag
            );
            for (position, field) in variant.fields.iter().enumerate() {
                let wire = format!("{:?}", field.name);
                if field.required {
                    let _ = writeln!(
                        out,
                        "                object.serialize_entry({wire}, field_{position})?;"
                    );
                } else {
                    let _ = writeln!(
                        out,
                        "                if let Some(field_{position}) = field_{position} {{"
                    );
                    let _ = writeln!(
                        out,
                        "                    object.serialize_entry({wire}, field_{position})?;"
                    );
                    out.push_str("                }\n");
                }
            }
            out.push_str("                object.end()\n            }\n");
        }
        out.push_str("            Self::Unknown(value) => value.serialize(serializer),\n");
        out.push_str("        }\n    }\n}\n\n");
        out
    }

    fn union_deserialize(&self, name: &str, definition: &TypeDef, variants: &[Variant]) -> String {
        let mut out = format!("impl<'de> Deserialize<'de> for {name} {{\n");
        out.push_str("    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {\n");
        out.push_str("        let value = Value::deserialize(deserializer)?;\n");
        out.push_str("        let Some(tag) = value.get(\"type\").and_then(Value::as_str) else {\n");
        out.push_str("            return Err(D::Error::custom(\"a union object needs a string `type`\"));\n");
        out.push_str("        };\n        match tag {\n");
        for variant in variants {
            let arm = variant_name(&variant.tag);
            let tag = format!("{:?}", variant.tag);
            if variant.fields.is_empty() {
                let _ = writeln!(out, "            {tag} => Ok(Self::{arm}),");
                continue;
            }
            let _ = writeln!(out, "            {tag} => {{");
            out.push_str("                #[derive(Deserialize)]\n                struct Fields {\n");
            for field in &variant.fields {
                out.push_str(&serde_attribute(field, 20));
                let ty = self.field_type(&definition.name, &variant.tag, field);
                let _ = writeln!(out, "                    {}: {ty},", ident(&field.name));
            }
            out.push_str("                }\n");
            let _ = writeln!(
                out,
                "                let fields = payload::<Fields, D::Error>({tag}, value)?;"
            );
            let assignments: Vec<String> = variant
                .fields
                .iter()
                .map(|field| format!("{0}: fields.{0}", ident(&field.name)))
                .collect();
            let _ = writeln!(out, "{}", braced(16, &format!("Ok(Self::{arm}"), &assignments, ")"));
            out.push_str("            }\n");
        }
        out.push_str("            _ => Ok(Self::Unknown(value)),\n        }\n    }\n}\n\n");
        out
    }

    fn field_type(&self, owner: &str, variant: &str, field: &Field) -> String {
        let key = (owner.to_owned(), variant.to_owned(), field.name.clone());
        let mut ty = rust_type(&field.ty);
        if self.boxed.contains(&key) {
            ty = format!("Box<{ty}>");
        }
        if field.required { ty } else { format!("Option<{ty}>") }
    }
}

// ── types ──

fn scalar_enum(definition: &TypeDef, base: Scalar, entries: &[EnumEntry]) -> String {
    let name = type_name(&definition.name);
    let (carrier, read, write) = match base {
        Scalar::String => (
            "String",
            "String::deserialize(deserializer)?",
            "serializer.serialize_str(self.as_str())",
        ),
        Scalar::Integer => (
            "i64",
            "i64::deserialize(deserializer)?",
            "serializer.serialize_i64(self.value())",
        ),
    };
    let mut out = doc(&definition.description, 0);
    out.push_str("#[derive(Clone, Debug, PartialEq, Eq, Hash)]\n#[non_exhaustive]\n");
    let _ = writeln!(out, "pub enum {name} {{");
    for entry in entries {
        out.push_str(&doc(&entry.description, 4));
        let _ = writeln!(out, "    {},", entry.name);
    }
    out.push_str("    /// A value this SDK was not generated for, kept as sent.\n");
    let _ = writeln!(out, "    Unknown({carrier}),\n}}\n");

    let _ = writeln!(out, "impl {name} {{");
    match base {
        Scalar::String => {
            out.push_str("    /// The wire value.\n    pub fn as_str(&self) -> &str {\n        match self {\n");
            for entry in entries {
                let _ = writeln!(out, "            Self::{} => {},", entry.name, literal(&entry.value));
            }
            out.push_str("            Self::Unknown(value) => value,\n        }\n    }\n\n");
            out.push_str("    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.\n");
            out.push_str("    pub fn from_value(value: String) -> Self {\n        match value.as_str() {\n");
            for entry in entries {
                let _ = writeln!(out, "            {} => Self::{},", literal(&entry.value), entry.name);
            }
            out.push_str("            _ => Self::Unknown(value),\n        }\n    }\n}\n\n");
            let _ = writeln!(out, "impl std::fmt::Display for {name} {{");
            out.push_str("    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n");
            out.push_str("        formatter.write_str(self.as_str())\n    }\n}\n\n");
        }
        Scalar::Integer => {
            out.push_str("    /// The wire value.\n    pub fn value(&self) -> i64 {\n        match self {\n");
            for entry in entries {
                let _ = writeln!(out, "            Self::{} => {},", entry.name, literal(&entry.value));
            }
            out.push_str("            Self::Unknown(value) => *value,\n        }\n    }\n\n");
            out.push_str("    /// The entry for a wire value; one the SDK does not know becomes `Unknown`.\n");
            out.push_str("    pub fn from_value(value: i64) -> Self {\n        match value {\n");
            for entry in entries {
                let _ = writeln!(out, "            {} => Self::{},", literal(&entry.value), entry.name);
            }
            out.push_str("            _ => Self::Unknown(value),\n        }\n    }\n}\n\n");
        }
    }
    let _ = writeln!(out, "impl Serialize for {name} {{");
    out.push_str("    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {\n");
    let _ = writeln!(out, "        {write}\n    }}\n}}\n");
    let _ = writeln!(out, "impl<'de> Deserialize<'de> for {name} {{");
    out.push_str("    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {\n");
    let _ = writeln!(out, "        Ok(Self::from_value({read}))\n    }}\n}}\n");
    out.push_str(&unknowns_impl(
        &name,
        "        if matches!(self, Self::Unknown(_)) {\n            out.push(pointer.clone());\n        }\n",
    ));
    out
}

/// The wire value of an enum entry as a Rust literal.
fn literal(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => format!("{text:?}"),
        other => other.to_string(),
    }
}

fn struct_walk(fields: &[Field]) -> String {
    fields
        .iter()
        .map(|field| {
            call_statement(
                8,
                "field",
                &[
                    "pointer",
                    "out",
                    &format!("{:?}", field.name),
                    &format!("&self.{}", ident(&field.name)),
                ],
            )
        })
        .collect()
}

fn unknowns_impl(name: &str, body: &str) -> String {
    let (pointer, out) = if body.is_empty() {
        ("_pointer", "_out")
    } else {
        ("pointer", "out")
    };
    let head = format!("fn collect_unknowns(&self, {pointer}: &mut String, {out}: &mut Vec<String>)");
    if body.is_empty() {
        return format!("impl Unknowns for {name} {{\n    {head} {{}}\n}}\n");
    }
    format!("impl Unknowns for {name} {{\n    {head} {{\n{body}    }}\n}}\n")
}

fn union_unknowns(name: &str, variants: &[Variant]) -> String {
    let mut body = String::from("        match self {\n");
    for variant in variants {
        let arm = variant_name(&variant.tag);
        if variant.fields.is_empty() {
            let _ = writeln!(body, "            Self::{arm} => {{}}");
            continue;
        }
        let _ = writeln!(
            body,
            "{}",
            braced(12, &format!("Self::{arm}"), &bindings(&variant.fields), " => {")
        );
        for (position, field) in variant.fields.iter().enumerate() {
            body.push_str(&call_statement(
                16,
                "field",
                &[
                    "pointer",
                    "out",
                    &format!("{:?}", field.name),
                    &format!("field_{position}"),
                ],
            ));
        }
        body.push_str("            }\n");
    }
    body.push_str("            Self::Unknown(_) => out.push(pointer.clone()),\n        }\n");
    format!("{}\n", unknowns_impl(name, &body).trim_end())
}

/// `rule: field_0, companion: field_1`: every field of a variant bound to a name nothing else can take.
fn bindings(fields: &[Field]) -> Vec<String> {
    fields
        .iter()
        .enumerate()
        .map(|(position, field)| format!("{}: field_{position}", ident(&field.name)))
        .collect()
}

/// The widest struct body, joined on a line, rustfmt keeps on the line of its braces.
const STRUCT_WIDTH: usize = 22;

/// `head { a, b }tail` at `indent`, or one item per line when the items are wider than rustfmt keeps on a line.
fn braced(indent: usize, head: &str, items: &[String], tail: &str) -> String {
    let pad = " ".repeat(indent);
    let joined = items.join(", ");
    if joined.len() <= STRUCT_WIDTH {
        return format!("{pad}{head} {{ {joined} }}{tail}");
    }
    let mut out = format!("{pad}{head} {{\n");
    for item in items {
        let _ = writeln!(out, "{pad}    {item},");
    }
    let _ = write!(out, "{pad}}}{tail}");
    out
}

/// `name(arguments);` at `indent`, one argument per line when they are wider than rustfmt keeps on a line.
fn call_statement(indent: usize, name: &str, arguments: &[&str]) -> String {
    let pad = " ".repeat(indent);
    let joined = arguments.join(", ");
    if joined.len() <= FN_PARAMS_WIDTH {
        return format!("{pad}{name}({joined});\n");
    }
    let mut out = format!("{pad}{name}(\n");
    for argument in arguments {
        let _ = writeln!(out, "{pad}    {argument},");
    }
    let _ = writeln!(out, "{pad});");
    out
}

fn serde_attribute(field: &Field, indent: usize) -> String {
    let mut parts = Vec::new();
    let name = ident(&field.name);
    if name.trim_start_matches("r#") != field.name {
        parts.push(format!("rename = {:?}", field.name));
    }
    if !field.required {
        parts.push("default".to_owned());
        parts.push("skip_serializing_if = \"Option::is_none\"".to_owned());
    }
    if parts.is_empty() {
        return String::new();
    }
    format!("{}#[serde({})]\n", " ".repeat(indent), parts.join(", "))
}

fn rust_type(reference: &TypeRef) -> String {
    match reference {
        TypeRef::Named(name) => type_name(name),
        TypeRef::String => "String".to_owned(),
        TypeRef::Integer => "i64".to_owned(),
        TypeRef::Number => "f64".to_owned(),
        TypeRef::Boolean => "bool".to_owned(),
        TypeRef::Array(item) => format!("Vec<{}>", rust_type(item)),
        TypeRef::Map(item) => format!("BTreeMap<String, {}>", rust_type(item)),
        TypeRef::Opaque => "Value".to_owned(),
    }
}

fn type_name(name: &str) -> String {
    let clean: String = name.chars().filter(char::is_ascii_alphanumeric).collect();
    if RESERVED_TYPES.contains(&clean.as_str()) {
        format!("{clean}Type")
    } else {
        clean
    }
}

/// The Rust identifier of a wire name.
fn ident(wire: &str) -> String {
    let mut name: String = wire
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if name.starts_with(|character: char| character.is_ascii_digit()) {
        name.insert(0, '_');
    }
    if NO_RAW.contains(&name.as_str()) {
        name.push('_');
    } else if KEYWORDS.contains(&name.as_str()) {
        name.insert_str(0, "r#");
    }
    name
}

/// Fields that must be boxed because their type reaches back to the type that holds them.
fn boxed_fields(ir: &Ir) -> BTreeSet<BoxedField> {
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut inline: Vec<(BoxedField, &str)> = Vec::new();
    for definition in &ir.types {
        let owner = definition.name.as_str();
        let held: Vec<(&str, &Field)> = match &definition.shape {
            Shape::Struct(fields) => fields.iter().map(|field| ("", field)).collect(),
            Shape::Union(variants) => variants
                .iter()
                .flat_map(|variant| variant.fields.iter().map(|field| (variant.tag.as_str(), field)))
                .collect(),
            Shape::Alias(TypeRef::Named(target)) => {
                edges.entry(owner).or_default().push(target);
                Vec::new()
            }
            Shape::Alias(_) | Shape::ScalarEnum { .. } => Vec::new(),
        };
        for (variant, field) in held {
            if let TypeRef::Named(target) = &field.ty {
                edges.entry(owner).or_default().push(target);
                inline.push(((owner.to_owned(), variant.to_owned(), field.name.clone()), target));
            }
        }
    }
    inline
        .into_iter()
        .filter(|(key, target)| reaches(&edges, target, &key.0))
        .map(|(key, _)| key)
        .collect()
}

/// Whether `to` can be reached from `from` through inline containment.
fn reaches(edges: &BTreeMap<&str, Vec<&str>>, from: &str, to: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(current) = stack.pop() {
        if current == to {
            return true;
        }
        if seen.insert(current) {
            stack.extend(edges.get(current).into_iter().flatten().copied());
        }
    }
    false
}

// ── docs ──

/// The most doc lines an item gets; longer descriptions keep their leading sentences.
const DOC_LINES: usize = 4;

/// The first paragraph of `text` as `///` lines at `indent`, wrapped, escaped for rustdoc and capped.
fn doc(text: &str, indent: usize) -> String {
    let paragraph = text
        .split("\n\n")
        .map(str::trim)
        .find(|paragraph| !paragraph.is_empty())
        .unwrap_or_default();
    let flat = paragraph
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("```", "`")
        .replace('[', "\\[")
        .replace(']', "\\]");
    if flat.is_empty() {
        return String::new();
    }
    let width = 100usize.saturating_sub(indent + 4).max(40);
    let lines = wrap(&flat, width);
    let lines = if lines.len() > DOC_LINES {
        wrap(&leading_sentences(&flat, width * DOC_LINES), width)
    } else {
        lines
    };
    let pad = " ".repeat(indent);
    lines.into_iter().map(|line| format!("{pad}/// {line}\n")).collect()
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split(' ') {
        match lines.last_mut() {
            Some(line) if line.len() + 1 + word.len() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines
}

/// The sentences of `text` that fit `budget` characters; the first one cut at a word when it alone does not fit.
fn leading_sentences(text: &str, budget: usize) -> String {
    let mut kept = String::new();
    for sentence in text.split_inclusive(". ") {
        if kept.len() + sentence.trim_end().len() > budget {
            break;
        }
        kept.push_str(sentence);
    }
    if kept.is_empty() {
        let cut = text[..budget.min(text.len())]
            .rfind(' ')
            .unwrap_or(budget.min(text.len()));
        return format!("{}...", &text[..cut]);
    }
    kept.trim_end().to_owned()
}

/// A whole source file: license header, module doc, imports in blank-line separated groups, then the body.
fn source(module_doc: &str, uses: &[Vec<String>], body: &str) -> String {
    let mut out = String::from(LICENSE_HEADER);
    let _ = writeln!(out, "//! {module_doc}\n");
    for group in uses.iter().filter(|group| !group.is_empty()) {
        out.push_str(&group.join("\n"));
        out.push_str("\n\n");
    }
    out.push_str(body.trim_end());
    out.push('\n');
    out
}

/// Whether a type uses `serde_json::Value`: an opaque payload, or the `Unknown` arm of a union.
fn mentions_value(shape: &Shape) -> bool {
    fn opaque(reference: &TypeRef) -> bool {
        match reference {
            TypeRef::Opaque => true,
            TypeRef::Array(item) | TypeRef::Map(item) => opaque(item),
            _ => false,
        }
    }
    match shape {
        Shape::Struct(fields) => fields.iter().any(|field| opaque(&field.ty)),
        Shape::Union(_) => true,
        Shape::Alias(target) => opaque(target),
        Shape::ScalarEnum { .. } => false,
    }
}

/// `use path::{a, b};`, filled to the line width the way rustfmt fills it.
fn use_statement(path: &str, names: &[&str]) -> String {
    let mut names = names.to_vec();
    names.sort_unstable();
    if let [only] = names.as_slice() {
        return format!("use {path}::{only};");
    }
    let single = format!("use {path}::{{{}}};", names.join(", "));
    if single.len() <= LINE_WIDTH {
        return single;
    }
    let mut out = format!("use {path}::{{\n");
    let mut line = String::from("   ");
    for name in names {
        if line.len() + 1 + name.len() + 1 > LINE_WIDTH {
            out.push_str(&line);
            out.push('\n');
            line = String::from("   ");
        }
        line.push(' ');
        line.push_str(name);
        line.push(',');
    }
    out.push_str(&line);
    out.push_str("\n};");
    out
}

impl Generator<'_> {
    fn root(&self) -> String {
        let body = "pub mod commands;\npub mod contract;\npub mod env;\nmod exit;\npub mod runtime;\npub mod spawn;\npub mod types;\npub mod wire;\n\npub use commands::*;\npub use runtime::{CancelToken, Error, Handshake, Limits, Ocx, Outcome, Raw};\npub use wire::{Secret, Unknowns};\n";
        source(
            "A client for the `ocx` machine interface: one function per command, its reports decoded into types, and the\n//! exit codes and error details as `ocx` registers them. Generated by `ocx-sdkgen`; do not edit.",
            &[],
            body,
        )
    }

    fn env(&self) -> String {
        let mut body = String::from(
            "/// One variable `ocx` documents as public.\n#[derive(Clone, Copy, Debug, PartialEq, Eq)]\npub struct EnvVar {\n    pub name: &'static str,\n    /// The kind of value it takes, as `cli.json` spells it.\n    pub value: &'static str,\n    pub summary: &'static str,\n    /// Its value stays out of `Debug` and logs.\n    pub secret: bool,\n}\n\n",
        );
        body.push_str("/// Every public variable, sorted by name. A child never inherits one: the SDK sets what the caller asks for.\n");
        body.push_str("pub const PUBLIC: &[EnvVar] = &[\n");
        for var in &self.ir.env {
            body.push_str(&env_entry(var));
        }
        body.push_str("];\n\n");
        body.push_str(IS_SECRET);
        source("The public environment manifest.", &[], &body)
    }

    fn exit(&self) -> String {
        let ir = self.ir;
        let mut uses = vec!["ErrorCategory", "ErrorDetail", "ExitCode"];
        let mut body = String::new();
        let success = ir.exit_codes.iter().find(|entry| entry.value == 0);
        body.push_str("impl ExitCode {\n    /// Whether the process succeeded. Any other code, one the SDK does not know included, is a failure.\n");
        body.push_str("    pub fn is_success(&self) -> bool {\n");
        match success {
            Some(entry) => {
                let _ = writeln!(body, "        matches!(self, Self::{})", entry.name);
            }
            None => body.push_str("        false\n"),
        }
        body.push_str("    }\n\n    /// The category of failure the code stands for.\n");
        body.push_str("    pub fn category(&self) -> Option<ErrorCategory> {\n        match self {\n");
        for entry in &ir.exit_codes {
            if let Some(category) = &entry.category {
                let _ = writeln!(
                    body,
                    "            Self::{} => Some({}),",
                    entry.name,
                    self.category_expr(category)
                );
            }
        }
        body.push_str("            _ => None,\n        }\n    }\n}\n\n");
        body.push_str("impl ErrorDetail {\n    /// The exit code a failure with this detail ships under.\n");
        body.push_str("    pub fn exit_code(&self) -> Option<ExitCode> {\n        match self {\n");
        for entry in &ir.details {
            if let Some(code) = entry.exit_code {
                let _ = writeln!(
                    body,
                    "            Self::{} => Some({}),",
                    entry.name,
                    self.exit_expr(code)
                );
            }
        }
        body.push_str("            _ => None,\n        }\n    }\n}\n");
        for (status, successes) in SUCCESS_STATUSES {
            let Some(definition) = ir.types.iter().find(|definition| definition.name == *status) else {
                continue;
            };
            let Shape::ScalarEnum { entries, .. } = &definition.shape else {
                continue;
            };
            let arms: Vec<String> = successes
                .iter()
                .filter_map(|value| entries.iter().find(|entry| entry.value == *value))
                .map(|entry| format!("Self::{}", entry.name))
                .collect();
            if arms.is_empty() {
                continue;
            }
            uses.push(status);
            let _ = write!(
                body,
                "\nimpl {status} {{\n    /// Whether the status is one the SDK vouches for as success. Any other, one it does not know included, is not.\n    pub fn is_success(&self) -> bool {{\n        matches!(self, {})\n    }}\n}}\n",
                arms.join(" | ")
            );
        }
        source(
            "Exit codes, error categories and details, and the success checks that fail closed.",
            &[vec![use_statement("super::types", &uses)]],
            &body,
        )
    }

    fn category_expr(&self, category: &str) -> String {
        match self.ir.categories.iter().find(|entry| entry.value == category) {
            Some(entry) => format!("ErrorCategory::{}", entry.name),
            None => format!("ErrorCategory::from_value({category:?}.to_owned())"),
        }
    }

    fn exit_expr(&self, code: i64) -> String {
        match self.ir.exit_codes.iter().find(|entry| entry.value == code) {
            Some(entry) => format!("ExitCode::{}", entry.name),
            None => format!("ExitCode::from_value({code})"),
        }
    }

    fn contract(&self) -> String {
        let ir = self.ir;
        let mut body = String::new();
        let _ = write!(
            body,
            "/// The oldest `ocx` release known to publish the contract.\npub const MINIMUM_OCX: &str = {MINIMUM_OCX:?};\n\n/// The `schema_version` of the error document.\npub const ERRORS: u32 = {};\n\n",
            ir.versions.errors
        );
        body.push_str("/// Each command's contract version, keyed by its words below `ocx`, sorted.\n");
        body.push_str("pub const COMMANDS: &[(&str, u32)] = &[\n");
        for command in &ir.commands {
            let _ = writeln!(body, "    ({:?}, {}),", command.path.join(" "), command.version);
        }
        body.push_str(
            "];\n\n/// Each report root's `schema_version`, keyed by the root name the command grammar uses, sorted.\n",
        );
        body.push_str("pub const REPORTS: &[(&str, u32)] = &[\n");
        for root in ir.roots.iter().filter(|root| root.name != ERROR_ROOT) {
            let _ = writeln!(body, "    ({:?}, {}),", root.name, root.version);
        }
        body.push_str("];\n\n");
        body.push_str(
            "/// The version of command `command`; 0 for one this SDK does not know, which no binary reports.\n",
        );
        body.push_str("pub fn command_version(command: &str) -> u32 {\n    lookup(COMMANDS, command)\n}\n\n");
        body.push_str("/// The `schema_version` of report root `root`; 0 for one this SDK does not know.\n");
        body.push_str("pub fn report_version(root: &str) -> u32 {\n    lookup(REPORTS, root)\n}\n\n");
        body.push_str("fn lookup(table: &[(&str, u32)], key: &str) -> u32 {\n    table\n        .iter()\n        .find(|(name, _)| *name == key)\n        .map_or(0, |(_, version)| *version)\n}\n\n");
        body.push_str("/// A decoded report root, for callers that pick the root by name at run time.\npub trait Decoded {\n    /// The report as JSON again: every unset optional field absent, every enum value as sent.\n    fn to_value(&self) -> Result<Value, serde_json::Error>;\n\n    /// The JSON pointers of every value that landed in an `Unknown` arm.\n    fn unknowns(&self) -> Vec<String>;\n}\n\n");
        body.push_str("impl<T: Serialize + Unknowns> Decoded for T {\n    fn to_value(&self) -> Result<Value, serde_json::Error> {\n        serde_json::to_value(self)\n    }\n\n    fn unknowns(&self) -> Vec<String> {\n        self.unknown_pointers()\n    }\n}\n\n");
        body.push_str("/// Decodes `document` as the report root `root`, after its `schema_version` matches; `None` for a root the\n/// contract does not publish.\n///\n/// # Errors\n///\n/// [`Error::ContractMismatch`] on another `schema_version`, [`Error::Decode`] when the document is not that root.\n");
        body.push_str("pub fn decode_root(root: &str, document: &Value) -> Option<Result<Box<dyn Decoded>, Error>> {\n    Some(match root {\n");
        let mut names: Vec<&str> = Vec::new();
        for root in &ir.roots {
            let payload = type_name(&root.payload);
            let _ = writeln!(body, "        {:?} => decode::<{payload}>(root, document),", root.name);
            names.push(root.payload.as_str());
        }
        body.push_str("        _ => return None,\n    })\n}\n\n");
        body.push_str("fn decode<T>(root: &str, document: &Value) -> Result<Box<dyn Decoded>, Error>\nwhere\n    T: DeserializeOwned + Serialize + Unknowns + 'static,\n{\n");
        let _ = write!(
            body,
            "    let expected = if root == {ERROR_ROOT:?} {{\n        ERRORS\n    }} else {{\n        report_version(root)\n    }};\n    check_version(&format!(\"report `{{root}}`\"), expected, document)?;\n    let report = T::deserialize(document).map_err(Error::Decode)?;\n    Ok(Box::new(report))\n}}\n"
        );
        let payloads: Vec<String> = names.iter().map(|name| type_name(name)).collect();
        let payloads: Vec<&str> = payloads.iter().map(String::as_str).collect();
        let external = vec![
            "use serde::Serialize;".to_owned(),
            "use serde::de::DeserializeOwned;".to_owned(),
            "use serde_json::Value;".to_owned(),
        ];
        let local = vec![
            use_statement("super::runtime", &["Error", "check_version"]),
            use_statement("super::types", &payloads),
            use_statement("super::wire", &["Unknowns"]),
        ];
        source(
            "The contract versions this SDK was generated for, and decoding by root name.",
            &[external, local],
            &body,
        )
    }
}

/// The root the error document is published under; its version is the error document's, not a report's.
const ERROR_ROOT: &str = "ErrorDocument";

fn env_entry(var: &EnvVar) -> String {
    let summary = var.summary.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "    EnvVar {{\n        name: {:?},\n        value: {:?},\n        summary: {summary:?},\n        secret: {},\n    }},\n",
        var.name, var.value, var.secret
    )
}

// ── commands ──

/// What a command function returns, decided by the output modes its grammar declares.
enum Mode {
    /// One report, exit 0.
    Report(String),
    /// The same report on success and on a non-zero exit.
    Outcome(String),
    /// Nothing on stdout.
    Empty,
    /// Anything else: the child's own output, or one of several documents; the caller decodes.
    Raw(Vec<String>),
}

fn classify(outputs: &[Output]) -> Mode {
    let roots: BTreeSet<&str> = outputs
        .iter()
        .filter_map(|output| match output {
            Output::Report(root) | Output::ReportThenFail(root) => Some(root.as_str()),
            _ => None,
        })
        .collect();
    let has_report = outputs.iter().any(|output| matches!(output, Output::Report(_)));
    let has_failing = outputs.iter().any(|output| matches!(output, Output::ReportThenFail(_)));
    let only_reports = outputs
        .iter()
        .all(|output| matches!(output, Output::Report(_) | Output::ReportThenFail(_)));
    match (roots.len(), only_reports, has_report, has_failing) {
        (1, true, true, false) => Mode::Report(roots.into_iter().next().unwrap_or_default().to_owned()),
        (1, true, true, true) if outputs.len() == 2 => {
            Mode::Outcome(roots.into_iter().next().unwrap_or_default().to_owned())
        }
        (0, false, _, _) if matches!(outputs, [Output::Empty]) => Mode::Empty,
        _ => Mode::Raw(roots.into_iter().map(str::to_owned).collect()),
    }
}

impl Generator<'_> {
    fn commands(&self) -> String {
        let globals: Vec<&Arg> = self
            .ir
            .globals
            .iter()
            .filter(|arg| !OWNED_GLOBALS.contains(&arg.id.as_str()))
            .collect();
        let mut body = options_struct(
            "GlobalOptions",
            "The options `ocx` takes before its subcommand. The SDK sets `--format` itself and never passes `--quiet`.",
            &globals,
        );
        for command in &self.ir.commands {
            if !command.args.is_empty() {
                body.push('\n');
                let args: Vec<&Arg> = command.args.iter().collect();
                body.push_str(&options_struct(
                    &args_name(command),
                    &format!("The arguments of `ocx {}`.", command.path.join(" ")),
                    &args,
                ));
            }
        }
        body.push_str("\nimpl Ocx {\n");
        let functions: Vec<String> = self
            .ir
            .commands
            .iter()
            .map(|command| self.command_function(command))
            .collect();
        body.push_str(&functions.join("\n"));
        body.push_str("}\n");

        let mut standard = Vec::new();
        if body.contains("PathBuf") {
            standard.push("use std::path::PathBuf;".to_owned());
        }
        let mut local = Vec::new();
        let runtime: Vec<&str> = ["Error", "Ocx", "Outcome", "Raw"]
            .into_iter()
            .filter(|name| match *name {
                "Outcome" => body.contains("Outcome<"),
                "Raw" => body.contains("Result<Raw"),
                _ => true,
            })
            .collect();
        local.push(use_statement("super::runtime", &runtime));
        let payloads: BTreeSet<String> = self
            .ir
            .commands
            .iter()
            .flat_map(|command| self.payloads_of(command))
            .collect();
        if !payloads.is_empty() {
            let payloads: Vec<&str> = payloads.iter().map(String::as_str).collect();
            local.push(use_statement("super::types", &payloads));
        }
        let wire: Vec<&str> = ["Argv", "Hyphen", "Presence", "Secret"]
            .into_iter()
            .filter(|name| match *name {
                "Hyphen" => body.contains("Hyphen::"),
                "Secret" => body.contains("Secret>"),
                _ => true,
            })
            .collect();
        local.push(use_statement("super::wire", &wire));
        source(
            "One function per command, and the arguments each takes.",
            &[standard, local],
            &body,
        )
    }

    fn payloads_of(&self, command: &Command) -> Vec<String> {
        match classify(&command.outputs) {
            Mode::Report(root) | Mode::Outcome(root) => vec![self.payload(&root)],
            Mode::Empty | Mode::Raw(_) => Vec::new(),
        }
    }

    fn payload(&self, root: &str) -> String {
        let payload = self
            .ir
            .roots
            .iter()
            .find(|candidate| candidate.name == root)
            .map_or(root, |found| found.payload.as_str());
        type_name(payload)
    }

    fn command_function(&self, command: &Command) -> String {
        let words = command
            .path
            .iter()
            .map(|word| format!("{word:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let name = function_name(command);
        let path = command.path.join(" ");
        let mut params = vec!["&self".to_owned()];
        if !command.args.is_empty() {
            params.push(format!("args: &{}", args_name(command)));
        }
        let mode = classify(&command.outputs);
        let (returns, call) = match &mode {
            Mode::Report(root) => (
                self.payload(root),
                call_expression("call", &[format!("{path:?}"), format!("{root:?}"), "argv".to_owned()]),
            ),
            Mode::Outcome(root) => (
                format!("Outcome<{}>", self.payload(root)),
                call_expression(
                    "call_outcome",
                    &[format!("{path:?}"), format!("{root:?}"), "argv".to_owned()],
                ),
            ),
            Mode::Empty => (
                "()".to_owned(),
                call_expression("call_empty", &[format!("{path:?}"), "argv".to_owned()]),
            ),
            Mode::Raw(roots) => {
                let list = roots
                    .iter()
                    .map(|root| format!("{root:?}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    "Raw".to_owned(),
                    call_expression(
                        "call_raw",
                        &[format!("{path:?}"), format!("&[{list}]"), "argv".to_owned()],
                    ),
                )
            }
        };
        let mut out = doc(&command.summary, 4);
        out.push_str(&format!("    ///\n    /// Runs `ocx {path}`.\n"));
        out.push_str(&signature(&name, &params, &format!("Result<{returns}, Error>")));
        let mutable = if command.args.is_empty() { "" } else { "mut " };
        out.push_str(&format!("        let {mutable}argv = self.argv(&[{words}]);\n"));
        if !command.args.is_empty() {
            out.push_str("        args.push(&mut argv);\n");
        }
        out.push_str(&format!("        {}\n    }}\n", call.trim_start()));
        out
    }
}

fn args_name(command: &Command) -> String {
    format!(
        "{}Args",
        command
            .path
            .iter()
            .map(|word| crate::ir::pascal(word))
            .collect::<String>()
    )
}

fn function_name(command: &Command) -> String {
    let name = ident(&command.path.join("_"));
    if OCX_METHODS.contains(&name.as_str()) {
        format!("{name}_command")
    } else {
        name
    }
}

/// A `pub fn` header, on one line when it fits and one parameter per line when it does not, as rustfmt breaks it.
fn signature(name: &str, params: &[String], returns: &str) -> String {
    let single = format!("    pub fn {name}({}) -> {returns} {{", params.join(", "));
    if single.len() <= LINE_WIDTH && params.join(", ").len() <= FN_PARAMS_WIDTH {
        return format!("{single}\n");
    }
    let mut out = format!("    pub fn {name}(\n");
    for param in params {
        out.push_str(&format!("        {param},\n"));
    }
    out.push_str(&format!("    ) -> {returns} {{\n"));
    out
}

/// The widest the arguments of one call may be, joined on a line, before rustfmt puts one on each line.
const FN_PARAMS_WIDTH: usize = 72;

/// `self.<method>(<arguments>)` as a statement-tail expression at the function body's indent.
fn call_expression(method: &str, arguments: &[String]) -> String {
    let joined = arguments.join(", ");
    if joined.len() <= FN_PARAMS_WIDTH && 8 + method.len() + joined.len() + 7 <= LINE_WIDTH {
        return format!("self.{method}({joined})");
    }
    let mut out = format!("self.{method}(\n");
    for argument in arguments {
        out.push_str(&format!("            {argument},\n"));
    }
    out.push_str("        )");
    out
}

/// An arguments struct, with the `push` that appends them to an argv.
fn options_struct(name: &str, summary: &str, args: &[&Arg]) -> String {
    let mut out = doc(summary, 0);
    out.push_str("#[derive(Clone, Debug, Default, PartialEq)]\n");
    out.push_str(&format!("pub struct {name} {{\n"));
    let mut pushes = String::new();
    let mut after_terminated = false;
    for arg in args {
        let (ty, push) = bind(arg);
        // Clap reads a `--`-terminated positional up to the `--`; without it the next positional is swallowed too.
        if arg.long.is_none() && !arg.after_terminator {
            if after_terminated {
                pushes.push_str("        argv.terminator();\n");
            }
            after_terminated = arg.terminated;
        }
        out.push_str(&doc(&arg.help, 4));
        out.push_str(&format!("    pub {}: {ty},\n", ident(&arg.id)));
        pushes.push_str(&push);
    }
    out.push_str("}\n\n");
    out.push_str(&format!(
        "impl {name} {{\n    /// Appends the arguments to `argv`.\n    pub fn push(&self, argv: &mut Argv) {{\n{pushes}    }}\n}}\n"
    ));
    out
}

/// The field type of an argument and the statement that pushes it.
fn bind(arg: &Arg) -> (String, String) {
    let field = format!("self.{}", ident(&arg.id));
    let presence = if arg.required {
        "Presence::Required"
    } else {
        "Presence::Optional"
    };
    let base = match &arg.value {
        ValueKind::Switch => "bool",
        ValueKind::Path => "PathBuf",
        ValueKind::Integer => "i64",
        ValueKind::String | ValueKind::Identifier | ValueKind::Platform | ValueKind::Choice(_) => "String",
    };
    let integer = arg.value == ValueKind::Integer;
    let (ty, values) = if arg.multiple {
        let values = if integer {
            format!("{field}.iter().map(ToString::to_string)")
        } else {
            format!("&{field}")
        };
        (format!("Vec<{base}>"), values)
    } else if arg.required {
        let values = if integer {
            format!("[{field}.to_string()]")
        } else {
            format!("[&{field}]")
        };
        (base.to_owned(), values)
    } else {
        let values = if integer {
            format!("{field}.map(|number| number.to_string())")
        } else {
            format!("&{field}")
        };
        (format!("Option<{base}>"), values)
    };
    let statement = match (&arg.value, &arg.long) {
        (ValueKind::Switch, Some(long)) if arg.stdin_secret => {
            return (
                "Option<Secret>".to_owned(),
                push_statement("secret", &[format!("{long:?}"), format!("{field}.as_ref()")]),
            );
        }
        (ValueKind::Switch, Some(long)) => {
            return (base.to_owned(), push_statement("switch", &[format!("{long:?}"), field]));
        }
        (_, Some(long)) => {
            let choices = match &arg.value {
                ValueKind::Choice(choices) => choice_array(choices),
                _ => "&[]".to_owned(),
            };
            push_statement("flag", &[format!("{long:?}"), values, presence.to_owned(), choices])
        }
        (_, None) if arg.after_terminator => {
            push_statement("operand", &[format!("{:?}", arg.id), values, presence.to_owned()])
        }
        (_, None) => {
            let hyphen = if arg.allow_hyphen_values {
                "Hyphen::Allow"
            } else {
                "Hyphen::Refuse"
            };
            push_statement(
                "positional",
                &[format!("{:?}", arg.id), values, presence.to_owned(), hyphen.to_owned()],
            )
        }
    };
    (ty, statement)
}

/// A choice list as an array expression, one choice per line when it is wider than rustfmt keeps on a line.
fn choice_array(choices: &[String]) -> String {
    let quoted: Vec<String> = choices.iter().map(|choice| format!("{choice:?}")).collect();
    let joined = quoted.join(", ");
    if joined.len() <= FN_PARAMS_WIDTH {
        return format!("&[{joined}]");
    }
    let mut out = String::from("&[\n");
    for choice in &quoted {
        let _ = writeln!(out, "                {choice},");
    }
    out.push_str("            ]");
    out
}

fn push_statement(method: &str, arguments: &[String]) -> String {
    let joined = arguments.join(", ");
    if joined.len() <= FN_PARAMS_WIDTH && 8 + "argv.".len() + method.len() + joined.len() + 3 <= LINE_WIDTH {
        return format!("        argv.{method}({joined});\n");
    }
    let mut out = format!("        argv.{method}(\n");
    for argument in arguments {
        out.push_str(&format!("            {argument},\n"));
    }
    out.push_str("        );\n");
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use super::*;
    use crate::ir;
    use crate::versions::Versions;

    fn contract() -> Ir {
        ir::load(&ir::tests::documents()).expect("the goldens load")
    }

    fn golden_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/rust")
    }

    fn empty_ir(types: Vec<TypeDef>) -> Ir {
        Ir {
            types,
            roots: Vec::new(),
            globals: Vec::new(),
            commands: Vec::new(),
            exit_codes: Vec::new(),
            categories: Vec::new(),
            details: Vec::new(),
            env: Vec::new(),
            versions: Versions::default(),
        }
    }

    fn field(name: &str, ty: TypeRef) -> Field {
        Field {
            name: name.to_owned(),
            description: String::new(),
            ty,
            required: true,
        }
    }

    fn definition(name: &str, shape: Shape) -> TypeDef {
        TypeDef {
            name: name.to_owned(),
            description: String::new(),
            shape,
        }
    }

    #[test]
    fn two_runs_are_byte_identical() {
        let ir = contract();
        assert_eq!(generate(&ir), generate(&ir));
    }

    #[test]
    fn files_come_out_sorted_and_the_crate_root_is_among_them() {
        let paths: Vec<_> = generate(&contract()).into_iter().map(|file| file.path).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted);
        assert!(paths.contains(&PathBuf::from(CRATE_ROOT)));
    }

    #[test]
    fn the_committed_golden_is_what_the_generator_emits() {
        let generated = generate(&contract());
        let mut committed: BTreeSet<String> = std::fs::read_dir(golden_dir())
            .expect("the golden directory exists")
            .map(|entry| {
                entry
                    .expect("a readable entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert!(!committed.is_empty(), "no committed golden files were read");
        for file in &generated {
            let name = file.path.to_string_lossy().into_owned();
            let on_disk = std::fs::read_to_string(golden_dir().join(&file.path))
                .unwrap_or_else(|error| panic!("{name}: {error}; regenerate with `ocx-sdkgen --lang rust`"));
            assert!(
                on_disk == file.contents,
                "{name} differs from the generator's output; regenerate the golden"
            );
            committed.remove(&name);
        }
        assert!(
            committed.is_empty(),
            "committed files the generator no longer emits: {committed:?}"
        );
    }

    #[test]
    fn every_file_carries_the_license_header_and_stays_inside_the_line_width() {
        for file in generate(&contract()) {
            let name = file.path.display().to_string();
            assert!(
                file.contents.starts_with(LICENSE_HEADER.trim_end()),
                "{name}: no license header"
            );
            for (number, line) in file.contents.lines().enumerate() {
                let width = line.chars().count();
                // rustfmt cannot break a string literal, so a long summary is the one line it leaves wide.
                let unbreakable = line.trim_start().starts_with("summary: \"");
                assert!(
                    width <= LINE_WIDTH || unbreakable,
                    "{name}:{}: {width} columns",
                    number + 1
                );
            }
        }
    }

    fn positional(id: &str, position: u32, terminated: bool, after_terminator: bool) -> Arg {
        Arg {
            id: id.to_owned(),
            long: None,
            position: Some(position),
            multiple: true,
            value: ValueKind::String,
            required: true,
            after_terminator,
            terminated,
            allow_hyphen_values: false,
            stdin_secret: false,
            help: String::new(),
        }
    }

    #[test]
    fn every_positional_after_a_terminated_one_is_preceded_by_the_terminator() {
        let (first, second, third) = (
            positional("first", 1, true, false),
            positional("second", 2, true, false),
            positional("third", 3, false, false),
        );
        let emitted = options_struct("XArgs", "", &[&first, &second, &third]);
        let calls: Vec<_> = emitted
            .lines()
            .filter_map(|line| line.trim().strip_prefix("argv."))
            .map(|call| call.split('(').next().unwrap_or_default())
            .collect();
        assert_eq!(
            calls,
            ["positional", "terminator", "positional", "terminator", "positional"]
        );
        let tail_operand = positional("rest", 2, false, true);
        let emitted = options_struct("YArgs", "", &[&first, &tail_operand]);
        assert!(!emitted.contains("terminator"), "{emitted}");
    }

    #[test]
    fn generated_files_name_their_siblings_through_super_only() {
        for file in generate(&contract()) {
            let code: String = file
                .contents
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !code.contains("crate::"),
                "{}: a path that only works as the crate root",
                file.path.display()
            );
        }
    }

    #[test]
    fn the_generated_code_depends_on_serde_serde_json_and_std() {
        for file in generate(&contract()) {
            for line in file.contents.lines().filter(|line| line.starts_with("use ")) {
                let root = line.trim_start_matches("use ").split("::").next().unwrap_or_default();
                assert!(
                    ["serde", "serde_json", "std", "super"].contains(&root),
                    "{}: unexpected dependency in `{line}`",
                    file.path.display()
                );
            }
        }
    }

    #[test]
    fn every_success_status_is_a_type_with_those_values_known() {
        let ir = contract();
        for (status, successes) in SUCCESS_STATUSES {
            let definition = ir
                .types
                .iter()
                .find(|definition| definition.name == *status)
                .unwrap_or_else(|| panic!("{status} is not a type of the contract"));
            let Shape::ScalarEnum { entries, .. } = &definition.shape else {
                panic!("{status} is not an enum");
            };
            for value in *successes {
                assert!(
                    entries.iter().any(|entry| entry.value == *value),
                    "{status} has no known value `{value}`"
                );
            }
        }
    }

    #[test]
    fn exit_code_success_is_closed_over_zero_alone() {
        let exit = contract_file("exit.rs");
        assert!(exit.contains("pub fn is_success(&self) -> bool"));
        assert!(
            exit.contains("matches!(self, Self::Success)"),
            "a known non-zero code must not count as success"
        );
    }

    fn contract_file(name: &str) -> String {
        generate(&contract())
            .into_iter()
            .find(|file| file.path == Path::new(name))
            .unwrap_or_else(|| panic!("{name} is not generated"))
            .contents
    }

    #[test]
    fn type_function_and_argument_struct_names_are_unique() {
        let ir = contract();
        let types: Vec<String> = ir.types.iter().map(|definition| type_name(&definition.name)).collect();
        assert_eq!(
            types.len(),
            types.iter().collect::<BTreeSet<_>>().len(),
            "two types share a Rust name"
        );
        let functions: Vec<String> = ir.commands.iter().map(function_name).collect();
        assert_eq!(
            functions.len(),
            functions.iter().collect::<BTreeSet<_>>().len(),
            "two commands share a function"
        );
        let arguments: Vec<String> = ir.commands.iter().map(args_name).collect();
        assert_eq!(
            arguments.len(),
            arguments.iter().collect::<BTreeSet<_>>().len(),
            "two commands share an arguments type"
        );
        for name in types.iter().chain(&arguments) {
            assert!(
                !RESERVED_TYPES.contains(&name.as_str()),
                "{name} shadows a name the generated code uses"
            );
        }
    }

    #[test]
    fn a_command_function_never_takes_the_name_of_a_method_the_client_has() {
        for command in &contract().commands {
            let name = function_name(command);
            assert!(
                !OCX_METHODS.contains(&name.as_str()),
                "`{name}` collides with an `Ocx` method"
            );
        }
    }

    #[test]
    fn identifiers_of_wire_names_are_valid_and_keywords_are_raw_or_suffixed() {
        assert_eq!(ident("type"), "r#type");
        assert_eq!(ident("self"), "self_");
        assert_eq!(ident("2fa"), "_2fa");
        assert_eq!(ident("some-key.name"), "some_key_name");
        assert_eq!(ident("plain"), "plain");
    }

    #[test]
    fn a_field_that_closes_a_cycle_is_boxed_and_one_that_does_not_is_not() {
        let ir = empty_ir(vec![
            definition(
                "Node",
                Shape::Struct(vec![
                    field("child", TypeRef::Named("Node".to_owned())),
                    field("label", TypeRef::String),
                ]),
            ),
            definition(
                "Leaf",
                Shape::Struct(vec![field("node", TypeRef::Named("Node".to_owned()))]),
            ),
            definition(
                "List",
                Shape::Struct(vec![field(
                    "items",
                    TypeRef::Array(Box::new(TypeRef::Named("List".to_owned()))),
                )]),
            ),
        ]);
        let boxed = boxed_fields(&ir);
        assert!(boxed.contains(&("Node".to_owned(), String::new(), "child".to_owned())));
        assert!(!boxed.contains(&("Leaf".to_owned(), String::new(), "node".to_owned())));
        assert!(
            !boxed.contains(&("List".to_owned(), String::new(), "items".to_owned())),
            "a Vec is already indirect"
        );
        let types = Generator::new(&ir).types();
        assert!(types.contains("pub child: Box<Node>,"));
    }

    #[test]
    fn a_mutual_cycle_through_a_union_boxes_a_field_on_the_way_back() {
        let ir = empty_ir(vec![
            definition(
                "Outer",
                Shape::Union(vec![Variant {
                    tag: "inner".to_owned(),
                    description: String::new(),
                    fields: vec![field("body", TypeRef::Named("Body".to_owned()))],
                }]),
            ),
            definition(
                "Body",
                Shape::Struct(vec![field("outer", TypeRef::Named("Outer".to_owned()))]),
            ),
        ]);
        let boxed = boxed_fields(&ir);
        assert!(!boxed.is_empty(), "a cycle with no indirection cannot be sized");
    }

    #[test]
    fn a_union_variant_with_a_var_shaped_ref_keeps_its_tag() {
        let ir = empty_ir(vec![definition(
            "Entry",
            Shape::Union(vec![
                Variant {
                    tag: "var".to_owned(),
                    description: String::new(),
                    fields: vec![field("key", TypeRef::String)],
                },
                Variant {
                    tag: "path".to_owned(),
                    description: String::new(),
                    fields: Vec::new(),
                },
            ]),
        )]);
        let types = Generator::new(&ir).types();
        assert!(
            types.contains("\"var\" => {"),
            "the `var` arm must be selected by its tag"
        );
        assert!(types.contains("\"path\" => Ok(Self::Path),"));
        assert!(types.contains("Unknown(Value)"));
    }

    #[test]
    fn a_union_decoder_reads_the_tag_first_so_a_bad_payload_is_an_error_not_an_unknown_arm() {
        let types = contract_file("types.rs");
        assert!(types.contains("payload::<Fields, D::Error>("));
        assert!(types.contains("_ => Ok(Self::Unknown(value)),"));
    }

    #[test]
    fn a_struct_without_fields_collects_no_unknowns() {
        let ir = empty_ir(vec![definition("Nothing", Shape::Struct(Vec::new()))]);
        let types = Generator::new(&ir).types();
        assert!(types.contains("fn collect_unknowns(&self, _pointer: &mut String, _out: &mut Vec<String>) {}"));
    }

    #[test]
    fn braces_and_calls_break_where_rustfmt_breaks_them() {
        let short = braced(4, "Self::A", &["a: field_0".to_owned()], " => {");
        assert_eq!(short, "    Self::A { a: field_0 } => {");
        let long = braced(
            4,
            "Self::A",
            &["rule: field_0".to_owned(), "companion: field_1".to_owned()],
            " => {",
        );
        assert_eq!(
            long,
            "    Self::A {\n        rule: field_0,\n        companion: field_1,\n    } => {"
        );
        assert_eq!(call_statement(4, "f", &["a", "b"]), "    f(a, b);\n");
        let wide = "x".repeat(FN_PARAMS_WIDTH);
        assert_eq!(
            call_statement(4, "f", &["a", &wide]),
            format!("    f(\n        a,\n        {wide},\n    );\n")
        );
    }
}
