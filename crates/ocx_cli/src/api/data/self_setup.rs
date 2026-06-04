// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use ocx_lib::setup::{BootstrapOutcome, ProfileOutcome, SetupOutcome};
use serde::Serialize;

use crate::api::Printable;

// ── StatusKind ────────────────────────────────────────────────────────────────

/// Top-level status discriminant for a `ocx self setup` run.
///
/// Serde serializes each variant to its `snake_case` name, matching the JSON
/// wire format the snapshot tests pin. `Skipped` is the dirty-RC outcome (the
/// CLI maps it to exit 82); every other status is exit 0.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum StatusKind {
    /// Shims and/or profiles were written or upgraded.
    Completed,
    /// Nothing changed — every shim and profile was already current.
    NoOp,
    /// At least one profile carried user edits and was left untouched.
    Skipped,
    /// A legacy footprint was migrated to the v1 fence.
    Migrated,
}

impl std::fmt::Display for StatusKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => f.write_str("completed"),
            Self::NoOp => f.write_str("no_op"),
            Self::Skipped => f.write_str("skipped"),
            Self::Migrated => f.write_str("migrated"),
        }
    }
}

// ── per-profile outcome ───────────────────────────────────────────────────────

/// JSON-serialized per-profile outcome (`{"path":"…","outcome":"completed"}`).
#[derive(Serialize)]
struct ProfileEntry {
    path: String,
    outcome: ProfileOutcomeKind,
}

/// Serde-facing mirror of [`ProfileOutcome`] (`snake_case` discriminant).
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ProfileOutcomeKind {
    Completed,
    NoOp,
    Migrated,
    SkippedDirty,
}

impl From<ProfileOutcome> for ProfileOutcomeKind {
    fn from(outcome: ProfileOutcome) -> Self {
        match outcome {
            ProfileOutcome::Completed => Self::Completed,
            ProfileOutcome::NoOp => Self::NoOp,
            ProfileOutcome::Migrated => Self::Migrated,
            ProfileOutcome::SkippedDirty => Self::SkippedDirty,
        }
    }
}

impl std::fmt::Display for ProfileOutcomeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Completed => f.write_str("completed"),
            Self::NoOp => f.write_str("no_op"),
            Self::Migrated => f.write_str("migrated"),
            Self::SkippedDirty => f.write_str("skipped_dirty"),
        }
    }
}

// ── bootstrap outcome ─────────────────────────────────────────────────────────

/// JSON-serialized bootstrap outcome.
///
/// `{"status":"already_present"}`, `{"status":"pulled","version":"1.2.3"}`, or
/// `{"status":"would_pull","version":"1.2.3"}` (dry-run).
#[derive(Serialize)]
struct BootstrapEntry {
    status: BootstrapStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum BootstrapStatus {
    AlreadyPresent,
    Pulled,
    WouldPull,
}

impl BootstrapEntry {
    fn from_outcome(outcome: &BootstrapOutcome) -> Self {
        match outcome {
            BootstrapOutcome::AlreadyPresent => Self {
                status: BootstrapStatus::AlreadyPresent,
                version: None,
            },
            BootstrapOutcome::Pulled { version } => Self {
                status: BootstrapStatus::Pulled,
                version: Some(version.clone()),
            },
            BootstrapOutcome::WouldPull { version } => Self {
                status: BootstrapStatus::WouldPull,
                version: Some(version.clone()),
            },
        }
    }

    /// Plain-text summary of the bootstrap outcome for the key/value table.
    fn summary(&self) -> String {
        match (&self.status, &self.version) {
            (BootstrapStatus::AlreadyPresent, _) => "already present".to_string(),
            (BootstrapStatus::Pulled, Some(version)) => format!("pulled {version}"),
            (BootstrapStatus::WouldPull, Some(version)) => format!("would pull {version}"),
            // `version` is always Some for Pulled / WouldPull (contract 2).
            (BootstrapStatus::Pulled, None) => "pulled".to_string(),
            (BootstrapStatus::WouldPull, None) => "would pull".to_string(),
        }
    }
}

// ── SelfSetupData ─────────────────────────────────────────────────────────────

/// CLI wrapper around [`SetupOutcome`] for API reporting.
///
/// Plain format: a key/value table — `Status`, `Bootstrap`, written shims,
/// per-profile outcomes, and any advisory (exec-policy / conflicting ocx /
/// reload hint) — with empty rows suppressed.
///
/// JSON format (discriminated by `status`):
/// - `{"status":"completed","bootstrap":{…},"shims":[…],"profiles":[{"path":"…","outcome":"completed"}]}`
/// - dirty → `{"status":"skipped",…,"dirty_profiles":["…"]}`
/// - `exec_policy_warning`, `conflicting_ocx`, and `reload_hint` appear only
///   when present.
#[derive(Serialize)]
pub struct SelfSetupData {
    status: StatusKind,
    bootstrap: BootstrapEntry,
    shims: Vec<String>,
    profiles: Vec<ProfileEntry>,
    /// Profiles skipped because the user edited the managed block; present iff
    /// status = `skipped`. Carried separately for a script to `case` on without
    /// scanning the `profiles` list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dirty_profiles: Vec<String>,
    /// Windows execution-policy `Restricted` advisory, if applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    exec_policy_warning: Option<String>,
    /// An `ocx` on `PATH` ahead of the directory the shim prepends, if found.
    #[serde(skip_serializing_if = "Option::is_none")]
    conflicting_ocx: Option<String>,
    /// Whether the user should re-source their profile to activate ocx now.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    reload_hint: bool,
}

impl SelfSetupData {
    pub fn from_outcome(outcome: &SetupOutcome) -> Self {
        let profiles: Vec<ProfileEntry> = outcome
            .profiles
            .iter()
            .map(|(path, profile_outcome)| ProfileEntry {
                path: path.display().to_string(),
                outcome: (*profile_outcome).into(),
            })
            .collect();

        let dirty_profiles: Vec<String> = outcome
            .profiles
            .iter()
            .filter(|(_, profile_outcome)| *profile_outcome == ProfileOutcome::SkippedDirty)
            .map(|(path, _)| path.display().to_string())
            .collect();

        Self {
            status: derive_status(outcome),
            bootstrap: BootstrapEntry::from_outcome(&outcome.bootstrap),
            shims: outcome
                .shims_written
                .iter()
                .map(|path| path.display().to_string())
                .collect(),
            profiles,
            dirty_profiles,
            exec_policy_warning: outcome.exec_policy_warning.clone(),
            conflicting_ocx: outcome.conflicting_ocx.as_ref().map(|path| path.display().to_string()),
            reload_hint: outcome.reload_hint,
        }
    }
}

/// Reduce the per-profile outcomes (and shim writes) to one top-level status.
///
/// Precedence: any dirty profile → `Skipped`; else any migrated → `Migrated`;
/// else any completed work (a written shim or a completed profile) →
/// `Completed`; else `NoOp`.
fn derive_status(outcome: &SetupOutcome) -> StatusKind {
    let profile_outcomes = || outcome.profiles.iter().map(|(_, profile_outcome)| *profile_outcome);

    if profile_outcomes().any(|p| p == ProfileOutcome::SkippedDirty) {
        return StatusKind::Skipped;
    }
    if profile_outcomes().any(|p| p == ProfileOutcome::Migrated) {
        return StatusKind::Migrated;
    }
    let wrote_shim = !outcome.shims_written.is_empty();
    let completed_profile = profile_outcomes().any(|p| p == ProfileOutcome::Completed);
    if wrote_shim || completed_profile {
        return StatusKind::Completed;
    }
    StatusKind::NoOp
}

impl Printable for SelfSetupData {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        // Key/value layout: only rows with payload appear (mirrors SelfUpdateData).
        let mut fields: Vec<Cell> = vec!["Status".into(), "Bootstrap".into()];
        let mut values: Vec<Cell> = vec![
            Cell::from(self.status.to_string()),
            Cell::from(self.bootstrap.summary()),
        ];

        for (index, shim) in self.shims.iter().enumerate() {
            let label = if index == 0 { "Shims".to_string() } else { String::new() };
            fields.push(Cell::from(label));
            values.push(Cell::from(shim.clone()));
        }

        for (index, profile) in self.profiles.iter().enumerate() {
            let label = if index == 0 {
                "Profiles".to_string()
            } else {
                String::new()
            };
            fields.push(Cell::from(label));
            values.push(Cell::from(format!("{} ({})", profile.path, profile.outcome)));
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ocx_lib::setup::{BootstrapOutcome, ProfileOutcome, SetupOutcome};
    use serde_json::json;

    use super::SelfSetupData;

    fn outcome() -> SetupOutcome {
        SetupOutcome {
            bootstrap: BootstrapOutcome::AlreadyPresent,
            shims_written: Vec::new(),
            profiles: Vec::new(),
            exec_policy_warning: None,
            conflicting_ocx: None,
            reload_hint: false,
        }
    }

    /// A run that wrote a shim and one completed profile serializes to
    /// `status: completed` with the typed bootstrap + profile shapes.
    #[test]
    fn completed_run_serializes_with_shims_and_profiles() {
        let mut base = outcome();
        base.bootstrap = BootstrapOutcome::Pulled {
            version: "1.2.3".to_string(),
        };
        base.shims_written = vec![PathBuf::from("/home/dev/.ocx/env.sh")];
        base.profiles = vec![(PathBuf::from("/home/dev/.bashrc"), ProfileOutcome::Completed)];
        base.reload_hint = true;

        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        assert_eq!(value["status"], json!("completed"));
        assert_eq!(value["bootstrap"], json!({"status": "pulled", "version": "1.2.3"}));
        assert_eq!(value["shims"], json!(["/home/dev/.ocx/env.sh"]));
        assert_eq!(
            value["profiles"],
            json!([{"path": "/home/dev/.bashrc", "outcome": "completed"}])
        );
        assert_eq!(value["reload_hint"], json!(true));
        assert!(
            value.get("dirty_profiles").is_none(),
            "no dirty profiles → field absent"
        );
    }

    /// An all-current run with no writes serializes to `status: no_op` and omits
    /// the optional fields (no shims, no profiles, no reload hint).
    #[test]
    fn no_op_run_serializes_minimally() {
        let value = serde_json::to_value(SelfSetupData::from_outcome(&outcome())).unwrap();
        assert_eq!(value["status"], json!("no_op"));
        assert_eq!(value["bootstrap"], json!({"status": "already_present"}));
        assert_eq!(value["shims"], json!([]));
        assert_eq!(value["profiles"], json!([]));
        assert!(value.get("reload_hint").is_none(), "false reload_hint is omitted");
        assert!(value.get("exec_policy_warning").is_none());
        assert!(value.get("conflicting_ocx").is_none());
    }

    /// A dirty profile drives `status: skipped` and lists the path under
    /// `dirty_profiles` (so a script can `case` on it directly).
    #[test]
    fn dirty_profile_serializes_as_skipped_with_dirty_list() {
        let mut base = outcome();
        base.profiles = vec![
            (PathBuf::from("/home/dev/.bashrc"), ProfileOutcome::Completed),
            (PathBuf::from("/home/dev/.zshrc"), ProfileOutcome::SkippedDirty),
        ];

        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        assert_eq!(value["status"], json!("skipped"));
        assert_eq!(value["dirty_profiles"], json!(["/home/dev/.zshrc"]));
        assert_eq!(
            value["profiles"],
            json!([
                {"path": "/home/dev/.bashrc", "outcome": "completed"},
                {"path": "/home/dev/.zshrc", "outcome": "skipped_dirty"}
            ])
        );
    }

    /// A migration (no dirty profile) drives `status: migrated`.
    #[test]
    fn migrated_profile_serializes_as_migrated() {
        let mut base = outcome();
        base.profiles = vec![(PathBuf::from("/home/dev/.bashrc"), ProfileOutcome::Migrated)];

        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        assert_eq!(value["status"], json!("migrated"));
    }

    /// The exec-policy advisory and conflicting-ocx path surface as JSON fields
    /// when present.
    #[test]
    fn advisories_surface_when_present() {
        let mut base = outcome();
        base.shims_written = vec![PathBuf::from("/home/dev/.ocx/env.ps1")];
        base.exec_policy_warning = Some("run Set-ExecutionPolicy …".to_string());
        base.conflicting_ocx = Some(PathBuf::from("/usr/local/bin/ocx"));

        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        assert_eq!(value["exec_policy_warning"], json!("run Set-ExecutionPolicy …"));
        assert_eq!(value["conflicting_ocx"], json!("/usr/local/bin/ocx"));
    }

    /// The dry-run bootstrap status (`would_pull`) round-trips through the report.
    #[test]
    fn would_pull_bootstrap_serializes() {
        let mut base = outcome();
        base.bootstrap = BootstrapOutcome::WouldPull {
            version: "2.0.0".to_string(),
        };
        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        assert_eq!(value["bootstrap"], json!({"status": "would_pull", "version": "2.0.0"}));
    }
}
