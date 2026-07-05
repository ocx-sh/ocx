// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use ocx_lib::setup::VersionSpec;

/// Refresh the managed-config snapshot from the registry.
///
/// Fetches the configured managed-config package, persists a new snapshot,
/// and reports what changed. Always bypasses the background-refresh throttle
/// (explicit user intent) — mirrors `ocx self update`.
///
/// An optional VERSION positional pins the sync to a specific tag, digest, or
/// `tag@digest` combination (rollback = `ocx config update <older-version>`).
/// `--pause <duration>` holds the background tick for up to 7 days (with a
/// VERSION: sync first, pause only after success); `--resume` clears the
/// pause and syncs. Any explicit update without `--pause` clears an active
/// pause. Pause affects the background tick only — never the required gate.
///
/// With `--check`, only reports the tier's current status (source, digest,
/// tag, fetched-at, refresh policy, pause state, active kill switches, and
/// live drift against the registry when reachable) — never fetches or swaps.
/// Offline degrades to a local-state-only report.
///
/// # Exit codes
///
/// | Outcome | Exit |
/// |---|---|
/// | `not_configured` / `already_current` / `checked` / `check_unavailable` / `updated` | 0 |
/// | conflicting flags, malformed VERSION, or `--pause` over the 7d cap | 64 |
/// | `tag@digest` mismatch (immutability assertion failed) | 65 |
/// | registry unreachable | 69 |
/// | resolved source not found in registry | 79 |
/// | invalid managed-config source or interval | 78 |
/// | authentication failed | 80 |
#[derive(Parser)]
pub struct ConfigUpdateArgs {
    /// Version to sync: tag, `sha256:<hex>`, or `tag@sha256:<hex>`.
    ///
    /// Omit to follow the seed's own source. A tag syncs that exact version
    /// (rollback included). A digest syncs the exact content. A `tag@digest`
    /// form verifies the tag resolves to the given digest before persisting.
    #[arg(
        value_name = "VERSION",
        value_parser = |s: &str| VersionSpec::from_str(s).map_err(|e| e.to_string()),
        conflicts_with_all = ["check", "resume"],
    )]
    version: Option<VersionSpec>,

    /// Report the managed-config tier's status without fetching or swapping.
    #[arg(long, conflicts_with_all = ["pause", "resume"])]
    check: bool,

    /// Pause the background refresh for a duration (e.g. `4h`, `3d`; max `7d`).
    ///
    /// Without a VERSION, freezes the on-disk state as-is (no fetch). With a
    /// VERSION, syncs the pin first and records the pause only on success.
    /// A pause is a temporary hold - for a permanent opt-out set
    /// `refresh = "manual"` in the `[managed]` seed.
    #[arg(long, value_name = "DURATION", value_parser = parse_pause_duration)]
    pause: Option<std::time::Duration>,

    /// Clear an active pause and refresh immediately.
    #[arg(long, conflicts_with = "pause")]
    resume: bool,
}

/// Clap value parser for `--pause`: the shared `\d+[smhd]?` interval grammar
/// plus the `MAX_PAUSE_INTERVAL` ceiling. Malformed or over-cap → clap error
/// (exit 64).
fn parse_pause_duration(value: &str) -> Result<std::time::Duration, String> {
    let duration = ocx_lib::parse_interval(value).map_err(|e| e.to_string())?;
    if duration > ocx_lib::managed_config::MAX_PAUSE_INTERVAL {
        return Err(format!(
            "pause duration '{value}' exceeds the maximum of 7d; use `refresh = \"manual\"` for a permanent hold"
        ));
    }
    Ok(duration)
}

impl ConfigUpdateArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        use ocx_lib::package_manager::ManagedConfigUpdateResult;
        use ocx_lib::resolve_managed_target;

        use crate::api::data::config_update::{ConfigUpdateData, ConfigUpdateStatus};

        // `resolve_managed_target` never enforces the required-snapshot gate
        // `Context::try_init` applies to ordinary commands — `config update`'s
        // entire job is to satisfy (or inspect) exactly that missing state.
        let resolved = resolve_managed_target(context.config(), context.managed_config_env_override())?;

        let Some(resolved) = resolved else {
            context.api().report(&ConfigUpdateData {
                status: ConfigUpdateStatus::NotConfigured,
                source: None,
                digest: None,
                fetched_at: None,
                policy: None,
                kill_switches: Vec::new(),
                drift: None,
                tag: None,
                paused_until: None,
                pinned: None,
            })?;
            return Ok(ExitCode::SUCCESS);
        };

        if self.check {
            return execute_check(&context, &resolved).await;
        }

        let state = &context.file_structure().state;

        // `--pause` without a VERSION freezes the on-disk state as-is: write
        // the pause, fetch nothing. The report is the same local-state shape
        // an offline `--check` produces (`check_unavailable` — no probe ran),
        // extended with the fresh pause window.
        if let Some(duration) = self.pause
            && self.version.is_none()
        {
            let pause = ocx_lib::managed_config::ManagedConfigPause::for_duration(duration, None);
            ocx_lib::managed_config::write_pause(state, &pause).await?;
            let snapshot = context.managed_config_snapshot();
            context.api().report(&ConfigUpdateData {
                status: ConfigUpdateStatus::CheckUnavailable,
                source: Some(resolved.source.to_string()),
                digest: snapshot.map(|snapshot| snapshot.digest.to_string()),
                fetched_at: snapshot.map(|snapshot| snapshot.fetched_at.clone()),
                policy: Some(resolved.refresh.to_string()),
                kill_switches: active_kill_switches(),
                drift: None,
                tag: snapshot.and_then(|snapshot| snapshot.tag.clone()),
                paused_until: Some(pause.paused_until),
                pinned: None,
            })?;
            return Ok(ExitCode::SUCCESS);
        }

        // Fetch leg: bare update / --resume / VERSION / --pause+VERSION.
        //
        // For `tag@digest` the fetch runs on the TAG-ONLY reference with the
        // digest as a fail-closed assertion (fetching by digest would satisfy
        // the pin trivially without verifying the tag).
        let (fetch_source, expected_digest) = match &self.version {
            None => (resolved.source.clone(), None),
            Some(VersionSpec::TagAndDigest { tag, digest }) => {
                (resolved.source.clone_with_tag(tag.as_str()), Some(digest.clone()))
            }
            Some(spec) => (spec.apply(resolved.source.clone()), None),
        };
        let target = ocx_lib::ResolvedManagedConfig {
            source: fetch_source,
            ..resolved.clone()
        };

        let result = context
            .manager()
            .update_managed_config(&target, expected_digest.as_ref())
            .await?;

        // Pause bookkeeping AFTER the persist succeeded: `--pause <d> <V>`
        // records the pause (with the pin visible for `--check`); every other
        // explicit update clears any active pause (`--resume` included).
        let paused_until = if let Some(duration) = self.pause {
            let pause = ocx_lib::managed_config::ManagedConfigPause::for_duration(
                duration,
                self.version.as_ref().map(|spec| spec.to_string()),
            );
            ocx_lib::managed_config::write_pause(state, &pause).await?;
            Some(pause.paused_until)
        } else {
            ocx_lib::managed_config::clear_pause(state).await?;
            None
        };

        let (status, digest) = match &result {
            ManagedConfigUpdateResult::AlreadyCurrent { digest } => {
                (ConfigUpdateStatus::AlreadyCurrent, Some(digest.to_string()))
            }
            ManagedConfigUpdateResult::Updated { digest } => (ConfigUpdateStatus::Updated, Some(digest.to_string())),
        };

        // A digest-pinned seed binds the required gate to that exact digest —
        // syncing a different VERSION would fail closed on the very next
        // command. Warn loudly instead of leaving a bricked-looking tier.
        if self.version.is_some()
            && let Some(seed_pin) = resolved.source.digest()
            && let ManagedConfigUpdateResult::Updated { digest } | ManagedConfigUpdateResult::AlreadyCurrent { digest } =
                &result
            && *digest != seed_pin
        {
            context.ui().warn(format!(
                "the [managed] seed pins digest {seed_pin} but this update synced {digest}; ordinary commands will \
                 fail the required gate until the seed pin is updated (re-run `ocx self setup --managed-config \
                 <ref>@<new-digest>`)"
            ));
        }

        context.api().report(&ConfigUpdateData {
            status,
            source: Some(target.source.to_string()),
            digest,
            fetched_at: None,
            policy: Some(resolved.refresh.to_string()),
            kill_switches: active_kill_switches(),
            drift: None,
            tag: target.source.tag().map(str::to_string),
            paused_until,
            pinned: self.pause.and(self.version.as_ref()).map(|spec| spec.to_string()),
        })?;
        Ok(ExitCode::SUCCESS)
    }
}

/// Probe-only report (`--check`): reports source/digest/tag/fetched-at/
/// policy/kill-switches/pause state, plus live drift when the registry is
/// reachable. Never fetches the full payload for persistence and never swaps
/// state — offline (or any fetch failure) degrades to a local-state-only
/// report. The pause file is read but never modified (`--check` is a pure
/// observer).
async fn execute_check(
    context: &crate::app::Context,
    resolved: &ocx_lib::ResolvedManagedConfig,
) -> anyhow::Result<ExitCode> {
    use crate::api::data::config_update::ConfigUpdateData;

    let snapshot = context.managed_config_snapshot();
    let source = resolved.source.to_string();
    let digest = snapshot.map(|snapshot| snapshot.digest.to_string());
    let fetched_at = snapshot.map(|snapshot| snapshot.fetched_at.clone());
    let tag = snapshot.and_then(|snapshot| snapshot.tag.clone());
    let policy = resolved.refresh.to_string();
    let pause = ocx_lib::managed_config::read_pause(&context.file_structure().state).await;

    // `probe_managed_config_digest` returns `None` for every case where the
    // probe did NOT run (offline / no client / source absent / auth / registry
    // error) as well as a genuine not-found — `derive_check_status` never turns
    // a probe that did not run into `already_current` (finding #3).
    let registry_digest = context.manager().probe_managed_config_digest(resolved).await;
    let (status, drift) = derive_check_status(registry_digest.as_ref(), snapshot.map(|snapshot| &snapshot.digest));

    context.api().report(&ConfigUpdateData {
        status,
        source: Some(source),
        digest,
        fetched_at,
        policy: Some(policy),
        kill_switches: active_kill_switches(),
        drift,
        tag,
        paused_until: pause.as_ref().map(|pause| pause.paused_until.clone()),
        pinned: pause.and_then(|pause| pause.pinned_version),
    })?;
    Ok(ExitCode::SUCCESS)
}

/// Derives the `--check` status and drift flag from the registry probe result
/// and the local snapshot digest.
///
/// Pure decision function, extracted so the finding-#3 invariant is unit
/// testable without a live registry: **`already_current` is reported only when
/// the probe actually ran and its digest matched the local snapshot.** A probe
/// that did not run (`registry_digest == None` — offline / no client / source
/// absent / auth / registry error) yields
/// [`ConfigUpdateStatus::CheckUnavailable`], never a false-healthy
/// `already_current`.
fn derive_check_status(
    registry_digest: Option<&ocx_lib::oci::Digest>,
    snapshot_digest: Option<&ocx_lib::oci::Digest>,
) -> (crate::api::data::config_update::ConfigUpdateStatus, Option<bool>) {
    use crate::api::data::config_update::ConfigUpdateStatus;

    match registry_digest {
        Some(remote) => {
            let drifted = snapshot_digest != Some(remote);
            let status = if drifted {
                ConfigUpdateStatus::Checked
            } else {
                ConfigUpdateStatus::AlreadyCurrent
            };
            (status, Some(drifted))
        }
        None => (ConfigUpdateStatus::CheckUnavailable, None),
    }
}

/// Names the active kill switches relevant to the managed-config tier, for
/// `--check`'s report.
fn active_kill_switches() -> Vec<String> {
    let mut switches = Vec::new();
    if ocx_lib::env::flag(ocx_lib::env::keys::OCX_NO_CONFIG_REFRESH, false) {
        switches.push(ocx_lib::env::keys::OCX_NO_CONFIG_REFRESH.to_string());
    }
    if ocx_lib::env::flag(ocx_lib::env::keys::OCX_NO_CONFIG, false) {
        switches.push(ocx_lib::env::keys::OCX_NO_CONFIG.to_string());
    }
    switches
}

#[cfg(test)]
mod tests {
    use ocx_lib::oci::Digest;

    use super::{derive_check_status, parse_pause_duration};
    use crate::api::data::config_update::ConfigUpdateStatus;

    fn digest(seed: char) -> Digest {
        Digest::Sha256(seed.to_string().repeat(64))
    }

    /// Finding #3 regression: a probe that did NOT run (`registry_digest ==
    /// None` — offline / no client / source absent / auth / registry error)
    /// must NEVER report `already_current`. It surfaces `check_unavailable` so
    /// operators can tell "verified current" from "couldn't check".
    #[test]
    fn probe_unavailable_is_never_already_current() {
        let (status, drift) = derive_check_status(None, Some(&digest('a')));
        assert_eq!(status, ConfigUpdateStatus::CheckUnavailable);
        assert_eq!(drift, None, "an unavailable probe reports no drift verdict");

        // Also holds when there is no local snapshot at all.
        let (status_no_snapshot, _) = derive_check_status(None, None);
        assert_eq!(status_no_snapshot, ConfigUpdateStatus::CheckUnavailable);
    }

    /// A probe that ran and matched the local snapshot digest is verified
    /// current — the only path to `already_current`.
    #[test]
    fn probe_matching_snapshot_is_already_current() {
        let (status, drift) = derive_check_status(Some(&digest('a')), Some(&digest('a')));
        assert_eq!(status, ConfigUpdateStatus::AlreadyCurrent);
        assert_eq!(drift, Some(false));
    }

    /// A probe that ran and differs from the local snapshot reports drift.
    #[test]
    fn probe_mismatched_snapshot_is_checked_with_drift() {
        let (status, drift) = derive_check_status(Some(&digest('b')), Some(&digest('a')));
        assert_eq!(status, ConfigUpdateStatus::Checked);
        assert_eq!(drift, Some(true));
    }

    /// A probe that ran with no local snapshot at all is drift (nothing to
    /// match), never a false `already_current`.
    #[test]
    fn probe_ran_without_local_snapshot_is_checked_with_drift() {
        let (status, drift) = derive_check_status(Some(&digest('b')), None);
        assert_eq!(status, ConfigUpdateStatus::Checked);
        assert_eq!(drift, Some(true));
    }

    // ── --pause value parser (shared interval grammar + 7d cap) ─────────────

    #[test]
    fn parse_pause_accepts_grammar_within_cap() {
        assert_eq!(
            parse_pause_duration("4h").unwrap(),
            std::time::Duration::from_secs(4 * 3600)
        );
        assert_eq!(
            parse_pause_duration("7d").unwrap(),
            ocx_lib::managed_config::MAX_PAUSE_INTERVAL,
            "the cap itself is accepted (inclusive ceiling)"
        );
    }

    #[test]
    fn parse_pause_rejects_over_cap() {
        let err = parse_pause_duration("8d").expect_err("over-cap must be rejected");
        assert!(err.contains("maximum of 7d"), "error names the cap: {err}");
    }

    #[test]
    fn parse_pause_rejects_malformed() {
        parse_pause_duration("soon").expect_err("malformed duration must be rejected");
        parse_pause_duration("").expect_err("empty duration must be rejected");
    }
}
