// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;

/// Version information reported by `ocx version`.
///
/// Plain format: bare version string (e.g. `0.3.0`), no trailing newline beyond
/// the implicit one from `println!`.
///
/// JSON format: `{ "version": "0.3.0" }`.
#[derive(Serialize)]
pub struct VersionData {
    version: String,
}

impl VersionData {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
        }
    }
}

impl Printable for VersionData {
    fn print_plain(&self, _printer: &ocx_lib::cli::DataInterface) {
        println!("{}", self.version);
    }
}

#[cfg(test)]
mod tests {
    use super::VersionData;

    /// `VersionData` serializes to `{"version":"<string>"}`.
    ///
    /// Pins the wire format so `ocx --format json version` callers can rely on
    /// the JSON shape.  The subprocess-based version source in
    /// `update_check.rs::query_installed_version` parses this exact shape.
    #[test]
    fn version_data_json_shape() {
        let data = VersionData::new("0.3.0");
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"version": "0.3.0"}),
            "VersionData JSON shape must be {{\"version\": \"<string>\"}}"
        );
    }

    /// `VersionData::new` accepts both `String` and `&str` and produces the
    /// same JSON wire output for byte-equal inputs.
    #[test]
    fn version_data_accepts_str_and_string() {
        let from_str = serde_json::to_value(VersionData::new("1.0.0")).unwrap();
        let from_string = serde_json::to_value(VersionData::new("1.0.0".to_string())).unwrap();
        assert_eq!(from_str, from_string);
    }
}
