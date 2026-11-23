// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Managed-config update and throttled background-refresh task methods for [`PackageManager`].

use super::super::PackageManager;
use ocx_config::managed::ResolvedManagedConfig;
use ocx_config::managed_config::ManagedConfigUpdateError;
use ocx_config::patch::ResolvedPatchConfig;
use ocx_config::refresh::RefreshPolicy;
use ocx_oci::Digest;
use std::time::Duration;

/// Bound on the background patch revalidation, so a slow patch registry cannot hold the user's command open.
const PATCH_REVALIDATION_DEADLINE: Duration = Duration::from_secs(5);

/// Outcome of a full managed-config update cycle.
#[derive(Debug, Clone)]
pub enum ManagedConfigUpdateResult {
    /// The local snapshot already matches the registry's current digest; nothing was written.
    AlreadyCurrent {
        /// The unchanged manifest digest.
        digest: Digest,
    },
    /// A new snapshot was fetched and persisted.
    Updated {
        /// The newly persisted manifest digest.
        digest: Digest,
    },
}

/// Outcome of one [`PackageManager::check_managed_config_refresh`] tick.
#[derive(Debug, Clone)]
pub enum ManagedConfigRefreshOutcome {
    /// No drift, or the throttle window has not elapsed.
    UpToDate,
    /// `notify` posture: drift detected, nothing fetched; the caller prints the advisory.
    DriftDetected,
    /// `apply` posture: drift detected and the new snapshot persisted.
    Applied {
        /// The newly persisted manifest digest.
        digest: Digest,
    },
    /// The probe could not reach the registry, or the apply-on-drift persist failed; already logged.
    Unreachable,
    /// An in-force `ocx config update --pause` short-circuited the tick; zero transport calls.
    Paused,
}

impl PackageManager {
    /// Runs a full managed-config update: fetch, persist, and report what changed.
    ///
    /// `expected_digest` pins a `tag@digest` request; `resolved.source` must then be the tag-only
    /// reference, or a digest-only fetch trivially satisfies the pin.
    ///
    /// # Errors
    ///
    /// [`ManagedConfigUpdateError`] on any fetch or persist failure, including
    /// [`ManagedConfigUpdateError::PinDigestMismatch`]; the existing snapshot is left untouched.
    pub async fn update_managed_config(
        &self,
        resolved: &ResolvedManagedConfig,
        expected_digest: Option<&Digest>,
    ) -> Result<ManagedConfigUpdateResult, ManagedConfigUpdateError> {
        let Some(client) = self.managed_config_client.as_ref() else {
            return Err(ManagedConfigUpdateError::Fetch(
                ocx_config::managed_config::ManagedConfigFetchError::FetchFailed {
                    source: ocx_oci::client::error::ClientError::Registry(Box::new(std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        "managed-config update requires network access (offline mode)",
                    ))),
                },
            ));
        };

        let Some(fetched) = ocx_config::managed_config::fetch_managed_config(client, &resolved.source).await? else {
            return Err(ManagedConfigUpdateError::SourceNotFound {
                effective_source: resolved.source.clone(),
            });
        };

        // Before the already-current check and persist, so a mismatch never touches disk.
        if let Some(expected) = expected_digest
            && fetched.manifest_digest != *expected
        {
            return Err(ManagedConfigUpdateError::PinDigestMismatch {
                expected: expected.clone(),
                fetched: fetched.manifest_digest,
            });
        }

        // Exact source match, not `snapshot_matches_source`, so a rollback to another tag at one digest re-persists.
        let canonical_source = resolved.source.to_string();
        let existing =
            ocx_config::managed_config::read_managed_config_snapshot(&self.file_structure.state.managed_config()).await;

        let payload_text = fetched.config_text.clone();

        let result = if let Some(existing) = &existing
            && existing.source == canonical_source
            && existing.digest == fetched.manifest_digest
        {
            ManagedConfigUpdateResult::AlreadyCurrent {
                digest: existing.digest.clone(),
            }
        } else {
            let snapshot = ocx_config::managed_config::persist_managed_config(
                &self.file_structure.state.managed_config(),
                &resolved.source,
                fetched,
            )
            .await?;
            ManagedConfigUpdateResult::Updated {
                digest: snapshot.digest,
            }
        };

        // A dropped `[patches]` must not be re-contacted: after an update `self` still holds the
        // pre-update tier, so only an unchanged config may fall back to it.
        let effective_tier_fallback = matches!(result, ManagedConfigUpdateResult::AlreadyCurrent { .. });
        sync_patches_from_payload(self, &payload_text, effective_tier_fallback).await;

        Ok(result)
    }

    /// Probes the managed-config manifest digest without downloading or persisting anything.
    ///
    /// `None` when the registry is unreachable or the source is absent.
    pub async fn probe_managed_config_digest(&self, resolved: &ResolvedManagedConfig) -> Option<Digest> {
        let client = self.managed_config_client.as_ref()?;
        match ocx_config::managed_config::probe_managed_config_digest(client, &resolved.source).await {
            Ok(digest) => digest,
            Err(error) => {
                log::debug!(
                    "managed-config digest probe for '{}' failed: {chain}",
                    resolved.source,
                    chain = ocx_util::error::render_chain(&error)
                );
                None
            }
        }
    }

    /// Throttled background-refresh probe; never fails the caller.
    ///
    /// The refresh marker is never touched on the throttle short-circuit or a failed `apply`,
    /// or the tier stays silent for the full interval instead of re-probing next tick.
    pub async fn check_managed_config_refresh(&self, resolved: &ResolvedManagedConfig) -> ManagedConfigRefreshOutcome {
        if ocx_config::managed_config::read_pause(&self.file_structure.state.managed_config())
            .await
            .is_some()
        {
            return ManagedConfigRefreshOutcome::Paused;
        }

        let marker = self.file_structure.state.managed_config().refresh_marker();
        let interval = resolved.interval;
        let marker_check = marker.clone();
        // `unwrap_or(false)`: a panicked blocking task must not wedge the probe into permanent skip.
        let throttled = tokio::task::spawn_blocking(move || {
            ocx_store::file_structure::StateStore::is_throttled(&marker_check, interval)
        })
        .await
        .unwrap_or(false);
        if throttled {
            return ManagedConfigRefreshOutcome::UpToDate;
        }

        let Some(client) = self.managed_config_client.as_ref() else {
            ocx_store::file_structure::StateStore::touch(marker).await;
            return ManagedConfigRefreshOutcome::Unreachable;
        };

        let probed_digest =
            match ocx_config::managed_config::probe_managed_config_digest(client, &resolved.source).await {
                Ok(Some(digest)) => digest,
                Ok(None) => {
                    log::debug!(
                        "managed-config refresh: source '{}' not found in registry",
                        resolved.source
                    );
                    ocx_store::file_structure::StateStore::touch(marker).await;
                    return ManagedConfigRefreshOutcome::Unreachable;
                }
                Err(error) => {
                    log::debug!(
                        "managed-config refresh probe failed: {chain}",
                        chain = ocx_util::error::render_chain(&error)
                    );
                    ocx_store::file_structure::StateStore::touch(marker).await;
                    return ManagedConfigRefreshOutcome::Unreachable;
                }
            };

        let existing =
            ocx_config::managed_config::read_managed_config_snapshot(&self.file_structure.state.managed_config()).await;
        let drift = existing.as_ref().is_none_or(|snapshot| {
            !ocx_config::managed::snapshot_matches_source(snapshot, &resolved.source)
                || snapshot.digest != probed_digest
        });
        if !drift {
            // Before the marker, so a revalidation that errors or times out still throttles the next tick.
            if let (RefreshPolicy::Apply, Some(snapshot)) = (&resolved.refresh, &existing) {
                revalidate_patches_from_payload(self, &snapshot.config, PATCH_REVALIDATION_DEADLINE).await;
            }
            ocx_store::file_structure::StateStore::touch(marker).await;
            return ManagedConfigRefreshOutcome::UpToDate;
        }

        match resolved.refresh {
            // `Manual` never reaches here; the caller gates on it.
            RefreshPolicy::Manual | RefreshPolicy::Notify => {
                ocx_store::file_structure::StateStore::touch(marker).await;
                ManagedConfigRefreshOutcome::DriftDetected
            }
            RefreshPolicy::Apply => {
                // Marker untouched until the persist succeeds, or a failed apply is throttled a full interval.
                let fetched = match ocx_config::managed_config::fetch_managed_config(client, &resolved.source).await {
                    Ok(Some(fetched)) => fetched,
                    Ok(None) => {
                        log::debug!(
                            "managed-config apply-on-drift: source '{}' vanished before the full fetch",
                            resolved.source
                        );
                        return ManagedConfigRefreshOutcome::Unreachable;
                    }
                    Err(error) => {
                        log::debug!(
                            "managed-config apply-on-drift fetch failed: {chain}",
                            chain = ocx_util::error::render_chain(&error)
                        );
                        return ManagedConfigRefreshOutcome::Unreachable;
                    }
                };
                match ocx_config::managed_config::persist_managed_config(
                    &self.file_structure.state.managed_config(),
                    &resolved.source,
                    fetched,
                )
                .await
                {
                    Ok(snapshot) => {
                        ocx_store::file_structure::StateStore::touch(marker).await;
                        sync_patches_from_payload(self, &snapshot.config, false).await;
                        ManagedConfigRefreshOutcome::Applied {
                            digest: snapshot.digest,
                        }
                    }
                    // A publisher-side defect the operator must see; other persist failures are transient.
                    Err(error @ ocx_config::managed_config::ManagedConfigPersistError::ExtraCaCertsInvalid { .. }) => {
                        log::warn!(
                            "managed-config apply-on-drift refused '{}': {}",
                            resolved.source,
                            ocx_util::error::render_chain(&error)
                        );
                        ManagedConfigRefreshOutcome::Unreachable
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

/// Best-effort patch-descriptor sync after a managed-config fetch; never fails the caller.
///
/// Reads `[patches]` from the payload, not `patches()`, which still holds the pre-update config.
/// With `effective_tier_fallback`, a payload without `[patches]` syncs `patches()` instead.
async fn sync_patches_from_payload(manager: &PackageManager, payload_toml: &str, effective_tier_fallback: bool) {
    if manager.is_offline() {
        return;
    }
    if manager.patch_snapshot().is_some() {
        log::debug!("managed-config patch sync: skipped under a patch snapshot");
        return;
    }
    let target = match patches_from_payload(payload_toml) {
        Some(resolved) => manager.clone().with_patches(Some(resolved)),
        None if effective_tier_fallback && manager.patches().is_some() => manager.clone(),
        None => return, // no patch tier to converge
    };

    let host = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
    match target.sync_patches(&[host]).await {
        Ok(_report) => log::debug!("managed-config patch sync: completed"),
        Err(error) => log::warn!(
            "managed-config patch sync failed (non-fatal): {chain}",
            chain = ocx_util::error::render_chain(&error)
        ),
    }
}

/// Best-effort patch revalidation on the payload's `[patches]` tier; bounded, never fails the caller.
///
/// A payload without `[patches]` makes no request, even when `patches()` is set: that tier may come
/// from `OCX_PATCHES` or a local config, and a background tick must never install from it.
async fn revalidate_patches_from_payload(manager: &PackageManager, payload_toml: &str, deadline: Duration) {
    let Some(resolved) = patches_from_payload(payload_toml) else {
        return;
    };
    let host = ocx_oci::Platform::current().unwrap_or_else(ocx_oci::Platform::any);
    let target = manager.clone().with_patches(Some(resolved));
    let timed_out = tokio::time::timeout(deadline, target.revalidate_patches(&host)).await;
    let marker = manager.file_structure.state.managed_config().patch_timeout_marker();
    match timed_out {
        Ok(Ok(())) => {
            log::debug!("managed-config patch revalidation: completed");
            // Only a completed revalidation ends the streak, or a registry alternating a timeout with
            // an error never warns. Best effort: a missing marker is the common case.
            let _ = tokio::fs::remove_file(&marker).await;
        }
        Ok(Err(error)) if operator_must_see(&error) => log::warn!(
            "managed-config patch revalidation failed (non-fatal): {chain}",
            chain = ocx_util::error::render_chain(&error)
        ),
        Ok(Err(error)) => log::debug!(
            "managed-config patch revalidation failed: {chain}",
            chain = ocx_util::error::render_chain(&error)
        ),
        Err(_elapsed) => {
            // The marker from the previous tick makes a repeat visible: one slow tick is noise, every tick is not.
            // Check-then-touch races across processes; the cost is one missed or doubled warning.
            if ocx_util::fs::path_exists_lossy(&marker).await {
                log::warn!(
                    "managed-config patch revalidation timed out again after {deadline:?}; patch descriptors are not being refreshed"
                );
            } else {
                log::debug!("managed-config patch revalidation: timed out");
            }
            ocx_store::file_structure::StateStore::touch(marker).await;
        }
    }
}

/// Whether a background revalidation failure is a refusal the operator must act on, not an unreachable registry.
///
/// An unclassified error warns on purpose, so a refusal never hides: a new transient error variant
/// must classify as `Unavailable` or `TempFail`, or it warns on every background tick.
fn operator_must_see(error: &crate::Error) -> bool {
    use ocx_exit::{ClassifyExitCode, ExitCode};
    !matches!(error.classify(), Some(ExitCode::Unavailable | ExitCode::TempFail))
}

/// The `[patches]` tier a managed-config payload declares; `None`, never an error, for any unusable payload.
fn patches_from_payload(payload_toml: &str) -> Option<ResolvedPatchConfig> {
    let config = match toml::from_str::<ocx_config::Config>(payload_toml) {
        Ok(config) => config,
        Err(error) => {
            log::debug!("managed-config patch sync: parsing payload TOML failed: {error}");
            return None;
        }
    };
    match ocx_config::patch::resolve_patch_config(&config) {
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
    use ocx_config::managed_config::test_support::{gzip_tar, seed_package, stub_client_with_package};
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
    use ocx_oci::{Client, OciIdentifier};
    use ocx_store::file_structure::FileStructure;

    /// Seeds a `StubTransport`-backed client with one managed-config package
    /// (v2 wire shape) at `identifier`.
    fn stub_client_with_artifact(identifier: &OciIdentifier, config_toml: &str) -> Client {
        stub_client_with_package(identifier, config_toml).0
    }

    /// Builds a manager rooted at `root`, wired with `client` as the
    /// managed-config-fetch client (offline `client()`/index — these tests
    /// only exercise the managed-config task methods).
    fn manager_with_client(root: &std::path::Path, client: Client) -> PackageManager {
        let fs = FileStructure::with_root(root.to_path_buf());
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(&fs),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        PackageManager::new(fs, index, None, "localhost:5000").with_managed_config_client(Some(client))
    }

    fn resolved(source: OciIdentifier, refresh: RefreshPolicy, interval: Duration) -> ResolvedManagedConfig {
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(&fs),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let manager = PackageManager::new(fs, index, None, "localhost:5000"); // no managed_config_client
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
        // Retain the stub-data handle so recorded transport calls can be inspected.
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"corp\"\n")]);
        seed_package(
            &stub_data,
            &identifier,
            layer,
            ("any", "any"),
            None,
            ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
        let stub_data = StubTransportData::new();
        let layer = gzip_tar(&[("config.toml", b"[registry]\ndefault = \"corp\"\n")]);
        seed_package(
            &stub_data,
            &identifier,
            layer,
            ("any", "any"),
            None,
            ocx_oci::media_type::MEDIA_TYPE_TAR_GZ,
        );
        let client = Client::with_transport(Box::new(StubTransport::new(stub_data.clone())));
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Apply, Duration::ZERO);

        let pause =
            ocx_config::managed_config::ManagedConfigPause::for_duration(std::time::Duration::from_secs(3600), None);
        ocx_config::managed_config::write_pause(&manager.file_structure.state.managed_config(), &pause)
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
        let client = stub_client_with_artifact(&identifier, "[registry]\ndefault = \"corp\"\n");
        let manager = manager_with_client(home.path(), client);
        let resolved = resolved(identifier, RefreshPolicy::Notify, Duration::ZERO);

        let expired = ocx_config::managed_config::ManagedConfigPause {
            paused_until: (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
            pinned_version: None,
        };
        ocx_config::managed_config::write_pause(&manager.file_structure.state.managed_config(), &expired)
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();

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
        let identifier =
            OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap();
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
        std::fs::create_dir_all(manager.file_structure.state.managed_config().dir()).unwrap();
        std::fs::write(
            manager.file_structure.state.managed_config().snapshot_file(),
            serde_json::to_vec(&snapshot).unwrap(),
        )
        .unwrap();
        std::fs::write(
            manager.file_structure.state.managed_config().toml_file(),
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

    // ── patch tier authority ─────────────────────────────────────────────────

    const PAYLOAD_WITH_PATCHES: &str = "[patches]\nregistry = \"patches.corp.com\"\n";
    const PAYLOAD_WITHOUT_PATCHES: &str = "[registry]\ndefault = \"corp\"\n";

    fn patch_tier() -> ResolvedPatchConfig {
        ResolvedPatchConfig {
            system_required: false,
            no_patches: std::collections::BTreeSet::new(),
            registry: "patches.corp.com".to_string(),
            path_template: "{registry}/{repository}".to_string(),
            required: false,
        }
    }

    /// A manager whose patch requests land on `patch_data` and whose managed-config fetches land on
    /// `client`, so a test can tell the two apart.
    fn manager_with_patch_transport(
        root: &std::path::Path,
        client: Client,
        patch_data: &StubTransportData,
        local_tier: Option<ResolvedPatchConfig>,
    ) -> PackageManager {
        let fs = FileStructure::with_root(root.to_path_buf());
        // One installed base, so a revalidation or sync has something to look up.
        let candidate = ocx_store::file_structure::SymlinkStore::new(root.join("symlinks"))
            .candidate(&ocx_oci::PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28"));
        std::fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        std::fs::write(&candidate, b"").unwrap();
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(&fs),
        });
        let index = ocx_index::Index::from_chained(local_index, vec![], ocx_index::ChainMode::Offline);
        let patch_client = ocx_oci::client::test_transport::stub_client(patch_data);
        PackageManager::new(fs, index, Some(patch_client), "localhost:5000")
            .with_managed_config_client(Some(client))
            .with_patches(local_tier)
    }

    fn patch_requests(data: &StubTransportData) -> usize {
        data.read().calls.len()
    }

    fn source() -> OciIdentifier {
        OciIdentifier::parse_target("corp.example.com/ocx-config:v1", ocx_oci::DEFAULT_REGISTRY).unwrap()
    }

    /// `Apply` with no drift revalidates the payload's patch tier.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_managed_config_refresh_apply_without_drift_revalidates_the_payload_tier() {
        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITH_PATCHES);
        let manager = manager_with_patch_transport(home.path(), client, &patch_data, None);
        let resolved = resolved(source(), RefreshPolicy::Apply, Duration::ZERO);

        // The first tick applies the drift; the second sees none.
        manager.check_managed_config_refresh(&resolved).await;
        patch_data.write().calls.clear();
        let outcome = manager.check_managed_config_refresh(&resolved).await;

        assert!(
            matches!(outcome, ManagedConfigRefreshOutcome::UpToDate),
            "got {outcome:?}"
        );
        assert!(
            patch_requests(&patch_data) > 0,
            "a tick without drift must revalidate the payload's patch tier"
        );
    }

    /// A payload without `[patches]` makes no patch request, even when the manager's own tier is set.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_managed_config_refresh_without_payload_patches_ignores_the_local_tier() {
        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITHOUT_PATCHES);
        let manager = manager_with_patch_transport(home.path(), client, &patch_data, Some(patch_tier()));
        let resolved = resolved(source(), RefreshPolicy::Apply, Duration::ZERO);

        manager.check_managed_config_refresh(&resolved).await;
        let outcome = manager.check_managed_config_refresh(&resolved).await;

        assert!(
            matches!(outcome, ManagedConfigRefreshOutcome::UpToDate),
            "got {outcome:?}"
        );
        assert_eq!(
            patch_requests(&patch_data),
            0,
            "the local tier must never trigger a background request"
        );
    }

    /// `Notify` makes no patch request, with or without drift.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_managed_config_refresh_notify_without_drift_makes_no_patch_request() {
        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITH_PATCHES);
        let manager = manager_with_patch_transport(home.path(), client, &patch_data, None);
        let apply = resolved(source(), RefreshPolicy::Apply, Duration::ZERO);
        manager.check_managed_config_refresh(&apply).await;
        patch_data.write().calls.clear();

        let notify = resolved(source(), RefreshPolicy::Notify, Duration::ZERO);
        let outcome = manager.check_managed_config_refresh(&notify).await;

        assert!(
            matches!(outcome, ManagedConfigRefreshOutcome::UpToDate),
            "got {outcome:?}"
        );
        assert_eq!(patch_requests(&patch_data), 0);
    }

    /// A revalidation that never answers is cut at its deadline without failing the caller.
    #[tokio::test(flavor = "multi_thread")]
    async fn revalidate_patches_from_payload_gives_up_at_the_deadline() {
        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        patch_data.write().tag_read_delay = Some(Duration::from_secs(3600));
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITH_PATCHES);
        let manager = manager_with_patch_transport(home.path(), client, &patch_data, None);
        // A fresh source is fetched, not probed; a recorded one reaches the delayed digest probe.
        let global = crate::tasks::patch_discovery::global_descriptor_id(&patch_tier());
        let path = manager.file_structure().patch_descriptor_path(&global);
        crate::tasks::patch_discovery::PatchTagMap::write_has_descriptor(
            &path,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            &crate::tasks::patch_discovery::stamp_beside(&path),
        )
        .await
        .unwrap();

        let started = std::time::Instant::now();
        revalidate_patches_from_payload(&manager, PAYLOAD_WITH_PATCHES, Duration::from_millis(100)).await;

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline must cut the wait"
        );
        assert!(
            patch_data
                .read()
                .calls
                .iter()
                .any(|call| call == "fetch_manifest_digest"),
            "setup: the delayed digest probe must have started"
        );
    }

    /// Records `digest` as the descriptor `patch_tier()`'s global source last held.
    async fn record_global_descriptor(manager: &PackageManager, digest: &str) {
        let global = crate::tasks::patch_discovery::global_descriptor_id(&patch_tier());
        let path = manager.file_structure().patch_descriptor_path(&global);
        crate::tasks::patch_discovery::PatchTagMap::write_has_descriptor(
            &path,
            digest,
            &crate::tasks::patch_discovery::stamp_beside(&path),
        )
        .await
        .unwrap();
    }

    /// A timed-out revalidation leaves the marker a later tick reads; only a completed one removes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_patch_timeout_marker_follows_the_last_revalidation() {
        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        patch_data.write().tag_read_delay = Some(Duration::from_secs(3600));
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITH_PATCHES);
        let manager = manager_with_patch_transport(home.path(), client, &patch_data, None);
        let marker = manager.file_structure.state.managed_config().patch_timeout_marker();
        record_global_descriptor(
            &manager,
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        )
        .await;

        revalidate_patches_from_payload(&manager, PAYLOAD_WITH_PATCHES, Duration::from_millis(100)).await;
        assert!(marker.exists(), "a timed-out revalidation must leave the marker");
        revalidate_patches_from_payload(&manager, PAYLOAD_WITH_PATCHES, Duration::from_millis(100)).await;
        assert!(marker.exists(), "a repeated timeout keeps the marker");

        patch_data.write().tag_read_delay = None;
        patch_data.write().ensure_auth_error_override = Some("denied".to_string());
        revalidate_patches_from_payload(&manager, PAYLOAD_WITH_PATCHES, Duration::from_secs(30)).await;
        assert!(marker.exists(), "a failed revalidation must not end the streak");

        // The recorded digest names a descriptor the registry never held, so that source keeps failing; with
        // no record the source is fetched fresh, found absent, and the revalidation completes.
        patch_data.write().ensure_auth_error_override = None;
        let global = crate::tasks::patch_discovery::global_descriptor_id(&patch_tier());
        std::fs::remove_file(manager.file_structure().patch_descriptor_path(&global)).unwrap();
        revalidate_patches_from_payload(&manager, PAYLOAD_WITH_PATCHES, Duration::from_secs(30)).await;
        assert!(!marker.exists(), "a completed revalidation must remove the marker");
    }

    /// A deadline that cuts a companion install mid-read leaves the old descriptor record, no pin and no
    /// package; the next tick with time to spare installs it and advances the record.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_deadline_cutting_a_companion_install_leaves_no_half_installed_state() {
        use crate::tasks::patch_discovery::{PatchDiscoveryState, PatchTagMap};
        use crate::tasks::patch_sync::tests::{build_descriptor_artifact, publish_companion};

        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        let companion = publish_companion(&patch_data, "ca-certs", &[("any", "any")]);
        let config_digest = ocx_oci::Algorithm::Sha256.hash(br#"{"type":"bundle","version":1}"#);
        let rules = serde_json::json!([{
            "match": "*", "packages": ["patches.corp.com/ca-certs:latest"], "required": true
        }]);
        let (manifest_bytes, manifest_digest, layer_bytes, layer_digest) = build_descriptor_artifact(rules);
        {
            let mut inner = patch_data.write();
            inner.manifests.insert(
                crate::tasks::patch_discovery::global_descriptor_id(&patch_tier()).to_string(),
                (manifest_bytes, manifest_digest.to_string()),
            );
            inner.blobs.insert(layer_digest.to_string(), layer_bytes);
            inner
                .blob_delays
                .insert(config_digest.to_string(), Duration::from_secs(3600));
        }
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITH_PATCHES);
        let fs = FileStructure::with_root(home.path().to_path_buf());
        let candidate = ocx_store::file_structure::SymlinkStore::new(home.path().join("symlinks"))
            .candidate(&ocx_oci::PackageRef::new_registry("cmake", "ocx.sh").clone_with_tag("3.28"));
        std::fs::create_dir_all(candidate.parent().unwrap()).unwrap();
        std::fs::write(&candidate, b"").unwrap();
        let local_index = ocx_index::LocalIndex::new(ocx_index::LocalConfig {
            index_store: ocx_index::IndexStore::machine_local(&fs),
        });
        // A source, or the companion's tag has nothing to resolve against.
        let source_index = ocx_index::Index::from_remote(ocx_index::OciIndex::new(ocx_index::OciIndexConfig {
            client: ocx_oci::client::test_transport::stub_client(&patch_data),
        }));
        let index = ocx_index::Index::from_chained(local_index, vec![source_index], ocx_index::ChainMode::Default);
        let manager = PackageManager::new(
            fs,
            index,
            Some(ocx_oci::client::test_transport::stub_client(&patch_data)),
            "localhost:5000",
        )
        .with_managed_config_client(Some(client));
        let old = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        record_global_descriptor(&manager, old).await;
        let global_path = manager
            .file_structure()
            .patch_descriptor_path(&crate::tasks::patch_discovery::global_descriptor_id(&patch_tier()));
        let companion_id = ocx_oci::PackageRef::parse("patches.corp.com/ca-certs:latest").unwrap();
        let pin_path = manager.file_structure().patch_companion_path(&companion_id);

        let started = std::time::Instant::now();
        revalidate_patches_from_payload(&manager, PAYLOAD_WITH_PATCHES, Duration::from_millis(100)).await;

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the deadline must cut the install"
        );
        assert!(
            patch_data
                .read()
                .calls
                .iter()
                .any(|call| *call == format!("pull_blob_streaming:{config_digest}")
                    || *call == format!("pull_blob:{config_digest}")
                    || *call == format!("pull_blob_to_file:{config_digest}")),
            "setup: the install must have reached the delayed companion blob"
        );
        assert_eq!(
            PatchTagMap::read(&global_path).await.unwrap(),
            PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: old.to_string()
            },
            "the descriptor record must keep the old digest"
        );
        assert_eq!(
            PatchTagMap::read_tag(&pin_path, "latest").await.unwrap(),
            None,
            "no companion pin"
        );
        assert!(
            manager.file_structure().packages.list_all().await.unwrap().is_empty(),
            "the package store must hold no entry"
        );

        patch_data.write().blob_delays.clear();
        revalidate_patches_from_payload(&manager, PAYLOAD_WITH_PATCHES, Duration::from_secs(30)).await;

        assert_eq!(
            PatchTagMap::read_tag(&pin_path, "latest").await.unwrap().as_deref(),
            Some(companion.top.to_string().as_str()),
            "the companion pin is recorded once it installed"
        );
        assert_eq!(
            manager.file_structure().packages.list_all().await.unwrap().len(),
            1,
            "the companion is installed"
        );
        assert_eq!(
            PatchTagMap::read(&global_path).await.unwrap(),
            PatchDiscoveryState::LookedHasDescriptor {
                manifest_digest: manifest_digest.to_string()
            },
            "the descriptor record advances"
        );
    }

    /// A no-drift tick reports `UpToDate` and touches the throttle marker after its revalidation.
    #[tokio::test(flavor = "multi_thread")]
    async fn check_managed_config_refresh_touches_the_marker_after_the_revalidation() {
        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITH_PATCHES);
        let manager = manager_with_patch_transport(home.path(), client, &patch_data, None);
        let resolved = resolved(source(), RefreshPolicy::Apply, Duration::ZERO);
        manager.check_managed_config_refresh(&resolved).await;
        let marker = manager.file_structure.state.managed_config().refresh_marker();
        std::fs::remove_file(&marker).unwrap();

        let outcome = manager.check_managed_config_refresh(&resolved).await;

        assert!(
            matches!(outcome, ManagedConfigRefreshOutcome::UpToDate),
            "got {outcome:?}"
        );
        assert!(
            marker.exists(),
            "a no-drift tick must touch the marker after revalidating"
        );
    }

    /// Only a refusal the operator can act on warns; an unreachable or retryable registry stays at debug.
    #[test]
    fn operator_must_see_separates_refusals_from_an_unreachable_registry() {
        use crate::error::{PackageError, PackageErrorKind};
        use ocx_oci::client::error::ClientError;
        let fetch_failed = |source| crate::Error::from(crate::patch::PatchError::FetchFailed { source });
        let unavailable = fetch_failed(ClientError::Registry(Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "connection refused",
        ))));
        let temp_fail = fetch_failed(ClientError::ShortBlobRead {
            expected: 10,
            actual: 5,
        });
        let auth = fetch_failed(ClientError::Authentication(Box::new(std::io::Error::other(
            "bad creds",
        ))));
        let required_companion = crate::Error::DiscoverFailed(vec![PackageError::new(
            ocx_oci::PackageRef::new_registry("java", "example.com"),
            PackageErrorKind::RequiredCompanionFailed {
                companion: ocx_oci::PackageRef::new_registry("license-server", "patches.corp.com"),
                source: Box::new(PackageErrorKind::NotFound),
            },
        )]);
        let unclassified = crate::Error::Singleflight(ocx_util::singleflight::Error::Failed(
            ocx_util::singleflight::SharedError::for_test(std::io::Error::other("leader failed")),
        ));
        {
            use ocx_exit::ClassifyExitCode;
            assert_eq!(
                unavailable.classify(),
                Some(ocx_exit::ExitCode::Unavailable),
                "setup: unreachable"
            );
            assert_eq!(
                temp_fail.classify(),
                Some(ocx_exit::ExitCode::TempFail),
                "setup: retryable"
            );
            assert_eq!(unclassified.classify(), None, "setup: no exit code");
        }

        for (name, error, loud) in [
            ("unavailable", &unavailable, false),
            ("temp fail", &temp_fail, false),
            ("required companion", &required_companion, true),
            ("auth", &auth, true),
            ("unclassified", &unclassified, true),
        ] {
            assert_eq!(operator_must_see(error), loud, "{name}");
        }
    }

    /// An unchanged config with no `[patches]` in the payload syncs the manager's effective tier.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_managed_config_already_current_without_payload_patches_syncs_the_effective_tier() {
        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITHOUT_PATCHES);
        let manager = manager_with_patch_transport(home.path(), client, &patch_data, Some(patch_tier()));
        let resolved = resolved(source(), RefreshPolicy::Notify, Duration::ZERO);
        manager.update_managed_config(&resolved, None).await.unwrap();
        patch_data.write().calls.clear();

        let second = manager.update_managed_config(&resolved, None).await.unwrap();

        assert!(matches!(second, ManagedConfigUpdateResult::AlreadyCurrent { .. }));
        assert!(patch_requests(&patch_data) > 0, "the effective tier must be synced");
    }

    /// An applied update whose payload drops `[patches]` contacts no patch registry, though the
    /// manager still holds the pre-update tier.
    #[tokio::test(flavor = "multi_thread")]
    async fn update_managed_config_updated_without_payload_patches_syncs_nothing() {
        let home = TempDir::new().unwrap();
        let patch_data = StubTransportData::new();
        let client = stub_client_with_artifact(&source(), PAYLOAD_WITHOUT_PATCHES);
        let manager = manager_with_patch_transport(home.path(), client, &patch_data, Some(patch_tier()));
        let resolved = resolved(source(), RefreshPolicy::Notify, Duration::ZERO);

        let result = manager.update_managed_config(&resolved, None).await.unwrap();

        assert!(matches!(result, ManagedConfigUpdateResult::Updated { .. }));
        assert_eq!(
            patch_requests(&patch_data),
            0,
            "a dropped registry must not be contacted"
        );
    }
}
