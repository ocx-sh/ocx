// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::env::LazyAdvisoryReport;
use crate::api::data::path_kind::PathKind;

/// The one JSON key of [`WarmedPaths`] that is not a pulled identifier.
///
/// Safe as a reserved key because every key beside it is a pinned identifier —
/// `registry/repository@sha256:…` — and no such string is a bare word.
const ADVISORIES_KEY: &str = "advisories";

/// One pre-warmed tool: what `ocx pull` put on disk for it, and which kind of
/// directory that is.
///
/// A tool pre-warmed eagerly yields its package root; a tool whose `lazy-mode`
/// resolved to `always` yields its generated shim directory instead, because
/// that is what the run created — its package directory does not exist yet and
/// naming one would be a lie in a machine-read field.
#[derive(Serialize)]
pub struct WarmedPath {
    #[serde(skip)]
    pub package: String,
    pub path: PathBuf,
    pub kind: PathKind,
}

/// Ordered list of pre-warmed tools, one per locked tool in scope.
///
/// Plain format: three-column table (Package | Kind | Path).
///
/// JSON format: object keyed by the pulled identifier, preserving lock order;
/// each value is `{"path": "...", "kind": "package"|"shim"}`. One reserved
/// sibling key, `"advisories"`, carries the deferred-tool advisories as an
/// array — always present, empty when nothing was deferred.
///
/// The advisories are a top-level sibling and not a per-row field so that one
/// `jq '.advisories'` reads the same payload here as it does off `ocx env` /
/// `ocx package env`, which publish the identical [`LazyAdvisoryReport`]
/// projection (C-015). Nesting them per package would have been the tidier map
/// but a second shape for the same fact.
///
/// Deliberately not [`super::paths::Paths`], which `ocx package which` and
/// `ocx package pull` share: those answer where an installed package *is*, and
/// their value is a bare path string. Widening that shape for every OCI-tier
/// consumer is a separate decision from this command's.
pub struct WarmedPaths {
    pub entries: Vec<WarmedPath>,
    /// Advisories raised for the **deferred** tools this run pre-warmed —
    /// warning-only, and also written to stderr for the plain channel.
    pub advisories: Vec<LazyAdvisoryReport>,
}

impl WarmedPaths {
    pub fn new(entries: Vec<WarmedPath>) -> Self {
        Self {
            entries,
            advisories: Vec::new(),
        }
    }

    /// Attaches the deferred-composition advisories, returning `self` for
    /// chaining after [`new`](Self::new) — the same seam
    /// [`EnvVars::with_advisories`](super::env::EnvVars::with_advisories) uses,
    /// so the two producers of C-015's payload stay one shape.
    #[must_use]
    pub fn with_advisories(mut self, advisories: Vec<LazyAdvisoryReport>) -> Self {
        self.advisories = advisories;
        self
    }
}

impl Serialize for WarmedPaths {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.entries.len() + 1))?;
        for entry in &self.entries {
            map.serialize_entry(&entry.package, entry)?;
        }
        map.serialize_entry(ADVISORIES_KEY, &self.advisories)?;
        map.end()
    }
}

impl Printable for WarmedPaths {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
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

    /// The wire shape F-11 fixes: keyed by identifier, each value an object
    /// naming the directory that exists AND which kind it is. A consumer must
    /// be able to tell a shim tree from a package root without probing disk.
    #[test]
    fn a_deferred_row_names_the_shim_directory_and_says_so() {
        let report = WarmedPaths::new(vec![
            warmed("example.com/eager@sha256:a", "/store/packages/eager", PathKind::Package),
            warmed("example.com/lazy@sha256:b", "/store/shims/lazy", PathKind::Shim),
        ]);

        let json = serde_json::to_value(&report).expect("serializes");
        assert_eq!(json["example.com/eager@sha256:a"]["kind"], "package");
        assert_eq!(json["example.com/eager@sha256:a"]["path"], "/store/packages/eager");
        assert_eq!(
            json["example.com/lazy@sha256:b"]["kind"], "shim",
            "a deferred tool's row must announce that its path is a shim tree: {json}"
        );
        assert_eq!(json["example.com/lazy@sha256:b"]["path"], "/store/shims/lazy");
        // The key carries the identifier, so it must not be repeated inside the
        // value — a duplicated field is a second place for it to go stale.
        assert!(
            json["example.com/lazy@sha256:b"].get("package").is_none(),
            "the identifier is the key, not a field: {json}"
        );
        assert_eq!(
            json["advisories"],
            serde_json::json!([]),
            "the advisories key is always present, empty when nothing was deferred: {json}"
        );
    }

    /// C-015: `ocx pull --lazy-mode always --format json` serializes the
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
            kind: "undeclared-binaries",
            package: "example.com/lazy@sha256:b".to_owned(),
            key: None,
            message: "declares no binaries".to_owned(),
        }]);

        let json = serde_json::to_value(&report).expect("serializes");
        assert_eq!(
            json["advisories"][0]["kind"], "undeclared-binaries",
            "an advisory raised by a deferred pull must reach the wire: {json}"
        );
        assert_eq!(json["advisories"][0]["package"], "example.com/lazy@sha256:b");
        // The reserved key must not shadow a pulled row, and vice versa.
        assert_eq!(json["example.com/lazy@sha256:b"]["kind"], "shim", "{json}");
    }
}
