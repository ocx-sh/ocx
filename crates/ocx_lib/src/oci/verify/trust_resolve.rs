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
//! Precedence, cheapest-to-override first:
//!
//! 1. `--sigstore-trusted-root` flag,
//! 2. `OCX_SIGSTORE_TRUSTED_ROOT`,
//! 3. `[trust.sigstore] trusted_root` / `trusted_root_json` from `config.toml`,
//! 4. `$OCX_HOME/sigstore/trusted-root.json` (convention path, no config),
//! 5. the fresh trust-root cache for this Rekor instance,
//! 6. the public-good root fetched over TUF.
//!
//! Rungs 1 and 2 arrive already collapsed into one path (the CLI passes
//! `flag.or(env)`; auto-verify passes the env value). Offline additionally
//! requires the resolved material to carry a pinned Rekor key, and never
//! reaches the TUF fetch.
//!
//! Rungs 1–3 name material the operator asked for, so a missing or unreadable
//! file is an error. Rung 4 is a convention: absent means "not configured this
//! way" and falls through, while an unreadable *present* file still fails.

use std::path::{Path, PathBuf};

use super::error::{TrustRootLoadReason, VerifyErrorKind};
use super::trust_cache::TrustRootCache;
use super::trust_root::TrustRoot;
use crate::file_structure::StateStore;
use crate::trust::SigstoreTrust;
use crate::utility::fs::{BoundedReadError, read_bounded};

/// The largest a trusted-root JSON document may be.
///
/// Named here rather than inferred from the caller: the public-good
/// `trusted_root.json` is roughly 20 KiB and a self-hosted one a few, so one
/// mebibyte is ~50x every honest document while still refusing the
/// `/dev/zero`-shaped read the cap exists for. It is deliberately the same
/// ceiling `MAX_SIGSTORE_RESPONSE_BYTES` puts on the *same document* arriving
/// over the network, so the transport an operator chose does not change how
/// large a trust root may be.
const MAX_TRUSTED_ROOT_BYTES: u64 = 1024 * 1024;

/// Resolve the trust root from the supplied overrides, then the configured
/// `[trust.sigstore]` material, the `$OCX_HOME` convention path, the trust-root
/// cache, and finally the embedded root — enforcing the offline
/// pinned-Rekor-key gate on every rung.
///
/// `explicit_override` is the already-resolved flag-or-env path (rungs 1–2).
/// `sigstore` is the merged `[trust.sigstore]` table, `home_trusted_root` the
/// `$OCX_HOME/sigstore/trusted-root.json` convention path. `state` owns the
/// trust-root cache layout; `rekor_cache_key` keys it by Rekor authority.
///
/// # Errors
/// Returns the [`VerifyErrorKind`] describing the failure (asset read failure,
/// a `trusted_root` / `trusted_root_json` ambiguity, JSON parse failure,
/// offline-with-no-pinned-key, or a failed TUF fetch). Callers tag it with the
/// target identifier.
pub async fn resolve_trust_root(
    explicit_override: Option<&Path>,
    sigstore: Option<&SigstoreTrust>,
    home_trusted_root: Option<&Path>,
    state: &StateStore,
    rekor_cache_key: &str,
    offline: bool,
) -> Result<TrustRoot, VerifyErrorKind> {
    let read_err = |error: BoundedReadError| {
        VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::AssetReadFailed {
            source: Box::new(error),
        })
    };

    // 1+2. `--sigstore-trusted-root` / `OCX_SIGSTORE_TRUSTED_ROOT`, already collapsed.
    if let Some(path) = explicit_override {
        let json_path = trusted_root_json_path(path).await;
        let bytes = read_trusted_root(&json_path).await.map_err(read_err)?;
        let root = TrustRoot::load_trusted_root_json(&bytes)?;
        return enforce_offline_rekor_key(root, offline);
    }

    // 3. `[trust.sigstore]` from the operator `config.toml` tiers. The two
    //    spellings are checked for ambiguity before either is read, so a
    //    misconfigured file fails the same way whichever one would have won.
    if let Some(sigstore) = sigstore {
        if sigstore.trusted_root.is_some() && sigstore.trusted_root_json.is_some() {
            return Err(VerifyErrorKind::TrustRootLoad(
                TrustRootLoadReason::AmbiguousTrustRootConfig,
            ));
        }
        if let Some(json) = sigstore.trusted_root_json.as_deref() {
            let root = TrustRoot::load_trusted_root_json(json.as_bytes())?;
            return enforce_offline_rekor_key(root, offline);
        }
        if let Some(path) = sigstore.trusted_root.as_deref() {
            let json_path = trusted_root_json_path(path).await;
            let bytes = read_trusted_root(&json_path).await.map_err(read_err)?;
            let root = TrustRoot::load_trusted_root_json(&bytes)?;
            return enforce_offline_rekor_key(root, offline);
        }
    }

    // 4. `$OCX_HOME/sigstore/trusted-root.json` — the drop-a-file convention.
    //    Absent falls through to the cache; present-but-unreadable does not,
    //    or a permission problem would masquerade as "not configured".
    if let Some(path) = home_trusted_root {
        match read_trusted_root(path).await {
            Ok(bytes) => {
                let root = TrustRoot::load_trusted_root_json(&bytes)?;
                return enforce_offline_rekor_key(root, offline);
            }
            // Absence, and only absence, falls through. `TooLarge` and
            // `NotRegularFile` refuse a file that IS there: routing either into
            // this arm would let a present-but-unusable trust root silently
            // downgrade to the cache and then to TUF — the same masquerade a
            // permission error would be, arriving through a newer door. A
            // wildcard arm here is what `BoundedReadError`'s own doc comment
            // warns against.
            Err(BoundedReadError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(read_err(error)),
        }
    }

    // 5. Fresh trust-root cache for this Rekor instance (Fulcio + Rekor key).
    //    A normal cache entry always carries a Rekor key, but route it through
    //    the same offline gate so a hand-edited keyless entry still yields the
    //    actionable error rather than a deeper Rekor failure.
    if let Ok(Some(cached)) = TrustRootCache::from_cache(rekor_cache_key, state).await {
        return enforce_offline_rekor_key(cached.into_trust_root(), offline);
    }

    // 6. Nothing cached or supplied. Offline cannot fall back to the online
    //    TUF fetch — fail with the remedy.
    if offline {
        return Err(VerifyErrorKind::TrustRootLoad(
            TrustRootLoadReason::OfflineTrustMaterialUnavailable,
        ));
    }
    TrustRoot::load_embedded(&state.tuf_cache_dir()).await
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

/// Read a trusted-root document, bounded at [`MAX_TRUSTED_ROOT_BYTES`] and
/// refusing anything that is not a regular file.
///
/// Both guards live in [`read_bounded`], which is blocking — so it goes to the
/// pool rather than growing an async twin of the guard: one bounded reader, not
/// two (the `options::tags` precedent).
///
/// A `JoinError` becomes [`BoundedReadError::Io`] carrying `ErrorKind::Other`,
/// never `NotFound`, so a panicking pool task can never be mistaken for an
/// absent file by rung 4's fall-through.
async fn read_trusted_root(path: &Path) -> Result<Vec<u8>, BoundedReadError> {
    let target = path.to_path_buf();
    match tokio::task::spawn_blocking(move || read_bounded(&target, MAX_TRUSTED_ROOT_BYTES)).await {
        Ok(result) => result,
        Err(join) => Err(BoundedReadError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(format!("trusted-root read task panicked: {join}")),
        }),
    }
}

/// Resolve a trusted-root override to the JSON file itself: the path as given
/// when it names a file, or `<dir>/trusted_root.json` when it names a directory.
///
/// Uses async `tokio::fs::metadata` — the sync `Path::is_dir` would block the
/// runtime worker on every trusted-root resolution.
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
        let result = resolve_trust_root(None, None, None, &state, "rekor.example", true).await;
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

    /// A cached entry without a pinned Rekor key diverges by mode: offline it is
    /// the actionable remedy, online it resolves (the key is fetched later).
    ///
    /// This is the network-free half of the fallback divergence. The other half
    /// — online with *nothing* cached — falls through to the TUF fetch, which
    /// opens a socket and therefore has no unit test by design (TEST-07); the
    /// acceptance suite covers it against the local stack.
    #[tokio::test]
    async fn keyless_cache_entry_diverges_between_offline_and_online() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        let entry = keyless_cache_entry();
        entry.write_cache(&state).await.unwrap();

        let offline = resolve_trust_root(None, None, None, &state, "rekor.example", true).await;
        assert!(
            matches!(
                offline,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::OfflineTrustMaterialUnavailable
                ))
            ),
            "offline + keyless cache must be OfflineTrustMaterialUnavailable, got {offline:?}"
        );

        let online = resolve_trust_root(None, None, None, &state, "rekor.example", false)
            .await
            .expect("online must accept a keyless cache entry");
        assert_eq!(
            online.der_certs().len(),
            1,
            "the cached anchor must survive the resolve"
        );
    }

    /// Both spellings of the same decision is a configuration error, not a
    /// silent pick — and it is refused before either is read, so the failure
    /// does not depend on which file happens to exist.
    #[tokio::test]
    async fn both_trusted_root_spellings_is_a_config_error() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        let sigstore = SigstoreTrust {
            trusted_root: Some(tmp.path().join("absent.json")),
            trusted_root_json: Some("{}".to_string()),
            ..SigstoreTrust::default()
        };
        let result = resolve_trust_root(None, Some(&sigstore), None, &state, "rekor.example", false).await;
        assert!(
            matches!(
                result,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::AmbiguousTrustRootConfig
                ))
            ),
            "both spellings must be AmbiguousTrustRootConfig, got {result:?}"
        );
    }

    /// An absent `$OCX_HOME/sigstore/trusted-root.json` is "not configured this
    /// way", so the ladder continues to the cache rung below it. Shown against
    /// a cache entry that only the fall-through can reach.
    #[tokio::test]
    async fn absent_home_trusted_root_falls_through_to_the_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        keyless_cache_entry().write_cache(&state).await.unwrap();

        let home = tmp.path().join("home/sigstore/trusted-root.json");
        let resolved = resolve_trust_root(None, None, Some(&home), &state, "rekor.example", false)
            .await
            .expect("an absent convention path must not stop the ladder");
        assert_eq!(
            resolved.der_certs().len(),
            1,
            "the cache rung below the convention path must have supplied the anchor"
        );
    }

    /// The converse of the test above: a *present* but unreadable convention
    /// file is a real failure, never a silent fall-through — otherwise a
    /// permission problem would look identical to "no file here".
    #[tokio::test]
    async fn unparseable_home_trusted_root_does_not_fall_through() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        // A reachable cache entry, so a fall-through would SUCCEED and the
        // assertion below can only pass by the convention rung failing loudly.
        keyless_cache_entry().write_cache(&state).await.unwrap();

        let home = tmp.path().join("trusted-root.json");
        tokio::fs::write(&home, b"not json at all").await.unwrap();
        let result = resolve_trust_root(None, None, Some(&home), &state, "rekor.example", false).await;
        assert!(
            matches!(result, Err(VerifyErrorKind::TrustRootLoad(_))),
            "a present-but-broken convention file must fail, got {result:?}"
        );
    }

    /// One Fulcio anchor, no Rekor key — the shape both cache-rung tests need.
    fn keyless_cache_entry() -> TrustRootCache {
        TrustRootCache {
            rekor_authority: "rekor.example".into(),
            fulcio_der_certs: vec![vec![0x30, 0x00]],
            ctfe_keys: std::collections::BTreeMap::new(),
            rekor_public_key_pem: None,
            cached_at: std::time::SystemTime::now(),
            ttl_seconds: 3600,
        }
    }

    /// Rung 1 over rung 3. Both rungs are wired to fail, with *different*
    /// errors, so the error kind names which one ran: reaching the config
    /// tier at all would report `AmbiguousTrustRootConfig` instead.
    #[tokio::test]
    async fn the_explicit_override_is_consulted_before_the_config_tier() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        keyless_cache_entry().write_cache(&state).await.unwrap();

        let explicit = tmp.path().join("absent-explicit.json");
        let sigstore = SigstoreTrust {
            trusted_root: Some(tmp.path().join("also-absent.json")),
            trusted_root_json: Some("{}".to_string()),
            ..SigstoreTrust::default()
        };
        let result = resolve_trust_root(Some(&explicit), Some(&sigstore), None, &state, "rekor.example", false).await;
        assert!(
            matches!(
                result,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::AssetReadFailed { .. }
                ))
            ),
            "the explicit override must be read first, got {result:?}"
        );
    }

    /// Rung 3 over rung 4, same discriminating-error technique: the convention
    /// path holds material that fails with a *different* kind, so a config tier
    /// that was skipped would be visible in the error.
    #[tokio::test]
    async fn the_config_tier_is_consulted_before_the_home_convention_path() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        keyless_cache_entry().write_cache(&state).await.unwrap();

        let home = tmp.path().join("trusted-root.json");
        tokio::fs::write(&home, b"not json at all").await.unwrap();
        let sigstore = SigstoreTrust {
            trusted_root: Some(tmp.path().join("absent.json")),
            trusted_root_json: Some("{}".to_string()),
            ..SigstoreTrust::default()
        };
        let result = resolve_trust_root(None, Some(&sigstore), Some(&home), &state, "rekor.example", false).await;
        assert!(
            matches!(
                result,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::AmbiguousTrustRootConfig
                ))
            ),
            "[trust.sigstore] must be consulted before the convention path, got {result:?}"
        );
    }

    /// C-010a. Rung 4's fall-through is for **absence only**.
    ///
    /// An absent convention file means "not configured this way" and continues
    /// down the ladder; a file that is *there* and unusable — a directory, or
    /// one past the cap — must not. Routing either refusal into the
    /// fall-through would let an operator-dropped trust root silently downgrade
    /// to the cache and then to TUF, which is the masquerade the arm exists to
    /// prevent, arriving through the door `read_bounded` opened.
    ///
    /// The two outcomes are told apart by the error kind, not by success: with
    /// nothing cached and `offline` set, falling through lands on
    /// `OfflineTrustMaterialUnavailable`, so a refusal that produced *that*
    /// would be indistinguishable from the absence case. Both halves are
    /// asserted in one test so the discriminator is proved, not assumed.
    #[tokio::test]
    async fn rung_four_falls_through_on_absence_but_not_on_a_present_unusable_file() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));

        let absent = tmp.path().join("absent-trusted-root.json");
        let fell_through = resolve_trust_root(None, None, Some(&absent), &state, "rekor.example", true).await;
        assert!(
            matches!(
                fell_through,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::OfflineTrustMaterialUnavailable
                ))
            ),
            "an absent convention file must fall through to the cache rung, got {fell_through:?}"
        );

        let a_directory = tmp.path().join("trusted-root-is-a-directory");
        std::fs::create_dir(&a_directory).unwrap();
        let refused_directory = resolve_trust_root(None, None, Some(&a_directory), &state, "rekor.example", true).await;
        assert!(
            matches!(
                refused_directory,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::AssetReadFailed { .. }
                ))
            ),
            "a convention path that is not a regular file must fail, not fall through, got {refused_directory:?}"
        );

        let past_the_cap = tmp.path().join("huge-trusted-root.json");
        std::fs::write(&past_the_cap, vec![b'x'; MAX_TRUSTED_ROOT_BYTES as usize + 1]).unwrap();
        let refused_huge = resolve_trust_root(None, None, Some(&past_the_cap), &state, "rekor.example", true).await;
        assert!(
            matches!(
                refused_huge,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::AssetReadFailed { .. }
                ))
            ),
            "a convention file past the cap must fail, not fall through, got {refused_huge:?}"
        );
    }

    /// C-010. The operator-typed rungs are bounded too, and the bound is what
    /// refuses — not the JSON parser downstream of an unbounded read.
    ///
    /// The discriminator is the error kind: an unbounded `fs::read` of this
    /// file succeeds and hands a megabyte of `x` to
    /// `load_trusted_root_json`, which answers `PemParseFailed`. Only the cap
    /// produces `AssetReadFailed`, so this assertion cannot pass on the
    /// pre-change code.
    #[tokio::test]
    async fn the_explicit_override_is_refused_at_the_cap_not_by_the_parser() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        let past_the_cap = tmp.path().join("huge-explicit.json");
        std::fs::write(&past_the_cap, vec![b'x'; MAX_TRUSTED_ROOT_BYTES as usize + 1]).unwrap();

        let result = resolve_trust_root(Some(&past_the_cap), None, None, &state, "rekor.example", false).await;
        assert!(
            matches!(
                result,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::AssetReadFailed { .. }
                ))
            ),
            "--sigstore-trusted-root past the cap must be refused by the read, got {result:?}"
        );
    }

    /// A `[trust.sigstore]` carrying neither spelling is not a configured trust
    /// root — it says something about Fulcio/Rekor URLs and nothing about the
    /// anchor — so the ladder continues past it rather than failing.
    #[tokio::test]
    async fn a_url_only_config_tier_does_not_stop_the_ladder() {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateStore::new(tmp.path().join("state"));
        keyless_cache_entry().write_cache(&state).await.unwrap();

        let sigstore = SigstoreTrust {
            fulcio_url: Some("https://fulcio.corp.example".to_string()),
            rekor_url: Some("https://rekor.corp.example".to_string()),
            ..SigstoreTrust::default()
        };
        let resolved = resolve_trust_root(None, Some(&sigstore), None, &state, "rekor.example", false)
            .await
            .expect("a URL-only [trust.sigstore] must not stop the ladder");
        assert_eq!(resolved.der_certs().len(), 1, "the cache rung supplied the anchor");
    }
}
