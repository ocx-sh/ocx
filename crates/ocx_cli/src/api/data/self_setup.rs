// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use ocx_lib::setup::{BootstrapOutcome, BootstrapStatus, ManagedConfigSetupOutcome, ProfileOutcome, SetupOutcome};
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
/// Unpinned path (no VERSION): `{"status":"already_present"}` or
/// `{"status":"pulled","version":"1.2.3"}`.
/// Pinned path (VERSION given): same shapes plus `"digest":"sha256:<hex>"` when
/// resolution produced one. `digest` is omitted on unpinned fast-path runs so
/// existing JSON consumers stay byte-identical (plan D7).
#[derive(Serialize)]
struct BootstrapEntry {
    status: ApiBootstrapStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// Resolved content digest; present when pinning produced one.
    ///
    /// Stringified at this API boundary — lib carries [`ocx_lib::oci::Digest`].
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
}

/// API-layer status discriminant (mirrors `ocx_lib::setup::BootstrapStatus`).
///
/// Named `ApiBootstrapStatus` to avoid shadowing the lib type imported above.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum ApiBootstrapStatus {
    AlreadyPresent,
    Pulled,
    WouldPull,
}

impl BootstrapEntry {
    fn from_outcome(outcome: &BootstrapOutcome) -> Self {
        let status = match outcome.status {
            BootstrapStatus::AlreadyPresent => ApiBootstrapStatus::AlreadyPresent,
            BootstrapStatus::Pulled => ApiBootstrapStatus::Pulled,
            BootstrapStatus::WouldPull => ApiBootstrapStatus::WouldPull,
        };
        Self {
            status,
            version: outcome.version.clone(),
            // Stringify the typed Digest at the API serialization boundary.
            digest: outcome.digest.as_ref().map(|d| d.to_string()),
        }
    }

    /// Plain-text summary of the bootstrap outcome for the key/value table.
    fn summary(&self) -> String {
        match (&self.status, &self.version) {
            (ApiBootstrapStatus::AlreadyPresent, _) => "already present".to_string(),
            (ApiBootstrapStatus::Pulled, Some(version)) => format!("pulled {version}"),
            (ApiBootstrapStatus::WouldPull, Some(version)) => format!("would pull {version}"),
            // `version` is always Some for Pulled / WouldPull (contract 2).
            (ApiBootstrapStatus::Pulled, None) => "pulled".to_string(),
            (ApiBootstrapStatus::WouldPull, None) => "would pull".to_string(),
        }
    }
}

// ── managed-config adoption outcome (phase 1.5) ──────────────────────────────

/// JSON-serialized managed-config adoption outcome:
/// `{"status":"…"}` or, for `Adopted`/`AlreadyAdopted`,
/// `{"status":"…","digest":"sha256:<hex>"}` (the digest is the operator's
/// TOFU signal — always visible on adopt paths).
#[derive(Serialize)]
struct ManagedConfigEntry {
    status: ManagedConfigStatusKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<String>,
}

/// Serde-facing mirror of [`ManagedConfigSetupOutcome`] (`snake_case` discriminant).
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ManagedConfigStatusKind {
    NotConfigured,
    AlreadyAdopted,
    Adopted,
    Cleared,
    Dirty,
    WouldAdopt,
}

impl ManagedConfigEntry {
    fn from_outcome(outcome: &ManagedConfigSetupOutcome) -> Self {
        match outcome {
            ManagedConfigSetupOutcome::NotConfigured => Self {
                status: ManagedConfigStatusKind::NotConfigured,
                digest: None,
            },
            ManagedConfigSetupOutcome::AlreadyAdopted { digest } => Self {
                status: ManagedConfigStatusKind::AlreadyAdopted,
                digest: Some(digest.to_string()),
            },
            ManagedConfigSetupOutcome::Adopted { digest } => Self {
                status: ManagedConfigStatusKind::Adopted,
                digest: Some(digest.to_string()),
            },
            ManagedConfigSetupOutcome::Cleared => Self {
                status: ManagedConfigStatusKind::Cleared,
                digest: None,
            },
            ManagedConfigSetupOutcome::Dirty => Self {
                status: ManagedConfigStatusKind::Dirty,
                digest: None,
            },
            ManagedConfigSetupOutcome::WouldAdopt => Self {
                status: ManagedConfigStatusKind::WouldAdopt,
                digest: None,
            },
        }
    }

    fn summary(&self) -> String {
        match (&self.status, &self.digest) {
            (ManagedConfigStatusKind::NotConfigured, _) => "not configured".to_string(),
            (ManagedConfigStatusKind::AlreadyAdopted, Some(digest)) => format!("already adopted ({digest})"),
            (ManagedConfigStatusKind::AlreadyAdopted, None) => "already adopted".to_string(),
            (ManagedConfigStatusKind::Adopted, Some(digest)) => format!("adopted ({digest})"),
            (ManagedConfigStatusKind::Adopted, None) => "adopted".to_string(),
            (ManagedConfigStatusKind::Cleared, _) => "cleared".to_string(),
            (ManagedConfigStatusKind::Dirty, _) => "dirty (edited by hand)".to_string(),
            (ManagedConfigStatusKind::WouldAdopt, _) => "would adopt".to_string(),
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
/// - `{"status":"completed","bootstrap":{…},"shims":[…],"profiles":[{"path":"…","outcome":"completed"}],"managed_config":{"status":"not_configured"}}`
/// - dirty → `{"status":"skipped",…,"dirty_profiles":["…"]}`
/// - `managed_config` is always present: `{"status":"…"}`, plus `"digest"` on
///   the adopt paths (`adopted` / `already_adopted`).
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
    /// Result of adopting/clearing the `--managed-config` tier (phase 1.5).
    managed_config: ManagedConfigEntry,
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
            managed_config: ManagedConfigEntry::from_outcome(&outcome.managed_config),
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

    if profile_outcomes().any(|p| p == ProfileOutcome::SkippedDirty)
        || matches!(outcome.managed_config, ManagedConfigSetupOutcome::Dirty)
    {
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

        if let Some(digest) = &self.bootstrap.digest {
            fields.push("Digest".into());
            values.push(Cell::from(digest.clone()));
        }

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

        if !matches!(self.managed_config.status, ManagedConfigStatusKind::NotConfigured) {
            fields.push("Managed config".into());
            values.push(Cell::from(self.managed_config.summary()));
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ocx_lib::setup::{BootstrapOutcome, BootstrapStatus, ManagedConfigSetupOutcome, ProfileOutcome, SetupOutcome};
    use serde_json::json;

    use super::SelfSetupData;

    fn outcome() -> SetupOutcome {
        SetupOutcome {
            bootstrap: BootstrapOutcome {
                status: BootstrapStatus::AlreadyPresent,
                version: None,
                digest: None,
            },
            shims_written: Vec::new(),
            profiles: Vec::new(),
            exec_policy_warning: None,
            conflicting_ocx: None,
            reload_hint: false,
            managed_config: ManagedConfigSetupOutcome::NotConfigured,
        }
    }

    /// A run that wrote a shim and one completed profile serializes to
    /// `status: completed` with the typed bootstrap + profile shapes.
    #[test]
    fn completed_run_serializes_with_shims_and_profiles() {
        let mut base = outcome();
        base.bootstrap = BootstrapOutcome {
            status: BootstrapStatus::Pulled,
            version: Some("1.2.3".to_string()),
            digest: None,
        };
        base.shims_written = vec![PathBuf::from("/home/dev/.ocx/env.sh")];
        base.profiles = vec![(PathBuf::from("/home/dev/.bashrc"), ProfileOutcome::Completed)];
        base.reload_hint = true;

        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        assert_eq!(value["status"], json!("completed"));
        // Unpinned pull: digest absent so JSON is byte-identical to former shape.
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
        base.bootstrap = BootstrapOutcome {
            status: BootstrapStatus::WouldPull,
            version: Some("2.0.0".to_string()),
            digest: None,
        };
        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        // Unpinned dry-run: digest absent so JSON is byte-identical to former shape.
        assert_eq!(value["bootstrap"], json!({"status": "would_pull", "version": "2.0.0"}));
    }

    /// A pinned dry-run (`WouldPull` + digest) serializes `bootstrap.digest` as
    /// `"sha256:<hex>"` string (plan D7: digest surfaces in JSON on pinned path).
    #[test]
    fn would_pull_with_digest_serializes_digest_field() {
        use ocx_lib::oci::Digest;

        let hex = "a".repeat(64);
        let digest = Digest::Sha256(hex.clone());
        let expected_digest_str = format!("sha256:{hex}");

        let mut base = outcome();
        base.bootstrap = BootstrapOutcome {
            status: BootstrapStatus::WouldPull,
            version: Some("0.9.2".to_string()),
            digest: Some(digest),
        };

        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        assert_eq!(value["bootstrap"]["status"], json!("would_pull"));
        assert_eq!(value["bootstrap"]["version"], json!("0.9.2"));
        assert_eq!(
            value["bootstrap"]["digest"],
            json!(expected_digest_str),
            "bootstrap.digest must serialize to the sha256:<hex> string"
        );
    }
}
