// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use ocx_setup::VersionSpec;

/// Refresh the managed-config snapshot from the registry.
///
/// Fetches the configured package, persists a new snapshot and reports what
/// changed, always bypassing the background-refresh throttle, as `ocx self
/// update` does. VERSION pins the sync (rollback = an older version).
/// `--pause` holds the background tick only, never the required gate, for up to
/// 7 days; with VERSION it pauses only after the sync succeeds, and any update
/// without it clears the pause. `--check` only reports, locally when offline.
///
/// Exit codes: <https://ocx.sh/docs/reference/command-line#config-update>
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

/// Clap value parser for `--pause`: the interval grammar, capped at `MAX_PAUSE_INTERVAL`.
fn parse_pause_duration(value: &str) -> Result<std::time::Duration, String> {
    let duration = ocx_config::refresh::parse_interval(value)
        .map_err(|error| ocx_config::managed::ManagedConfigError::InvalidInterval(error).to_string())?;
    if duration > ocx_config::managed_config::MAX_PAUSE_INTERVAL {
        return Err(format!(
            "pause duration '{value}' exceeds the maximum of 7d; use `refresh = \"manual\"` for a permanent hold"
        ));
    }
    Ok(duration)
}

impl ConfigUpdateArgs {
    pub async fn execute(&self, context: crate::app::Context) -> anyhow::Result<ExitCode> {
        use ocx_config::managed::resolve_managed_target;
        use ocx_package_manager::ManagedConfigUpdateResult;

        use crate::api::data::config_update::{ConfigUpdateData, ConfigUpdateStatus, parse_timestamp};

        // No required-snapshot gate here: this command exists to satisfy exactly that missing state.
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
                pause_ends_at: None,
                pinned: None,
            })?;
            return Ok(ExitCode::SUCCESS);
        };

        if self.check {
            return execute_check(&context, &resolved).await;
        }

        let managed_paths = context.file_structure().state.managed_config();

        // `--pause` without VERSION writes the pause and fetches nothing, reported in offline
        // `--check`'s shape.
        if let Some(duration) = self.pause
            && self.version.is_none()
        {
            let pause = ocx_config::managed_config::ManagedConfigPause::for_duration(duration, None);
            ocx_config::managed_config::write_pause(&managed_paths, &pause).await?;
            let snapshot = context.managed_config_snapshot();
            context.api().report(&ConfigUpdateData {
                status: ConfigUpdateStatus::CheckUnavailable,
                source: Some(resolved.source.to_string()),
                digest: snapshot.map(|snapshot| snapshot.digest.clone()),
                fetched_at: snapshot.and_then(|snapshot| parse_timestamp(&snapshot.fetched_at)),
                policy: Some(resolved.refresh.to_string()),
                kill_switches: active_kill_switches(),
                drift: None,
                tag: snapshot.and_then(|snapshot| snapshot.tag.clone()),
                pause_ends_at: parse_timestamp(&pause.paused_until),
                pinned: None,
            })?;
            return Ok(ExitCode::SUCCESS);
        }

        // `tag@digest` fetches the tag with the digest as a fail-closed assertion; fetching by digest
        // would never verify the tag.
        let (fetch_source, expected_digest) = match &self.version {
            None => (resolved.source.clone(), None),
            Some(VersionSpec::TagAndDigest { tag, digest }) => {
                (resolved.source.clone_with_tag(tag.as_str()), Some(digest.clone()))
            }
            Some(VersionSpec::Tag(tag)) => (resolved.source.clone_with_tag(tag.as_str()), None),
            Some(VersionSpec::Digest(digest)) => (resolved.source.clone_with_digest(digest.clone()), None),
        };
        let target = ocx_config::managed::ResolvedManagedConfig {
            source: fetch_source,
            ..resolved.clone()
        };

        let result = context
            .manager()
            .update_managed_config(&target, expected_digest.as_ref())
            .await?;

        // Only after the persist succeeded: `--pause` with VERSION records the pause, any other update
        // clears it.
        let pause_ends_at = if let Some(duration) = self.pause {
            let pause = ocx_config::managed_config::ManagedConfigPause::for_duration(
                duration,
                self.version.as_ref().map(|spec| spec.to_string()),
            );
            ocx_config::managed_config::write_pause(&managed_paths, &pause).await?;
            parse_timestamp(&pause.paused_until)
        } else {
            ocx_config::managed_config::clear_pause(&managed_paths).await?;
            None
        };

        let (status, digest) = match &result {
            ManagedConfigUpdateResult::AlreadyCurrent { digest } => {
                (ConfigUpdateStatus::AlreadyCurrent, Some(digest.clone()))
            }
            ManagedConfigUpdateResult::Updated { digest } => (ConfigUpdateStatus::Updated, Some(digest.clone())),
        };

        // A digest-pinned seed binds the required gate to that digest, so syncing another fails the
        // next command closed.
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
            pause_ends_at,
            pinned: self.pause.and(self.version.as_ref()).map(|spec| spec.to_string()),
        })?;
        Ok(ExitCode::SUCCESS)
    }
}

/// `--check`: the local state plus live drift when the registry is reachable. Never persists,
/// swaps or modifies the pause; any probe failure degrades to the local state.
async fn execute_check(
    context: &crate::app::Context,
    resolved: &ocx_config::managed::ResolvedManagedConfig,
) -> anyhow::Result<ExitCode> {
    use crate::api::data::config_update::{ConfigUpdateData, parse_timestamp};

    let snapshot = context.managed_config_snapshot();
    let source = resolved.source.to_string();
    let digest = snapshot.map(|snapshot| snapshot.digest.clone());
    let fetched_at = snapshot.and_then(|snapshot| parse_timestamp(&snapshot.fetched_at));
    let tag = snapshot.and_then(|snapshot| snapshot.tag.clone());
    let policy = resolved.refresh.to_string();
    let pause = ocx_config::managed_config::read_pause(&context.file_structure().state.managed_config()).await;

    // `None` also covers a probe that never ran.
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
        pause_ends_at: pause.as_ref().and_then(|pause| parse_timestamp(&pause.paused_until)),
        pinned: pause.and_then(|pause| pause.pinned_version),
    })?;
    Ok(ExitCode::SUCCESS)
}

/// `--check` status and drift flag. `already_current` only when the probe ran and matched the
/// snapshot; a probe that did not run is `CheckUnavailable`, never a false-healthy `already_current`.
fn derive_check_status(
    registry_digest: Option<&ocx_oci::Digest>,
    snapshot_digest: Option<&ocx_oci::Digest>,
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

/// The active kill switches relevant to the managed-config tier.
fn active_kill_switches() -> Vec<String> {
    let mut switches = Vec::new();
    for var in [&ocx_env::OCX_NO_CONFIG_REFRESH, &ocx_env::OCX_NO_CONFIG] {
        if var.bool_or(false).unwrap_or(false) {
            switches.push(var.name.to_owned());
        }
    }
    switches
}

#[cfg(test)]
mod tests {
    use ocx_oci::Digest;

    use super::{derive_check_status, parse_pause_duration};
    use crate::api::data::config_update::ConfigUpdateStatus;

    fn digest(seed: char) -> Digest {
        Digest::Sha256(seed.to_string().repeat(64))
    }

    /// Regression: a probe that did NOT run (`registry_digest ==
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
            ocx_config::managed_config::MAX_PAUSE_INTERVAL,
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
