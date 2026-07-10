// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use ocx_lib::setup::ManagedConfigSetupOutcome;
use serde::Serialize;

use crate::api::Printable;
use crate::api::data::self_setup::ManagedConfigEntry;

/// CLI wrapper around [`ManagedConfigSetupOutcome`] for `ocx config setup`.
///
/// Plain format: a key/value table with a `Managed config` summary row.
///
/// JSON format: `{"managed_config":{"status":"…"}}`, plus `"digest"` on the
/// adopt paths (`adopted` / `already_adopted`) — the same `managed_config`
/// entry shape `ocx self setup` reports, so fleet tooling can parse both with
/// one schema.
#[derive(Serialize)]
pub struct ConfigSetupData {
    managed_config: ManagedConfigEntry,
}

impl ConfigSetupData {
    pub fn from_outcome(outcome: &ManagedConfigSetupOutcome) -> Self {
        Self {
            managed_config: ManagedConfigEntry::from_outcome(outcome),
        }
    }
}

impl Printable for ConfigSetupData {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        printer.print_table(
            &["Field".into(), "Value".into()],
            &[
                vec![Cell::from("Managed config".to_string())],
                vec![Cell::from(self.managed_config.summary())],
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::setup::ManagedConfigSetupOutcome;
    use serde_json::json;

    use super::ConfigSetupData;

    /// An adopt outcome serializes to the same `managed_config` entry shape
    /// `ocx self setup` reports (status + digest).
    #[test]
    fn adopted_serializes_with_digest() {
        let hex = "a".repeat(64);
        let outcome = ManagedConfigSetupOutcome::Adopted {
            digest: ocx_lib::oci::Digest::Sha256(hex.clone()),
        };
        let value = serde_json::to_value(ConfigSetupData::from_outcome(&outcome)).unwrap();
        assert_eq!(value["managed_config"]["status"], json!("adopted"));
        assert_eq!(value["managed_config"]["digest"], json!(format!("sha256:{hex}")));
    }

    /// A clear outcome carries no digest.
    #[test]
    fn cleared_serializes_without_digest() {
        let value = serde_json::to_value(ConfigSetupData::from_outcome(&ManagedConfigSetupOutcome::Cleared)).unwrap();
        assert_eq!(value["managed_config"]["status"], json!("cleared"));
        assert!(value["managed_config"].get("digest").is_none());
    }
}
