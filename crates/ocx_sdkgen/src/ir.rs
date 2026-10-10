// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The generator's language-neutral model: the three documents lowered into named types, report roots, commands and
//! the registries. Every backend reads this and nothing else; anything outside the representation subset is refused
//! here, so a backend never sees a construct it cannot emit.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::Documents;
use crate::subset::{ALLOWLIST, X_ENUM, arm_tag, is_opaque, is_unknown_arm, ref_target};
use crate::versions::{self, VersionError, Versions};

/// Everything a backend emits from.
#[derive(Clone, Debug, PartialEq)]
pub struct Ir {
    /// Every named type reachable from a report root or the error document, sorted by name.
    pub types: Vec<TypeDef>,
    /// Every report root and the error document's root, sorted by name.
    pub roots: Vec<Root>,
    /// The options an `ocx` invocation takes before its subcommand, in declaration order.
    pub globals: Vec<Arg>,
    /// Every versioned, non-hidden, non-deprecated command, sorted by path.
    pub commands: Vec<Command>,
    /// The exit-code registry.
    pub exit_codes: Vec<EnumEntry>,
    /// The error-category registry.
    pub categories: Vec<EnumEntry>,
    /// The `error.detail` slug registry.
    pub details: Vec<EnumEntry>,
    /// The Public env manifest, sorted by name.
    pub env: Vec<EnvVar>,
    /// What the SDK compares against the running `ocx` before it spawns a command.
    pub versions: Versions,
}

/// A named type and its shape.
#[derive(Clone, Debug, PartialEq)]
pub struct TypeDef {
    /// Unique across the IR; a backend maps it to its own naming convention.
    pub name: String,
    pub description: String,
    pub shape: Shape,
}

/// The representation subset's constructs that carry a name.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// An object with known properties; unknown properties on the wire are ignored.
    Struct(Vec<Field>),
    /// An open scalar enum: known entries plus any other value of `base`.
    ScalarEnum { base: Scalar, entries: Vec<EnumEntry> },
    /// A union tagged by `type`, open to unknown tags.
    Union(Vec<Variant>),
    /// A named alias of another type, such as a vocabulary string.
    Alias(TypeRef),
}

/// One object property.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// The wire name.
    pub name: String,
    pub description: String,
    pub ty: TypeRef,
    /// Absent from the wire when unset, never `null`, when false.
    pub required: bool,
}

/// One known arm of a tagged union.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    /// The `type` const that selects the arm.
    pub tag: String,
    pub description: String,
    /// The arm's other properties: the union's shared ones, a referenced struct's, then the arm's own.
    pub fields: Vec<Field>,
}

/// A reference to a type at a use site.
#[derive(Clone, Debug, PartialEq)]
pub enum TypeRef {
    Named(String),
    String,
    Integer,
    Number,
    Boolean,
    Array(Box<TypeRef>),
    /// A string-keyed map.
    Map(Box<TypeRef>),
    /// Publisher JSON passed through unchanged.
    Opaque,
}

/// The base type of a scalar enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scalar {
    String,
    Integer,
}

/// One `x-ocx-enum` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct EnumEntry {
    /// The wire value, a string or an integer.
    pub value: Value,
    /// The generated variant name.
    pub name: String,
    pub description: String,
    /// On an exit code: its category.
    pub category: Option<String>,
    /// On a slug: the exit code it ships under.
    pub exit_code: Option<i64>,
}

/// A published root and its version.
#[derive(Clone, Debug, PartialEq)]
pub struct Root {
    /// The name `reports` lists it under, or `ErrorDocument`.
    pub name: String,
    /// The payload type, the wrapper's properties without `schema_version`.
    pub payload: String,
    pub version: u32,
}

/// A command the SDK exposes as one function.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// Its words below `ocx`.
    pub path: Vec<String>,
    pub version: u32,
    pub summary: String,
    /// Arguments an ancestor declares `global` first, then the command's own, in declaration order.
    pub args: Vec<Arg>,
    pub outputs: Vec<Output>,
}

/// One argument of a command.
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    /// The clap id, unique within the command.
    pub id: String,
    /// Passed as one `--long=value` token; `None` for a positional.
    pub long: Option<String>,
    /// The positional index, for a positional.
    pub position: Option<u32>,
    pub value: ValueKind,
    pub required: bool,
    /// Accepts more than one value.
    pub multiple: bool,
    /// Its operands follow `--`.
    pub after_terminator: bool,
    /// Its operands end at `--`.
    pub terminated: bool,
    /// A value may start with `-`; otherwise the SDK refuses one.
    pub allow_hyphen_values: bool,
    /// Sent on stdin, never in argv.
    pub stdin_secret: bool,
    pub help: String,
}

/// An argument's value type.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueKind {
    /// A flag with no value.
    Switch,
    String,
    Path,
    Identifier,
    Platform,
    Integer,
    /// One of the visible choice values.
    Choice(Vec<String>),
}

/// What a command prints under `--format json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    Report(String),
    /// A report on stdout and a non-zero exit.
    ReportThenFail(String),
    Empty,
    /// The child owns stdout.
    Passthrough,
    ShellStream,
    /// A document the command passes through unchanged.
    RawDocument,
}

/// One Public env variable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    /// The value kind as `cli.json` spells it.
    pub value: String,
    pub summary: String,
    /// Its value is redacted from `Debug`.
    pub secret: bool,
}

/// Why the documents cannot be lowered.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IrError {
    /// A construct outside the representation subset.
    #[error("{pointer}: `{keyword}` is outside the representation subset")]
    OutsideSubset { pointer: String, keyword: String },
    /// A node the subset allows but whose content is malformed.
    #[error("{pointer}: {reason}")]
    Malformed { pointer: String, reason: String },
    #[error(transparent)]
    Versions(#[from] VersionError),
}

fn malformed(pointer: impl Into<String>, reason: impl Into<String>) -> IrError {
    IrError::Malformed {
        pointer: pointer.into(),
        reason: reason.into(),
    }
}

/// The catch-all arm every open enum and union gets; a known value spelled `unknown` must not take its name.
const CATCH_ALL: &str = "Unknown";

/// `snake_case`, `kebab-case` and mixed spellings to `PascalCase`; a leading digit gets a `V`.
pub fn pascal(value: &str) -> String {
    let mut name = String::new();
    for word in value
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            name.push(first.to_ascii_uppercase());
            name.extend(chars);
        }
    }
    if name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, 'V');
    }
    name
}

/// The variant a known enum value or union tag becomes, never the catch-all's own name.
pub fn variant_name(value: &str) -> String {
    let name = pascal(value);
    if name == CATCH_ALL {
        format!("{name}Value")
    } else {
        name
    }
}

/// Lowers the three documents into the IR.
///
/// # Errors
///
/// A construct outside the representation subset, a malformed node, or a missing version.
pub fn load(documents: &Documents) -> Result<Ir, IrError> {
    let versions = versions::read(documents)?;
    let mut lowering = Lowering::new(documents)?;
    lowering.lower_defs()?;
    let roots = lowering.roots(documents, &versions)?;
    let types = lowering.reachable(&roots)?;
    let exit_codes = registry(&types, "ExitCode")?;
    let categories = registry(&types, "ErrorCategory")?;
    let details = registry(&types, "ErrorDetail")?;
    let cli = lower_cli(&documents.cli, &roots)?;
    Ok(Ir {
        types,
        roots,
        globals: cli.globals,
        commands: cli.commands,
        exit_codes,
        categories,
        details,
        env: cli.env,
        versions,
    })
}

struct Lowering<'a> {
    defs: BTreeMap<String, &'a Value>,
    types: BTreeMap<String, TypeDef>,
}

impl<'a> Lowering<'a> {
    /// The `$defs` of `reports` and `errors` as one set; a name both define must define it identically.
    fn new(documents: &'a Documents) -> Result<Self, IrError> {
        let mut defs = BTreeMap::new();
        for document in [&documents.reports, &documents.errors] {
            let own = document
                .get("$defs")
                .and_then(Value::as_object)
                .ok_or_else(|| malformed("/$defs", "not an object"))?;
            for (name, node) in own {
                if defs.insert(name.clone(), node).is_some_and(|other| other != node) {
                    return Err(malformed(
                        format!("/$defs/{name}"),
                        "defined differently in `reports` and `errors`",
                    ));
                }
            }
        }
        Ok(Self {
            defs,
            types: BTreeMap::new(),
        })
    }

    fn lower_defs(&mut self) -> Result<(), IrError> {
        let defs: Vec<_> = self.defs.iter().map(|(name, node)| (name.clone(), *node)).collect();
        for (name, node) in defs {
            let pointer = format!("/$defs/{name}");
            let shape = self.shape(node, &pointer, &name)?;
            self.add(TypeDef {
                description: description(node),
                name,
                shape,
            })?;
        }
        Ok(())
    }

    /// Adds a type; the same definition twice (a nested enum seen again through a flattened arm) is one type.
    fn add(&mut self, def: TypeDef) -> Result<(), IrError> {
        match self.types.get(&def.name) {
            Some(existing) if existing == &def => Ok(()),
            Some(_) => Err(malformed(
                format!("/$defs/{}", def.name),
                "a second, different type has this name",
            )),
            None => {
                self.types.insert(def.name.clone(), def);
                Ok(())
            }
        }
    }

    fn shape(&mut self, node: &Value, pointer: &str, name: &str) -> Result<Shape, IrError> {
        let object = object(node, pointer)?;
        check_keywords(object, pointer)?;
        if object.contains_key("oneOf") {
            return self.union(object, pointer, name);
        }
        if object.contains_key(X_ENUM) {
            return scalar_enum(object, pointer);
        }
        if node_type(object, pointer)? == Some("object") && object.contains_key("properties") {
            return Ok(Shape::Struct(self.fields(object, pointer, name)?));
        }
        Ok(Shape::Alias(self.type_ref(node, pointer, name, "")?))
    }

    fn fields(&mut self, node: &Map<String, Value>, pointer: &str, owner: &str) -> Result<Vec<Field>, IrError> {
        let required = required_names(node, pointer)?;
        let Some(properties) = node.get("properties") else {
            return Ok(Vec::new());
        };
        let properties = self::object(properties, &format!("{pointer}/properties"))?;
        let mut fields = Vec::new();
        for (name, property) in properties {
            if name == "type" && owner.is_empty() {
                continue;
            }
            let at = format!("{pointer}/properties/{name}");
            fields.push(Field {
                name: name.clone(),
                description: description(property),
                ty: self.type_ref(property, &at, owner, name)?,
                required: required.contains(name.as_str()),
            });
        }
        Ok(fields)
    }

    /// The type a property or alias names. A scalar enum written inline becomes a type of its own, `<Owner><Field>`.
    fn type_ref(&mut self, node: &Value, pointer: &str, owner: &str, field: &str) -> Result<TypeRef, IrError> {
        let object = object(node, pointer)?;
        check_keywords(object, pointer)?;
        if object.contains_key("$ref") {
            let target = ref_target(node).ok_or_else(|| malformed(pointer, "a `$ref` outside `#/$defs/`"))?;
            if !self.defs.contains_key(target) {
                return Err(malformed(pointer, format!("`$ref` to missing `$defs/{target}`")));
            }
            return Ok(TypeRef::Named(target.to_owned()));
        }
        if is_opaque(node) {
            return Ok(TypeRef::Opaque);
        }
        // A union is only lowered as a named `$def`; written inline it would silently become opaque.
        if object.contains_key("oneOf") {
            return Err(outside(pointer, "oneOf"));
        }
        if object.contains_key(X_ENUM) {
            let name = format!("{owner}{}", pascal(field));
            let shape = scalar_enum(object, pointer)?;
            self.add(TypeDef {
                name: name.clone(),
                description: description(node),
                shape,
            })?;
            return Ok(TypeRef::Named(name));
        }
        Ok(match node_type(object, pointer)? {
            Some("string") => TypeRef::String,
            Some("integer") => TypeRef::Integer,
            Some("number") => TypeRef::Number,
            Some("boolean") => TypeRef::Boolean,
            Some("array") => {
                let items = object
                    .get("items")
                    .ok_or_else(|| malformed(pointer, "an array without `items`"))?;
                TypeRef::Array(Box::new(self.type_ref(
                    items,
                    &format!("{pointer}/items"),
                    owner,
                    field,
                )?))
            }
            Some("object") => match object.get("additionalProperties") {
                Some(Value::Bool(true)) => TypeRef::Map(Box::new(TypeRef::Opaque)),
                Some(Value::Bool(false)) => return Err(outside(pointer, "additionalProperties")),
                Some(values) => TypeRef::Map(Box::new(self.type_ref(
                    values,
                    &format!("{pointer}/additionalProperties"),
                    owner,
                    field,
                )?)),
                None if object.contains_key("properties") => {
                    return Err(malformed(pointer, "an object with properties but no name"));
                }
                None => TypeRef::Opaque,
            },
            None => TypeRef::Opaque,
            Some(_) => return Err(outside(pointer, "type")),
        })
    }

    fn union(&mut self, object: &Map<String, Value>, pointer: &str, name: &str) -> Result<Shape, IrError> {
        let arms = object
            .get("oneOf")
            .and_then(Value::as_array)
            .ok_or_else(|| malformed(pointer, "`oneOf` is not an array"))?;
        let known: BTreeSet<&str> = arms.iter().filter_map(arm_tag).collect();
        let (unknown, tagged): (Vec<_>, Vec<_>) = arms.iter().partition(|arm| arm_tag(arm).is_none());
        if unknown.len() != 1 || !is_unknown_arm(unknown[0], &known) || tagged.len() != known.len() {
            return Err(malformed(
                pointer,
                "not a tagged union with distinct tags and exactly one unknown arm",
            ));
        }
        let shared = self.fields(object, pointer, name)?;
        let mut variants = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            let Some(tag) = arm_tag(arm) else { continue };
            let at = format!("{pointer}/oneOf/{index}");
            let arm_object = self::object(arm, &at)?;
            check_keywords(arm_object, &at)?;
            let mut fields = shared.clone();
            let mut arm_description = description(arm);
            if let Some(target) = ref_target(arm) {
                let node = self
                    .defs
                    .get(target)
                    .copied()
                    .ok_or_else(|| malformed(&at, format!("`$ref` to missing `$defs/{target}`")))?;
                let target_object = self::object(node, &at)?;
                fields.extend(self.fields(target_object, &format!("/$defs/{target}"), target)?);
                if arm_description.is_empty() {
                    arm_description = description(node);
                }
            }
            fields.extend(self.fields(arm_object, &at, name)?);
            fields.retain(|field| field.name != "type");
            let names: BTreeSet<_> = fields.iter().map(|field| field.name.as_str()).collect();
            if names.len() != fields.len() {
                return Err(malformed(at, "a property name occurs twice in one arm"));
            }
            variants.push(Variant {
                tag: tag.to_owned(),
                description: arm_description,
                fields,
            });
        }
        let names: BTreeSet<_> = variants.iter().map(|variant| variant_name(&variant.tag)).collect();
        if names.len() != variants.len() {
            return Err(malformed(pointer, "two tags become the same variant name"));
        }
        Ok(Shape::Union(variants))
    }

    /// Every report root, and the error document as the last one.
    fn roots(&mut self, documents: &Documents, versions: &Versions) -> Result<Vec<Root>, IrError> {
        let listed = documents
            .reports
            .get("reports")
            .and_then(Value::as_object)
            .ok_or_else(|| malformed("/reports", "no root list"))?;
        let mut roots = Vec::new();
        for (name, root) in listed {
            let pointer = format!("/reports/{name}");
            let payload = ref_target(root)
                .and_then(|wrapper| wrapper.strip_suffix("Root"))
                .ok_or_else(|| malformed(&pointer, "not a `$ref` to a `<Payload>Root` wrapper"))?;
            if !matches!(self.types.get(payload).map(|def| &def.shape), Some(Shape::Struct(_))) {
                return Err(malformed(
                    pointer,
                    format!("payload `{payload}` is not a struct `$def`"),
                ));
            }
            roots.push(Root {
                name: name.clone(),
                payload: payload.to_owned(),
                version: versions.reports.get(name).copied().unwrap_or_default(),
            });
        }
        let document = object(&documents.errors, "")?;
        check_keywords(document, "")?;
        let mut fields = self.fields(document, "", "")?;
        fields.retain(|field| field.name != "schema_version");
        self.add(TypeDef {
            name: "ErrorDocument".to_owned(),
            description: description(&documents.errors),
            shape: Shape::Struct(fields),
        })?;
        roots.push(Root {
            name: "ErrorDocument".to_owned(),
            payload: "ErrorDocument".to_owned(),
            version: versions.errors,
        });
        roots.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(roots)
    }

    /// The types a root payload names, directly or through other types, sorted by name.
    fn reachable(&self, roots: &[Root]) -> Result<Vec<TypeDef>, IrError> {
        let mut seen = BTreeSet::new();
        let mut pending: Vec<&str> = roots.iter().map(|root| root.payload.as_str()).collect();
        while let Some(name) = pending.pop() {
            if !seen.insert(name) {
                continue;
            }
            let def = self
                .types
                .get(name)
                .ok_or_else(|| malformed(format!("/$defs/{name}"), "a referenced type was not lowered"))?;
            pending.extend(referenced(&def.shape));
        }
        Ok(self
            .types
            .values()
            .filter(|def| seen.contains(def.name.as_str()))
            .cloned()
            .collect())
    }
}

fn referenced(shape: &Shape) -> Vec<&str> {
    fn named(ty: &TypeRef) -> Option<&str> {
        match ty {
            TypeRef::Named(name) => Some(name),
            TypeRef::Array(inner) | TypeRef::Map(inner) => named(inner),
            _ => None,
        }
    }
    match shape {
        Shape::Struct(fields) => fields.iter().filter_map(|field| named(&field.ty)).collect(),
        Shape::Union(variants) => variants
            .iter()
            .flat_map(|variant| variant.fields.iter().filter_map(|field| named(&field.ty)))
            .collect(),
        Shape::Alias(ty) => named(ty).into_iter().collect(),
        Shape::ScalarEnum { .. } => Vec::new(),
    }
}

fn outside(pointer: &str, keyword: &str) -> IrError {
    IrError::OutsideSubset {
        pointer: pointer.to_owned(),
        keyword: keyword.to_owned(),
    }
}

fn object<'v>(node: &'v Value, pointer: &str) -> Result<&'v Map<String, Value>, IrError> {
    node.as_object().ok_or_else(|| malformed(pointer, "not an object"))
}

/// Refuses every keyword outside the allowlist. `enum` is allowed only inside an unknown arm's `not`, which this
/// reader never descends into, so a scalar enum spelled with it is refused here.
fn check_keywords(node: &Map<String, Value>, pointer: &str) -> Result<(), IrError> {
    match node
        .keys()
        .find(|key| !ALLOWLIST.contains(&key.as_str()) || key.as_str() == "enum")
    {
        Some(keyword) => Err(outside(pointer, keyword)),
        None => Ok(()),
    }
}

fn node_type<'v>(node: &'v Map<String, Value>, pointer: &str) -> Result<Option<&'v str>, IrError> {
    match node.get("type") {
        None => Ok(None),
        Some(Value::String(name)) => Ok(Some(name)),
        Some(_) => Err(outside(pointer, "type")),
    }
}

fn description(node: &Value) -> String {
    node.get("description")
        .and_then(Value::as_str)
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

fn required_names<'v>(node: &'v Map<String, Value>, pointer: &str) -> Result<BTreeSet<&'v str>, IrError> {
    match node.get("required") {
        None => Ok(BTreeSet::new()),
        Some(list) => list
            .as_array()
            .and_then(|names| names.iter().map(Value::as_str).collect())
            .ok_or_else(|| malformed(pointer, "`required` is not a list of names")),
    }
}

fn scalar_enum(node: &Map<String, Value>, pointer: &str) -> Result<Shape, IrError> {
    let base = match node_type(node, pointer)? {
        Some("string") => Scalar::String,
        Some("integer") => Scalar::Integer,
        _ => return Err(malformed(pointer, "a scalar enum is a string or an integer")),
    };
    let listed = node
        .get(X_ENUM)
        .and_then(Value::as_array)
        .ok_or_else(|| malformed(pointer, "`x-ocx-enum` is not a list"))?;
    let mut entries = Vec::new();
    for (index, entry) in listed.iter().enumerate() {
        let at = format!("{pointer}/{X_ENUM}/{index}");
        let value = entry.get("value").cloned().unwrap_or(Value::Null);
        let derived = match (&value, base) {
            (Value::String(text), Scalar::String) => variant_name(text),
            (Value::Number(number), Scalar::Integer) if number.is_i64() => match number.as_i64() {
                Some(negative) if negative < 0 => format!("CodeNeg{}", negative.unsigned_abs()),
                _ => format!("Code{number}"),
            },
            _ => return Err(malformed(at, "an entry's `value` does not match the enum's base type")),
        };
        entries.push(EnumEntry {
            name: entry.get("name").and_then(Value::as_str).map_or(derived, variant_name),
            description: description(entry),
            category: entry.get("category").and_then(Value::as_str).map(str::to_owned),
            exit_code: entry.get("exit_code").and_then(Value::as_i64),
            value,
        });
    }
    let names: BTreeSet<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
    let values: BTreeSet<_> = entries.iter().map(|entry| entry.value.to_string()).collect();
    if names.len() != entries.len() || values.len() != entries.len() || names.contains("") {
        return Err(malformed(pointer, "entry names or values are empty or repeat"));
    }
    Ok(Shape::ScalarEnum { base, entries })
}

fn registry(types: &[TypeDef], name: &str) -> Result<Vec<EnumEntry>, IrError> {
    match types.iter().find(|def| def.name == name).map(|def| &def.shape) {
        Some(Shape::ScalarEnum { entries, .. }) => Ok(entries.clone()),
        _ => Err(malformed(format!("/$defs/{name}"), "the registry is not a scalar enum")),
    }
}

/// `cli.json`, as far as the generator reads it.
#[derive(Deserialize)]
struct CliDocument {
    root: CommandNode,
    #[serde(default)]
    env: Vec<EnvNode>,
}

#[derive(Deserialize)]
struct CommandNode {
    path: Vec<String>,
    version: u32,
    summary: String,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    deprecated: Option<Value>,
    #[serde(default)]
    output: Vec<OutputNode>,
    #[serde(default)]
    args: Vec<ArgNode>,
    #[serde(default)]
    commands: Vec<CommandNode>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutputNode {
    Report { root: String },
    ReportThenFail { root: String },
    Empty,
    Passthrough,
    ShellStream,
    RawDocument,
}

#[derive(Deserialize)]
struct ArgNode {
    id: String,
    #[serde(default)]
    long: Option<String>,
    #[serde(default)]
    position: Option<u32>,
    value: ValueNode,
    #[serde(default)]
    required: bool,
    num_args: Arity,
    #[serde(default)]
    repeatable: bool,
    #[serde(default)]
    global: bool,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    deprecated: Option<Value>,
    #[serde(default)]
    allow_hyphen_values: bool,
    #[serde(default)]
    last: bool,
    #[serde(default)]
    value_terminator: Option<String>,
    #[serde(default)]
    stdin_secret: bool,
    #[serde(default)]
    help: String,
}

#[derive(Deserialize)]
struct Arity {
    #[serde(default)]
    max: Option<u32>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ValueNode {
    Switch,
    String,
    Path,
    Identifier,
    Platform,
    Integer,
    Choice { choices: Vec<ChoiceNode> },
}

#[derive(Deserialize)]
struct ChoiceNode {
    value: String,
    #[serde(default)]
    hidden: bool,
}

#[derive(Deserialize)]
struct EnvNode {
    name: String,
    value: String,
    visibility: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    secret: bool,
}

struct Lowered {
    globals: Vec<Arg>,
    commands: Vec<Command>,
    env: Vec<EnvVar>,
}

fn lower_cli(cli: &Value, roots: &[Root]) -> Result<Lowered, IrError> {
    let document = CliDocument::deserialize(cli).map_err(|error| malformed("/root", error.to_string()))?;
    let published: BTreeSet<&str> = roots.iter().map(|root| root.name.as_str()).collect();
    let mut commands = Vec::new();
    collect_commands(&document.root, &[], &published, &mut commands)?;
    commands.sort_by(|a, b| a.path.cmp(&b.path));
    let mut env: Vec<EnvVar> = document
        .env
        .into_iter()
        .filter(|var| var.visibility == "public")
        .map(|var| EnvVar {
            name: var.name,
            value: var.value,
            summary: var.summary.trim().to_owned(),
            secret: var.secret,
        })
        .collect();
    env.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Lowered {
        globals: arguments(&document.root.args, "/root")?
            .into_iter()
            .map(|(_, arg)| arg)
            .collect(),
        commands,
        env,
    })
}

/// The visible arguments of one node, each with its `global` flag; hidden and deprecated ones are not exposed.
fn arguments(nodes: &[ArgNode], pointer: &str) -> Result<Vec<(bool, Arg)>, IrError> {
    let mut args = Vec::new();
    for node in nodes.iter().filter(|node| !node.hidden && node.deprecated.is_none()) {
        let at = format!("{pointer}/args/{}", node.id);
        if node.long.is_none() == node.position.is_none() {
            return Err(malformed(
                at,
                "an argument is a flag or a positional, not both or neither",
            ));
        }
        let value = match &node.value {
            ValueNode::Switch => ValueKind::Switch,
            ValueNode::String => ValueKind::String,
            ValueNode::Path => ValueKind::Path,
            ValueNode::Identifier => ValueKind::Identifier,
            ValueNode::Platform => ValueKind::Platform,
            ValueNode::Integer => ValueKind::Integer,
            ValueNode::Choice { choices } => ValueKind::Choice(
                choices
                    .iter()
                    .filter(|choice| !choice.hidden)
                    .map(|choice| choice.value.clone())
                    .collect(),
            ),
        };
        let terminated = node.value_terminator.is_some();
        args.push((
            node.global,
            Arg {
                id: node.id.clone(),
                long: node.long.clone(),
                position: node.position,
                multiple: !matches!(value, ValueKind::Switch)
                    && (node.repeatable || node.num_args.max.is_none_or(|max| max > 1)),
                value,
                required: node.required,
                after_terminator: node.last,
                terminated,
                allow_hyphen_values: node.allow_hyphen_values,
                stdin_secret: node.stdin_secret,
                help: node.help.trim().to_owned(),
            },
        ));
    }
    Ok(args)
}

fn collect_commands(
    node: &CommandNode,
    inherited: &[Arg],
    published: &BTreeSet<&str>,
    out: &mut Vec<Command>,
) -> Result<(), IrError> {
    let pointer = format!("/root/{}", node.path.join("/"));
    let own = arguments(&node.args, &pointer)?;
    let is_root = node.path.is_empty();
    if node.version > 0 && !node.hidden && node.deprecated.is_none() {
        let outputs = node
            .output
            .iter()
            .map(|mode| output(mode, published, &pointer))
            .collect::<Result<_, _>>()?;
        out.push(Command {
            path: node.path.clone(),
            version: node.version,
            summary: node.summary.trim().to_owned(),
            args: inherited
                .iter()
                .cloned()
                .chain(own.iter().map(|(_, arg)| arg.clone()))
                .collect(),
            outputs,
        });
    }
    // The root's own options are the SDK's global options, so they are not repeated on every command.
    let passed_down: Vec<Arg> = if is_root {
        Vec::new()
    } else {
        inherited
            .iter()
            .cloned()
            .chain(own.into_iter().filter(|(global, _)| *global).map(|(_, arg)| arg))
            .collect()
    };
    for child in &node.commands {
        collect_commands(child, &passed_down, published, out)?;
    }
    Ok(())
}

fn output(mode: &OutputNode, published: &BTreeSet<&str>, pointer: &str) -> Result<Output, IrError> {
    let root = |name: &String| {
        if published.contains(name.as_str()) {
            Ok(name.clone())
        } else {
            Err(malformed(
                pointer,
                format!("names root `{name}`, which `reports` does not publish"),
            ))
        }
    };
    Ok(match mode {
        OutputNode::Report { root: name } => Output::Report(root(name)?),
        OutputNode::ReportThenFail { root: name } => Output::ReportThenFail(root(name)?),
        OutputNode::Empty => Output::Empty,
        OutputNode::Passthrough => Output::Passthrough,
        OutputNode::ShellStream => Output::ShellStream,
        OutputNode::RawDocument => Output::RawDocument,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::*;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn golden(name: &str) -> Value {
        let path = workspace_root().join(format!("crates/ocx_schema/tests/golden/{name}.json"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    /// The three live goldens.
    pub(crate) fn documents() -> Documents {
        Documents {
            reports: golden("reports"),
            errors: golden("errors"),
            cli: golden("cli"),
        }
    }

    fn type_named<'a>(ir: &'a Ir, name: &str) -> &'a TypeDef {
        ir.types
            .iter()
            .find(|def| def.name == name)
            .unwrap_or_else(|| panic!("no type {name}"))
    }

    #[test]
    fn the_goldens_lower_and_every_part_is_populated() {
        let ir = load(&documents()).expect("the goldens lower");
        assert_eq!(ir.roots.len(), 57, "56 report roots and the error document");
        assert_eq!(ir.commands.len(), 67, "versioned, visible commands");
        assert!(ir.types.len() > 150, "{} types", ir.types.len());
        assert!(ir.exit_codes.len() == 13 && ir.categories.len() == 11 && ir.details.len() > 100);
        assert!(!ir.env.is_empty() && ir.env.iter().all(|var| !var.name.is_empty()));
        assert!(ir.globals.iter().any(|arg| arg.id == "format"));
        let names: Vec<_> = ir.types.iter().map(|def| def.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "types are sorted and unique");
        for command in &ir.commands {
            for output in &command.outputs {
                if let Output::Report(root) | Output::ReportThenFail(root) = output {
                    assert!(ir.roots.iter().any(|published| &published.name == root), "{root}");
                }
            }
        }
    }

    #[test]
    fn a_union_carries_the_shared_referenced_and_own_fields_of_each_arm() {
        let ir = load(&documents()).expect("the goldens lower");
        let Shape::Union(variants) = &type_named(&ir, "Var").shape else {
            panic!("Var is a union");
        };
        let path = variants.iter().find(|arm| arm.tag == "path").expect("a path arm");
        let fields: Vec<_> = path.fields.iter().map(|field| field.name.as_str()).collect();
        assert_eq!(fields, ["key", "visibility", "required", "value"]);
        let visibility = type_named(&ir, "VarVisibility");
        assert!(matches!(
            visibility.shape,
            Shape::ScalarEnum {
                base: Scalar::String,
                ..
            }
        ));
    }

    #[test]
    fn a_known_value_spelled_unknown_does_not_take_the_catch_all_name() {
        let ir = load(&documents()).expect("the goldens lower");
        let Shape::ScalarEnum { entries, .. } = &type_named(&ir, "CapabilityStatus").shape else {
            panic!("CapabilityStatus is a scalar enum");
        };
        let unknown = entries
            .iter()
            .find(|entry| entry.value == "unknown")
            .expect("an `unknown` value");
        assert_eq!(unknown.name, "UnknownValue");
        assert_eq!(variant_name("path"), "Path");
        assert_eq!(pascal("7z-archive"), "V7zArchive");
    }

    #[test]
    fn the_error_document_is_a_root_without_its_version_field() {
        let ir = load(&documents()).expect("the goldens lower");
        let Shape::Struct(fields) = &type_named(&ir, "ErrorDocument").shape else {
            panic!("ErrorDocument is a struct");
        };
        assert!(fields.iter().all(|field| field.name != "schema_version"));
        let root = ir
            .roots
            .iter()
            .find(|root| root.name == "ErrorDocument")
            .expect("a root");
        assert_eq!(root.version, ir.versions.errors);
    }

    #[test]
    fn a_command_keeps_its_visible_arguments_and_drops_hidden_ones() {
        let ir = load(&documents()).expect("the goldens lower");
        assert!(
            ir.commands
                .iter()
                .all(|command| command.path != ["package", "describe"])
        );
        let exec = ir
            .commands
            .iter()
            .find(|command| command.path == ["exec"])
            .expect("exec");
        let argv = exec.args.iter().find(|arg| arg.id == "argv").expect("argv");
        assert!(argv.after_terminator && argv.allow_hyphen_values && argv.multiple && argv.required);
        let names = exec.args.iter().find(|arg| arg.id == "names").expect("names");
        assert!(names.terminated && !names.required);
        let login = ir
            .commands
            .iter()
            .find(|command| command.path == ["login"])
            .expect("login");
        assert!(
            login
                .args
                .iter()
                .any(|arg| arg.id == "password_stdin" && arg.stdin_secret)
        );
    }

    #[test]
    fn two_loads_of_the_same_documents_are_equal() {
        assert_eq!(load(&documents()), load(&documents()));
    }

    fn mutated(pointer: &str, replace: Value) -> Documents {
        let mut documents = documents();
        *documents.reports.pointer_mut(pointer).expect("the pointer exists") = replace;
        documents
    }

    #[test]
    fn a_construct_outside_the_subset_is_refused_with_its_pointer() {
        let any_of = mutated(
            "/$defs/About/properties/registry",
            json!({"anyOf": [{"type": "string"}, {"type": "integer"}]}),
        );
        assert_eq!(
            load(&any_of),
            Err(IrError::OutsideSubset {
                pointer: "/$defs/About/properties/registry".to_owned(),
                keyword: "anyOf".to_owned()
            })
        );
        let min_items = mutated(
            "/$defs/About/properties/platforms",
            json!({"type": "array", "items": {"type": "string"}, "minItems": 1}),
        );
        assert!(matches!(load(&min_items), Err(IrError::OutsideSubset { keyword, .. }) if keyword == "minItems"));
        let type_list = mutated("/$defs/About/properties/registry", json!({"type": ["string", "null"]}));
        assert!(matches!(load(&type_list), Err(IrError::OutsideSubset { keyword, .. }) if keyword == "type"));
        let plain_enum = mutated(
            "/$defs/WriteStatus",
            json!({"type": "string", "enum": ["unchanged", "updated"]}),
        );
        assert!(matches!(load(&plain_enum), Err(IrError::OutsideSubset { keyword, .. }) if keyword == "enum"));
    }

    #[test]
    fn an_inline_one_of_is_outside_the_subset_not_silently_opaque() {
        let inline = mutated(
            "/$defs/About/properties/registry",
            json!({"oneOf": [{"type": "string"}, {"type": "integer"}]}),
        );
        assert_eq!(
            load(&inline),
            Err(IrError::OutsideSubset {
                pointer: "/$defs/About/properties/registry".to_owned(),
                keyword: "oneOf".to_owned()
            })
        );
    }

    #[test]
    fn two_union_tags_that_become_one_variant_name_are_malformed() {
        let mut documents = documents();
        let arms = documents
            .reports
            .pointer_mut("/$defs/Note/oneOf")
            .and_then(Value::as_array_mut)
            .expect("Note is a union");
        let mut twin = arms[1].clone();
        twin["properties"]["type"]["const"] = json!("active-via-paths-grant");
        arms.insert(2, twin);
        assert!(matches!(
            load(&documents),
            Err(IrError::Malformed { pointer, .. }) if pointer == "/$defs/Note"
        ));
    }

    #[test]
    fn a_negative_integer_entry_without_a_name_becomes_a_valid_identifier() {
        let mut documents = documents();
        documents.errors["$defs"]["ExitCode"]["x-ocx-enum"][1] =
            json!({"value": -1, "description": "below zero", "category": "internal"});
        let ir = load(&documents).expect("the mutated registry lowers");
        assert!(ir.exit_codes.iter().any(|entry| entry.name == "CodeNeg1"));
    }

    #[test]
    fn a_union_without_its_unknown_arm_is_malformed() {
        let mut documents = documents();
        documents
            .reports
            .pointer_mut("/$defs/Note/oneOf")
            .and_then(Value::as_array_mut)
            .expect("Note is a union")
            .pop();
        assert!(matches!(load(&documents), Err(IrError::Malformed { .. })));
    }

    #[test]
    fn a_def_that_reports_and_errors_define_differently_is_malformed() {
        let mut documents = documents();
        documents.errors["$defs"]["PackageRef"]["description"] = json!("another meaning");
        assert!(matches!(load(&documents), Err(IrError::Malformed { pointer, .. }) if pointer == "/$defs/PackageRef"));
    }
}
