// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::path_kind::PathKind;

/// A single resolved package → package-root entry.
///
/// `path` is the package root directory (parent of `content/` and
/// `entrypoints/`). Consumers traverse into `<path>/content/` for the
/// installed files or `<path>/entrypoints/` for generated launchers.
#[derive(Serialize)]
pub struct PathEntry {
    pub package: String,
    pub path: PathBuf,
}

/// Ordered list of resolved package roots, one per requested package.
///
/// Plain format: two-column table (Package | Path).
///
/// JSON format: object keyed by the input package identifier, preserving
/// request order.
///
/// Reports `ocx package pull` only. `ocx package which` reports
/// [`LocatedPaths`] instead — see that type for why the two did not merge.
pub struct Paths {
    pub entries: Vec<PathEntry>,
}

impl Paths {
    pub fn new(entries: Vec<PathEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for Paths {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for entry in &self.entries {
            map.serialize_entry(&entry.package, &entry.path)?;
        }
        map.end()
    }
}

impl Printable for Paths {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
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

/// One located package: the directory `ocx package which` reports for it, and
/// which kind of directory that is (contract C-016).
///
/// A tool composed lazily has no package directory until its first invocation
/// materializes one, so the answer to "where is this on disk" is its generated
/// shim tree. [`PathKind`] is the discriminator that lets a consumer tell the
/// two apart — minted once for the whole feature and shared with `ocx pull`'s
/// report, never re-spelled here.
#[derive(Serialize)]
pub struct LocatedPath {
    /// The requested identifier, verbatim. Serialized as the map **key**, never
    /// as a field: it would be byte-identical to the key it sits under, and a
    /// duplicated value is a second place for it to go stale. Kept on the struct
    /// for the plain table's `Package` column, matching `WarmedPath`.
    #[serde(skip)]
    pub package: String,
    pub path: PathBuf,
    pub kind: PathKind,
}

/// Ordered list of located packages, one per requested identifier.
///
/// Plain format: three-column table (Package | Kind | Path) — three of the five
/// columns `subsystem-cli-api.md` allows.
///
/// JSON format: object keyed by the requested identifier, preserving request
/// order; each value is `{"path": "...", "kind": "package"|"shim"}`.
///
/// **The wire break C-016 sanctions** (PLAN-NC-1, resolved): the map stays a
/// map — every other multi-package report in this CLI is a keyed object — and
/// only the *value* grows from a bare path string into an object, so a consumer
/// edit is `.["cmake:3.28"]` → `.["cmake:3.28"].path` rather than a rewritten
/// `select()` scan.
///
/// Deliberately not a widened [`Paths`], which still reports `ocx package pull`:
/// every row that command produces is a materialized package root, so a `kind`
/// column there would carry one constant value in every row (the plain-mode
/// column budget forbids exactly that) and its JSON would take a wire break
/// C-016 does not sanction.
pub struct LocatedPaths {
    pub entries: Vec<LocatedPath>,
}

impl LocatedPaths {
    pub fn new(entries: Vec<LocatedPath>) -> Self {
        Self { entries }
    }
}

impl Serialize for LocatedPaths {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for entry in &self.entries {
            map.serialize_entry(&entry.package, entry)?;
        }
        map.end()
    }
}

impl Printable for LocatedPaths {
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

    fn located(package: &str, path: &str, kind: PathKind) -> LocatedPath {
        LocatedPath {
            package: package.to_owned(),
            path: PathBuf::from(path),
            kind,
        }
    }

    /// The wire shape C-016 settles (PLAN-NC-1): still a **map** keyed by the
    /// requested identifier — not the array the ADR first proposed — with the
    /// value grown from a bare path string into an object.
    #[test]
    fn the_report_is_a_map_keyed_by_the_requested_identifier() {
        let report = LocatedPaths::new(vec![located("cmake:3.28", "/store/packages/cmake", PathKind::Package)]);

        let json = serde_json::to_value(&report).expect("serializes");
        assert!(
            json.is_object(),
            "the top level must stay a map keyed by identifier, never an array: {json}"
        );
        assert_eq!(json["cmake:3.28"]["path"], "/store/packages/cmake");
        assert_eq!(json["cmake:3.28"]["kind"], "package");
        // The identifier is the key, so it must not also be a field — it would
        // be byte-identical to the key and a second place to go stale.
        assert!(
            json["cmake:3.28"].get("package").is_none(),
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
        assert_eq!(
            json["ripgrep:14"]["kind"], "shim",
            "a deferred row must announce itself: {json}"
        );
        assert_eq!(json["ripgrep:14"]["path"], "/store/shims/ripgrep");
        // Both rows survive, in request order, so the map is not collapsing
        // entries that share a kind.
        assert_eq!(json["cmake:3.28"]["kind"], "package");
        assert_eq!(
            json.as_object().map(serde_json::Map::len),
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
            json["cmake:3.28"], "/store/packages/cmake",
            "ocx package pull's value must stay a bare path string: {json}"
        );
    }
}
