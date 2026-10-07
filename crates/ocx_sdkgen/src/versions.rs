// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The version numbers the documents carry: one per report root, one per versioned command, one for the error
//! document. The compat gate checks them against the baseline; the generator embeds them so an SDK refuses a
//! mismatched command before spawning it. The shape matches `ocx version --format json`'s `contract` field.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::Documents;

/// Every version a set of documents declares.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Versions {
    /// The error document's version: the major of its `$id` (`…/errors/v<N>.json`).
    pub errors: u32,
    /// Report root name → the `schema_version` const of its wrapper.
    pub reports: BTreeMap<String, u32>,
    /// Command path, its words joined by one space (`"package push"`) → its `version`. Groups carry 0 and are absent.
    pub commands: BTreeMap<String, u32>,
}

/// Where a document does not carry a version the contract requires.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
#[error("{pointer}: {reason}")]
pub struct VersionError {
    /// JSON pointer to the node that lacks a readable version.
    pub pointer: String,
    /// What is missing or malformed.
    pub reason: &'static str,
}

fn error(pointer: impl Into<String>, reason: &'static str) -> VersionError {
    VersionError {
        pointer: pointer.into(),
        reason,
    }
}

fn as_u32(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
}

/// Reads every version the three documents declare.
///
/// # Errors
///
/// A report root that is not a `$ref` to a wrapper with a `schema_version` const, an `errors` `$id` without a
/// `v<N>.json` tail, or a command node without an integer `version`.
pub fn read(documents: &Documents) -> Result<Versions, VersionError> {
    Ok(Versions {
        errors: errors_version(&documents.errors)?,
        reports: report_versions(&documents.reports)?,
        commands: command_versions(&documents.cli)?,
    })
}

fn errors_version(errors: &Value) -> Result<u32, VersionError> {
    errors
        .get("$id")
        .and_then(Value::as_str)
        .and_then(|id| id.rsplit('/').next())
        .and_then(|tail| tail.strip_prefix('v'))
        .and_then(|tail| tail.strip_suffix(".json"))
        .and_then(|major| major.parse().ok())
        .ok_or_else(|| error("/$id", "not an `…/v<N>.json` URL"))
}

fn report_versions(reports: &Value) -> Result<BTreeMap<String, u32>, VersionError> {
    let roots = reports
        .get("reports")
        .and_then(Value::as_object)
        .ok_or_else(|| error("/reports", "no root list"))?;
    roots
        .iter()
        .map(|(name, root)| {
            let pointer = format!("/reports/{name}");
            let wrapper = crate::subset::ref_target(root)
                .and_then(|def| reports.get("$defs")?.get(def))
                .ok_or_else(|| error(pointer.clone(), "not a `$ref` to a `$defs` wrapper"))?;
            let version = as_u32(wrapper.pointer("/properties/schema_version/const"))
                .ok_or_else(|| error(pointer, "wrapper has no integer `schema_version` const"))?;
            Ok((name.clone(), version))
        })
        .collect()
}

fn command_versions(cli: &Value) -> Result<BTreeMap<String, u32>, VersionError> {
    let mut versions = BTreeMap::new();
    let mut pending = vec![(
        String::from("/root"),
        cli.get("root").ok_or_else(|| error("/root", "no command tree"))?,
    )];
    while let Some((pointer, node)) = pending.pop() {
        let version = as_u32(node.get("version")).ok_or_else(|| error(pointer.clone(), "no integer `version`"))?;
        if version > 0 {
            let words = node
                .get("path")
                .and_then(Value::as_array)
                .and_then(|path| path.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
                .ok_or_else(|| error(pointer.clone(), "`path` is not a list of words"))?;
            versions.insert(words.join(" "), version);
        }
        let children = node
            .get("commands")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        pending.extend(
            children
                .iter()
                .enumerate()
                .map(|(index, child)| (format!("{pointer}/commands/{index}"), child)),
        );
    }
    Ok(versions)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn documents() -> Documents {
        Documents {
            reports: json!({
                "reports": {
                    "PushReport": {"$ref": "#/$defs/PushReportRoot"},
                    "StatusReport": {"$ref": "#/$defs/StatusReportRoot"},
                },
                "$defs": {
                    "PushReportRoot": {"properties": {"schema_version": {"type": "integer", "const": 2}}},
                    "StatusReportRoot": {"properties": {"schema_version": {"type": "integer", "const": 1}}},
                },
            }),
            errors: json!({"$id": "https://ocx.sh/schemas/errors/v3.json"}),
            cli: json!({
                "root": {"path": [], "version": 0, "commands": [
                    {"path": ["package"], "version": 0, "commands": [
                        {"path": ["package", "push"], "version": 4, "commands": []},
                    ]},
                    {"path": ["status"], "version": 1, "commands": []},
                ]},
            }),
        }
    }

    #[test]
    fn reads_every_root_command_and_the_errors_major() {
        let versions = read(&documents()).expect("readable");
        assert_eq!(versions.errors, 3);
        assert_eq!(
            versions.reports,
            BTreeMap::from([("PushReport".to_owned(), 2), ("StatusReport".to_owned(), 1)])
        );
        assert_eq!(
            versions.commands,
            BTreeMap::from([("package push".to_owned(), 4), ("status".to_owned(), 1)])
        );
    }

    #[test]
    fn a_wrapper_without_a_const_names_its_root() {
        let mut documents = documents();
        documents.reports["$defs"]["StatusReportRoot"]["properties"]["schema_version"]
            .as_object_mut()
            .expect("object")
            .remove("const");
        assert_eq!(read(&documents).unwrap_err().pointer, "/reports/StatusReport");
    }

    #[test]
    fn an_errors_id_without_a_major_is_refused() {
        let mut documents = documents();
        documents.errors = json!({"$id": "https://ocx.sh/schemas/errors/latest.json"});
        assert_eq!(read(&documents).unwrap_err().pointer, "/$id");
    }

    #[test]
    fn a_command_without_a_version_names_its_node() {
        let mut documents = documents();
        documents.cli["root"]["commands"][1]
            .as_object_mut()
            .expect("object")
            .remove("version");
        assert_eq!(read(&documents).unwrap_err().pointer, "/root/commands/1");
    }

    #[test]
    fn reads_the_committed_goldens() {
        let golden = |name: &str| -> Value {
            // A lexical parent, not `..`: under Bazel's runfiles this crate's own directory may not exist.
            let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("a crate directory");
            let path = crates.join("ocx_schema/tests/golden").join(format!("{name}.json"));
            let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        };
        let documents = Documents {
            reports: golden("reports"),
            errors: golden("errors"),
            cli: golden("cli"),
        };
        let versions = read(&documents).expect("the goldens carry every version");
        let roots = documents.reports["reports"].as_object().expect("root list").len();
        assert!(roots >= 54, "read {roots} roots");
        assert_eq!(versions.reports.len(), roots);
        assert_eq!(versions.errors, 2);
        assert_eq!(versions.commands.get("package push"), Some(&1));
    }
}
