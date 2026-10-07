// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::env::LazyAdvisoryReport;
use crate::api::data::path_kind::PathKind;

/// One pre-warmed tool: what `ocx pull` put on disk for it, and which kind of
/// directory that is.
///
/// A tool pre-warmed eagerly yields its package root; a tool whose `lazy-mode`
/// resolved to `always` yields its generated shim directory instead: its
/// package directory does not exist yet.
// Never name the package directory for a lazy tool: a machine-read field would point at nothing.
#[derive(Serialize, schemars::JsonSchema)]
pub struct WarmedPath {
    /// The pulled identifier: serialized only as the map key, kept for the plain `Package` column.
    #[serde(skip)]
    pub package: String,
    /// The directory that now exists for the tool.
    pub path: PathBuf,
    /// Whether `path` is a package root or a shim tree.
    pub kind: PathKind,
}

/// Pre-warmed tools, one per locked tool in scope.
#[derive(Serialize, schemars::JsonSchema)]
pub struct WarmedPaths {
    /// Each pre-warmed tool, keyed by pulled identifier, in lock order.
    #[serde(rename = "paths", serialize_with = "warmed_by_package")]
    #[schemars(with = "std::collections::BTreeMap<String, WarmedPath>")]
    pub entries: Vec<WarmedPath>,
    /// Advisories for the deferred tools this run pre-warmed, in the shape `ocx env` emits; also written to stderr.
    pub advisories: Vec<LazyAdvisoryReport>,
}

/// Lock order is the contract, so the entries serialize as a map without passing through a sorted one.
fn warmed_by_package<S: serde::Serializer>(entries: &[WarmedPath], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_map(entries.iter().map(|entry| (&entry.package, entry)))
}

impl WarmedPaths {
    pub fn new(entries: Vec<WarmedPath>) -> Self {
        Self {
            entries,
            advisories: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_advisories(mut self, advisories: Vec<LazyAdvisoryReport>) -> Self {
        self.advisories = advisories;
        self
    }
}

impl Printable for WarmedPaths {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "WarmedPaths";

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

    fn warmed(package: &str, path: &str, kind: PathKind) -> WarmedPath {
        WarmedPath {
            package: package.to_owned(),
            path: PathBuf::from(path),
            kind,
        }
    }

    /// The wire shape: keyed by identifier, each value an object
    /// naming the directory that exists AND which kind it is. A consumer must
    /// be able to tell a shim tree from a package root without probing disk.
    #[test]
    fn a_deferred_row_names_the_shim_directory_and_says_so() {
        let report = WarmedPaths::new(vec![
            warmed("example.com/eager@sha256:a", "/store/packages/eager", PathKind::Package),
            warmed("example.com/lazy@sha256:b", "/store/shims/lazy", PathKind::Shim),
        ]);

        let json = serde_json::to_value(&report).expect("serializes");
        let paths = &json["paths"];
        assert_eq!(paths["example.com/eager@sha256:a"]["kind"], "package");
        assert_eq!(paths["example.com/eager@sha256:a"]["path"], "/store/packages/eager");
        assert_eq!(
            paths["example.com/lazy@sha256:b"]["kind"], "shim",
            "a deferred tool's row must announce that its path is a shim tree: {json}"
        );
        assert_eq!(paths["example.com/lazy@sha256:b"]["path"], "/store/shims/lazy");
        // The key carries the identifier, so it must not be repeated inside the
        // value — a duplicated field is a second place for it to go stale.
        assert!(
            paths["example.com/lazy@sha256:b"].get("package").is_none(),
            "the identifier is the key, not a field: {json}"
        );
        assert_eq!(
            json["advisories"],
            serde_json::json!([]),
            "the advisories key is always present, empty when nothing was deferred: {json}"
        );
    }

    /// `ocx pull --lazy-mode always --format json` serializes the
    /// advisories it raises, in the same projection and under the same key as
    /// `ocx env`. Without the field `jq '.advisories'` answers `null` while the
    /// identical advisory for the identical package is readable off `ocx env`.
    #[test]
    fn a_deferred_pull_serializes_its_advisories_under_the_shared_key() {
        let report = WarmedPaths::new(vec![warmed(
            "example.com/lazy@sha256:b",
            "/store/shims/lazy",
            PathKind::Shim,
        )])
        .with_advisories(vec![LazyAdvisoryReport {
            kind: crate::api::data::env::LazyAdvisoryKind::UndeclaredBinaries,
            package: "example.com/lazy@sha256:b".to_owned(),
            key: None,
            message: "declares no binaries".to_owned(),
        }]);

        let json = serde_json::to_value(&report).expect("serializes");
        assert_eq!(
            json["advisories"][0]["kind"], "undeclared_binaries",
            "an advisory raised by a deferred pull must reach the wire: {json}"
        );
        assert_eq!(json["advisories"][0]["package"], "example.com/lazy@sha256:b");
        assert_eq!(json["paths"]["example.com/lazy@sha256:b"]["kind"], "shim", "{json}");
    }
}
