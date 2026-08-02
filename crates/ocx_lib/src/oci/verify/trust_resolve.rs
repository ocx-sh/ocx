// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Trust-root resolution ladder shared by `ocx package verify` (flag-driven)
//! and the policy-gated auto-verify hook (env-driven).
//!
//! Both paths must apply the identical offline gate — a policy-covered package
//! must never be verified against material that cannot check the Rekor SET
//! offline, and must never silently skip verification. Keeping the ladder in
//! one place makes that gate a single source of truth. The CLI command layers
//! flag-vs-env override resolution and per-identifier error tagging on top.
//!
//! Precedence: `--tuf-root`/`OCX_SIGSTORE_TUF_ROOT` (Fulcio CA + pinned Rekor
//! key) → `--trust-root`/`OCX_SIGSTORE_TRUST_ROOT` (Fulcio-CA PEM, no Rekor
//! key) → the fresh trust-root cache for this Rekor instance → the embedded
//! root (stubbed → exit 78). Offline additionally requires the resolved
//! material to carry a pinned Rekor key.

use std::path::{Path, PathBuf};

use super::error::{TrustRootLoadReason, VerifyErrorKind};
use super::trust_cache::TrustRootCache;
use super::trust_root::TrustRoot;
use crate::file_structure::StateStore;

/// Resolve the trust root from the supplied overrides, then the trust-root
/// cache, then the embedded root — enforcing the offline pinned-Rekor-key gate.
///
/// `tuf_override` / `pem_override` are the already-resolved override paths (the
/// CLI passes `flag.or(env)`, auto-verify passes the env value). `state` owns the
/// trust-root cache layout; `rekor_cache_key` keys it by Rekor authority.
///
/// # Errors
/// Returns the [`VerifyErrorKind`] describing the failure (asset read failure,
/// PEM/JSON parse failure, offline-with-no-pinned-key, or the stubbed embedded
/// root). Callers tag it with the target identifier.
pub async fn resolve_trust_root(
    tuf_override: Option<&Path>,
    pem_override: Option<&Path>,
    state: &StateStore,
    rekor_cache_key: &str,
    offline: bool,
) -> Result<TrustRoot, VerifyErrorKind> {
    let read_err = |source: std::io::Error| {
        VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::AssetReadFailed {
            source: Box::new(source),
        })
    };

    // 1. TUF trusted-root JSON override (Fulcio CA + pinned Rekor key).
    if let Some(path) = tuf_override {
        let json_path = trusted_root_json_path(path).await;
        let bytes = tokio::fs::read(&json_path).await.map_err(read_err)?;
        let root = TrustRoot::load_trusted_root_json(&bytes)?;
        return enforce_offline_rekor_key(root, offline);
    }

    // 2. Fulcio-CA PEM override (no Rekor key — fetched online, or pinned from cache).
    if let Some(path) = pem_override {
        let bytes = tokio::fs::read(path).await.map_err(read_err)?;
        let root = TrustRoot::load_from_pem(&bytes)?;
        return enforce_offline_rekor_key(root, offline);
    }

    // 3. Fresh trust-root cache for this Rekor instance (Fulcio + Rekor key).
    //    A normal cache entry always carries a Rekor key, but route it through
    //    the same offline gate so a hand-edited keyless entry still yields the
    //    actionable error rather than a deeper Rekor failure.
    if let Ok(Some(cached)) = TrustRootCache::from_cache(rekor_cache_key, state).await {
        return enforce_offline_rekor_key(cached.into_trust_root(), offline);
    }

    // 4. Nothing cached or supplied. Offline cannot fall back to the online-only
    //    embedded/fetch path — fail with the remedy.
    if offline {
        return Err(VerifyErrorKind::TrustRootLoad(
            TrustRootLoadReason::OfflineTrustMaterialUnavailable,
        ));
    }
    TrustRoot::load_embedded()
}

/// Offline verify needs a pinned Rekor key (the SET cannot be checked without
/// one and there is no network to fetch it). A trust root that lacks one is an
/// actionable error offline; online it is fine (the key is fetched, then cached).
fn enforce_offline_rekor_key(root: TrustRoot, offline: bool) -> Result<TrustRoot, VerifyErrorKind> {
    if offline && root.rekor_public_key_pem().is_none() {
        return Err(VerifyErrorKind::TrustRootLoad(
            TrustRootLoadReason::OfflineTrustMaterialUnavailable,
        ));
    }
    Ok(root)
}

/// Resolve a `--tuf-root` value to the trusted-root JSON file: the path itself
/// when it is a file, or `<dir>/trusted_root.json` when it names a directory.
///
/// Uses async `tokio::fs::metadata` — the sync `Path::is_dir` would block the
/// runtime worker on every `--tuf-root`/`OCX_SIGSTORE_TUF_ROOT` resolution.
async fn trusted_root_json_path(path: &Path) -> PathBuf {
    let is_dir = tokio::fs::metadata(path).await.map(|m| m.is_dir()).unwrap_or(false);
    if is_dir {
        path.join("trusted_root.json")
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Offline with no override and an empty cache must fail with the actionable
    /// "no pinned Rekor key" error — never fall through to the online-only
    /// embedded/fetch path (that would silently skip the offline SET check).
    #[tokio::test]
    async fn offline_with_no_material_fails_closed_with_remedy() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        let result = resolve_trust_root(None, None, &state, "rekor.example", true).await;
        assert!(
            matches!(
                result,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::OfflineTrustMaterialUnavailable
                ))
            ),
            "offline + no material must be OfflineTrustMaterialUnavailable, got {result:?}"
        );
    }

    /// Online with no override and an empty cache reaches the embedded root,
    /// which is stubbed → `TrustRootUnavailable` (distinct from the offline
    /// remedy). Pins that the two fallbacks diverge.
    #[tokio::test]
    async fn online_with_no_material_reaches_embedded_stub() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        let result = resolve_trust_root(None, None, &state, "rekor.example", false).await;
        assert!(
            matches!(result, Err(VerifyErrorKind::TrustRootUnavailable)),
            "online + no material must reach the embedded stub, got {result:?}"
        );
    }
}
