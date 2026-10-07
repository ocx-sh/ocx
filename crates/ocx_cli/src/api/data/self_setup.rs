// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_console::Cell;
use ocx_oci::Digest;
use ocx_setup::{
    BootstrapOutcome, BootstrapStatus, ExtraCaCertsOutcome, ManagedConfigSetupOutcome, ProfileOutcome,
    SessionPathOutcome, SetupOutcome,
};
use serde::Serialize;

use crate::api::Printable;

/// The overall outcome of an `ocx self setup` run; `skipped` exits 81, every other status 0.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "SelfSetupStatus")]
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

/// One shell profile and what this run did to its managed block.
#[derive(Serialize, schemars::JsonSchema)]
struct ProfileEntry {
    /// The profile file.
    path: String,
    /// What this run did to the profile's managed block.
    outcome: ProfileOutcomeKind,
}

/// What a run did to one profile's managed block.
#[derive(Serialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ProfileOutcomeKind {
    /// The managed block was written or upgraded.
    Completed,
    /// The managed block was already current.
    NoOp,
    /// A legacy footprint was migrated to the current fence.
    Migrated,
    /// The managed block carried user edits and was left untouched.
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

/// One session-PATH store and what this run did to it.
// `location`, not `path`: on Windows the store is `HKCU\Environment\Path`, a registry location.
#[derive(Serialize, schemars::JsonSchema)]
struct SessionPathEntry {
    /// Where the store lives: a file, or a registry location on Windows.
    location: String,
    /// What this run did to the store.
    outcome: SessionPathOutcomeKind,
}

/// What a run did to one session-PATH store.
#[derive(Serialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum SessionPathOutcomeKind {
    /// The ocx directories were written to the store.
    Written,
    /// The store already held the ocx directories.
    Unchanged,
    /// The ocx directories were removed from the store.
    Removed,
    /// Skipped: PATH modification is opted out.
    SkippedOptOut,
    /// Skipped: this host has no such store.
    SkippedUnsupported,
    /// Writing the store failed; the run warned and carried on.
    Failed,
}

impl From<SessionPathOutcome> for SessionPathOutcomeKind {
    fn from(outcome: SessionPathOutcome) -> Self {
        match outcome {
            SessionPathOutcome::Written => Self::Written,
            SessionPathOutcome::Unchanged => Self::Unchanged,
            SessionPathOutcome::Removed => Self::Removed,
            SessionPathOutcome::SkippedOptOut => Self::SkippedOptOut,
            SessionPathOutcome::SkippedUnsupported => Self::SkippedUnsupported,
            SessionPathOutcome::Failed => Self::Failed,
        }
    }
}

impl std::fmt::Display for SessionPathOutcomeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Written => f.write_str("written"),
            Self::Unchanged => f.write_str("unchanged"),
            Self::Removed => f.write_str("removed"),
            Self::SkippedOptOut => f.write_str("skipped_opt_out"),
            Self::SkippedUnsupported => f.write_str("skipped_unsupported"),
            Self::Failed => f.write_str("failed"),
        }
    }
}

/// Whether this run installed ocx into its own content store.
#[derive(Serialize, schemars::JsonSchema)]
struct BootstrapEntry {
    /// What the bootstrap did.
    status: ApiBootstrapStatus,
    /// The version pulled, or that a dry run would pull.
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    /// Resolved content digest; present when a pinned version produced one.
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<Digest>,
}

/// What a bootstrap did.
#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(rename = "BootstrapStatus")]
enum ApiBootstrapStatus {
    /// The requested ocx was already installed.
    AlreadyPresent,
    /// The requested ocx was pulled.
    Pulled,
    /// Dry run: the requested ocx would be pulled.
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
            digest: outcome.digest.clone(),
        }
    }

    /// Plain-text summary of the bootstrap outcome for the key/value table.
    fn summary(&self) -> String {
        match (&self.status, &self.version) {
            (ApiBootstrapStatus::AlreadyPresent, _) => "already present".to_string(),
            (ApiBootstrapStatus::Pulled, Some(version)) => format!("pulled {version}"),
            (ApiBootstrapStatus::WouldPull, Some(version)) => format!("would pull {version}"),
            // `version` is always `Some` for `Pulled`/`WouldPull`.
            (ApiBootstrapStatus::Pulled, None) => "pulled".to_string(),
            (ApiBootstrapStatus::WouldPull, None) => "would pull".to_string(),
        }
    }
}

/// The managed-config adoption outcome, shared by `ocx self setup` and `ocx config setup`.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ManagedConfigEntry {
    /// What the run did to the managed-config tier.
    status: ManagedConfigStatusKind,
    /// The adopted snapshot's digest, on every status that has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    digest: Option<Digest>,
    /// The digest the snapshot carried before a `refreshed` run.
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_digest: Option<Digest>,
    /// Why a `refresh_unavailable` run could not reach the registry.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// What a run did to the managed-config tier.
#[derive(Serialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ManagedConfigStatusKind {
    /// No managed config is configured.
    NotConfigured,
    /// The snapshot was already adopted and current.
    AlreadyAdopted,
    /// A snapshot was fetched and adopted.
    Adopted,
    /// The adopted snapshot was replaced by a newer one.
    Refreshed,
    /// The registry could not be reached; the adopted snapshot is kept.
    RefreshUnavailable,
    /// The managed-config tier was removed.
    Cleared,
    /// The managed block in `config.toml` carried user edits and was left untouched.
    Dirty,
    /// Dry run: a snapshot would be adopted.
    WouldAdopt,
    /// Dry run: the adopted snapshot would be refreshed.
    WouldRefresh,
}

impl ManagedConfigEntry {
    /// A status with an optional digest and no refresh-only fields.
    fn with_digest(status: ManagedConfigStatusKind, digest: Option<Digest>) -> Self {
        Self {
            status,
            digest,
            previous_digest: None,
            reason: None,
        }
    }

    pub fn from_outcome(outcome: &ManagedConfigSetupOutcome) -> Self {
        match outcome {
            ManagedConfigSetupOutcome::NotConfigured => Self::with_digest(ManagedConfigStatusKind::NotConfigured, None),
            ManagedConfigSetupOutcome::AlreadyAdopted { digest } => {
                Self::with_digest(ManagedConfigStatusKind::AlreadyAdopted, Some(digest.clone()))
            }
            ManagedConfigSetupOutcome::Adopted { digest } => {
                Self::with_digest(ManagedConfigStatusKind::Adopted, Some(digest.clone()))
            }
            ManagedConfigSetupOutcome::Refreshed { from, to } => Self {
                status: ManagedConfigStatusKind::Refreshed,
                digest: Some(to.clone()),
                previous_digest: Some(from.clone()),
                reason: None,
            },
            ManagedConfigSetupOutcome::RefreshUnavailable { digest, reason } => Self {
                status: ManagedConfigStatusKind::RefreshUnavailable,
                digest: Some(digest.clone()),
                previous_digest: None,
                reason: Some(reason.clone()),
            },
            ManagedConfigSetupOutcome::Cleared => Self::with_digest(ManagedConfigStatusKind::Cleared, None),
            ManagedConfigSetupOutcome::Dirty => Self::with_digest(ManagedConfigStatusKind::Dirty, None),
            ManagedConfigSetupOutcome::WouldAdopt => Self::with_digest(ManagedConfigStatusKind::WouldAdopt, None),
            ManagedConfigSetupOutcome::WouldRefresh { digest } => {
                Self::with_digest(ManagedConfigStatusKind::WouldRefresh, Some(digest.clone()))
            }
        }
    }

    /// The adoption outcome's plain row; the `refresh_unavailable` cause stays on stderr, too wide for
    /// the column budget.
    pub fn summary(&self) -> String {
        let digest = || self.digest.as_ref().map(Digest::to_string).unwrap_or_default();
        match self.status {
            ManagedConfigStatusKind::NotConfigured => "not configured".to_string(),
            ManagedConfigStatusKind::AlreadyAdopted => match &self.digest {
                Some(digest) => format!("already adopted ({digest})"),
                None => "already adopted".to_string(),
            },
            ManagedConfigStatusKind::Adopted => match &self.digest {
                Some(digest) => format!("adopted ({digest})"),
                None => "adopted".to_string(),
            },
            ManagedConfigStatusKind::Refreshed => match &self.previous_digest {
                Some(previous) => format!("refreshed ({previous} -> {})", digest()),
                None => format!("refreshed ({})", digest()),
            },
            ManagedConfigStatusKind::RefreshUnavailable => {
                format!("refresh unavailable, keeping {}", digest())
            }
            ManagedConfigStatusKind::Cleared => "cleared".to_string(),
            ManagedConfigStatusKind::Dirty => "dirty (edited by hand)".to_string(),
            ManagedConfigStatusKind::WouldAdopt => "would adopt".to_string(),
            ManagedConfigStatusKind::WouldRefresh => format!("would refresh ({})", digest()),
        }
    }
}

/// The `OCX_EXTRA_CA_CERTS` persistence outcome.
#[derive(Serialize, schemars::JsonSchema)]
pub struct ExtraCaCertsEntry {
    /// What the run did with the extra CA roots.
    status: ExtraCaCertsStatusKind,
    /// How many certificates the resolved value holds; absent on `not_configured` and `system_locked`.
    #[serde(skip_serializing_if = "Option::is_none")]
    certificates: Option<usize>,
}

/// What a run did with the extra CA roots.
#[derive(Serialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ExtraCaCertsStatusKind {
    /// `OCX_EXTRA_CA_CERTS` is not set.
    NotConfigured,
    /// `config.toml` already holds the same roots.
    Unchanged,
    /// The roots were written to `config.toml`.
    Persisted,
    /// Dry run: the roots would be written to `config.toml`.
    WouldPersist,
    /// The system tier locks the setting, so nothing was validated or written.
    SystemLocked,
}

impl ExtraCaCertsEntry {
    pub fn from_outcome(outcome: &ExtraCaCertsOutcome) -> Self {
        match outcome {
            ExtraCaCertsOutcome::NotConfigured => Self {
                status: ExtraCaCertsStatusKind::NotConfigured,
                certificates: None,
            },
            ExtraCaCertsOutcome::Unchanged { certificates } => Self {
                status: ExtraCaCertsStatusKind::Unchanged,
                certificates: Some(*certificates),
            },
            ExtraCaCertsOutcome::Persisted { certificates } => Self {
                status: ExtraCaCertsStatusKind::Persisted,
                certificates: Some(*certificates),
            },
            ExtraCaCertsOutcome::WouldPersist { certificates } => Self {
                status: ExtraCaCertsStatusKind::WouldPersist,
                certificates: Some(*certificates),
            },
            ExtraCaCertsOutcome::SystemLocked => Self {
                status: ExtraCaCertsStatusKind::SystemLocked,
                certificates: None,
            },
        }
    }

    /// The plain key/value row, omitted when `not_configured`.
    pub fn summary(&self) -> String {
        // Every resolved outcome carries a count, so the default is unreachable.
        let count = self.certificates.unwrap_or_default();
        let certificates = || format!("{count} certificate{}", if count == 1 { "" } else { "s" });
        match self.status {
            ExtraCaCertsStatusKind::NotConfigured => "not configured".to_string(),
            ExtraCaCertsStatusKind::Unchanged => format!("unchanged ({})", certificates()),
            ExtraCaCertsStatusKind::Persisted => format!("persisted ({})", certificates()),
            ExtraCaCertsStatusKind::WouldPersist => format!("would persist ({})", certificates()),
            ExtraCaCertsStatusKind::SystemLocked => "system-locked (not persisted)".to_string(),
        }
    }
}

/// Report of `ocx self setup`.
///
/// Plain format: a key/value table (`Status`, `Bootstrap`, written shims,
/// per-profile outcomes, and any advisory), with empty rows suppressed.
#[derive(Serialize, schemars::JsonSchema)]
pub struct SelfSetupData {
    /// The run's overall outcome; `skipped` when a managed block was left dirty.
    status: StatusKind,
    /// Whether this run installed ocx into its own content store.
    bootstrap: BootstrapEntry,
    /// The env shim files this run wrote.
    shims: Vec<String>,
    /// One entry per shell profile this run considered.
    profiles: Vec<ProfileEntry>,
    /// One entry per session-PATH store this host owns, the skipped ones included; empty only
    /// where the platform has no session-PATH facility.
    // Always serialized, or a `failed` store with no payload is an outcome computed and discarded.
    session_path_stores: Vec<SessionPathEntry>,
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
    /// Whether this run changed a PATH surface that the user must act on: a shim or
    /// managed profile block (re-source the shell), or a session-PATH store (log out
    /// and back in, so programs started outside a shell see it too).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    reload_hint: bool,
    /// Result of adopting/clearing the `--managed-config` tier.
    managed_config: ManagedConfigEntry,
    /// Result of persisting `OCX_EXTRA_CA_CERTS` into `config.toml`.
    ///
    /// Always present; only `certificates` is omitted, on `not_configured`. The
    /// plain table suppresses its row the same way `managed_config`'s does, when
    /// `not_configured`.
    extra_ca_certs: ExtraCaCertsEntry,
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
            session_path_stores: outcome
                .session_path
                .iter()
                .map(|(location, session_outcome)| SessionPathEntry {
                    location: location.display().to_string(),
                    outcome: (*session_outcome).into(),
                })
                .collect(),
            dirty_profiles,
            exec_policy_warning: outcome.exec_policy_warning.clone(),
            conflicting_ocx: outcome.conflicting_ocx.as_ref().map(|path| path.display().to_string()),
            reload_hint: outcome.reload_hint,
            managed_config: ManagedConfigEntry::from_outcome(&outcome.managed_config),
            extra_ca_certs: ExtraCaCertsEntry::from_outcome(&outcome.extra_ca_certs),
        }
    }
}

/// Reduces the profile outcomes and shim writes to one status: dirty > migrated > completed > no-op.
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
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "SelfSetupData";

    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        // Only rows with a payload appear.
        let mut fields: Vec<Cell> = vec!["Status".into(), "Bootstrap".into()];
        let mut values: Vec<Cell> = vec![
            Cell::from(self.status.to_string()),
            Cell::from(self.bootstrap.summary()),
        ];

        if let Some(digest) = &self.bootstrap.digest {
            fields.push("Digest".into());
            values.push(Cell::from(digest.to_string()));
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

        for (index, entry) in self.session_path_stores.iter().enumerate() {
            let label = if index == 0 {
                "Session PATH".to_string()
            } else {
                String::new()
            };
            fields.push(Cell::from(label));
            values.push(Cell::from(format!("{} ({})", entry.location, entry.outcome)));
        }

        if !matches!(self.managed_config.status, ManagedConfigStatusKind::NotConfigured) {
            fields.push("Managed config".into());
            values.push(Cell::from(self.managed_config.summary()));
        }

        if !matches!(self.extra_ca_certs.status, ExtraCaCertsStatusKind::NotConfigured) {
            fields.push("Extra CA certs".into());
            values.push(Cell::from(self.extra_ca_certs.summary()));
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ocx_setup::{
        BootstrapOutcome, BootstrapStatus, ExtraCaCertsOutcome, ManagedConfigSetupOutcome, ProfileOutcome,
        SessionPathOutcome, SetupOutcome,
    };
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
            session_path: Vec::new(),
            extra_ca_certs: ExtraCaCertsOutcome::NotConfigured,
        }
    }

    /// `session_path_stores` is present in **every** state, including
    /// the two skip outcomes — a `failed` store that reached no payload would
    /// be an outcome computed and discarded, and absence-as-signal is not this
    /// report's convention.
    #[test]
    fn every_session_path_outcome_reaches_the_json_payload() {
        for (session_outcome, expected) in [
            (SessionPathOutcome::Written, "written"),
            (SessionPathOutcome::Unchanged, "unchanged"),
            (SessionPathOutcome::Removed, "removed"),
            (SessionPathOutcome::SkippedOptOut, "skipped_opt_out"),
            (SessionPathOutcome::SkippedUnsupported, "skipped_unsupported"),
            (SessionPathOutcome::Failed, "failed"),
        ] {
            let mut base = outcome();
            base.session_path = vec![(
                PathBuf::from("/home/dev/.config/environment.d/ocx.conf"),
                session_outcome,
            )];

            let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
            assert_eq!(
                value["session_path_stores"],
                json!([{"location": "/home/dev/.config/environment.d/ocx.conf", "outcome": expected}]),
                "for {session_outcome:?}"
            );
        }
    }

    /// C-036 / E-X12: the per-store vector carries one outcome per store, so a
    /// run that wrote one and failed the other reports both rather than
    /// collapsing to a single verdict.
    #[test]
    fn a_failed_store_travels_beside_a_written_one() {
        let mut base = outcome();
        base.session_path = vec![
            (
                PathBuf::from("/home/dev/.config/environment.d/ocx.conf"),
                SessionPathOutcome::Failed,
            ),
            (PathBuf::from("HKCU\\Environment\\Path"), SessionPathOutcome::Written),
        ];

        let value = serde_json::to_value(SelfSetupData::from_outcome(&base)).unwrap();
        assert_eq!(
            value["session_path_stores"],
            json!([
                {"location": "/home/dev/.config/environment.d/ocx.conf", "outcome": "failed"},
                {"location": "HKCU\\Environment\\Path", "outcome": "written"}
            ])
        );
        assert_eq!(
            value["status"],
            json!("no_op"),
            "a failed session-PATH store is warned about, not promoted to the top-level status"
        );
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
        assert_eq!(
            value["session_path_stores"],
            json!([]),
            "the key is always present; only a host with no facility makes it empty"
        );
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
        use ocx_oci::Digest;

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
