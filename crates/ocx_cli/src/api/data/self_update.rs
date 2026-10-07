// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_console::Cell;
use ocx_oci::PackageRef;
use ocx_package_manager::{HandoffFailure, SelfUpdateResult, SkippedReason, UpdateCheckResult};
use serde::{Serialize, Serializer};

use crate::api::Printable;

/// Outcome of an update check or a self-update.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "SelfUpdateStatus")]
enum StatusKind {
    /// The installed version is the latest.
    UpToDate,
    /// The check did not run; `skipped_reason` says why.
    Skipped,
    /// A newer version is available (`--check`).
    UpdateAvailable,
    /// A newer version was pulled and activated.
    Installed,
    /// The release was downloaded but nothing was activated — `current` still
    /// names the previously installed binary.
    Pulled,
}

/// Why an update check was skipped.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[schemars(rename = "SkippedReason")]
enum SkippedReasonData<'a> {
    /// The installed version could not be queried, so the check cannot compare versions.
    Bootstrap,
    /// Offline mode blocked the registry probe.
    Offline,
    /// The 24-hour throttle window has not elapsed since the last probe.
    Throttled,
    /// The registry probe failed.
    RegistryProbeFailed {
        /// The probe error.
        detail: &'a str,
    },
    /// The package was not found in the registry.
    NotFound,
    /// The installed version string is not a version.
    UnparseableCurrent {
        /// The installed version string.
        version: &'a str,
    },
    /// The registry's latest tag is not a version.
    UnparseableLatest,
    /// The registry lists no `major.minor.patch` release tag.
    NoReleaseTag,
}

impl<'a> From<&'a SkippedReason> for SkippedReasonData<'a> {
    fn from(reason: &'a SkippedReason) -> Self {
        match reason {
            SkippedReason::Bootstrap => Self::Bootstrap,
            SkippedReason::Offline => Self::Offline,
            SkippedReason::Throttled => Self::Throttled,
            SkippedReason::RegistryProbeFailed(detail) => Self::RegistryProbeFailed { detail },
            SkippedReason::NotFound => Self::NotFound,
            SkippedReason::UnparseableCurrent(version) => Self::UnparseableCurrent { version },
            SkippedReason::UnparseableLatest => Self::UnparseableLatest,
            SkippedReason::NoReleaseTag => Self::NoReleaseTag,
        }
    }
}

fn serialize_skipped_reason<S: Serializer>(reason: &Option<SkippedReason>, serializer: S) -> Result<S::Ok, S::Error> {
    reason.as_ref().map(SkippedReasonData::from).serialize(serializer)
}

/// How the hand-off to the new binary's own `ocx self setup` ended, when it did not end cleanly.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[schemars(rename = "HandoffFailure")]
enum HandoffFailureData<'a> {
    /// The new binary could not be started.
    SpawnFailed {
        /// The underlying error.
        detail: &'a str,
    },
    /// The new binary's setup exited non-zero; `81` means it left a user-edited profile alone.
    Exited {
        /// The setup's exit code.
        exit_code: i32,
    },
    /// The new binary's setup was killed by a signal (Unix only).
    Signalled {
        /// The signal number.
        signal: i32,
    },
}

impl<'a> From<&'a HandoffFailure> for HandoffFailureData<'a> {
    fn from(failure: &'a HandoffFailure) -> Self {
        match failure {
            HandoffFailure::SpawnFailed(detail) => Self::SpawnFailed { detail },
            HandoffFailure::Exited(exit_code) => Self::Exited { exit_code: *exit_code },
            HandoffFailure::Signalled(signal) => Self::Signalled { signal: *signal },
        }
    }
}

fn serialize_handoff<S: Serializer>(handoff: &Option<HandoffFailure>, serializer: S) -> Result<S::Ok, S::Error> {
    handoff.as_ref().map(HandoffFailureData::from).serialize(serializer)
}

impl std::fmt::Display for StatusKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Must match the serde `snake_case` names.
        match self {
            Self::UpToDate => f.write_str("up_to_date"),
            Self::Skipped => f.write_str("skipped"),
            Self::UpdateAvailable => f.write_str("update_available"),
            Self::Installed => f.write_str("installed"),
            Self::Pulled => f.write_str("pulled"),
        }
    }
}

/// Result of `ocx self update --check`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct UpdateCheckData {
    /// What the check found.
    status: StatusKind,
    /// Identifier of the available update; present iff status = `update_available`.
    #[serde(skip_serializing_if = "Option::is_none")]
    identifier: Option<PackageRef>,
    /// Why the check was skipped; present iff status = `skipped`.
    // Serialized through the report's own tagged shape; plain output keeps the library's prose.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "serialize_skipped_reason")]
    #[schemars(with = "Option<SkippedReasonData<'static>>")]
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
                identifier: Some(identifier.clone()),
                skipped_reason: None,
            },
        }
    }
}

impl Printable for UpdateCheckData {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "UpdateCheckData";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        // Only fields with a payload get a row.
        let mut fields: Vec<Cell> = vec!["Status".into()];
        let mut values: Vec<Cell> = vec![Cell::from(self.status.to_string())];

        if let Some(identifier) = &self.identifier {
            fields.push("Identifier".into());
            values.push(Cell::from(identifier.to_string()));
        }
        if let Some(reason) = &self.skipped_reason {
            fields.push("Skipped reason".into());
            values.push(Cell::from(reason.to_string()));
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}

/// Result of `ocx self update`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct SelfUpdateData {
    /// What the update did.
    status: StatusKind,
    /// Previously installed version; present on `installed` and `pulled` when
    /// the old binary's version query succeeded. Absent when that query was not
    /// available (binary absent, non-zero exit, malformed JSON output).
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    /// Newly downloaded version; present iff status is `installed` or `pulled`.
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    /// Why the update was skipped; present iff status = `skipped`.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "serialize_skipped_reason")]
    #[schemars(with = "Option<SkippedReasonData<'static>>")]
    skipped_reason: Option<SkippedReason>,
    /// How the hand-off to the new binary's own `ocx self setup` ended, when it
    /// did not end cleanly. Present on `installed` (the swap landed, some setup
    /// surface may not have been written) and on `pulled` (nothing was
    /// activated); absent whenever the child completed.
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "serialize_handoff")]
    #[schemars(with = "Option<HandoffFailureData<'static>>")]
    handoff: Option<HandoffFailure>,
}

impl SelfUpdateData {
    pub fn from_result(result: &SelfUpdateResult) -> Self {
        match result {
            SelfUpdateResult::AlreadyUpToDate => Self {
                status: StatusKind::UpToDate,
                from: None,
                to: None,
                skipped_reason: None,
                handoff: None,
            },
            SelfUpdateResult::Installed { from, to, handoff } => Self {
                status: StatusKind::Installed,
                from: from.clone(),
                to: Some(to.clone()),
                skipped_reason: None,
                handoff: handoff.clone(),
            },
            SelfUpdateResult::Pulled { from, to, handoff } => Self {
                status: StatusKind::Pulled,
                from: from.clone(),
                to: Some(to.clone()),
                skipped_reason: None,
                handoff: handoff.clone(),
            },
            SelfUpdateResult::Skipped(reason) => Self {
                status: StatusKind::Skipped,
                from: None,
                to: None,
                skipped_reason: Some(reason.clone()),
                handoff: None,
            },
        }
    }
}

impl Printable for SelfUpdateData {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "SelfUpdateData";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        // Only fields with a payload get a row.
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
        if let Some(handoff) = &self.handoff {
            fields.push("Handoff".into());
            values.push(Cell::from(handoff.to_string()));
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}

#[cfg(test)]
mod tests {
    use ocx_package_manager::{HandoffFailure, SelfUpdateResult, SkippedReason, UpdateCheckResult};
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
            ocx_oci::PackageRef::new_registry("ocx/cli", ocx_oci::OCX_SH_REGISTRY).clone_with_tag("1.2.3".to_string());
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
    /// object — `{"type": "bootstrap"}` — not a Display string.
    #[test]
    fn update_check_data_skipped_bootstrap_serializes_structured() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::Bootstrap));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value,
            json!({"status": "skipped", "skipped_reason": {"type": "bootstrap"}})
        );
    }

    /// `Skipped(Offline)` → `{"type":"offline"}`.
    #[test]
    fn update_check_data_skipped_offline_serializes_structured() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::Offline));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"type": "offline"}));
    }

    /// `Skipped(Throttled)` → `{"type":"throttled"}`.
    #[test]
    fn update_check_data_skipped_throttled_serializes_structured() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::Throttled));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"type": "throttled"}));
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
            json!({"type": "registry_probe_failed", "detail": "connection refused"})
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
            json!({"type": "unparseable_current", "version": "not-a-version"})
        );
    }

    /// `Skipped(UnparseableLatest)` is a unit variant — no detail field.
    #[test]
    fn update_check_data_skipped_unparseable_latest_serializes_unit() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::UnparseableLatest));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"type": "unparseable_latest"}));
    }

    /// `Skipped(NoReleaseTag)` unit variant.
    #[test]
    fn update_check_data_skipped_no_release_tag_serializes_unit() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::NoReleaseTag));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"type": "no_release_tag"}));
    }

    /// `Skipped(NotFound)` unit variant.
    #[test]
    fn update_check_data_skipped_not_found_serializes_unit() {
        let data = UpdateCheckData::from_result(&UpdateCheckResult::Skipped(SkippedReason::NotFound));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"type": "not_found"}));
    }

    // ── SelfUpdateData JSON shape snapshots ───────────────────────────────────

    /// `AlreadyUpToDate` carries only `status`.
    #[test]
    fn self_update_data_already_up_to_date_omits_optional_fields() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::AlreadyUpToDate);
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value, json!({"status": "up_to_date"}));
    }

    /// `Installed { from: Some, to }` carries both `from` and `to` — and, when
    /// the hand-off completed, exactly the payload this command has always
    /// emitted, with no `handoff` key.
    #[test]
    fn self_update_data_installed_with_known_from() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Installed {
            from: Some("0.2.9".to_string()),
            to: "0.3.0".to_string(),
            handoff: None,
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
            handoff: None,
        });
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value, json!({"status": "installed", "to": "0.3.0"}));
        assert!(value.get("from").is_none(), "absent from must be omitted");
    }

    /// A swap that landed with an unfinished setup stays `installed` and adds
    /// the structured `handoff` — the status must not become a failure.
    #[test]
    fn self_update_data_installed_carries_a_handoff_failure() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Installed {
            from: Some("0.6.0".to_string()),
            to: "0.6.1".to_string(),
            handoff: Some(HandoffFailure::Exited(81)),
        });
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value,
            json!({
                "status": "installed",
                "from": "0.6.0",
                "to": "0.6.1",
                "handoff": {"type": "exited", "exit_code": 81},
            })
        );
    }

    /// `Pulled` is its own status, carrying what was downloaded and why it was
    /// not activated.
    #[test]
    fn self_update_data_pulled_reports_its_own_status() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Pulled {
            from: Some("0.6.0".to_string()),
            to: "0.6.1".to_string(),
            handoff: Some(HandoffFailure::SpawnFailed("permission denied".to_string())),
        });
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value,
            json!({
                "status": "pulled",
                "from": "0.6.0",
                "to": "0.6.1",
                "handoff": {"type": "spawn_failed", "detail": "permission denied"},
            })
        );
    }

    /// A signalled child serializes under its own reason.
    #[test]
    fn self_update_data_pulled_reports_a_signalled_child() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Pulled {
            from: None,
            to: "0.6.1".to_string(),
            handoff: Some(HandoffFailure::Signalled(9)),
        });
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["handoff"], json!({"type": "signalled", "signal": 9}));
    }

    /// `Skipped(Bootstrap)` serializes the structured `SkippedReason`.
    #[test]
    fn self_update_data_skipped_bootstrap_serializes_structured() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Skipped(SkippedReason::Bootstrap));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(
            value,
            json!({"status": "skipped", "skipped_reason": {"type": "bootstrap"}})
        );
    }

    /// `Skipped(Throttled)` end-to-end through `SelfUpdateData`.
    #[test]
    fn self_update_data_skipped_throttled_serializes_structured() {
        let data = SelfUpdateData::from_result(&SelfUpdateResult::Skipped(SkippedReason::Throttled));
        let value = serde_json::to_value(&data).unwrap();
        assert_eq!(value["skipped_reason"], json!({"type": "throttled"}));
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
            json!({"type": "registry_probe_failed", "detail": "503"})
        );
    }
}
