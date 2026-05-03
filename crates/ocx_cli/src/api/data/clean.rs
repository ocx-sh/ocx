// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use ocx_lib::package_manager::CleanedObject;

use crate::api::Printable;

/// The kind of resource cleaned up.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum CleanKind {
    Object,
    Temp,
}

impl fmt::Display for CleanKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CleanKind::Object => write!(f, "object"),
            CleanKind::Temp => write!(f, "temp"),
        }
    }
}

/// A single cleaned-up resource entry.
///
/// The `held_by` field lists the absolute paths of every registered project's
/// `ocx.lock` that pins this package. Non-empty only for `object` kind entries
/// in dry-run mode when the package would have been collected without the
/// project registry. Empty in non-dry-run output (held entries are never
/// collected) and always empty for `temp` entries.
///
/// See [`adr_clean_project_backlinks.md`] "`ocx clean` UX" for the column
/// layout and JSON shape specification.
#[derive(Serialize)]
pub struct CleanEntry {
    pub kind: CleanKind,
    pub dry_run: bool,
    pub path: PathBuf,
    /// Project `ocx.lock` paths holding this entry. Empty when the entry is not
    /// protected by any registered project, or when `--force` was specified.
    pub held_by: Vec<PathBuf>,
}

/// Results of a clean operation: unreferenced objects and stale temp directories
/// that were removed (or would be removed in a dry run).
///
/// Plain format: three-column table `Type | Held By | Path` when any entry
/// carries non-empty `held_by` attribution (i.e. dry-run with project-registry
/// pins); two-column `Type | Path` otherwise. See
/// [`adr_clean_project_backlinks.md`] "Dry-run preview shape (plain)".
///
/// JSON format: array of `{ kind, dry_run, path, held_by }` objects.
pub struct Clean {
    pub entries: Vec<CleanEntry>,
}

impl Clean {
    /// Constructs a `Clean` report from the task result.
    ///
    /// `objects` carries the richer [`CleanedObject`] shape so that
    /// `held_by` attribution flows through to the plain and JSON output.
    /// `temp` entries always have an empty `held_by` — stale temp directories
    /// are not governed by the project registry (see
    /// [`adr_clean_project_backlinks.md`] "`ocx clean` UX").
    pub fn new(objects: Vec<CleanedObject>, temp: Vec<PathBuf>, dry_run: bool) -> Self {
        let mut entries = Vec::with_capacity(objects.len() + temp.len());
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
        Self { entries }
    }
}

impl Serialize for Clean {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Printable for Clean {
    /// Prints a two- or three-column table depending on whether any entry
    /// carries `held_by` attribution.
    ///
    /// When attribution is present (dry-run with project-registry pins):
    ///   `Type | Held By | Path`
    /// where the `Held By` cell joins multiple paths with `, ` and is blank
    /// for entries with no holding project.
    ///
    /// When no entry has attribution (non-dry-run, or dry-run with `--force`):
    ///   `Type | Path`
    ///
    /// See [`adr_clean_project_backlinks.md`] "Dry-run preview shape (plain)".
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
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
            printer.print_table(&["Type", "Held By", "Path"], &rows);
        } else {
            let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
            for entry in &self.entries {
                rows[0].push(entry.kind.to_string());
                rows[1].push(entry.path.display().to_string());
            }
            printer.print_table(&["Type", "Path"], &rows);
        }
    }
}
