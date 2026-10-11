// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Reads the [`DocsModel`] from its sources: the clap tree, the env registry, the published schemas and
//! the exit-code vocabulary.

use std::collections::{BTreeMap, BTreeSet};

use clap::{Arg, ArgAction, Command};
use ocx_env::{EnvValue, EnvVar, Visibility};
use ocx_exit::ExitCode;
use serde_json::Value;

use super::{CommandEntry, DocsModel, EnvVarEntry, ExitCodeEntry, FlagEntry, KeyEntry, SlugEntry};

pub(super) fn model() -> DocsModel {
    let mut root = crate::cli::ocx_command();
    root.build();
    let no_globals = BTreeSet::new();
    let mut commands = Vec::new();
    walk(&root, &mut Vec::new(), &no_globals, &mut commands);
    DocsModel {
        commands,
        global_flags: flags(&root, &no_globals, true),
        env_vars: env_vars(),
        config_keys: keys("config"),
        project_keys: keys("project"),
        exit_codes: exit_codes(),
    }
}

/// Visible commands below `cmd`, depth first, in declaration order.
fn walk(cmd: &Command, path: &mut Vec<String>, inherited: &BTreeSet<String>, out: &mut Vec<CommandEntry>) {
    let mut globals = inherited.clone();
    globals.extend(
        cmd.get_arguments()
            .filter(|arg| arg.is_global_set())
            .map(|arg| arg.get_id().to_string()),
    );
    for sub in cmd
        .get_subcommands()
        .filter(|sub| !sub.is_hide_set() && sub.get_name() != "help")
    {
        path.push(sub.get_name().to_owned());
        let (summary, description) = split_help(
            sub.get_about().map(ToString::to_string),
            sub.get_long_about().map(ToString::to_string),
        );
        out.push(CommandEntry {
            path: path.clone(),
            summary,
            description,
            usage: usage(sub),
            aliases: sub.get_visible_aliases().map(str::to_owned).collect(),
            flags: flags(sub, &globals, false),
        });
        walk(sub, path, &globals, out);
        path.pop();
    }
}

fn usage(cmd: &Command) -> String {
    cmd.clone()
        .render_usage()
        .to_string()
        .lines()
        .map(|line| line.trim().trim_start_matches("Usage:").trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The visible `--long` flags `cmd` declares itself; `root` joins each to the variable that stands in for it.
fn flags(cmd: &Command, inherited: &BTreeSet<String>, root: bool) -> Vec<FlagEntry> {
    cmd.get_arguments()
        .filter(|arg| !is_builtin(arg) && !arg.is_hide_set() && !inherited.contains(arg.get_id().as_str()))
        .filter_map(|arg| {
            let long = arg.get_long()?;
            let (help, description) = split_help(
                arg.get_help().map(ToString::to_string),
                arg.get_long_help().map(ToString::to_string),
            );
            let env = root
                .then(|| {
                    ocx::app::ENV_FLAGS
                        .iter()
                        .find(|(_, flag)| flag.strip_prefix("--") == Some(long))
                        .map(|(var, _)| var.name.to_owned())
                })
                .flatten();
            Some(FlagEntry {
                long: long.to_owned(),
                short: arg.get_short(),
                value: arg.get_action().takes_values().then(|| value_placeholder(arg)),
                env,
                help,
                description,
            })
        })
        .collect()
}

fn is_builtin(arg: &Arg) -> bool {
    matches!(
        arg.get_action(),
        ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
    )
}

/// The accepted values, each name followed by its aliases in parentheses, joined by `|` when the flag is a closed set, else its value name.
///
/// A flag that hides its possible values lists them in its help text instead.
fn value_placeholder(arg: &Arg) -> String {
    let choices: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|value| !arg.is_hide_possible_values_set() && !value.is_hide_set())
        .map(|value| {
            let aliases: Vec<&str> = value.get_name_and_aliases().skip(1).collect();
            match aliases.as_slice() {
                [] => value.get_name().to_owned(),
                _ => format!("{} ({})", value.get_name(), aliases.join(", ")),
            }
        })
        .collect();
    if !choices.is_empty() {
        return choices.join("|");
    }
    arg.get_value_names()
        .and_then(<[_]>::first)
        .map_or_else(|| arg.get_id().as_str().to_uppercase(), |name| name.as_str().to_owned())
}

/// `(summary, description)` of a clap help pair.
///
/// clap derive repeats the short paragraph as the first paragraph of the long text; that repeat is dropped.
fn split_help(short: Option<String>, long: Option<String>) -> (String, String) {
    let long = long.unwrap_or_default();
    let long = long.trim();
    let (first, rest) = long.split_once("\n\n").unwrap_or((long, ""));
    let summary = short
        .map(|text| one_line(&text))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| one_line(first));
    let repeats = one_line(first).trim_end_matches('.') == summary.trim_end_matches('.');
    let description = if long.is_empty() {
        String::new()
    } else if repeats {
        rest.trim().to_owned()
    } else {
        long.to_owned()
    };
    (summary, description)
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn env_vars() -> Vec<EnvVarEntry> {
    ocx_env::all()
        .filter(|var| var.visibility != Visibility::Testing)
        .map(|var| {
            // Declaration docs keep rustdoc's single leading space on every line.
            let doc: Vec<&str> = var
                .doc
                .lines()
                .map(|line| line.strip_prefix(' ').unwrap_or(line))
                .collect();
            let doc = doc.join("\n");
            let (first, rest) = doc.trim().split_once("\n\n").unwrap_or((doc.trim(), ""));
            EnvVarEntry {
                name: var.name.to_owned(),
                visibility: match var.visibility {
                    Visibility::Public => "public",
                    Visibility::Foreign => "foreign",
                    Visibility::Plumbing | Visibility::Testing => "plumbing",
                }
                .to_owned(),
                value: value_grammar(var),
                summary: one_line(first),
                description: rest.trim().to_owned(),
                flag: ocx::app::ENV_FLAGS
                    .iter()
                    .find(|(flagged, _)| std::ptr::eq(*flagged, var))
                    .map(|(_, flag)| (*flag).to_owned()),
            }
        })
        .collect()
}

fn value_grammar(var: &EnvVar) -> String {
    match var.value {
        EnvValue::Bool => "bool".to_owned(),
        EnvValue::TriBool => "tri_bool".to_owned(),
        EnvValue::String => "string".to_owned(),
        EnvValue::Integer => "integer".to_owned(),
        EnvValue::Path => "path".to_owned(),
        EnvValue::PathList => "path_list".to_owned(),
        EnvValue::PathOrPem => "path_or_pem".to_owned(),
        EnvValue::HostList => "host_list".to_owned(),
        EnvValue::Choice(choices) => format!("choice: {}", choices.join(", ")),
        EnvValue::Json => "json".to_owned(),
    }
}

fn exit_codes() -> Vec<ExitCodeEntry> {
    let registry = ocx::exit::detail_registry();
    ExitCode::ALL
        .iter()
        .map(|code| {
            let mut slugs: Vec<SlugEntry> = registry
                .iter()
                .filter(|entry| entry.exit_code == *code)
                .map(|entry| SlugEntry {
                    slug: entry.slug.to_owned(),
                    summary: one_line(entry.summary),
                })
                .collect();
            slugs.sort_by(|a, b| a.slug.cmp(&b.slug));
            slugs.dedup_by(|a, b| a.slug == b.slug);
            let kind = match code {
                ExitCode::Success => String::new(),
                other => serde_json::to_value(other.category())
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            };
            ExitCodeEntry {
                code: *code as u8,
                name: format!("{code:?}"),
                summary: code.summary().to_owned(),
                kind,
                slugs,
            }
        })
        .collect()
}

/// Every key of the published schema `kind`, tables and map values included.
fn keys(kind: &str) -> Vec<KeyEntry> {
    let schema: Value = crate::schema_for(kind)
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or(Value::Null);
    let mut walker = Walker {
        defs: schema.get("$defs").cloned().unwrap_or(Value::Null),
        out: Vec::new(),
        seen: BTreeSet::new(),
        tags: BTreeMap::new(),
    };
    if let Some((root, stack)) = walker.resolve(&schema, &[]) {
        walker.children(&[], &root, &stack);
    }
    walker.out
}

type Resolved = (Value, Vec<String>);

/// Walks a draft 2020-12 schema as schemars writes it: `$ref` into `$defs`, `anyOf`/`oneOf` unions,
/// `additionalProperties` maps and `items` arrays.
struct Walker {
    defs: Value,
    out: Vec<KeyEntry>,
    seen: BTreeSet<String>,
    /// The values of a union's tag property, by dotted key: the tag is declared in every branch, so its
    /// entry would otherwise list only the first branch's value.
    tags: BTreeMap<String, Vec<(String, String)>>,
}

impl Walker {
    /// Follows `$ref`s; the referring node's own keys win. `None` on a reference cycle or a dangling one.
    fn resolve(&self, node: &Value, stack: &[String]) -> Option<Resolved> {
        let mut node = node.clone();
        let mut stack = stack.to_vec();
        while let Some(target) = node.get("$ref").and_then(Value::as_str).map(str::to_owned) {
            let name = target.rsplit('/').next().unwrap_or_default().to_owned();
            if stack.contains(&name) {
                return None;
            }
            let mut merged = self.defs.get(&name)?.clone();
            overlay(&mut merged, &node, &["$ref"]);
            stack.push(name);
            node = merged;
        }
        Some((node, stack))
    }

    /// The non-null branches of `anyOf`/`oneOf`, resolved.
    fn variants(&self, node: &Value, stack: &[String]) -> Vec<Resolved> {
        ["anyOf", "oneOf"]
            .iter()
            .filter_map(|keyword| node.get(*keyword).and_then(Value::as_array))
            .flatten()
            .filter(|branch| branch.get("type").and_then(Value::as_str) != Some("null"))
            .filter_map(|branch| self.resolve(branch, stack))
            .collect()
    }

    fn visit(&mut self, path: &[String], node: &Value, stack: &[String], outer_desc: Option<&str>) {
        let Some((mut node, mut stack)) = self.resolve(node, stack) else {
            return;
        };
        let mut branches = self.variants(&node, &stack);
        if branches.len() == 1 {
            let (mut inner, inner_stack) = branches.remove(0);
            overlay(&mut inner, &node, &["anyOf", "oneOf"]);
            node = inner;
            stack = inner_stack;
            // The wrapper was an `Option<T>` around `T`; `T` may be a union of its own.
            branches = self.variants(&node, &stack);
        }
        let key = path.join(".");
        let tag_values = self.tags.get(&key).cloned();
        let desc = outer_desc
            .map(str::to_owned)
            .or_else(|| description(&node))
            .or_else(|| branches.iter().find_map(|(branch, _)| description(branch)))
            .or_else(|| {
                tag_values
                    .is_some()
                    .then(|| "Selects the variant of this entry; the other fields depend on the value.".to_owned())
            })
            .unwrap_or_default();
        // A map is its value, keyed `<name>`: `[registries.<name>]`, not a `registries` table beside it.
        let is_map = node.get("properties").is_none() && branches.is_empty();
        if is_map && let Some(value) = node.get("additionalProperties").filter(|value| value.is_object()) {
            let mut child = path.to_vec();
            child.push("<name>".to_owned());
            let outer = (!desc.is_empty()).then_some(desc.as_str());
            self.visit(&child, value, &stack, outer);
            return;
        }
        if self.seen.insert(key.clone()) {
            self.out.push(KeyEntry {
                key,
                type_name: type_name(&node, &branches),
                default: node.get("default").map(ToString::to_string),
                env: None,
                summary: desc,
                values: tag_values.unwrap_or_else(|| enum_values(&node, &branches)),
            });
        }
        self.children(path, &node, &stack);
    }

    /// Visits what `node` nests under `path`: properties, a map's values, array items and union branches.
    fn children(&mut self, path: &[String], node: &Value, stack: &[String]) {
        if let Some(properties) = node.get("properties").and_then(Value::as_object) {
            for (name, property) in properties {
                let mut child = path.to_vec();
                child.push(name.clone());
                self.visit(&child, property, stack, None);
            }
        }
        if node.get("properties").is_some()
            && let Some(value) = node.get("additionalProperties").filter(|value| value.is_object())
        {
            let mut child = path.to_vec();
            child.push("<name>".to_owned());
            self.visit(&child, value, stack, None);
        }
        if let Some((items, items_stack)) = node
            .get("items")
            .filter(|items| items.is_object())
            .and_then(|items| self.resolve(items, stack))
        {
            self.children(path, &items, &items_stack);
        }
        let branches = self.variants(node, stack);
        if let Some((field, values)) = tag_values(&branches) {
            let mut tag = path.to_vec();
            tag.push(field);
            self.tags.insert(tag.join("."), values);
        }
        for (branch, branch_stack) in branches {
            self.children(path, &branch, &branch_stack);
        }
    }
}

/// Copies `over`'s keys onto `base`, except the named ones.
///
/// `properties` are merged, not replaced: a union branch that tags itself with `kind` and `$ref`s the
/// struct holding the rest must keep both.
fn overlay(base: &mut Value, over: &Value, skip: &[&str]) {
    if let (Some(base), Some(over)) = (base.as_object_mut(), over.as_object()) {
        for (key, value) in over {
            if skip.contains(&key.as_str()) {
                continue;
            }
            match (key.as_str(), base.get_mut(key), value) {
                ("properties", Some(Value::Object(held)), Value::Object(added)) => {
                    held.extend(added.iter().map(|(name, property)| (name.clone(), property.clone())));
                }
                _ => {
                    base.insert(key.clone(), value.clone());
                }
            }
        }
    }
}

/// The first sentence of a schema description, on one line.
fn first_sentence(text: &str) -> String {
    let line = one_line(text.trim().split("\n\n").next().unwrap_or_default());
    match line.find(". ") {
        Some(end) => line[..=end].to_owned(),
        None => line,
    }
}

/// `(value, meaning)` of a closed value set: `const` branches of a union, an `enum` list or a lone `const`.
fn enum_values(node: &Value, branches: &[Resolved]) -> Vec<(String, String)> {
    let literal = |value: &Value| value.as_str().map_or_else(|| value.to_string(), str::to_owned);
    if !branches.is_empty() && branches.iter().all(|(branch, _)| branch.get("const").is_some()) {
        return branches
            .iter()
            .filter_map(|(branch, _)| {
                let meaning = description(branch)
                    .map(|text| first_sentence(&text))
                    .unwrap_or_default();
                branch.get("const").map(|value| (literal(value), meaning))
            })
            .collect();
    }
    if let Some(values) = node.get("enum").and_then(Value::as_array) {
        return values.iter().map(|value| (literal(value), String::new())).collect();
    }
    node.get("const")
        .map(|value| vec![(literal(value), String::new())])
        .unwrap_or_default()
}

/// The tag property every branch of a union fixes with a `const`, with each branch's value and meaning.
fn tag_values(branches: &[Resolved]) -> Option<(String, Vec<(String, String)>)> {
    if branches.len() < 2 {
        return None;
    }
    let (first, _) = branches.first()?;
    let field = first
        .get("properties")?
        .as_object()?
        .iter()
        .find(|(_, property)| property.get("const").is_some())
        .map(|(name, _)| name.clone())?;
    let values = branches
        .iter()
        .map(|(branch, _)| {
            let value = branch
                .pointer(&format!("/properties/{field}/const"))?
                .as_str()?
                .to_owned();
            let meaning = description(branch)
                .map(|text| first_sentence(&text))
                .unwrap_or_default();
            Some((value, meaning))
        })
        .collect::<Option<Vec<_>>>()?;
    Some((field, values))
}

fn description(node: &Value) -> Option<String> {
    node.get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn type_name(node: &Value, branches: &[Resolved]) -> String {
    if !branches.is_empty() {
        if branches
            .iter()
            .all(|(branch, _)| branch.get("const").is_some() || branch.get("enum").is_some())
        {
            return branches
                .first()
                .and_then(|(branch, _)| branch.get("type").and_then(Value::as_str))
                .unwrap_or("enum")
                .to_owned();
        }
        let mut names: Vec<String> = branches
            .iter()
            .map(|(branch, _)| type_name(branch, &[]))
            .filter(|name| !name.is_empty())
            .collect();
        names.dedup();
        return names.join(" or ");
    }
    match node.get("type") {
        Some(Value::String(name)) if name == "object" && node.get("properties").is_some() => "table".to_owned(),
        Some(Value::String(name)) => name.clone(),
        Some(Value::Array(names)) => names
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| *name != "null")
            .collect::<Vec<_>>()
            .join(" or "),
        _ if node.get("enum").is_some() || node.get("const").is_some() => "enum".to_owned(),
        _ => String::new(),
    }
}
