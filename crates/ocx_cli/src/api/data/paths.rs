// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::path_kind::PathKind;

/// A resolved package and its package root, the parent of `content/` and `entrypoints/`.
pub struct PathEntry {
    pub package: String,
    pub path: PathBuf,
}

/// Resolved package roots for `ocx package pull`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Paths {
    /// Each package root, keyed by the identifier as given, in request order.
    #[serde(rename = "paths", serialize_with = "paths_by_package")]
    #[schemars(with = "std::collections::BTreeMap<String, PathBuf>")]
    pub entries: Vec<PathEntry>,
}

impl Paths {
    pub fn new(entries: Vec<PathEntry>) -> Self {
        Self { entries }
    }
}

/// Request order is the contract, so the entries serialize as a map without passing through a sorted one.
fn paths_by_package<S: serde::Serializer>(entries: &[PathEntry], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_map(entries.iter().map(|entry| (&entry.package, &entry.path)))
}

impl Printable for Paths {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "Paths";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.path.display().to_string());
        }
        printer.print_table(
            &["Package".into(), "Path".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

// `PathKind` is shared with `ocx pull`'s report; never re-spell it here.
/// One located package: the directory `ocx package which` reports for it, and
/// which kind of directory that is.
///
/// A tool composed lazily has no package directory until its first invocation
/// materializes one, so the answer to "where is this on disk" is its generated
/// shim tree. `kind` tells the two apart.
#[derive(Serialize, schemars::JsonSchema)]
pub struct LocatedPath {
    /// The requested identifier: serialized only as the map key, kept for the plain `Package` column.
    #[serde(skip)]
    pub package: String,
    /// The located directory.
    pub path: PathBuf,
    /// Whether `path` is a package root or a shim tree.
    pub kind: PathKind,
}

/// Located packages for `ocx package which`.
// Not a widened `Paths`: every `pull` row is a materialized root, so `kind` there would be constant.
#[derive(Serialize, schemars::JsonSchema)]
pub struct LocatedPaths {
    /// Each located package, keyed by the identifier as given, in request order.
    #[serde(rename = "paths", serialize_with = "located_by_package")]
    #[schemars(with = "std::collections::BTreeMap<String, LocatedPath>")]
    pub entries: Vec<LocatedPath>,
}

impl LocatedPaths {
    pub fn new(entries: Vec<LocatedPath>) -> Self {
        Self { entries }
    }
}

/// Request order is the contract, so the entries serialize as a map without passing through a sorted one.
fn located_by_package<S: serde::Serializer>(entries: &[LocatedPath], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_map(entries.iter().map(|entry| (&entry.package, entry)))
}

impl Printable for LocatedPaths {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "LocatedPaths";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.kind.to_string());
            rows[2].push(entry.path.display().to_string());
        }
        printer.print_table(
            &["Package".into(), "Kind".into(), "Path".into()],
            &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn located(package: &str, path: &str, kind: PathKind) -> LocatedPath {
        LocatedPath {
            package: package.to_owned(),
            path: PathBuf::from(path),
            kind,
        }
    }

    /// A map keyed by the requested identifier, under the named `paths`
    /// property, with each value an object.
    #[test]
    fn the_report_is_a_map_keyed_by_the_requested_identifier() {
        let report = LocatedPaths::new(vec![located("cmake:3.28", "/store/packages/cmake", PathKind::Package)]);

        let json = serde_json::to_value(&report).expect("serializes");
        assert_eq!(
            json.as_object()
                .map(|root| root.keys().map(String::as_str).collect::<Vec<_>>()),
            Some(vec!["paths"]),
            "the map sits under one named property: {json}"
        );
        let paths = &json["paths"];
        assert_eq!(paths["cmake:3.28"]["path"], "/store/packages/cmake");
        assert_eq!(paths["cmake:3.28"]["kind"], "package");
        // The identifier is the key, so it must not also be a field — it would
        // be byte-identical to the key and a second place to go stale.
        assert!(
            paths["cmake:3.28"].get("package").is_none(),
            "the identifier is the key, not a field: {json}"
        );
    }

    /// A deferred tool's row names the shim tree **and says so**. Reporting a
    /// package directory that does not exist yet would be a lie in a
    /// machine-read field; reporting the shim without the discriminator would
    /// be a silent change of meaning.
    #[test]
    fn a_deferred_row_names_the_shim_directory_and_says_so() {
        let report = LocatedPaths::new(vec![
            located("cmake:3.28", "/store/packages/cmake", PathKind::Package),
            located("ripgrep:14", "/store/shims/ripgrep", PathKind::Shim),
        ]);

        let json = serde_json::to_value(&report).expect("serializes");
        let paths = &json["paths"];
        assert_eq!(
            paths["ripgrep:14"]["kind"], "shim",
            "a deferred row must announce itself: {json}"
        );
        assert_eq!(paths["ripgrep:14"]["path"], "/store/shims/ripgrep");
        // Both rows survive, so the map is not collapsing entries that share a kind.
        assert_eq!(paths["cmake:3.28"]["kind"], "package");
        assert_eq!(
            paths.as_object().map(serde_json::Map::len),
            Some(2),
            "one identifier in, one entry out, for each request: {json}"
        );
    }

    /// The break is `ocx package which`'s alone. `ocx package pull` reports
    /// [`Paths`], whose value stays a bare path string — this is the guard that
    /// reds if a later change widens the shared type instead of this one.
    #[test]
    fn the_shared_paths_report_still_serializes_bare_path_strings() {
        let report = Paths::new(vec![PathEntry {
            package: "cmake:3.28".to_owned(),
            path: PathBuf::from("/store/packages/cmake"),
        }]);

        let json = serde_json::to_value(&report).expect("serializes");
        assert_eq!(
            json["paths"]["cmake:3.28"], "/store/packages/cmake",
            "ocx package pull's value must stay a bare path string: {json}"
        );
    }

    /// Request order survives: the map is written as given, never re-sorted.
    #[test]
    fn keys_keep_request_order() {
        let report = LocatedPaths::new(vec![
            located("zeta:1", "/z", PathKind::Package),
            located("alpha:1", "/a", PathKind::Package),
        ]);
        let json = serde_json::to_string(&report).expect("serializes");
        let position = |key: &str| json.find(key).expect("key present");
        assert!(
            position("zeta:1") < position("alpha:1"),
            "request order must survive serialization: {json}"
        );
    }
}
