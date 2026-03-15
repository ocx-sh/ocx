// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::fmt;
use std::path::PathBuf;

use ocx_lib::profile::ProfileMode;
use serde::Serialize;

use crate::api::Reportable;

/// Whether the profiled package's symlink is active (resolvable) or broken.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ProfileStatus {
    Active,
    Broken,
}

impl fmt::Display for ProfileStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileStatus::Active => write!(f, "active"),
            ProfileStatus::Broken => write!(f, "broken"),
        }
    }
}

/// A single entry in the profile list report.
///
/// Plain format: four-column table (Package | Mode | Status | Path).
///
/// JSON format: array of `{ package, mode, status, path }` objects.
#[derive(Serialize)]
pub struct ProfileListEntry {
    pub package: String,
    pub mode: ProfileMode,
    pub status: ProfileStatus,
    pub path: Option<PathBuf>,
}

impl ProfileListEntry {
    pub fn new(package: String, mode: ProfileMode, status: ProfileStatus, path: Option<PathBuf>) -> Self {
        Self {
            package,
            mode,
            status,
            path,
        }
    }
}

/// Report of all profiled packages and their status.
///
/// Plain format: four-column table (Package | Mode | Status | Path).
///
/// JSON format: array of `{ package, mode, status, path }` objects.
pub struct ProfileList {
    pub entries: Vec<ProfileListEntry>,
}

impl ProfileList {
    pub fn new(entries: Vec<ProfileListEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for ProfileList {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Reportable for ProfileList {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.mode.to_string());
            rows[2].push(entry.status.to_string());
            rows[3].push(
                entry
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or("-".into()),
            );
        }
        printer.print_table(&["Package", "Mode", "Status", "Path"], &rows);
    }
}
