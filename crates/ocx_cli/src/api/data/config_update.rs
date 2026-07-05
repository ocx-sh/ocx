// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::cli::Cell;
use serde::Serialize;

use crate::api::Printable;

/// Top-level status discriminant for [`ConfigUpdateData`].
///
/// Serde serializes each variant to its `snake_case` name. Status derivation
/// from the underlying probe / update result is an `Implement`-phase concern
/// (managed-config phase 4) — this type only carries the already-decided
/// value.
///
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ConfigUpdateStatus {
    /// No managed-config tier is configured.
    NotConfigured,
    /// The registry was probed and its digest matches the local snapshot —
    /// verified current. Reported only when the probe actually ran.
    AlreadyCurrent,
    /// A new snapshot was fetched and persisted (full-update path only).
    Updated,
    /// A probe-only report (`--check`) that ran and detected drift — no swap
    /// was attempted.
    Checked,
    /// A probe-only report (`--check`) whose registry probe could NOT run —
    /// offline, no client, source absent, auth failure, or a registry error.
    /// Distinct from [`Self::AlreadyCurrent`] so operators can tell "verified
    /// current" apart from "couldn't check" — the report degrades to
    /// local-state only (source/digest/fetched-at, no live drift).
    CheckUnavailable,
}

impl std::fmt::Display for ConfigUpdateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("not_configured"),
            Self::AlreadyCurrent => f.write_str("already_current"),
            Self::Updated => f.write_str("updated"),
            Self::Checked => f.write_str("checked"),
            Self::CheckUnavailable => f.write_str("check_unavailable"),
        }
    }
}

/// CLI report for `ocx config update` — both the full-update path and the
/// `--check` probe-only path (mirrors `ocx self update --check`).
///
/// Plain format: key/value table; rows with no payload are suppressed.
///
/// JSON format:
/// `{"status":"…","source":"…","digest":"…","fetched_at":"…","policy":"…","kill_switches":["…"],"drift":true}`
/// — optional fields present only when the underlying probe produced them.
///
///
/// Pure data carrier — construct with a struct literal at the call site (no
/// status derivation lives here; see [`ConfigUpdateStatus`] doc). Fields are
/// `pub(crate)` so `command/config_update.rs` builds it directly.
#[derive(Serialize)]
pub struct ConfigUpdateData {
    pub(crate) status: ConfigUpdateStatus,
    /// The effective managed-config source (flag > env > seed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) source: Option<String>,
    /// The local snapshot's manifest digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) digest: Option<String>,
    /// ISO-8601 UTC timestamp of the snapshot's last fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) fetched_at: Option<String>,
    /// The tier's refresh policy (`apply` / `notify` / `manual`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) policy: Option<String>,
    /// Active kill switches (e.g. `OCX_NO_CONFIG_REFRESH`), by env-var name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) kill_switches: Vec<String>,
    /// Whether the registry's current digest differs from the local
    /// snapshot; present only when reachable (`--check`, online).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) drift: Option<bool>,
    /// The tag the local snapshot was fetched under (snapshot v2 bookkeeping —
    /// shows which floating/pinned tag the tier tracks).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) tag: Option<String>,
    /// ISO-8601 UTC instant until which the background tick is paused
    /// (`ocx config update --pause`); absent when no pause is in force.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) paused_until: Option<String>,
    /// The version spec pinned alongside an in-force pause
    /// (`--pause <d> <VERSION>`); absent when the pause carries no pin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pinned: Option<String>,
}

impl Printable for ConfigUpdateData {
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        let mut fields: Vec<Cell> = vec!["Status".into()];
        let mut values: Vec<Cell> = vec![Cell::from(self.status.to_string())];

        if let Some(source) = &self.source {
            fields.push("Source".into());
            values.push(Cell::from(source.clone()));
        }
        if let Some(digest) = &self.digest {
            fields.push("Digest".into());
            values.push(Cell::from(digest.clone()));
        }
        if let Some(fetched_at) = &self.fetched_at {
            fields.push("Fetched at".into());
            values.push(Cell::from(fetched_at.clone()));
        }
        if let Some(policy) = &self.policy {
            fields.push("Policy".into());
            values.push(Cell::from(policy.clone()));
        }
        for (index, kill_switch) in self.kill_switches.iter().enumerate() {
            let label = if index == 0 {
                "Kill switches".to_string()
            } else {
                String::new()
            };
            fields.push(Cell::from(label));
            values.push(Cell::from(kill_switch.clone()));
        }
        if let Some(drift) = self.drift {
            fields.push("Drift".into());
            values.push(Cell::from(drift.to_string()));
        }
        if let Some(tag) = &self.tag {
            fields.push("Tag".into());
            values.push(Cell::from(tag.clone()));
        }
        if let Some(paused_until) = &self.paused_until {
            fields.push("Paused until".into());
            values.push(Cell::from(paused_until.clone()));
        }
        if let Some(pinned) = &self.pinned {
            fields.push("Pinned".into());
            values.push(Cell::from(pinned.clone()));
        }

        printer.print_table(&["Field".into(), "Value".into()], &[fields, values]);
    }
}
