// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use crate::api::Reportable;

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
#[derive(Serialize)]
pub struct CleanEntry {
    pub kind: CleanKind,
    pub dry_run: bool,
    pub path: PathBuf,
}

/// Results of a clean operation: unreferenced objects and stale temp directories
/// that were removed (or would be removed in a dry run).
///
/// Plain format: three-column table (Type | Dry Run | Path).
///
/// JSON format: array of `{ kind, dry_run, path }` objects.
pub struct Clean {
    pub entries: Vec<CleanEntry>,
}

impl Clean {
    pub fn new(objects: Vec<PathBuf>, temp: Vec<PathBuf>, dry_run: bool) -> Self {
        let mut entries = Vec::with_capacity(objects.len() + temp.len());
        for path in objects {
            entries.push(CleanEntry {
                kind: CleanKind::Object,
                dry_run,
                path,
            });
        }
        for path in temp {
            entries.push(CleanEntry {
                kind: CleanKind::Temp,
                dry_run,
                path,
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

impl Reportable for Clean {
    fn print_plain(&self) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.kind.to_string());
            rows[1].push(entry.path.display().to_string());
        }
        crate::stdout::print_table(&["Type", "Path"], &rows);
    }
}
