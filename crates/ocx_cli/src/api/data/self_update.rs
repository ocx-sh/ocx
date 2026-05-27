// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use ocx_lib::package_manager::{SelfUpdateResult, SkippedReason, UpdateCheckResult};
use serde::Serialize;

use crate::api::Printable;

// ── StatusKind ────────────────────────────────────────────────────────────────

/// Typed status discriminant shared by [`UpdateCheckData`] and [`SelfUpdateData`].
///
/// Prevents typos at compile time (a stringly-typed `&'static str` would accept
/// `"up_to-date"` silently). Serde serializes each variant to its `snake_case`
/// name, matching the JSON wire format pinned by the snapshot tests.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum StatusKind {
    UpToDate,
    Skipped,
    UpdateAvailable,
    Installed,
}

impl std::fmt::Display for StatusKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Mirror the serde snake_case output for use in plain-text table cells.
        match self {
            Self::UpToDate => f.write_str("up_to_date"),
            Self::Skipped => f.write_str("skipped"),
            Self::UpdateAvailable => f.write_str("update_available"),
            Self::Installed => f.write_str("installed"),
        }
    }
}

// ── UpdateCheckResult wrapper ─────────────────────────────────────────────────

/// CLI wrapper around [`UpdateCheckResult`] for API reporting.
///
/// JSON format (discriminated by `status`):
/// - `{"status": "up_to_date"}` — no payload
/// - `{"status": "update_available", "identifier": "<id>"}` — newer version
///   available at `identifier`
/// - `{"status": "skipped", "skipped_reason": {"reason": "<variant>"[, "detail": "…"]}}`
///   — structured `SkippedReason` discriminator, programmatic without string parsing
///
/// Plain format: key/value table; rows with no payload (e.g. `identifier` when
/// status = `up_to_date`) are suppressed.
#[derive(Serialize)]
pub struct UpdateCheckData {
    status: StatusKind,
    /// Identifier of the available update; present iff status = `update_available`.
    #[serde(skip_serializing_if = "Option::is_none")]
    identifier: Option<String>,
    /// Why the check was skipped; present iff status = `skipped`. Carries the
    /// structured [`SkippedReason`] enum so scripts can dispatch on `reason`
    /// without string parsing.
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_reason: Option<SkippedReason>,
}

impl UpdateCheckData {
    pub fn from_result(result: &UpdateCheckResult) -> Self {
        match result {
            UpdateCheckResult::AlreadyUpToDate => Self {
                status: StatusKind::UpToDate,
                identifier: None,
                skipped_reason: None,
            },
            UpdateCheckResult::Skipped(reason) => Self {
                status: StatusKind::Skipped,
                identifier: None,
                skipped_reason: Some(reason.clone()),
            },
            UpdateCheckResult::UpdateAvailable(identifier) => Self {
                status: StatusKind::UpdateAvailable,
                identifier: Some(identifier.to_string()),
                skipped_reason: None,
            },
        }
    }
}

impl Printable for UpdateCheckData {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        // Key/value layout: rows[0] = field names, rows[1] = values. Conditional
        // rows keep the output narrow — only fields with payload appear.
        let mut fields: Vec<Cell> = vec!["Status".into()];
        let mut values: Vec<Cell> = vec![Cell::from(self.status.to_string())];

        if let Some(identifier) = &self.identifier {
            fields.push("Identifier".into());
            values.push(Cell::from(identifier.clone()));
        }
        if let Some(reason) = &self.skipped_reason {
            fields.push("Skipped reason".into());
            values.push(Cell::from(reason.to_string()));
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}

// ── SelfUpdateResult wrapper ──────────────────────────────────────────────────

/// CLI wrapper around [`SelfUpdateResult`] for API reporting.
///
/// JSON format (discriminated by `status`):
/// - `{"status": "up_to_date"}` — no payload
/// - `{"status": "installed", "from": "0.0.1", "to": "0.0.2"}` (`from` omitted
///   when subprocess version query failed — bootstrap mode)
/// - `{"status": "skipped", "skipped_reason": {"reason": "<variant>"[, "detail": "…"]}}`
///
/// Plain format: key/value table with conditional rows; `Status` always
/// present, `From`/`To`/`Skipped reason` appear only when applicable.
#[derive(Serialize)]
pub struct SelfUpdateData {
    status: StatusKind,
    /// Previously installed version; present iff status = `installed` AND the
    /// subprocess version query succeeded. `None` indicates the subprocess
    /// invocation was not available (binary absent, non-zero exit, malformed
    /// JSON output).
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    /// Newly installed version; present iff status = `installed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    /// Why the update was skipped; present iff status = `skipped`. Carries the
    /// structured [`SkippedReason`] enum so scripts can dispatch on `reason`
    /// without string parsing.
    #[serde(skip_serializing_if = "Option::is_none")]
    skipped_reason: Option<SkippedReason>,
}

impl SelfUpdateData {
    pub fn from_result(result: &SelfUpdateResult) -> Self {
        match result {
            SelfUpdateResult::AlreadyUpToDate => Self {
                status: StatusKind::UpToDate,
                from: None,
                to: None,
                skipped_reason: None,
            },
            SelfUpdateResult::Installed { from, to } => Self {
                status: StatusKind::Installed,
                from: from.clone(),
                to: Some(to.clone()),
                skipped_reason: None,
            },
            SelfUpdateResult::Skipped(reason) => Self {
                status: StatusKind::Skipped,
                from: None,
                to: None,
                skipped_reason: Some(reason.clone()),
            },
        }
    }
}

impl Printable for SelfUpdateData {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        // Key/value layout: only fields with payload appear. Suppresses the empty
        // rows the old 4-column `Status|From|To|Reason` layout produced.
        let mut fields: Vec<Cell> = vec!["Status".into()];
        let mut values: Vec<Cell> = vec![Cell::from(self.status.to_string())];

        if let Some(from) = &self.from {
            fields.push("From".into());
            values.push(Cell::from(from.clone()));
        }
        if let Some(to) = &self.to {
            fields.push("To".into());
            values.push(Cell::from(to.clone()));
        }
        if let Some(reason) = &self.skipped_reason {
            fields.push("Skipped reason".into());
            values.push(Cell::from(reason.to_string()));
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}

#[cfg(test)]
mod tests {
    use ocx_lib::{
        oci,
        package_manager::{SelfUpdateResult, SkippedReason, UpdateCheckResult},
    };
    use serde_json::json;

    use super::{SelfUpdateData, UpdateCheckData};

    // ── UpdateCheckData JSON shape snapshots ─────────────────────────────────

    /// `AlreadyUpToDate` serializes to `{"status":"up_to_date"}` only — the
    /// `identifier` and `skipped_reason` fields are absent (not empty strings).
    #[test]
    fn update_check_data_already_up_to_date_omits_optional_fields() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::AlreadyUpToDate);
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value, json!({"status": "up_to_date"}));
    }

    /// `UpdateAvailable` carries the identifier under a typed field name.
    #[test]
    fn update_check_data_update_available_carries_identifier() {
        let identifier =
            oci::Identifier::new_registry("ocx/cli", oci::OCX_SH_REGISTRY).clone_with_tag("1.2.3".to_string());
        let data = UpdateCheckData::from_result(&UpdateCheckResult::UpdateAvailable(identifier));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["status"], json!("update_available"));
        let id = value["identifier"].as_str().unwrap();
        assert!(
            id.contains("1.2.3"),
            "identifier must carry the version tag; got {id:?}"
        );
        assert!(value.get("skipped_reason").is_none());
    }

    /// `Skipped(Bootstrap)` serializes the `SkippedReason` as a structured
    /// object — `{"reason": "bootstrap"}` — not a Display string.
    #[test]
    fn update_check_data_skipped_bootstrap_serializes_structured() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::Bootstrap));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value,
            json!({"status": "skipped", "skipped_reason": {"reason": "bootstrap"}})
        );
    }

    /// `Skipped(Offline)` → `{"reason":"offline"}`.
    #[test]
    fn update_check_data_skipped_offline_serializes_structured() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::Offline));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"reason": "offline"}));
    }

    /// `Skipped(Throttled)` → `{"reason":"throttled"}`.
    #[test]
    fn update_check_data_skipped_throttled_serializes_structured() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::Throttled));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"reason": "throttled"}));
    }

    /// `Skipped(RegistryProbeFailed)` carries the detail string under the
    /// `detail` field of the discriminator object.
    #[test]
    fn update_check_data_skipped_registry_probe_failed_carries_detail() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::RegistryProbeFailed(
            "connection refused".into(),
        )));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value["skipped_reason"],
            json!({"reason": "registry_probe_failed", "detail": "connection refused"})
        );
    }

    /// `Skipped(UnparseableCurrent)` carries the version string.
    #[test]
    fn update_check_data_skipped_unparseable_current_carries_detail() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::UnparseableCurrent(
            "not-a-version".into(),
        )));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value["skipped_reason"],
            json!({"reason": "unparseable_current", "detail": "not-a-version"})
        );
    }

    /// `Skipped(UnparseableLatest)` is a unit variant — no detail field.
    #[test]
    fn update_check_data_skipped_unparseable_latest_serializes_unit() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::UnparseableLatest));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"reason": "unparseable_latest"}));
    }

    /// `Skipped(NoReleaseTag)` unit variant.
    #[test]
    fn update_check_data_skipped_no_release_tag_serializes_unit() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::NoReleaseTag));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"reason": "no_release_tag"}));
    }

    /// `Skipped(NotFound)` unit variant.
    #[test]
    fn update_check_data_skipped_not_found_serializes_unit() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::NotFound));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"reason": "not_found"}));
    }

    // ── SelfUpdateData JSON shape snapshots ───────────────────────────────────

    /// `AlreadyUpToDate` carries only `status`.
    #[test]
    fn self_update_data_already_up_to_date_omits_optional_fields() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::AlreadyUpToDate);
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value, json!({"status": "up_to_date"}));
    }

    /// `Installed { from: Some, to }` carries both `from` and `to`.
    #[test]
    fn self_update_data_installed_with_known_from() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Installed {
            from: Some("0.2.9".to_string()),
            to: "0.3.0".to_string(),
        });
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value, json!({"status": "installed", "from": "0.2.9", "to": "0.3.0"}));
    }

    /// `Installed { from: None, to }` (bootstrap) omits `from` instead of
    /// serializing it as `""` — the old wire format coerced absent to empty.
    #[test]
    fn self_update_data_installed_with_unknown_from_omits_field() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Installed {
            from: None,
            to: "0.3.0".to_string(),
        });
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value, json!({"status": "installed", "to": "0.3.0"}));
        assert!(value.get("from").is_none(), "absent from must be omitted");
    }

    /// `Skipped(Bootstrap)` serializes the structured `SkippedReason`.
    #[test]
    fn self_update_data_skipped_bootstrap_serializes_structured() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Skipped(SkippedReason::Bootstrap));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value,
            json!({"status": "skipped", "skipped_reason": {"reason": "bootstrap"}})
        );
    }

    /// `Skipped(Throttled)` end-to-end through `SelfUpdateData`.
    #[test]
    fn self_update_data_skipped_throttled_serializes_structured() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Skipped(SkippedReason::Throttled));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"reason": "throttled"}));
    }

    /// `Skipped(RegistryProbeFailed)` through `SelfUpdateData` carries detail.
    #[test]
    fn self_update_data_skipped_registry_probe_failed_carries_detail() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Skipped(SkippedReason::RegistryProbeFailed(
            "503".into(),
        )));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value["skipped_reason"],
            json!({"reason": "registry_probe_failed", "detail": "503"})
        );
    }
}
