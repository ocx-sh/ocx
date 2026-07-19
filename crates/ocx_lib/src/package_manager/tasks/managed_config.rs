// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Managed-config update + background-refresh task methods for
//! [`PackageManager`].
//!
//! Mirrors the throttle/probe shape of `tasks/update_check.rs`: a full update
//! path (`update_managed_config`, called by `ocx config update` and the setup
//! phase 1.5 sync fetch) and a throttled background probe
//! (`check_managed_config_refresh`, called by the CLI's background-tick hook)
//! that never fails its caller.

use super::super::PackageManager;
use crate::config::managed::{RefreshPolicy, ResolvedManagedConfig};
use crate::config::patch::ResolvedPatchConfig;
use crate::managed_config::ManagedConfigUpdateError;
use crate::oci::Digest;
use crate::{log, oci};

/// Outcome of a full managed-config update cycle.
#[derive(Debug, Clone)]
pub enum ManagedConfigUpdateResult {
    /// The local snapshot already matches the registry's current digest —
    /// nothing was fetched or written.
    AlreadyCurrent {
        /// The existing (unchanged) manifest digest.
        digest: Digest,
    },
    /// A new snapshot was fetched and persisted.
    Updated {
        /// The newly persisted manifest digest.
        digest: Digest,
    },
}

/// Outcome of a single background-refresh probe tick
/// ([`PackageManager::check_managed_config_refresh`]).
#[derive(Debug, Clone)]
pub enum ManagedConfigRefreshOutcome {
    /// Probe succeeded; the registry's current digest matches the local
    /// snapshot (or the throttle window has not elapsed) — nothing to report.
    UpToDate,
    /// `notify` posture: drift detected. Content was NOT fetched; the caller
    /// prints the advisory ("run `ocx config update`").
    DriftDetected,
    /// `apply` posture: drift detected and the new snapshot was fetched,
    /// persisted, and swapped in silently.
    Applied {
        /// The newly persisted manifest digest.
        digest: Digest,
    },
    /// The probe could not reach the registry, or the apply-on-drift persist
    /// failed. Already logged at debug by this function; the tick still never
    /// fails its caller.
    Unreachable,
    /// An in-force pause file (`ocx config update --pause`) short-circuited
    /// the tick before the throttle and probe — zero transport calls.
    Paused,
}

impl PackageManager {
    /// Runs a full managed-config update: fetch the package, persist the
    /// snapshot, and report what changed.
    ///
    /// Called by `ocx config update` (throttle bypassed — explicit intent) and
    /// by `ocx self setup --managed-config` (synchronous, fence written only
    /// on success — ADR "Setup ordering"). Fetches via the dedicated
    /// managed-config client (local-only mirror map — never the payload's own
    /// `[mirrors]`).
    ///
    /// `expected_digest` is the `tag@digest` immutability assertion from
    /// `ocx config update <tag>@<digest>`: `resolved.source` must then be the
    /// TAG-ONLY reference (fetching by digest would trivially satisfy the
    /// assertion without verifying the tag), and a fetched top digest that
    /// differs fails closed ([`ManagedConfigUpdateError::PinDigestMismatch`],
    /// exit 65) with the existing snapshot untouched. Pass `None` for every
    /// unpinned update.
    ///
    /// # Errors
    ///
    /// Returns [`ManagedConfigUpdateError`] on any fetch or persist failure;
    /// the existing snapshot (if any) is left untouched on error.
    pub async fn update_managed_config(
        &self,
        resolved: &ResolvedManagedConfig,
        expected_digest: Option<&Digest>,
    ) -> Result<ManagedConfigUpdateResult, ManagedConfigUpdateError> {
        let Some(client) = self.managed_config_client.as_ref() else {
            return Err(ManagedConfigUpdateError::Fetch(
                crate::managed_config::ManagedConfigFetchError::FetchFailed {
                    source: oci::client::error::ClientError::Registry(Box::new(std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        "managed-config update requires network access (offline mode)",
                    ))),
                },
            ));
        };

        let Some(fetched) = crate::managed_config::fetch_managed_config(client, &resolved.source).await? else {
            return Err(ManagedConfigUpdateError::SourceNotFound {
                effective_source: resolved.source.clone(),
            });
        };

        // Fail-closed `tag@digest` immutability assertion — BEFORE the
        // already-current check and persist, so a mismatch never touches disk.
        if let Some(expected) = expected_digest
            && fetched.manifest_digest != *expected
        {
            return Err(ManagedConfigUpdateError::PinDigestMismatch {
                expected: expected.clone(),
                fetched: fetched.manifest_digest,
            });
        }

        // Deliberate divergence from `snapshot_matches_source` (which floats
        // tags within a repository): the explicit-update already-current check
        // demands an EXACT source-string match so a pin/rollback to a
        // different tag at the same digest still re-persists — the snapshot's
        // `tag`/`fetched_at` bookkeeping must track the newly requested ref.
        let canonical_source = resolved.source.to_string();
        let existing = crate::managed_config::read_managed_config_snapshot(&self.file_structure.state).await;

        // Capture the payload text before `persist_managed_config` consumes
        // `fetched` — the patch piggyback below needs it on both the persist
        // and the already-current paths.
        let payload_text = fetched.config_text.clone();

        let result = if let Some(existing) = &existing
            && existing.source == canonical_source
            && existing.digest == fetched.manifest_digest
        {
            ManagedConfigUpdateResult::AlreadyCurrent {
                digest: existing.digest.clone(),
            }
        } else {
            let snapshot =
                crate::managed_config::persist_managed_config(&self.file_structure.state, &resolved.source, fetched)
                    .await?;
            ManagedConfigUpdateResult::Updated {
                digest: snapshot.digest,
            }
        };

        // Best-effort: converge patch descriptors declared by the freshly
        // fetched payload's `[patches]` pointer. The manager's construction-time
        // `patches()` reflects the PRE-update config, so a managed config that
        // introduces or moves the patch tier would otherwise never sync until
        // the user ran `ocx patch sync` by hand (the reported bug). Mirrors the
        // `ocx index update` piggyback — never fails this update.
        sync_patches_from_payload(self, &payload_text).await;

        Ok(result)
    }

    /// Probe-only: fetches the managed-config artifact's current manifest
    /// digest from the registry WITHOUT downloading the config layer or ever
    /// persisting/swapping state.
    ///
    /// Used by `ocx config update --check`'s live-drift report. Returns
    /// `None` when the registry is unreachable OR the source is absent —
    /// `--check` degrades to a local-state-only report in that case. The
    /// caller (`execute_check`) distinguishes a probe that ran from one that
    /// did not by whether this returns `Some`.
    pub async fn probe_managed_config_digest(&self, resolved: &ResolvedManagedConfig) -> Option<Digest> {
        let client = self.managed_config_client.as_ref()?;
        match crate::managed_config::probe_managed_config_digest(client, &resolved.source).await {
            Ok(digest) => digest,
            Err(error) => {
                // W8: surface the discarded cause at debug so a failing probe
                // ("check_unavailable") is diagnosable from `--log-level debug`.
                log::debug!("managed-config digest probe for '{}' failed: {error}", resolved.source);
                None
            }
        }
    }

    /// Throttled background-refresh probe (ADR Decision F).
    ///
    /// `notify` posture reports drift without fetching content; `apply`
    /// posture silently fetches + persists + swaps on drift; `manual` posture
    /// is skipped entirely by the caller before this is ever invoked. Never
    /// fails the caller — any network/auth error is logged at debug and
    /// swallowed, mirroring
    /// [`PackageManager::self_check_update`](crate::package_manager::PackageManager::self_check_update)'s
    /// "auto-check must never fail a user command" contract.
    ///
    /// Touch policy (mirrors `check_update`'s update-check throttle contract):
    /// touch the refresh marker on a probe error, on a no-drift probe, on a
    /// `notify`/`manual` drift advisory, and on a successful `apply` persist.
    /// NEVER touch on the throttle short-circuit, and NEVER when an
    /// `apply`-on-drift fetch or persist fails — a failed apply must re-probe
    /// on the next tick instead of being throttled for the full interval
    /// (marking a failed apply here would silence the tier until the window
    /// elapsed).
    pub async fn check_managed_config_refresh(&self, resolved: &ResolvedManagedConfig) -> ManagedConfigRefreshOutcome {
        // Pause short-circuit BEFORE the throttle and probe: an in-force
        // `ocx config update --pause` freezes the tick entirely (expired or
        // corrupt pause files read as absent). Pause never affects the
        // required gate or an explicit `ocx config update`.
        if crate::managed_config::read_pause(&self.file_structure.state)
            .await
            .is_some()
        {
            return ManagedConfigRefreshOutcome::Paused;
        }

        let marker = self.file_structure.state.managed_config_refresh_marker();
        let interval = resolved.interval;
        let marker_check = marker.clone();
        // Run is_throttled (sync fs I/O) off the async executor via spawn_blocking.
        // `.unwrap_or(false)` collapses a task panic (JoinError) to "not throttled"
        // so a broken blocking thread cannot wedge the refresh probe into permanent skip.
        let throttled = tokio::task::spawn_blocking(move || {
            crate::file_structure::StateStore::is_throttled(&marker_check, interval)
        })
        .await
        .unwrap_or(false);
        if throttled {
            return ManagedConfigRefreshOutcome::UpToDate;
        }

        let Some(client) = self.managed_config_client.as_ref() else {
            crate::file_structure::StateStore::touch(marker).await;
            return ManagedConfigRefreshOutcome::Unreachable;
        };

        // Digest-only probe (ADR Decision F): the tick detects drift from the
        // manifest digest alone — the ≤64 KiB config layer is pulled ONLY on
        // the `apply`-on-drift branch below, never for `notify`/`manual`.
        let probed_digest = match crate::managed_config::probe_managed_config_digest(client, &resolved.source).await {
            Ok(Some(digest)) => digest,
            Ok(None) => {
                log::debug!(
                    "managed-config refresh: source '{}' not found in registry",
                    resolved.source
                );
                crate::file_structure::StateStore::touch(marker).await;
                return ManagedConfigRefreshOutcome::Unreachable;
            }
            Err(error) => {
                log::debug!("managed-config refresh probe failed: {error}");
                crate::file_structure::StateStore::touch(marker).await;
                return ManagedConfigRefreshOutcome::Unreachable;
            }
        };

        let existing = crate::managed_config::read_managed_config_snapshot(&self.file_structure.state).await;
        // Drift = identity mismatch (gate v2: repository identity, digest-pin
        // binding — shared `snapshot_matches_source` predicate) OR a content
        // digest that differs from the registry's current top digest.
        let drift = existing.as_ref().is_none_or(|snapshot| {
            !crate::config::managed::snapshot_matches_source(snapshot, &resolved.source)
                || snapshot.digest != probed_digest
        });
        if !drift {
            // A successful probe that found no drift closes the throttle window.
            crate::file_structure::StateStore::touch(marker).await;
            return ManagedConfigRefreshOutcome::UpToDate;
        }

        match resolved.refresh {
            // Defensive fallback only — the caller gates on `Manual` before
            // ever invoking this probe. A drift advisory (no content fetched)
            // still closes the throttle window.
            RefreshPolicy::Manual | RefreshPolicy::Notify => {
                crate::file_structure::StateStore::touch(marker).await;
                ManagedConfigRefreshOutcome::DriftDetected
            }
            RefreshPolicy::Apply => {
                // Only now — with drift confirmed — pull the full layer for
                // persistence. The marker is deliberately NOT touched before
                // this fetch: a failed apply must re-probe on the next tick,
                // not be throttled for the full interval (Codex-F1).
                let fetched = match crate::managed_config::fetch_managed_config(client, &resolved.source).await {
                    Ok(Some(fetched)) => fetched,
                    Ok(None) => {
                        log::debug!(
                            "managed-config apply-on-drift: source '{}' vanished before the full fetch",
                            resolved.source
                        );
                        return ManagedConfigRefreshOutcome::Unreachable;
                    }
                    Err(error) => {
                        log::debug!("managed-config apply-on-drift fetch failed: {error}");
                        return ManagedConfigRefreshOutcome::Unreachable;
                    }
                };
                match crate::managed_config::persist_managed_config(
                    &self.file_structure.state,
                    &resolved.source,
                    fetched,
                )
                .await
                {
                    Ok(snapshot) => {
                        // Touch only after the persist succeeds — the tier is
                        // now current, so the window may safely close.
                        crate::file_structure::StateStore::touch(marker).await;
                        // Same best-effort patch convergence as the explicit
                        // update path — a silently-applied managed config must
                        // also pull the patch descriptors it now points at.
                        sync_patches_from_payload(self, &snapshot.config).await;
                        ManagedConfigRefreshOutcome::Applied {
                            digest: snapshot.digest,
                        }
                    }
                    Err(error) => {
                        log::debug!("managed-config apply-on-drift persist failed: {error}");
                        ManagedConfigRefreshOutcome::Unreachable
                    }
                }
            }
        }
    }
}

/// Best-effort patch-descriptor sync driven by a just-fetched managed-config
/// payload.
///
/// A managed config commonly distributes the fleet's `[patches]` registry
/// pointer (ADR `adr_managed_config_tier.md`), but the manager's `patches()`
/// field is resolved once at construction from the PRE-update config — so the
/// pointer the payload just introduced or moved is invisible to it. Re-resolve
/// `[patches]` from the payload text and sync against that instead of trusting
/// the stale field.
///
/// Never fails the caller: parse/resolve problems log at debug, a sync failure
/// logs a warning, and offline / no-patch-tier payloads are skipped. Mirrors
/// the `ocx index update` piggyback (host platform only).
async fn sync_patches_from_payload(manager: &PackageManager, payload_toml: &str) {
    if manager.is_offline() {
        return;
    }
    let Some(resolved) = patches_from_payload(payload_toml) else {
        return; // payload declares no (usable) patch tier — nothing to converge
    };

    // `with_patches` overrides the manager's construction-time (stale) tier with
    // the one the payload just distributed; host platform only, matching the
    // `ocx index update` piggyback.
    let host = oci::Platform::current().unwrap_or_else(oci::Platform::any);
    match manager.clone().with_patches(Some(resolved)).sync_patches(&[host]).await {
        Ok(_report) => log::debug!("managed-config patch sync: completed"),
        Err(error) => log::warn!("managed-config patch sync failed (non-fatal): {error}"),
    }
}

/// Resolve the `[patches]` tier from a managed-config payload's TOML text.
///
/// Returns `None` — never an error — when the payload is unparseable, declares
/// no patch tier, or declares a malformed one. The piggyback is best-effort, so
/// every such case collapses to "nothing to converge" (logged at debug). This
/// is the seam that fixes the reported bug: patches are resolved from the
/// freshly fetched payload, not from the manager's pre-update `patches()`.
fn patches_from_payload(payload_toml: &str) -> Option<ResolvedPatchConfig> {
    let config = match toml::from_str::<crate::config::Config>(payload_toml) {
        Ok(config) => config,
        Err(error) => {
            log::debug!("managed-config patch sync: parsing payload TOML failed: {error}");
            return None;
        }
    };
    match crate::config::patch::resolve_patch_config(&config) {
        Ok(resolved) => resolved,
        Err(error) => {
            log::debug!("managed-config patch sync: resolving [patches] from payload failed: {error}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tempfile::TempDir;

    use super::*;
    use crate::file_structure::FileStructure;
    use crate::managed_config::test_support::{gzip_tar, seed_package, stub_client_with_package};
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};
    use crate::oci::{Client, Identifier};

    /// Seeds a `StubTransport`-backed client with one managed-config package
    /// (v2 wire shape) at `identifier`.
    fn stub_client_with_artifact(identifier: &Identifier, config_toml: &str) -> Client {
        stub_client_with_package(identifier, config_toml).0
    }

    /// Builds a manager rooted at `root`, wired with `client` as the
    /// managed-config-fetch client (offline `client()`/index — these tests
    /// only exercise the managed-config task methods).
    fn manager_with_client(root: &std::path::Path, client: Client) -> PackageManager {
        let fs = FileStructure::with_root(root.to_path_buf());
        let local_index = crate::oci::index::LocalIndex::new(crate::oci::index::LocalConfig {
            snapshot_store: fs.index.clone(),
        });
        let index = crate::oci::index::Index::from_chained(local_index, vec![], crate::oci::index::ChainMode::Offline);
        PackageManager::new(fs, index, None, "localhost:5000").with_managed_config_client(Some(client))
    }

    fn resolved(source: Identifier, refresh: RefreshPolicy, interval: Duration) -> ResolvedManagedConfig {
        ResolvedManagedConfig {
            source,
            required: true,
            refresh,
            interval,
            system_required: false,
        }
    }

    // ── patches_from_payload (managed-config → patch tier resolution) ─────────

    /// A payload that declares a `[patches]` registry resolves to a usable
    /// tier — the seam that lets a managed config distribute the fleet's patch
    /// pointer and have it synced without a manual `ocx patch sync`.
    #[test]
    fn patches_from_payload_extracts_configured_tier() {
        let payload = "[patches]\nregistry = \"patches.corp.com\"\n";
        let resolved = patches_from_payload(payload).expect("payload with [patches] must resolve a tier");
        assert_eq!(resolved.registry, "patches.corp.com");
    }

    /// A payload without a `[patches]` section resolves to `None` — the
    /// piggyback then skips the sync entirely (no patch tier to converge).
    #[test]
    fn patches_from_payload_none_when_absent() {
        let payload = "[registry]\ndefault = \"corp\"\n";
        assert!(
            patches_from_payload(payload).is_none(),
            "payload without [patches] must resolve to None"
        );
    }

    /// Unparseable payload collapses to `None` (best-effort — never an error),
    /// so a garbled managed config can never poison the update path.
    #[test]
    fn patches_from_payload_none_when_malformed() {
        assert!(
            patches_from_payload("this = = not valid toml").is_none(),
            "malformed payload must resolve to None, not panic or error"
        );
    }

    // ── update_managed_config ────────────────────────────────────────────────

    #[tokio::test]
    async fn update_managed_config_first_fetch_is_updated() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let client = stub_client_with_artifact(&identifier, "[registry]\ndefault = \"corp\"\n");
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::from_secs(86_400));

        let result = manager
            .update_managed_config(&resolved, None)
            .await
            .expect("update must succeed");
        assert!(matches!(result, ManagedConfigUpdateResult::Updated { .. }));
    }

    #[tokio::test]
    async fn update_managed_config_second_call_same_content_is_already_current() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let client = stub_client_with_artifact(&identifier, "[registry]\ndefault = \"corp\"\n");
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::from_secs(86_400));

        manager.update_managed_config(&resolved, None).await.unwrap();
        let second = manager.update_managed_config(&resolved, None).await.unwrap();
        assert!(matches!(second, ManagedConfigUpdateResult::AlreadyCurrent { .. }));
    }

    #[tokio::test]
    async fn update_managed_config_offline_errors() {
        let home = TempDir::new().unwrap();
        let fs = FileStructure::with_root(home.path().to_path_buf());
        let local_index = crate::oci::index::LocalIndex::new(crate::oci::index::LocalConfig {
            snapshot_store: fs.index.clone(),
        });
        let index = crate::oci::index::Index::from_chained(local_index, vec![], crate::oci::index::ChainMode::Offline);
        let manager = PackageManager::new(fs, index, None, "localhost:5000"); // no managed_config_client
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::from_secs(86_400));

        let result = manager.update_managed_config(&resolved, None).await;
        assert!(
            result.is_err(),
            "offline (no managed-config client) must error, not silently no-op"
        );
    }

    /// The resolved source's artifact genuinely does not exist in the
    /// registry (`fetch_managed_config` returns `Ok(None)`) — `update_managed_config`
    /// must surface `ManagedConfigUpdateError::SourceNotFound`, not a silent
    /// `NotConfigured` success (amended post-Codex-gate 2026-07-05).
    #[tokio::test]
    async fn update_managed_config_absent_source_errors_source_not_found() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        // Stub client with NO artifact registered for `identifier` — the
        // manifest fetch resolves to `Ok(None)` (mirrors a registry 404).
        let client = Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier.clone(), RefreshPolicy::Notify, Duration::from_secs(86_400));

        let result = manager.update_managed_config(&resolved, None).await;
        match result {
            Err(ManagedConfigUpdateError::SourceNotFound { effective_source }) => {
                assert_eq!(effective_source, identifier);
            }
            other => panic!("expected SourceNotFound, got {other:?}"),
        }
    }

    // ── check_managed_config_refresh (crit 15/16 — notify/apply tick) ────────

    /// Criterion 15: `notify` posture on drift reports `DriftDetected` and
    /// never persists a snapshot (content is not fetched-for-write).
    #[tokio::test]
    async fn check_managed_config_refresh_notify_on_drift_reports_without_persisting() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let client = stub_client_with_artifact(&identifier, "[registry]\ndefault = \"corp\"\n");
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::ZERO);

        let outcome = manager.check_managed_config_refresh(&resolved).await;
        assert!(matches!(outcome, ManagedConfigRefreshOutcome::DriftDetected));
        assert!(
            !home
                .path()
                .join("state")
                .join("managed-config")
                .join("snapshot.json")
                .exists(),
            "notify posture must never write the snapshot"
        );
    }

    /// Finding #10 regression: the `notify` digest-only probe path must NOT
    /// download the config layer blob — it detects drift from the manifest
    /// digest alone. Asserts no blob pull was recorded during a drift tick.
    #[tokio::test]
    async fn check_managed_config_refresh_notify_probe_does_not_pull_layer_blob() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        // Retain the stub-data handle so recorded transport calls can be inspected.
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"corp\"\n")]);
        seed_package(
            &stub_data,
            &identifier,
            layer,
            ("any", "any"),
            None,
            crate::media_type::MEDIA_TYPE_TAR_GZ,
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data.clone())));
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::ZERO);

        let outcome = manager.check_managed_config_refresh(&resolved).await;
        assert!(matches!(outcome, ManagedConfigRefreshOutcome::DriftDetected));

        let calls = stub_data.read().calls.clone();
        assert!(
            !calls.iter().any(|call| call.starts_with("pull_blob")),
            "the notify digest-only probe must never pull the layer blob, recorded calls: {calls:?}"
        );
    }

    /// Criterion 16: `apply` posture on drift silently fetches, persists, and
    /// swaps the snapshot.
    #[tokio::test]
    async fn check_managed_config_refresh_apply_on_drift_persists_silently() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let client = stub_client_with_artifact(&identifier, "[registry]\ndefault = \"applied\"\n");
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Apply, Duration::ZERO);

        let outcome = manager.check_managed_config_refresh(&resolved).await;
        assert!(matches!(outcome, ManagedConfigRefreshOutcome::Applied { .. }));
        let snapshot_path = home.path().join("state").join("managed-config").join("snapshot.json");
        assert!(snapshot_path.exists(), "apply posture must persist the new snapshot");
    }

    /// No drift (a matching snapshot already on disk) → `UpToDate`, no re-fetch
    /// side effects beyond the throttle-marker touch.
    #[tokio::test]
    async fn check_managed_config_refresh_no_drift_is_up_to_date() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let client = stub_client_with_artifact(&identifier, "[registry]\ndefault = \"corp\"\n");
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Apply, Duration::ZERO);

        // First tick applies and persists; second tick sees no drift.
        manager.check_managed_config_refresh(&resolved).await;
        let outcome = manager.check_managed_config_refresh(&resolved).await;
        assert!(matches!(outcome, ManagedConfigRefreshOutcome::UpToDate));
    }

    /// An in-force pause short-circuits the tick BEFORE the throttle and
    /// probe — zero transport calls (no auth, no HEAD, no manifest pull).
    #[tokio::test]
    async fn check_managed_config_refresh_pause_short_circuits_with_zero_transport_calls() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"corp\"\n")]);
        seed_package(
            &stub_data,
            &identifier,
            layer,
            ("any", "any"),
            None,
            crate::media_type::MEDIA_TYPE_TAR_GZ,
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data.clone())));
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Apply, Duration::ZERO);

        let pause = crate::managed_config::ManagedConfigPause::for_duration(std::time::Duration::from_secs(3600), None);
        crate::managed_config::write_pause(&manager.file_structure.state, &pause)
            .await
            .unwrap();

        let outcome = manager.check_managed_config_refresh(&resolved).await;
        assert!(
            matches!(outcome, ManagedConfigRefreshOutcome::Paused),
            "got {outcome:?}"
        );

        let calls = stub_data.read().calls.clone();
        assert!(
            calls.is_empty(),
            "a paused tick must make zero transport calls, recorded: {calls:?}"
        );
    }

    /// An expired pause reads as absent — the next tick resumes normal
    /// drift detection.
    #[tokio::test]
    async fn check_managed_config_refresh_resumes_after_pause_expiry() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let client = stub_client_with_artifact(&identifier, "[registry]\ndefault = \"corp\"\n");
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::ZERO);

        let expired = crate::managed_config::ManagedConfigPause {
            paused_until: (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            pinned_version: None,
        };
        crate::managed_config::write_pause(&manager.file_structure.state, &expired)
            .await
            .unwrap();

        let outcome = manager.check_managed_config_refresh(&resolved).await;
        assert!(
            matches!(outcome, ManagedConfigRefreshOutcome::DriftDetected),
            "an expired pause must not block the tick, got {outcome:?}"
        );
    }

    /// Throttle short-circuit: a very long interval means the second tick
    /// never re-probes (would otherwise re-detect drift against a stale
    /// snapshot were the throttle not respected).
    #[tokio::test]
    async fn check_managed_config_refresh_respects_throttle() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let client = stub_client_with_artifact(&identifier, "[registry]\ndefault = \"corp\"\n");
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::from_secs(86_400));

        let first = manager.check_managed_config_refresh(&resolved).await;
        assert!(matches!(first, ManagedConfigRefreshOutcome::DriftDetected));
        let second = manager.check_managed_config_refresh(&resolved).await;
        assert!(
            matches!(second, ManagedConfigRefreshOutcome::UpToDate),
            "the second tick within the throttle window must short-circuit, got {second:?}"
        );
    }

    /// Codex-F1 regression: an `apply`-on-drift tick whose full fetch fails must
    /// NOT touch the throttle marker — the next tick within the window
    /// re-probes (transport called again) instead of being throttled for the
    /// full interval. Were the marker touched before the failed fetch, the
    /// second tick would short-circuit on the (large) throttle.
    #[tokio::test]
    async fn check_managed_config_refresh_apply_fetch_failure_does_not_throttle() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();

        // Probe succeeds (HEAD reads the `digest` override) but the full fetch
        // fails: `manifests` is empty, so `pull_manifest_raw` hits the error
        // override. Drift is real (no snapshot on disk), so `apply` attempts
        // the full fetch and fails.
        let stub_data = StubTransportData::new();
        {
            let mut inner = stub_data.write();
            inner.digest = Some(format!("sha256:{}", "a".repeat(64)));
            inner.pull_manifest_error_override = Some("registry unreachable mid-fetch".to_string());
        }
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data.clone())));
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Apply, Duration::from_secs(86_400));

        let first = manager.check_managed_config_refresh(&resolved).await;
        assert!(
            matches!(first, ManagedConfigRefreshOutcome::Unreachable),
            "a failed apply fetch reports Unreachable, got {first:?}"
        );

        let second = manager.check_managed_config_refresh(&resolved).await;
        assert!(
            matches!(second, ManagedConfigRefreshOutcome::Unreachable),
            "the second tick must re-run (not throttle), got {second:?}"
        );

        let probe_calls = stub_data
            .read()
            .calls
            .iter()
            .filter(|call| call.as_str() == "fetch_manifest_digest")
            .count();
        assert_eq!(
            probe_calls, 2,
            "a failed apply must not touch the marker; the next tick must re-probe"
        );
    }

    /// Finding S7: the tick detects drift from the identity clause alone
    /// (`!snapshot_matches_source`) even when the content digest is unchanged —
    /// a snapshot recorded under a DIFFERENT repository but the SAME digest the
    /// registry now serves is still drift (cross-repo cache-poison defense).
    #[tokio::test]
    async fn check_managed_config_refresh_drift_via_identity_at_matching_digest() {
        let home = TempDir::new().unwrap();
        let identifier = Identifier::parse("corp.example.com/ocx-config:v1").unwrap();
        let (client, index_digest) = stub_client_with_package(&identifier, "[registry]\ndefault = \"corp\"\n");
        let manager = manager_with_client(home.path(), client);

        // Seed a snapshot whose content digest equals the registry's current
        // top digest but whose provenance names a different repository. The
        // digest clause (`snapshot.digest != probed`) is therefore FALSE — only
        // the identity clause can flag drift here.
        let snapshot = serde_json::json!({
            "source": "other.example.com/ocx-config:v1",
            "digest": index_digest,
            "fetched_at": "2026-07-05T00:00:00Z",
        });
        std::fs::create_dir_all(manager.file_structure.state.managed_config_dir()).unwrap();
        std::fs::write(
            manager.file_structure.state.managed_config_snapshot_file(),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        std::fs::write(
            manager.file_structure.state.managed_config_toml_file(),
            "[registry]\ndefault = \"x\"\n",
        )
        .unwrap();

        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::ZERO);
        let outcome = manager.check_managed_config_refresh(&resolved).await;
        assert!(
            matches!(outcome, ManagedConfigRefreshOutcome::DriftDetected),
            "a cross-repo snapshot at an unchanged digest must still drift, got {outcome:?}"
        );
    }
}
