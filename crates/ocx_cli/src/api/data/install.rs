// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;
use std::path::PathBuf;

use ocx_lib::{oci, package::metadata::Metadata};
use serde::Serialize;

use crate::api::Reportable;

/// A single install or select result entry for CLI output.
///
/// The `path` field holds the symlink that was created or updated
/// (candidate for install, current for select).
#[derive(Serialize)]
pub struct InstallEntry {
    pub identifier: oci::Identifier,
    pub metadata: Metadata,
    pub path: PathBuf,
}

/// Installed or selected packages keyed by the user-supplied identifier string.
///
/// Plain format: three-column table (Package | Version | Path).
///
/// JSON format: object keyed by package identifier, each value an
/// `{ identifier, metadata, path }` object.
#[derive(Serialize)]
pub struct Installs {
    #[serde(flatten)]
    pub packages: HashMap<String, InstallEntry>,
}

impl Installs {
    pub fn new(packages: HashMap<String, InstallEntry>) -> Self {
        Self { packages }
    }
}

impl Reportable for Installs {
    fn print_plain(&self) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (package, entry) in &self.packages {
            rows[0].push(package.clone());
            rows[1].push(entry.identifier.to_string());
            rows[2].push(entry.path.display().to_string());
        }
        ocx_lib::cli::stdout::print_table(&["Package", "Version", "Path"], &rows);
    }
}
