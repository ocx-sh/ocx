// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::path::PathBuf;

use ocx_console::Cell;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::path_kind::PathKind;

/// A resolved package and its package root, the parent of `content/` and `entrypoints/`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PathEntry {
    pub package: String,
    pub path: PathBuf,
}

/// Resolved package roots for `ocx package pull`, keyed by input identifier in request order.
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
    pub path: PathBuf,
    pub kind: PathKind,
}

/// Located packages for `ocx package which`, keyed by requested identifier in request order.
///
/// Not a widened [`Paths`]: every `pull` row is a materialized root, so `kind` there would be constant.
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

// Hand-written: `Serialize` writes a map keyed by package, not the struct's fields.
impl schemars::JsonSchema for Paths {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Paths".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "additionalProperties": generator.subschema_for::<std::path::PathBuf>(),
        })
    }
}

// Hand-written: `Serialize` writes a map keyed by package, not the struct's fields.
impl schemars::JsonSchema for LocatedPaths {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LocatedPaths".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "additionalProperties": generator.subschema_for::<LocatedPath>(),
        })
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
