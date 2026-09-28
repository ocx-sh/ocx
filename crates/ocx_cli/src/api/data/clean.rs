// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use ocx_console::Cell;
use ocx_package_manager::CleanedObject;

use crate::api::Printable;

/// The kind of resource cleaned up.
#[derive(Serialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum CleanKind {
    Object,
    Temp,
    /// A `state/projects/<key>/` directory whose consent stamp was swept.
    // Reported like the other two: revoking a project's activation consent is the
    // most consequential thing `ocx clean` does (the project goes inert at the
    // next prompt), and a `--dry-run` that did not name it would preview
    // everything except the part a user would want to stop.
    Consent,
}

impl fmt::Display for CleanKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CleanKind::Object => write!(f, "object"),
            CleanKind::Temp => write!(f, "temp"),
            CleanKind::Consent => write!(f, "consent"),
        }
    }
}

/// A single cleaned-up resource entry.
///
/// The `held_by` field lists the absolute paths of every registered project's
/// `ocx.lock` that pins this package. Non-empty only for `object` kind entries
/// in dry-run mode when the package would have been collected without the
/// project registry. Empty in non-dry-run output (held entries are never
/// collected) and always empty for `temp` and `consent` entries.
// Column layout and JSON shape: adr_clean_project_backlinks.md § `ocx clean` UX.
#[derive(Serialize, schemars::JsonSchema)]
pub struct CleanEntry {
    pub kind: CleanKind,
    pub dry_run: bool,
    pub path: PathBuf,
    /// Project `ocx.lock` paths holding this entry. Empty when the entry is not
    /// protected by any registered project, or when `--force` was specified.
    pub held_by: Vec<PathBuf>,
}

/// Objects, temp directories and consent stamps a clean removed, or would remove in a dry run.
pub struct Clean {
    pub entries: Vec<CleanEntry>,
}

impl Clean {
    /// Builds the report from every [`CleanResult`](ocx_package_manager::CleanResult) field; a
    /// dropped `consent` list would make the stamp sweep silent, which its contract forbids.
    pub fn new(objects: Vec<CleanedObject>, temp: Vec<PathBuf>, consent: Vec<PathBuf>, dry_run: bool) -> Self {
        let mut entries = Vec::with_capacity(objects.len() + temp.len() + consent.len());
        for obj in objects {
            entries.push(CleanEntry {
                kind: CleanKind::Object,
                dry_run,
                path: obj.path,
                held_by: obj.held_by,
            });
        }
        for path in temp {
            entries.push(CleanEntry {
                kind: CleanKind::Temp,
                dry_run,
                path,
                held_by: Vec::new(),
            });
        }
        for path in consent {
            entries.push(CleanEntry {
                kind: CleanKind::Consent,
                dry_run,
                path,
                held_by: Vec::new(),
            });
        }
        Self { entries }
    }
}

impl Serialize for Clean {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Printable for Clean {
    /// `Type | Held By | Path` when any entry carries `held_by`, else `Type | Path`
    /// (`adr_clean_project_backlinks.md` "Dry-run preview shape (plain)").
    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        let has_attribution = self.entries.iter().any(|e| !e.held_by.is_empty());

        if has_attribution {
            let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for entry in &self.entries {
                rows[0].push(entry.kind.to_string());
                rows[1].push(
                    entry
                        .held_by
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                rows[2].push(entry.path.display().to_string());
            }
            printer.print_table(
                &["Type".into(), "Held By".into(), "Path".into()],
                &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
            );
        } else {
            let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
            for entry in &self.entries {
                rows[0].push(entry.kind.to_string());
                rows[1].push(entry.path.display().to_string());
            }
            printer.print_table(
                &["Type".into(), "Path".into()],
                &rows.map(|c| c.into_iter().map(Cell::from).collect::<Vec<_>>()),
            );
        }
    }
}

// Transparent `Serialize`: the schema is the bare entry array.
impl schemars::JsonSchema for Clean {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Clean".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Vec<CleanEntry>>::json_schema(generator)
    }
}
