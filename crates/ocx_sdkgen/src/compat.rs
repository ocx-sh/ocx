// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The compat gate: a structural differ between the last release's documents and the current ones, and the gate
//! that holds every breaking finding to a ledger entry and a version bump.
//!
//! The differ compares over the representation subset's keyword allowlist; a keyword outside it, in either document,
//! is an unmodelled finding and breaks. `$def` names are not contract: `$ref`s are resolved and findings are
//! attributed to every root or command that reaches the changed node. The `schema_version` property, `$id` and each
//! unknown arm's derived `not.enum` are skipped, since the version step owns the first two and the third follows
//! from the known arms.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::Documents;
use crate::lint::{Kind, escape};
use crate::subset::{self, ALLOWLIST, ENUM_ENTRY_MEMBERS, X_ENUM};
use crate::versions;

/// The release this build belongs to: a hidden, deprecated item may be removed once its `removal` release is reached.
pub const CURRENT_RELEASE: &str = env!("CARGO_PKG_VERSION");

id_enum! {
    /// Every differ code with what it reports. Each one breaks; a description change breaks only through a
    /// `semantic` ledger entry. Additive changes produce no coded finding.
    Code, "differ code", {
        B01 "report root removed",
        B02 "property removed",
        B03 "required property became optional",
        B04 "type or resolved target changed, map values and array items included",
        B05 "enum value or union variant removed",
        B06 "pattern, format, minimum, maximum, maxLength, uniqueItems or propertyNames changed",
        B07 "opaque leaf became structured, or the reverse",
        B08 "enum entry name changed, which renames the generated SDK variant",
        D01 "description changed; needs a `doc` or `semantic` ledger entry",
        U01 "keyword outside the representation subset",
        R01 "exit code removed or renumbered",
        R02 "exit code category changed",
        R03 "slug removed, or its exit code changed",
        G01 "command removed, or hidden without a deprecation",
        G02 "long flag removed or renamed outside a deprecation window",
        G03 "short letter removed or changed",
        G04 "required argument added, or an argument became required",
        G05 "choice removed",
        G06 "value type changed",
        G07 "positional order, narrowed arity, or last/trailing/terminator semantics changed",
        G08 "flag no longer global",
        G09 "default changed",
        G10 "requires or conflicts added, or a group made required",
        G11 "require_equals turned on, or allow_hyphen_values turned off",
        G12 "output mode removed or changed",
        G13 "public env variable removed or renamed without a retirement window",
        G14 "public env value type changed, choice removed, or invalid values now refused",
        G15 "stdin_secret changed",
    }
}

/// One difference the differ classifies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The code the differ classified it under.
    pub code: Code,
    /// The changed node: into the base document for a removal or change, into the current one for an addition.
    /// Array-index pointers for `cli.json`.
    pub pointer: String,
    /// Every report root or versioned command path that reaches the node; `*` for the document-wide `errors`,
    /// `env` and `retired` sections.
    pub subjects: BTreeSet<String>,
    /// What changed, for the gate's red message.
    pub message: String,
}

/// One diff run: its findings and how much it read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    /// Sorted by pointer, then code.
    pub findings: Vec<Finding>,
    /// Keywords classified across both documents; the caller floors it against an independent raw key count.
    pub keywords: usize,
}

/// Compares two documents of one kind. `kind` is [`Kind::Reports`], [`Kind::Errors`] or [`Kind::Cli`]; an input
/// schema has no compat contract of its own and compares equal to anything.
pub fn diff(base: &Value, current: &Value, kind: Kind) -> Diff {
    match kind {
        Kind::Reports => reports(base, current),
        Kind::Errors => errors(base, current),
        Kind::Cli => cli(base, current),
        Kind::Input => Diff::default(),
    }
}

/// Findings keyed by pointer, then code: the order a [`Diff`] reports in.
type Found = BTreeMap<(String, Code), Finding>;

fn record<'a>(
    found: &mut Found,
    code: Code,
    pointer: &str,
    subjects: impl IntoIterator<Item = &'a String>,
    message: &str,
) {
    found
        .entry((pointer.to_owned(), code))
        .or_insert_with(|| Finding {
            code,
            pointer: pointer.to_owned(),
            subjects: BTreeSet::new(),
            message: message.to_owned(),
        })
        .subjects
        .extend(subjects.into_iter().cloned());
}

// Keyword reading. A keyword counts as read when the walk looked at it or charged it wholesale; `raw` is the same
// count taken without any structure, so the gate can floor the walk against it.

/// Objects whose keys are names, not keywords.
const NAME_MAPS: &[&str] = &["properties", "$defs", "reports"];
/// The children a schema node's walk recurses into itself. `$defs` is accounted for by the defs it visits.
const WALKED: &[&str] = &["properties", "additionalProperties", "items", "oneOf", "$defs"];
/// Constraint keywords: a change to one is a constraint break.
const CONSTRAINTS: &[&str] = &[
    "pattern",
    "format",
    "minimum",
    "maximum",
    "maxLength",
    "uniqueItems",
    "propertyNames",
];

/// Every object key under `value` except the names inside a name map.
fn raw(value: &Value) -> usize {
    match value {
        Value::Object(map) => map.len() + map.iter().map(|(key, child)| raw_entry(key, child)).sum::<usize>(),
        Value::Array(items) => items.iter().map(raw).sum(),
        _ => 0,
    }
}

fn raw_entry(key: &str, child: &Value) -> usize {
    match child {
        Value::Object(names) if NAME_MAPS.contains(&key) => names.values().map(raw).sum(),
        _ => raw(child),
    }
}

/// The keys of `map` plus everything under them that `skip` does not name.
fn shallow(map: &Map<String, Value>, skip: &[&str]) -> usize {
    map.len()
        + map
            .iter()
            .filter(|(key, _)| !skip.contains(&key.as_str()))
            .map(|(key, child)| raw_entry(key, child))
            .sum::<usize>()
}

fn walked_cost(map: &Map<String, Value>) -> usize {
    map.iter()
        .filter(|(key, _)| key.as_str() != "$defs" && WALKED.contains(&key.as_str()))
        .map(|(key, child)| raw_entry(key, child))
        .sum()
}

fn entries<'a>(document: &'a Value, key: &str) -> BTreeMap<&'a str, &'a Value> {
    document
        .get(key)
        .and_then(Value::as_object)
        .map(|map| map.iter().map(|(name, value)| (name.as_str(), value)).collect())
        .unwrap_or_default()
}

fn def<'a>(document: &'a Value, name: &str) -> Option<&'a Value> {
    document.get("$defs")?.get(name)
}

/// A keyword the differ models. `enum` and `not` live only inside an unknown arm, which the differ never enters.
fn modelled(keyword: &str) -> bool {
    ALLOWLIST.contains(&keyword) && keyword != "enum" && keyword != "not"
}

/// Walks a `reports` or `errors` document pair from its roots, attributing every finding to the root that reaches it.
struct Schemas<'a> {
    /// Base then current.
    docs: [&'a Value; 2],
    /// The root the walk is under; findings are attributed to it.
    subject: String,
    found: Found,
    /// Node pairs whose keywords are already charged.
    counted: BTreeSet<(String, String)>,
    /// `$def` pairs entered under the current root, which stops a recursive schema and a repeated visit.
    visiting: BTreeSet<(String, String)>,
    /// The `$def` names walked, per side; the rest are charged at the end and judged by no root.
    defs: [BTreeSet<String>; 2],
    keywords: usize,
    /// Differently named `$ref` pairs being compared right now, so a cycle between renamed defs ends.
    probing: BTreeSet<(String, String)>,
}

impl<'a> Schemas<'a> {
    fn new(docs: [&'a Value; 2]) -> Self {
        Self {
            docs,
            subject: String::new(),
            found: Found::new(),
            counted: BTreeSet::new(),
            visiting: BTreeSet::new(),
            defs: [BTreeSet::new(), BTreeSet::new()],
            keywords: 0,
            probing: BTreeSet::new(),
        }
    }

    fn emit(&mut self, code: Code, pointer: &str, message: &str) {
        record(&mut self.found, code, pointer, [&self.subject], message);
    }

    fn finish(mut self) -> Diff {
        for (side, document) in self.docs.into_iter().enumerate() {
            let defs = document.get("$defs").and_then(Value::as_object);
            for (name, schema) in defs.into_iter().flatten() {
                if !self.defs[side].contains(name) {
                    self.keywords += raw(schema);
                }
            }
        }
        Diff {
            findings: self.found.into_values().collect(),
            keywords: self.keywords,
        }
    }

    /// Compares one schema node: `base_ptr` is its pointer in the base, `current_ptr` in the current document.
    fn node(&mut self, a: &Value, b: &Value, base_ptr: &str, current_ptr: &str, top: bool) {
        let first = self.counted.insert((base_ptr.to_owned(), current_ptr.to_owned()));
        let (Some(old_map), Some(new_map)) = (a.as_object(), b.as_object()) else {
            if first {
                self.keywords += raw(a) + raw(b);
            }
            if a != b {
                self.emit(Code::B04, base_ptr, "schema kind changed");
            }
            return;
        };
        if first {
            self.keywords += shallow(old_map, WALKED) + shallow(new_map, WALKED);
        }
        for (map, pointer) in [(old_map, base_ptr), (new_map, current_ptr)] {
            let stray: Vec<&str> = map.keys().map(String::as_str).filter(|key| !modelled(key)).collect();
            if !stray.is_empty() {
                self.emit(Code::U01, pointer, &format!("unmodelled keyword {}", stray.join(", ")));
                break;
            }
        }

        let (old_ref, new_ref) = (subset::ref_target(a), subset::ref_target(b));
        let broken = if old_ref.is_some() != new_ref.is_some() {
            Some((Code::B04, "a `$ref` and an inline schema swapped"))
        } else if subset::is_opaque(a) != subset::is_opaque(b) {
            Some((Code::B07, "opaque and structured swapped"))
        } else if old_map.contains_key("oneOf") != new_map.contains_key("oneOf")
            || old_map.get("type") != new_map.get("type")
            || old_map.get("const") != new_map.get("const")
        {
            Some((Code::B04, "type changed"))
        } else {
            None
        };
        if let Some((code, message)) = broken {
            self.emit(code, base_ptr, message);
            if first {
                self.keywords += walked_cost(old_map) + walked_cost(new_map);
            }
            return;
        }
        if old_map.get("description") != new_map.get("description") {
            self.emit(Code::D01, base_ptr, "description changed");
        }
        if CONSTRAINTS.iter().any(|key| old_map.get(*key) != new_map.get(*key)) {
            self.emit(Code::B06, base_ptr, "constraint keyword changed");
        }
        // A `$ref` arm may carry siblings (a flattened tagged union), so its own structure is compared as well.
        if let (Some(x), Some(y)) = (old_ref, new_ref) {
            self.reference(x, y, base_ptr, top);
        }
        self.properties(old_map, new_map, (base_ptr, current_ptr), top, first);
        for key in ["additionalProperties", "items"] {
            self.single(old_map, new_map, key, (base_ptr, current_ptr), first);
        }
        self.union(old_map, new_map, (base_ptr, current_ptr), first);
        self.enum_entries(old_map, new_map, (base_ptr, current_ptr));
    }

    /// Two `$ref`s: the same `$def` is entered once per root; different names are compared as resolved schemas,
    /// so a rename that moves no wire byte is silent and any difference is one changed-type break at the reference.
    fn reference(&mut self, x: &str, y: &str, base_ptr: &str, top: bool) {
        if x == y {
            if self.visiting.insert((x.to_owned(), y.to_owned())) {
                self.enter(x, y, top);
            }
            return;
        }
        let pair = (x.to_owned(), y.to_owned());
        if self.probing.contains(&pair) {
            return;
        }
        let mut probe = Schemas::new(self.docs);
        probe.probing = self.probing.clone();
        probe.probing.insert(pair);
        probe.enter(x, y, top);
        if !probe.found.is_empty() {
            self.emit(Code::B04, base_ptr, &format!("resolved target changed from {x} to {y}"));
        }
    }

    fn enter(&mut self, x: &str, y: &str, top: bool) {
        let docs = self.docs;
        if let (Some(a), Some(b)) = (def(docs[0], x), def(docs[1], y)) {
            self.defs[0].insert(x.to_owned());
            self.defs[1].insert(y.to_owned());
            let (base_ptr, current_ptr) = (format!("/$defs/{}", escape(x)), format!("/$defs/{}", escape(y)));
            self.node(a, b, &base_ptr, &current_ptr, top);
        }
    }

    fn properties(
        &mut self,
        old_map: &Map<String, Value>,
        new_map: &Map<String, Value>,
        at: (&str, &str),
        top: bool,
        first: bool,
    ) {
        let (base_ptr, current_ptr) = at;
        let named = |map: &'_ Map<String, Value>| -> BTreeMap<String, Value> {
            map.get("properties")
                .and_then(Value::as_object)
                .map(|props| {
                    props
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default()
        };
        let (old, new) = (named(old_map), named(new_map));
        // The version step owns `schema_version` of a root wrapper and of the error document.
        let skipped = |name: &str| top && name == "schema_version";
        for (name, a) in &old {
            if skipped(name) {
                if first {
                    self.keywords += raw(a) + new.get(name).map_or(0, raw);
                }
                continue;
            }
            let escaped = escape(name);
            match new.get(name) {
                Some(b) => self.node(
                    a,
                    b,
                    &format!("{base_ptr}/properties/{escaped}"),
                    &format!("{current_ptr}/properties/{escaped}"),
                    false,
                ),
                None => {
                    self.emit(
                        Code::B02,
                        &format!("{base_ptr}/properties/{escaped}"),
                        "property removed",
                    );
                    if first {
                        self.keywords += raw(a);
                    }
                }
            }
        }
        for (name, b) in &new {
            if !old.contains_key(name) && first {
                self.keywords += raw(b);
            }
        }
        let required = |map: &Map<String, Value>| -> BTreeSet<String> {
            map.get("required")
                .and_then(Value::as_array)
                .map(|names| names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default()
        };
        let still_required = required(new_map);
        for name in required(old_map) {
            if !skipped(&name) && old.contains_key(&name) && new.contains_key(&name) && !still_required.contains(&name)
            {
                self.emit(
                    Code::B03,
                    &format!("{base_ptr}/properties/{}", escape(&name)),
                    "required became optional",
                );
            }
        }
    }

    /// `additionalProperties` or `items`: a schema that must keep matching, whose loss is a type change.
    fn single(
        &mut self,
        old_map: &Map<String, Value>,
        new_map: &Map<String, Value>,
        key: &str,
        at: (&str, &str),
        first: bool,
    ) {
        let (base_ptr, current_ptr) = at;
        match (old_map.get(key), new_map.get(key)) {
            (Some(a), Some(b)) => self.node(
                a,
                b,
                &format!("{base_ptr}/{key}"),
                &format!("{current_ptr}/{key}"),
                false,
            ),
            (Some(a), None) => {
                self.emit(Code::B04, &format!("{base_ptr}/{key}"), "schema removed");
                if first {
                    self.keywords += raw(a);
                }
            }
            (None, Some(b)) if first => self.keywords += raw(b),
            _ => {}
        }
    }

    /// `oneOf` arms are keyed by their tag, so reordering is silent; the unknown arm's `not.enum` is derived from
    /// the known arms and never compared.
    fn union(&mut self, old_map: &Map<String, Value>, new_map: &Map<String, Value>, at: (&str, &str), first: bool) {
        let (base_ptr, current_ptr) = at;
        let arms = |map: &'_ Map<String, Value>| -> Vec<(String, Value)> {
            let list = map
                .get("oneOf")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            list.iter()
                .enumerate()
                .map(|(index, arm)| {
                    let key = match subset::arm_tag(arm) {
                        Some(tag) => tag.to_owned(),
                        None if subset::is_marked_unknown(arm) => "?unknown".to_owned(),
                        None => format!("#{index}"),
                    };
                    (key, arm.clone())
                })
                .collect()
        };
        let (old, new) = (arms(old_map), arms(new_map));
        for (index, (key, a)) in old.iter().enumerate() {
            let pointer = format!("{base_ptr}/oneOf/{index}");
            match new.iter().position(|(other, _)| other == key) {
                Some(next) if subset::is_marked_unknown(a) => {
                    let b = &new[next].1;
                    if self
                        .counted
                        .insert((pointer.clone(), format!("{current_ptr}/oneOf/{next}")))
                    {
                        self.keywords += raw(a) + raw(b);
                    }
                    if a.get("description") != b.get("description") {
                        self.emit(Code::D01, &pointer, "description changed");
                    }
                }
                Some(next) => self.node(a, &new[next].1, &pointer, &format!("{current_ptr}/oneOf/{next}"), false),
                None => {
                    self.emit(Code::B05, &pointer, "union variant removed");
                    if first {
                        self.keywords += raw(a);
                    }
                }
            }
        }
        for (key, b) in &new {
            if first && !old.iter().any(|(other, _)| other == key) {
                self.keywords += raw(b);
            }
        }
    }

    /// `x-ocx-enum` entries are keyed by `value`; each member has its own code. Their keywords are charged with the node.
    fn enum_entries(&mut self, old_map: &Map<String, Value>, new_map: &Map<String, Value>, at: (&str, &str)) {
        let (base_ptr, current_ptr) = at;
        let (a, b) = (
            old_map.get(X_ENUM).and_then(Value::as_array),
            new_map.get(X_ENUM).and_then(Value::as_array),
        );
        let (old, new) = match (a, b) {
            (Some(old), Some(new)) => (old, new),
            (Some(_), None) => {
                self.emit(Code::B04, base_ptr, "enum documentation removed");
                return;
            }
            _ => return,
        };
        for (index, x) in old.iter().enumerate() {
            let pointer = format!("{base_ptr}/{X_ENUM}/{index}");
            let Some(next) = new.iter().position(|y| y.get("value") == x.get("value")) else {
                let code = if x.get("category").is_some() {
                    Code::R01
                } else if x.get("exit_code").is_some() {
                    Code::R03
                } else {
                    Code::B05
                };
                self.emit(code, &pointer, "enum value removed");
                continue;
            };
            let y = &new[next];
            let stray = |entry: &Value| {
                entry
                    .as_object()
                    .is_some_and(|map| map.keys().any(|key| !ENUM_ENTRY_MEMBERS.contains(&key.as_str())))
            };
            if stray(x) {
                self.emit(Code::U01, &pointer, "unmodelled enum entry member");
            } else if stray(y) {
                self.emit(
                    Code::U01,
                    &format!("{current_ptr}/{X_ENUM}/{next}"),
                    "unmodelled enum entry member",
                );
            }
            for (member, code, message) in [
                ("name", Code::B08, "enum entry name changed"),
                ("description", Code::D01, "description changed"),
                ("category", Code::R02, "exit code category changed"),
                ("exit_code", Code::R03, "slug exit code changed"),
            ] {
                if x.get(member) != y.get(member) {
                    self.emit(code, &pointer, message);
                }
            }
        }
    }
}

fn reports(base: &Value, current: &Value) -> Diff {
    let mut walk = Schemas::new([base, current]);
    walk.keywords += [base, current]
        .iter()
        .map(|document| document.as_object().map_or(0, Map::len))
        .sum::<usize>();
    let (old, new) = (entries(base, "reports"), entries(current, "reports"));
    let mut wrappers = Vec::new();
    for (name, site) in &old {
        let pointer = format!("/reports/{}", escape(name));
        walk.subject = (*name).to_owned();
        match new.get(name) {
            None => {
                walk.keywords += raw(site);
                walk.emit(Code::B01, &pointer, "report root removed");
            }
            Some(next) => {
                walk.visiting.clear();
                walk.node(site, next, &pointer, &pointer, true);
                wrappers.extend(subset::ref_target(site));
            }
        }
    }
    for (name, site) in &new {
        if !old.contains_key(name) {
            walk.keywords += raw(site);
        }
    }
    walk.merge_mirrors(&wrappers);
    walk.finish()
}

impl Schemas<'_> {
    /// A wrapper `<Name>Root` inlines its payload `<Name>`. When another root also reaches the payload, the change is
    /// reported once at the payload and the wrapper's copy joins its subjects; otherwise it stays on the wrapper.
    fn merge_mirrors(&mut self, wrappers: &[&str]) {
        for wrapper in wrappers {
            let Some(payload) = wrapper.strip_suffix("Root") else {
                continue;
            };
            let (from, to) = (
                format!("/$defs/{}", escape(wrapper)),
                format!("/$defs/{}", escape(payload)),
            );
            let under = format!("{from}/");
            let mirrored: Vec<_> = self
                .found
                .keys()
                .filter(|(pointer, _)| *pointer == from || pointer.starts_with(&under))
                .cloned()
                .collect();
            for key in mirrored {
                let target = (format!("{to}{}", &key.0[from.len()..]), key.1);
                if self.found.contains_key(&target)
                    && let Some(mirror) = self.found.remove(&key)
                    && let Some(shared) = self.found.get_mut(&target)
                {
                    shared.subjects.extend(mirror.subjects);
                }
            }
        }
    }
}

fn errors(base: &Value, current: &Value) -> Diff {
    let mut walk = Schemas::new([base, current]);
    walk.subject = "*".to_owned();
    walk.node(base, current, "", "", true);
    walk.finish()
}

// The grammar: `cli.json` is read as the input direction, so what breaks is what a script already typed.

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn names<'a>(value: &'a Value, key: &str) -> BTreeSet<&'a str> {
    list(value, key).iter().filter_map(Value::as_str).collect()
}

fn flag(value: &Value, key: &str) -> bool {
    value.get(key) == Some(&Value::Bool(true))
}

fn present(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(|member| !member.is_null())
}

/// A member that carries meaning only when set: absent, `null` and `false` read alike.
fn set<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value
        .get(key)
        .filter(|member| !member.is_null() && **member != Value::Bool(false))
}

fn command_path(command: &Value) -> Option<String> {
    let words = list(command, "path")
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    Some(words.join(" "))
}

/// The versioned commands that carry `command`: itself when versioned, else every versioned command below it.
fn subjects_of(command: &Value) -> BTreeSet<String> {
    let mut subjects = BTreeSet::new();
    let mut pending = vec![command];
    while let Some(node) = pending.pop() {
        if node.get("version").and_then(Value::as_u64).unwrap_or(0) > 0 {
            subjects.extend(command_path(node));
        } else {
            pending.extend(list(node, "commands"));
        }
    }
    subjects
}

fn release_components(release: &str) -> Vec<u64> {
    let mut parts: Vec<u64> = release
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect();
    parts.resize(parts.len().max(3), 0);
    parts
}

/// Whether `release` is at or before this build's.
fn release_reached(release: &str) -> bool {
    release_components(release) <= release_components(CURRENT_RELEASE)
}

/// A hidden, deprecated item whose named removal release has arrived may be deleted.
fn removable(item: &Value) -> bool {
    flag(item, "hidden")
        && item
            .pointer("/deprecated/removal")
            .and_then(Value::as_str)
            .is_some_and(release_reached)
}

#[derive(Default)]
struct Grammar {
    found: Found,
    keywords: usize,
}

fn cli(base: &Value, current: &Value) -> Diff {
    let mut grammar = Grammar::default();
    for document in [base, current] {
        if let Some(map) = document.as_object() {
            grammar.keywords += shallow(map, &["root", "env", "retired"]);
        }
        grammar.keywords += document.get("env").map_or(0, raw) + document.get("retired").map_or(0, raw);
    }
    match (base.get("root"), current.get("root")) {
        (Some(a), Some(b)) => grammar.command(a, b, "/root", "/root"),
        (a, b) => grammar.keywords += a.map_or(0, raw) + b.map_or(0, raw),
    }
    grammar.env(base, current);
    Diff {
        findings: grammar.found.into_values().collect(),
        keywords: grammar.keywords,
    }
}

impl Grammar {
    fn emit(&mut self, code: Code, pointer: &str, subjects: &BTreeSet<String>, message: &str) {
        record(&mut self.found, code, pointer, subjects, message);
    }

    fn command(&mut self, a: &Value, b: &Value, base_ptr: &str, current_ptr: &str) {
        let (Some(old_map), Some(new_map)) = (a.as_object(), b.as_object()) else {
            return;
        };
        let children = ["args", "groups", "commands"];
        self.keywords += shallow(old_map, &children) + shallow(new_map, &children);
        let subjects = subjects_of(a);
        if !flag(a, "hidden") && flag(b, "hidden") && !present(b, "deprecated") {
            self.emit(Code::G01, base_ptr, &subjects, "command hidden without a deprecation");
        }
        let output = list(b, "output");
        for (index, mode) in list(a, "output").iter().enumerate() {
            if !output.contains(mode) {
                self.emit(
                    Code::G12,
                    &format!("{base_ptr}/output/{index}"),
                    &subjects,
                    "output mode removed or changed",
                );
            }
        }
        self.args(a, b, (base_ptr, current_ptr), &subjects);
        self.groups(a, b, (base_ptr, current_ptr), &subjects);
        let by_path: BTreeMap<String, (usize, &Value)> = list(b, "commands")
            .iter()
            .enumerate()
            .filter_map(|(index, child)| Some((command_path(child)?, (index, child))))
            .collect();
        for (index, child) in list(a, "commands").iter().enumerate() {
            let pointer = format!("{base_ptr}/commands/{index}");
            match command_path(child).and_then(|path| by_path.get(&path)) {
                Some(&(next, other)) => self.command(child, other, &pointer, &format!("{current_ptr}/commands/{next}")),
                None => {
                    self.keywords += raw(child);
                    if !removable(child) {
                        self.emit(Code::G01, &pointer, &subjects_of(child), "command removed");
                    }
                }
            }
        }
        for child in list(b, "commands") {
            if !list(a, "commands")
                .iter()
                .any(|old| command_path(old) == command_path(child))
            {
                self.keywords += raw(child);
            }
        }
    }

    fn args(&mut self, a: &Value, b: &Value, at: (&str, &str), subjects: &BTreeSet<String>) {
        let (base_ptr, current_ptr) = at;
        let (old, new) = (list(a, "args"), list(b, "args"));
        for (index, arg) in old.iter().enumerate() {
            let pointer = format!("{base_ptr}/args/{index}");
            match new.iter().find(|other| text(other, "id") == text(arg, "id")) {
                Some(next) => {
                    self.keywords += raw(arg) + raw(next);
                    self.arg(arg, next, &pointer, subjects);
                }
                None => {
                    self.keywords += raw(arg);
                    self.arg_removed(arg, new, &pointer, subjects);
                }
            }
        }
        for (index, arg) in new.iter().enumerate() {
            if !old.iter().any(|other| text(other, "id") == text(arg, "id")) {
                self.keywords += raw(arg);
                if flag(arg, "required") {
                    self.emit(
                        Code::G04,
                        &format!("{current_ptr}/args/{index}"),
                        subjects,
                        "required argument added",
                    );
                }
            }
        }
    }

    /// A removed argument is silent when its window has closed. Otherwise each spelling it had must live on under
    /// another id, and the argument that carries one is compared like a surviving one.
    fn arg_removed(&mut self, arg: &Value, current: &[Value], pointer: &str, subjects: &BTreeSet<String>) {
        if removable(arg) {
            return;
        }
        let mut carrier = None;
        let mut spelled = false;
        for (key, code, message) in [
            ("long", Code::G02, "long flag removed"),
            ("short", Code::G03, "short flag removed"),
        ] {
            if !present(arg, key) {
                continue;
            }
            spelled = true;
            match current.iter().find(|other| text(other, key) == text(arg, key)) {
                Some(other) => {
                    carrier.get_or_insert(other);
                }
                None => self.emit(code, pointer, subjects, message),
            }
        }
        if !spelled {
            self.emit(Code::G07, pointer, subjects, "positional removed");
        }
        if let Some(next) = carrier {
            self.arg(arg, next, pointer, subjects);
        }
    }

    fn arg(&mut self, a: &Value, b: &Value, pointer: &str, subjects: &BTreeSet<String>) {
        let mut breaks: Vec<(Code, &str)> = Vec::new();
        if present(a, "long") && text(a, "long") != text(b, "long") {
            breaks.push((Code::G02, "long flag changed"));
        }
        if present(a, "short") && text(a, "short") != text(b, "short") {
            breaks.push((Code::G03, "short letter changed"));
        }
        let unless = names(b, "required_unless");
        if (!flag(a, "required") && flag(b, "required"))
            || (flag(a, "required") && flag(b, "required") && !names(a, "required_unless").is_subset(&unless))
        {
            breaks.push((Code::G04, "argument became required"));
        }
        let kind = |arg: &Value| arg.pointer("/value/type").and_then(Value::as_str).map(str::to_owned);
        if kind(a) != kind(b) {
            breaks.push((Code::G06, "value type changed"));
        }
        let arity = |arg: &Value| {
            let count = |key: &str| arg.pointer(&format!("/num_args/{key}")).and_then(Value::as_u64);
            (count("min").unwrap_or(0), count("max"))
        };
        let ((min_a, max_a), (min_b, max_b)) = (arity(a), arity(b));
        let narrowed = min_b > min_a
            || matches!((max_a, max_b), (None, Some(_)))
            || max_a.zip(max_b).is_some_and(|(old, new)| new < old);
        if a.get("position") != b.get("position")
            || narrowed
            || (flag(a, "repeatable") && !flag(b, "repeatable"))
            || ["last", "trailing_var_arg", "value_terminator"]
                .iter()
                .any(|key| set(a, key) != set(b, key))
        {
            breaks.push((Code::G07, "positional or arity semantics changed"));
        }
        if flag(a, "global") && !flag(b, "global") {
            breaks.push((Code::G08, "flag no longer global"));
        }
        if list(a, "default") != list(b, "default") {
            breaks.push((Code::G09, "default changed"));
        }
        if ["requires", "conflicts"]
            .iter()
            .any(|key| !names(b, key).is_subset(&names(a, key)))
        {
            breaks.push((Code::G10, "requires or conflicts added"));
        }
        if (!flag(a, "require_equals") && flag(b, "require_equals"))
            || (flag(a, "allow_hyphen_values") && !flag(b, "allow_hyphen_values"))
        {
            breaks.push((Code::G11, "require_equals or allow_hyphen_values tightened"));
        }
        if flag(a, "stdin_secret") != flag(b, "stdin_secret") {
            breaks.push((Code::G15, "stdin_secret changed"));
        }
        for (code, message) in breaks {
            self.emit(code, pointer, subjects, message);
        }
        if kind(a).as_deref() == Some("choice") && kind(b) == kind(a) {
            let choices = |arg: &Value| -> Vec<Value> {
                arg.pointer("/value/choices")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            let kept = choices(b);
            for (index, choice) in choices(a).iter().enumerate() {
                if !kept.iter().any(|other| text(other, "value") == text(choice, "value")) {
                    self.emit(
                        Code::G05,
                        &format!("{pointer}/value/choices/{index}"),
                        subjects,
                        "choice removed",
                    );
                }
            }
        }
    }

    fn groups(&mut self, a: &Value, b: &Value, at: (&str, &str), subjects: &BTreeSet<String>) {
        let (base_ptr, current_ptr) = at;
        let (old, new) = (list(a, "groups"), list(b, "groups"));
        for (index, group) in old.iter().enumerate() {
            let next = new.iter().find(|other| text(other, "id") == text(group, "id"));
            self.keywords += raw(group) + next.map_or(0, raw);
            if !flag(group, "required") && next.is_some_and(|next| flag(next, "required")) {
                self.emit(
                    Code::G10,
                    &format!("{base_ptr}/groups/{index}"),
                    subjects,
                    "group made required",
                );
            }
        }
        for (index, group) in new.iter().enumerate() {
            if !old.iter().any(|other| text(other, "id") == text(group, "id")) {
                self.keywords += raw(group);
                if flag(group, "required") {
                    self.emit(
                        Code::G10,
                        &format!("{current_ptr}/groups/{index}"),
                        subjects,
                        "required group added",
                    );
                }
            }
        }
    }

    /// The env-removal and env-type rules read the Public entries of `env` and the windows of `retired`.
    fn env(&mut self, base: &Value, current: &Value) {
        let everywhere = BTreeSet::from(["*".to_owned()]);
        let windows: BTreeSet<&str> = list(current, "retired")
            .iter()
            .filter(|entry| text(entry, "status") == Some("window"))
            .filter_map(|entry| text(entry, "name"))
            .collect();
        let public = |entry: &&Value| text(entry, "visibility") == Some("public");
        let now: Vec<&Value> = list(current, "env").iter().filter(public).collect();
        for (index, old) in list(base, "env").iter().enumerate().filter(|(_, entry)| public(entry)) {
            let pointer = format!("/env/{index}");
            let name = text(old, "name");
            let Some(next) = now.iter().find(|entry| text(entry, "name") == name) else {
                if !name.is_some_and(|name| windows.contains(name)) {
                    self.emit(
                        Code::G13,
                        &pointer,
                        &everywhere,
                        "public env variable removed without a window",
                    );
                }
                continue;
            };
            let refused = text(old, "on_invalid") != Some("error") && text(next, "on_invalid") == Some("error");
            if text(old, "value") != text(next, "value")
                || !names(old, "choices").is_subset(&names(next, "choices"))
                || refused
            {
                self.emit(Code::G14, &pointer, &everywhere, "public env value contract narrowed");
            }
        }
    }
}

/// What a ledger entry acknowledges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryRule {
    /// A differ code.
    Code(Code),
    /// A reworded description; no bump.
    Doc,
    /// A meaning change under the same name; bumps.
    Semantic,
}

impl EntryRule {
    /// The ledger spelling.
    pub fn id(self) -> &'static str {
        match self {
            Self::Code(code) => code.id(),
            Self::Doc => "doc",
            Self::Semantic => "semantic",
        }
    }

    /// The entry a reader adds for a finding under `code`: a reworded description is `doc`.
    fn acknowledging(code: Code) -> Self {
        if code == Code::D01 { Self::Doc } else { Self::Code(code) }
    }

    /// Whether the entry speaks to a finding under `code`; `doc` and `semantic` stand for a changed description.
    fn speaks_to(self, code: Code) -> bool {
        match self {
            Self::Code(own) => own == code,
            Self::Doc | Self::Semantic => code == Code::D01,
        }
    }
}

impl<'de> Deserialize<'de> for EntryRule {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        match id.as_str() {
            "doc" => Ok(Self::Doc),
            "semantic" => Ok(Self::Semantic),
            other => Code::from_id(other)
                .map(Self::Code)
                .ok_or_else(|| serde::de::Error::custom(format!("unknown ledger rule `{other}`"))),
        }
    }
}

/// One acknowledged break in `crates/ocx_schema/contract/ledger.toml`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    /// `reports`, `errors` or `cli`.
    pub document: String,
    /// Report root names, command paths (`"package push"`), or `["*"]` for `errors`.
    pub subjects: Vec<String>,
    pub rule: EntryRule,
    /// The finding's pointer.
    pub pointer: String,
    /// Why the break is accepted.
    pub reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerFile {
    #[serde(rename = "break", default)]
    entries: Vec<LedgerEntry>,
}

/// Parses `ledger.toml`: a list of `[[break]]` tables.
///
/// # Errors
///
/// The text is not TOML, or an entry has a missing or unknown key.
pub fn parse_ledger(text: &str) -> Result<Vec<LedgerEntry>, toml::de::Error> {
    Ok(toml::from_str::<LedgerFile>(text)?.entries)
}

/// Why the gate refuses a set of documents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// A breaking finding no ledger entry acknowledges.
    Unacknowledged { kind: Kind, finding: Finding },
    /// A ledger entry that matches no finding (a `semantic` entry excepted).
    Stale(LedgerEntry),
    /// A surviving item's version is not baseline + 1 when an entry lists it, or not the baseline when none does;
    /// an added item's is not 1.
    Version {
        kind: Kind,
        subject: String,
        expected: u32,
        found: u32,
    },
    /// The differ read fewer keywords than the documents hold.
    ReaderFloor { kind: Kind, read: usize, floor: usize },
    /// A document's versions could not be read.
    Unversioned(crate::versions::VersionError),
}

/// Names the finding and the exact edit that turns the gate green: a ledger stanza to paste, or a version to set.
impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let quoted = |text: &str| serde_json::to_string(text).unwrap_or_default();
        match self {
            Self::Unacknowledged { kind, finding } => {
                let subjects: Vec<_> = finding.subjects.iter().map(|subject| quoted(subject)).collect();
                // A reworded description is a `doc` entry; use `semantic` instead when its meaning changed.
                let rule = EntryRule::acknowledging(finding.code);
                write!(
                    f,
                    "{}: {} at {} ({}); acknowledge it by adding to ledger.toml:\n[[break]]\ndocument = {}\nsubjects = [{}]\nrule = {}\npointer = {}\nreason = \"<why the break is accepted>\"",
                    document_name(*kind),
                    finding.message,
                    finding.pointer,
                    finding.code,
                    quoted(document_name(*kind)),
                    subjects.join(", "),
                    quoted(rule.id()),
                    quoted(&finding.pointer),
                )
            }
            Self::Stale(entry) => write!(
                f,
                "ledger.toml entry for {} {} at {} matches no finding; delete it",
                entry.document,
                entry.rule.id(),
                entry.pointer
            ),
            Self::Version {
                kind,
                subject,
                expected,
                found,
            } => write!(
                f,
                "bump {} {subject} to version {expected} (it is {found})",
                document_name(*kind)
            ),
            Self::ReaderFloor { kind, read, floor } => write!(
                f,
                "the differ read {read} keywords of the {} documents, which hold {floor}",
                document_name(*kind)
            ),
            Self::Unversioned(error) => write!(f, "{error}"),
        }
    }
}

fn document_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Reports => "reports",
        Kind::Errors => "errors",
        Kind::Cli => "cli",
        Kind::Input => "input",
    }
}

/// Whether `entry` speaks to `finding`, subjects aside: its document, pointer and rule, with `doc` and `semantic`
/// standing for a changed description.
fn speaks_to(entry: &LedgerEntry, kind: Kind, finding: &Finding) -> bool {
    entry.document == document_name(kind) && entry.pointer == finding.pointer && entry.rule.speaks_to(finding.code)
}

/// Runs the gate over all three documents: diff each, match every breaking finding to an entry, flag stale entries,
/// then check every version.
pub fn gate(base: &Documents, current: &Documents, ledger: &[LedgerEntry]) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut spoken = vec![false; ledger.len()];
    for (kind, old, new) in [
        (Kind::Reports, &base.reports, &current.reports),
        (Kind::Errors, &base.errors, &current.errors),
        (Kind::Cli, &base.cli, &current.cli),
    ] {
        let run = diff(old, new, kind);
        let floor = raw(old) + raw(new);
        if run.keywords < floor {
            violations.push(Violation::ReaderFloor {
                kind,
                read: run.keywords,
                floor,
            });
        }
        for finding in run.findings {
            let mut acknowledged = false;
            for (index, entry) in ledger.iter().enumerate() {
                if speaks_to(entry, kind, &finding) {
                    spoken[index] = true;
                    acknowledged |= finding.subjects.iter().all(|subject| entry.subjects.contains(subject));
                }
            }
            if !acknowledged {
                violations.push(Violation::Unacknowledged { kind, finding });
            }
        }
    }
    violations.extend(
        ledger
            .iter()
            .zip(&spoken)
            .filter(|(entry, spoken)| !**spoken && entry.rule != EntryRule::Semantic)
            .map(|(entry, _)| Violation::Stale(entry.clone())),
    );
    match (versions::read(base), versions::read(current)) {
        (Ok(old), Ok(new)) => violations.extend(version_violations(base, current, &old, &new, ledger)),
        (old, new) => violations.extend([old.err(), new.err()].into_iter().flatten().map(Violation::Unversioned)),
    }
    violations
}

/// The in-band `schema_version` of the error document.
fn errors_schema_version(errors: &Value) -> Option<u32> {
    let version = errors.pointer("/properties/schema_version/const")?.as_u64()?;
    u32::try_from(version).ok()
}

fn version_violations(
    base: &Documents,
    current: &Documents,
    old: &versions::Versions,
    new: &versions::Versions,
    ledger: &[LedgerEntry],
) -> Vec<Violation> {
    // A breaking or `semantic` entry that lists the subject bumps it; a `doc` entry bumps nothing. `"*"` names the
    // error document's own versions: the env registry is document-wide and has no version to bump.
    let bumped = |kind: Kind, subject: &str| {
        let bump = ledger.iter().any(|entry| {
            entry.document == document_name(kind)
                && entry.rule != EntryRule::Doc
                && entry
                    .subjects
                    .iter()
                    .any(|listed| listed == subject || (kind == Kind::Errors && listed == "*"))
        });
        u32::from(bump)
    };
    let mut out = Vec::new();
    let mut check = |kind: Kind, subject: &str, baseline: Option<u32>, found: u32| {
        let expected = baseline.map_or(1, |baseline| baseline + bumped(kind, subject));
        if expected != found {
            out.push(Violation::Version {
                kind,
                subject: subject.to_owned(),
                expected,
                found,
            });
        }
    };
    for (name, found) in &new.reports {
        check(Kind::Reports, name, old.reports.get(name).copied(), *found);
    }
    for (path, found) in &new.commands {
        check(Kind::Cli, path, old.commands.get(path).copied(), *found);
    }
    check(Kind::Errors, "$id", Some(old.errors), new.errors);
    if let (Some(before), Some(after)) = (
        errors_schema_version(&base.errors),
        errors_schema_version(&current.errors),
    ) {
        check(Kind::Errors, "schema_version", Some(before), after);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_is_reached_at_or_below_the_build() {
        assert!(release_reached(CURRENT_RELEASE));
        assert!(release_reached("0.1"));
        assert!(!release_reached("99.0"));
        assert!(!release_reached("99.0.0-rc1"));
    }

    #[test]
    fn release_components_pad_and_ignore_a_suffix() {
        assert_eq!(release_components("v0.6"), [0, 6, 0]);
        assert_eq!(release_components("1.2.3-dev"), [1, 2, 3]);
    }
}
