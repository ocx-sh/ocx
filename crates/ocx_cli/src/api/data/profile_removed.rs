// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use serde::Serialize;

use crate::api::Reportable;

/// Whether the package was removed from the profile or was already absent.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ProfileRemovedStatus {
    Removed,
    Absent,
}

impl fmt::Display for ProfileRemovedStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileRemovedStatus::Removed => write!(f, "removed"),
            ProfileRemovedStatus::Absent => write!(f, "absent"),
        }
    }
}

/// A single profile remove result entry.
///
/// Plain format: two-column table (Package | Status).
///
/// JSON format: `{ package, status }` object.
#[derive(Serialize)]
pub struct ProfileRemovedEntry {
    pub package: String,
    pub status: ProfileRemovedStatus,
}

impl ProfileRemovedEntry {
    pub fn new(package: String, status: ProfileRemovedStatus) -> Self {
        Self { package, status }
    }
}

/// Results of a profile remove operation.
///
/// Plain format: two-column table (Package | Status).
///
/// JSON format: array of `{ package, status }` objects.
pub struct ProfileRemoved {
    pub entries: Vec<ProfileRemovedEntry>,
}

impl ProfileRemoved {
    pub fn new(entries: Vec<ProfileRemovedEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for ProfileRemoved {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Reportable for ProfileRemoved {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.status.to_string());
        }
        printer.print_table(&["Package", "Status"], &rows);
    }
}
