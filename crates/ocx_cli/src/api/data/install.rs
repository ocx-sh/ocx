// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use ocx_lib::cli::Cell;
use ocx_lib::{oci, package::metadata::Metadata};
use serde::Serialize;

use crate::api::Printable;

/// A single install or select result entry for CLI output.
///
/// The `path` field holds the symlink that was created or updated (candidate
/// for install, current for select), or `None` when no host symlink was written
/// — a foreign-platform install populates the object store but writes neither
/// host pointer (issue #179).
#[derive(Serialize)]
pub struct InstallEntry {
    pub identifier: oci::Identifier,
    pub metadata: Metadata,
    pub path: Option<PathBuf>,
}

/// Installed or selected packages keyed by the user-supplied identifier string.
///
/// Plain format: three-column table (Package | Version | Path).
///
/// JSON format: object keyed by package identifier, each value an
/// `{ identifier, metadata, path }` object.
#[derive(Serialize)]
pub struct Installs {
    #[serde(flatten)]
    pub packages: BTreeMap<String, InstallEntry>,
}

impl Installs {
    pub fn new(packages: HashMap<String, InstallEntry>) -> Self {
        // Collect into a `BTreeMap` so the table and JSON outputs key packages
        // in a reproducible order regardless of the incoming hash order.
        Self {
            packages: packages.into_iter().collect(),
        }
    }
}

impl Printable for Installs {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let theme = printer.theme();
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (package, entry) in &self.packages {
            rows[0].push(package.clone());
            rows[1].push(theme.of(&entry.identifier));
            rows[2].push(
                entry
                    .path
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |path| path.display().to_string()),
            );
        }
        printer.print_table(
            &["Package".into(), "Version".into(), "Path".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocx_lib::oci::Identifier;

    fn sample_metadata() -> Metadata {
        serde_json::from_str(r#"{"type":"bundle","version":1}"#).expect("metadata parses")
    }

    /// Assert the quoted keys appear in ascending byte order in the raw JSON.
    ///
    /// Scans the raw string rather than re-parsing: `serde_json::Value` stores
    /// objects in a `BTreeMap` and would re-sort keys, hiding a `HashMap`-order
    /// regression. Each package name appears only inside its own entry, so the
    /// first match of a quoted name is always its top-level key.
    fn assert_keys_ascending(json: &str, keys: &[&str]) {
        let positions: Vec<usize> = keys
            .iter()
            .map(|key| {
                json.find(&format!("\"{key}\""))
                    .unwrap_or_else(|| panic!("key {key:?} missing from output:\n{json}"))
            })
            .collect();
        let mut sorted = positions.clone();
        sorted.sort_unstable();
        assert_eq!(positions, sorted, "keys not in ascending order:\n{json}");
    }

    #[test]
    fn new_emits_sorted_package_keys() {
        // Fourteen keys make a HashMap-order match with sorted order vanishingly
        // unlikely, so this test fails on the former HashMap representation.
        let names = [
            "mike", "alpha", "zeta", "november", "bravo", "yankee", "charlie", "xray", "delta", "whiskey", "echo",
            "victor", "foxtrot", "uniform",
        ];
        let packages: HashMap<String, InstallEntry> = names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    InstallEntry {
                        identifier: Identifier::new_registry(*name, "registry.example"),
                        metadata: sample_metadata(),
                        path: Some(PathBuf::from(format!("/packages/{name}"))),
                    },
                )
            })
            .collect();

        let installs = Installs::new(packages);
        let json = serde_json::to_string_pretty(&installs).expect("serializes");

        assert_keys_ascending(
            &json,
            &[
                "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "mike", "november", "uniform", "victor",
                "whiskey", "xray", "yankee", "zeta",
            ],
        );
    }
}
