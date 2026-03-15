// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;

use ocx_lib::profile::ProfileMode;
use serde::Serialize;

use crate::api::Reportable;

/// Whether the package was newly added or an existing entry was updated.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ProfileAddedStatus {
    Added,
    Updated,
}

impl fmt::Display for ProfileAddedStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileAddedStatus::Added => write!(f, "added"),
            ProfileAddedStatus::Updated => write!(f, "updated"),
        }
    }
}

/// A single profile add result entry.
///
/// Plain format: three-column table (Package | Mode | Status).
///
/// JSON format: `{ package, mode, status, previous_mode? }` object.
#[derive(Serialize)]
pub struct ProfileAddedEntry {
    pub package: String,
    pub mode: ProfileMode,
    pub status: ProfileAddedStatus,
    /// The previous mode, present only when `status` is `updated` and the mode changed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_mode: Option<ProfileMode>,
}

impl ProfileAddedEntry {
    pub fn new(
        package: String,
        mode: ProfileMode,
        status: ProfileAddedStatus,
        previous_mode: Option<ProfileMode>,
    ) -> Self {
        Self {
            package,
            mode,
            status,
            previous_mode,
        }
    }
}

/// Results of a profile add operation.
///
/// Plain format: three-column table (Package | Mode | Status).
///
/// JSON format: array of `{ package, mode, status }` objects.
pub struct ProfileAdded {
    pub entries: Vec<ProfileAddedEntry>,
}

impl ProfileAdded {
    pub fn new(entries: Vec<ProfileAddedEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for ProfileAdded {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Reportable for ProfileAdded {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.mode.to_string());
            rows[2].push(entry.status.to_string());
        }
        printer.print_table(&["Package", "Mode", "Status"], &rows);
    }
}
