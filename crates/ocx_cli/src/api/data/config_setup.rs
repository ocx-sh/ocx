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
/// adopt/refresh paths (`adopted` / `already_adopted` / `refreshed` /
/// `refresh_unavailable` / `would_refresh`); `refreshed` additionally carries
/// `"previous_digest"`, `refresh_unavailable` carries `"reason"`, both omitted
/// everywhere else — the same `managed_config` entry shape `ocx self setup`
/// reports, so fleet tooling can parse both with one schema.
#[derive(Serialize, schemars::JsonSchema)]
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

    /// A refresh reports the new digest under `digest` and the one it replaced
    /// under `previous_digest`; `reason` stays absent.
    #[test]
    fn refreshed_serializes_previous_digest() {
        let from = "a".repeat(64);
        let to = "b".repeat(64);
        let outcome = ManagedConfigSetupOutcome::Refreshed {
            from: ocx_lib::oci::Digest::Sha256(from.clone()),
            to: ocx_lib::oci::Digest::Sha256(to.clone()),
        };
        let value = serde_json::to_value(ConfigSetupData::from_outcome(&outcome)).unwrap();
        assert_eq!(value["managed_config"]["status"], json!("refreshed"));
        assert_eq!(value["managed_config"]["digest"], json!(format!("sha256:{to}")));
        assert_eq!(
            value["managed_config"]["previous_digest"],
            json!(format!("sha256:{from}"))
        );
        assert!(value["managed_config"].get("reason").is_none());
    }

    /// A failed refresh reports the RETAINED digest plus the cause, and never
    /// `already_adopted` — a refresh that did not run must not look healthy.
    #[test]
    fn refresh_unavailable_serializes_reason() {
        let hex = "c".repeat(64);
        let outcome = ManagedConfigSetupOutcome::RefreshUnavailable {
            digest: ocx_lib::oci::Digest::Sha256(hex.clone()),
            reason: "registry unreachable".to_string(),
        };
        let value = serde_json::to_value(ConfigSetupData::from_outcome(&outcome)).unwrap();
        assert_eq!(value["managed_config"]["status"], json!("refresh_unavailable"));
        assert_eq!(value["managed_config"]["digest"], json!(format!("sha256:{hex}")));
        assert_eq!(value["managed_config"]["reason"], json!("registry unreachable"));
        assert!(value["managed_config"].get("previous_digest").is_none());
    }

    /// A dry run against an adopted seed reports `would_refresh` with the
    /// existing digest and neither optional field.
    #[test]
    fn would_refresh_serializes_digest_only() {
        let hex = "d".repeat(64);
        let outcome = ManagedConfigSetupOutcome::WouldRefresh {
            digest: ocx_lib::oci::Digest::Sha256(hex.clone()),
        };
        let value = serde_json::to_value(ConfigSetupData::from_outcome(&outcome)).unwrap();
        assert_eq!(value["managed_config"]["status"], json!("would_refresh"));
        assert_eq!(value["managed_config"]["digest"], json!(format!("sha256:{hex}")));
        assert!(value["managed_config"].get("previous_digest").is_none());
        assert!(value["managed_config"].get("reason").is_none());
    }

    /// The two new fields are omitted on every pre-existing status, so
    /// consumers pinned to the old shape stay byte-identical.
    #[test]
    fn existing_statuses_omit_the_new_optional_fields() {
        let outcome = ManagedConfigSetupOutcome::Adopted {
            digest: ocx_lib::oci::Digest::Sha256("e".repeat(64)),
        };
        let value = serde_json::to_value(ConfigSetupData::from_outcome(&outcome)).unwrap();
        assert!(value["managed_config"].get("previous_digest").is_none());
        assert!(value["managed_config"].get("reason").is_none());
    }
}
