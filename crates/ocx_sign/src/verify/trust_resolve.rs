// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Trust-root resolution ladder shared by `ocx package verify` and the auto-verify hook, so both apply one offline
//! gate.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::error::{TrustRootLoadReason, VerifyErrorKind};
use super::trust_cache::TrustRootCache;
use super::trust_root::TrustRoot;
use crate::sign::state::SigningStatePaths;
use ocx_trust::{CompiledPolicy, PolicyBackend, SigstoreAuthority, SigstoreInstances};
use ocx_util::fs::BoundedReadError;

/// The largest a trusted-root JSON document may be.
// Keep equal to `MAX_SIGSTORE_RESPONSE_BYTES`, so the same document's size limit never depends on its transport.
pub const MAX_TRUSTED_ROOT_BYTES: u64 = 1024 * 1024;

/// A trust root a policy pass resolved, with whether this process fetched it over TUF.
///
/// Only a fetched root may be cached, and only once a verdict has proven it: an operator-named or cached root is
/// already where the operator or an earlier verify put it.
#[derive(Debug, Clone)]
pub struct ResolvedRoot {
    pub root: TrustRoot,
    /// The root came from the public-good TUF fetch of this pass.
    pub fetched: bool,
}

impl ResolvedRoot {
    /// Record this root in the trust-root cache after a catalog or statement it verified, so a later process
    /// skips the TUF fetch.
    ///
    /// Writes only a fetched root that pins exactly one Rekor key, keyed and best-effort as the writer
    /// `ocx package verify` uses, so both agree. A key that differs from the cached one replaces it: this one just
    /// verified, the cached one did not. Call it on success only, never offline.
    pub async fn cache_after_verdict(&self, rekor_cache_key: &str, state: &SigningStatePaths) {
        if !self.fetched {
            return;
        }
        if let Some(pem) = self.root.single_rekor_key_pem() {
            TrustRootCache::store_material(&self.root, pem, rekor_cache_key, state).await;
        }
    }
}

/// Resolve the public instance's trust root, first rung wins: `explicit_override` (flag-or-env), the configured
/// instance table, `home_trusted_root`, the fresh cache under `rekor_cache_key`, then the public-good root over TUF.
///
/// Offline, every rung must yield a pinned Rekor key and TUF is never reached.
///
/// # Errors
///
/// An unreadable operator-named file (exit 74), a `trusted_root` / `trusted_root_json` ambiguity,
/// a JSON parse failure, offline without a pinned key, or a failed TUF fetch.
pub async fn resolve_trust_root(
    explicit_override: Option<&Path>,
    sigstore: Option<&SigstoreInstances>,
    home_trusted_root: Option<&Path>,
    state: &SigningStatePaths,
    rekor_cache_key: &str,
    offline: bool,
) -> Result<TrustRoot, VerifyErrorKind> {
    resolve_instance_trust_root(
        &SigstoreAuthority::public(),
        explicit_override,
        sigstore,
        home_trusted_root,
        state,
        rekor_cache_key,
        offline,
    )
    .await
}

/// [`resolve_trust_root`] for the instance `instance`.
///
/// The six sources keep their order and scope to that instance: `explicit_override` is the caller's choice for this
/// instance only, the table is `instance`'s own, `home_trusted_root` is its convention path and the cache is its
/// slot. TUF is the public instance's last rung; a private instance with none of the first five fails with
/// [`VerifyErrorKind::SigstoreTrustedRootMissing`], and `rekor_cache_key` is read for the public instance only.
///
/// # Errors
///
/// As [`resolve_trust_root`], plus [`VerifyErrorKind::SigstoreTrustedRootMissing`].
pub async fn resolve_instance_trust_root(
    instance: &SigstoreAuthority,
    explicit_override: Option<&Path>,
    sigstore: Option<&SigstoreInstances>,
    home_trusted_root: Option<&Path>,
    state: &SigningStatePaths,
    rekor_cache_key: &str,
    offline: bool,
) -> Result<TrustRoot, VerifyErrorKind> {
    resolve_instance_with(
        instance,
        explicit_override,
        sigstore,
        home_trusted_root,
        state,
        rekor_cache_key,
        offline,
        || async { TrustRoot::load_embedded(&state.tuf_cache_dir()).await },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn resolve_instance_with<F, Fut>(
    instance: &SigstoreAuthority,
    explicit_override: Option<&Path>,
    sigstore: Option<&SigstoreInstances>,
    home_trusted_root: Option<&Path>,
    state: &SigningStatePaths,
    rekor_cache_key: &str,
    offline: bool,
    tuf: F,
) -> Result<TrustRoot, VerifyErrorKind>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<TrustRoot, VerifyErrorKind>>,
{
    let local = resolve_local_instance_trust_root(
        instance,
        explicit_override,
        sigstore,
        home_trusted_root,
        state,
        rekor_cache_key,
    )
    .await?;
    match local {
        Some(root) => enforce_offline_rekor_key(root, offline),
        None if *instance != SigstoreAuthority::public() => Err(VerifyErrorKind::SigstoreTrustedRootMissing {
            authority: instance.to_string(),
        }),
        None if offline => Err(VerifyErrorKind::OfflineNoPinnedRekorKey),
        None => tuf().await,
    }
}

/// The public instance's trust root a first-rung-wins pass finds with no network: the override, the configured
/// instance table, the home file or the fresh cache. `None` when no rung holds one, which is not an error: a
/// key-signed candidate needs none.
///
/// # Errors
///
/// An unreadable operator-named file (exit 74), a `trusted_root` / `trusted_root_json` ambiguity or a JSON parse
/// failure. Never a Rekor-key refusal: whether the root can check a log entry is the caller's question.
pub async fn resolve_local_trust_root(
    explicit_override: Option<&Path>,
    sigstore: Option<&SigstoreInstances>,
    home_trusted_root: Option<&Path>,
    state: &SigningStatePaths,
    rekor_cache_key: &str,
) -> Result<Option<TrustRoot>, VerifyErrorKind> {
    resolve_local_instance_trust_root(
        &SigstoreAuthority::public(),
        explicit_override,
        sigstore,
        home_trusted_root,
        state,
        rekor_cache_key,
    )
    .await
}

/// [`resolve_local_trust_root`] for the instance `instance`; the returned root names it.
///
/// # Errors
///
/// As [`resolve_local_trust_root`].
pub async fn resolve_local_instance_trust_root(
    instance: &SigstoreAuthority,
    explicit_override: Option<&Path>,
    sigstore: Option<&SigstoreInstances>,
    home_trusted_root: Option<&Path>,
    state: &SigningStatePaths,
    rekor_cache_key: &str,
) -> Result<Option<TrustRoot>, VerifyErrorKind> {
    let public = *instance == SigstoreAuthority::public();
    let table = match sigstore {
        Some(instances) if public => Some(instances.public()),
        Some(instances) => instances.get(instance).cloned(),
        None => None,
    };
    let sigstore = table.as_ref();
    // Exit 74, not 78: every read here is an operator-typed path, matching `--key file:<missing>`.
    let read_err = |error: BoundedReadError| {
        VerifyErrorKind::TrustRootLoad(TrustRootLoadReason::TrustRootUnreadable {
            source: Box::new(error),
        })
    };
    let tagged = |root: Result<TrustRoot, VerifyErrorKind>| root.map(|root| Some(root.for_instance(instance.clone())));

    if let Some(path) = explicit_override {
        let json_path = trusted_root_json_path(path).await;
        let bytes = read_trusted_root(&json_path).await.map_err(read_err)?;
        return tagged(TrustRoot::load_trusted_root_json(&bytes));
    }

    if let Some(sigstore) = sigstore {
        if sigstore.trusted_root.is_some() && sigstore.trusted_root_json.is_some() {
            return Err(VerifyErrorKind::TrustRootLoad(
                TrustRootLoadReason::AmbiguousTrustRootConfig,
            ));
        }
        if let Some(json) = sigstore.trusted_root_json.as_deref() {
            return tagged(TrustRoot::load_trusted_root_json(json.as_bytes()));
        }
        if let Some(path) = sigstore.trusted_root.as_deref() {
            let json_path = trusted_root_json_path(path).await;
            let bytes = read_trusted_root(&json_path).await.map_err(read_err)?;
            return tagged(TrustRoot::load_trusted_root_json(&bytes));
        }
    }

    if let Some(path) = home_trusted_root {
        match read_trusted_root(path).await {
            Ok(bytes) => return tagged(TrustRoot::load_trusted_root_json(&bytes)),
            // Only `NotFound` falls through; any other error here silently downgrades a present trust root to
            // cache/TUF.
            Err(BoundedReadError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(read_err(error)),
        }
    }

    let cached = if public {
        TrustRootCache::from_cache(rekor_cache_key, state).await
    } else {
        TrustRootCache::from_instance_cache(instance, state).await
    };
    if let Ok(Some(cached)) = cached {
        return Ok(Some(cached.into_trust_root().for_instance(instance.clone())));
    }
    Ok(None)
}

/// The trust roots a candidate set is verified against, one per Sigstore instance a keyless entry names.
///
/// Every instance takes its root from its own table, `home_root_of` (its convention path), the cache, then TUF
/// for the public instance only. A keyless entry naming an instance with
/// no local table contributes nothing: no network, no error. Key entries need no root, so a key-only set yields an
/// empty list.
///
/// # Errors
///
/// As [`resolve_instance_trust_root`] for each instance, the first failure ending the pass.
pub async fn resolve_policy_trust_roots(
    policies: &[CompiledPolicy],
    sigstore: Option<&SigstoreInstances>,
    home_root_of: &(dyn Fn(&SigstoreAuthority) -> Option<PathBuf> + Sync),
    state: &SigningStatePaths,
    rekor_cache_key: &str,
    offline: bool,
) -> Result<Vec<ResolvedRoot>, VerifyErrorKind> {
    resolve_policy_roots_with(
        policies,
        sigstore,
        home_root_of,
        state,
        rekor_cache_key,
        offline,
        || async { TrustRoot::load_embedded(&state.tuf_cache_dir()).await },
    )
    .await
}

async fn resolve_policy_roots_with<F, Fut>(
    policies: &[CompiledPolicy],
    sigstore: Option<&SigstoreInstances>,
    home_root_of: &(dyn Fn(&SigstoreAuthority) -> Option<PathBuf> + Sync),
    state: &SigningStatePaths,
    rekor_cache_key: &str,
    offline: bool,
    tuf: F,
) -> Result<Vec<ResolvedRoot>, VerifyErrorKind>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<TrustRoot, VerifyErrorKind>>,
{
    let instances: BTreeSet<&SigstoreAuthority> = policies
        .iter()
        .flat_map(|policy| policy.backends.iter())
        .filter_map(|backend| match backend {
            PolicyBackend::Keyless(keyless) => Some(&keyless.instance),
            PolicyBackend::Key(_) => None,
        })
        .collect();
    let public = SigstoreAuthority::public();
    let mut tuf = Some(tuf);
    let mut roots = Vec::with_capacity(instances.len());
    for instance in instances {
        if *instance == public {
            let fetch = tuf.take();
            let fetched = std::sync::atomic::AtomicBool::new(false);
            let fetched_flag = &fetched;
            let root = resolve_instance_with(
                instance,
                None,
                sigstore,
                home_root_of(instance).as_deref(),
                state,
                rekor_cache_key,
                offline,
                || async move {
                    let Some(fetch) = fetch else {
                        return Err(VerifyErrorKind::TrustRootUnavailable);
                    };
                    let root = fetch().await?;
                    fetched_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(root)
                },
            )
            .await?;
            roots.push(ResolvedRoot {
                root,
                fetched: fetched.load(std::sync::atomic::Ordering::SeqCst),
            });
        } else if sigstore.is_some_and(|instances| instances.get(instance).is_some()) {
            let root = resolve_instance_with(
                instance,
                None,
                sigstore,
                home_root_of(instance).as_deref(),
                state,
                rekor_cache_key,
                offline,
                || async { Err(VerifyErrorKind::TrustRootUnavailable) },
            )
            .await?;
            roots.push(ResolvedRoot { root, fetched: false });
        }
    }
    Ok(roots)
}

/// Refuse, offline only, a trust root without a pinned Rekor key: nothing else can check the SET.
fn enforce_offline_rekor_key(root: TrustRoot, offline: bool) -> Result<TrustRoot, VerifyErrorKind> {
    if offline && root.rekor_public_key_pem().is_none() {
        return Err(VerifyErrorKind::OfflineNoPinnedRekorKey);
    }
    Ok(root)
}

/// Read a trusted-root document, bounded at [`MAX_TRUSTED_ROOT_BYTES`], refusing a non-regular file.
async fn read_trusted_root(path: &Path) -> Result<Vec<u8>, BoundedReadError> {
    ocx_util::fs::read_bounded_async(path, MAX_TRUSTED_ROOT_BYTES).await
}

/// The path as given when it names a file, or `<dir>/trusted_root.json` when it names a directory.
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
    use ocx_trust::SigstoreInstance;

    /// Offline with no override and an empty cache must fail with the actionable
    /// "no pinned Rekor key" error — never fall through to the online-only
    /// embedded/fetch path (that would silently skip the offline SET check).
    #[tokio::test]
    async fn offline_with_no_material_fails_closed_with_remedy() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let result = resolve_trust_root(None, None, None, &state, "rekor.example", true).await;
        assert!(
            matches!(result, Err(VerifyErrorKind::OfflineNoPinnedRekorKey)),
            "offline + no material must be OfflineNoPinnedRekorKey, got {result:?}"
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
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let entry = keyless_cache_entry();
        entry.write_cache(&state).await.unwrap();

        let offline = resolve_trust_root(None, None, None, &state, "rekor.example", true).await;
        assert!(
            matches!(offline, Err(VerifyErrorKind::OfflineNoPinnedRekorKey)),
            "offline + keyless cache must be OfflineNoPinnedRekorKey, got {offline:?}"
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

    /// the local pass reports "no root here" as `None`, never as a refusal and never by reaching for TUF, so a
    /// key candidate offline with nothing cached is not failed by the trust-root ladder.
    #[tokio::test]
    async fn the_local_pass_finds_nothing_without_a_network_or_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let found = resolve_local_trust_root(None, None, None, &state, "rekor.example").await;
        assert!(matches!(found, Ok(None)), "got {found:?}");
    }

    /// The local pass accepts a cached root that has no pinned Rekor key (the strict ladder refuses it offline), and
    /// stays the same cache rung the full ladder reads.
    #[tokio::test]
    async fn the_local_pass_returns_a_cached_root_without_judging_its_rekor_key() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        keyless_cache_entry().write_cache(&state).await.unwrap();
        let local = resolve_local_trust_root(None, None, None, &state, "rekor.example")
            .await
            .expect("the cache rung reads")
            .expect("a cached root is local");
        assert_eq!(local.der_certs().len(), 1);
        assert!(local.rekor_public_key_pem().is_none());
    }

    /// Both spellings of the same decision is a configuration error, not a
    /// silent pick — and it is refused before either is read, so the failure
    /// does not depend on which file happens to exist.
    #[tokio::test]
    async fn both_trusted_root_spellings_is_a_config_error() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let sigstore = SigstoreInstances::with_public(SigstoreInstance {
            trusted_root: Some(tmp.path().join("absent.json")),
            trusted_root_json: Some("{}".to_string()),
            ..SigstoreInstance::default()
        });
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
        let state = SigningStatePaths::new(tmp.path().join("state"));
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
        let state = SigningStatePaths::new(tmp.path().join("state"));
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
        let state = SigningStatePaths::new(tmp.path().join("state"));
        keyless_cache_entry().write_cache(&state).await.unwrap();

        let explicit = tmp.path().join("absent-explicit.json");
        let sigstore = SigstoreInstances::with_public(SigstoreInstance {
            trusted_root: Some(tmp.path().join("also-absent.json")),
            trusted_root_json: Some("{}".to_string()),
            ..SigstoreInstance::default()
        });
        let result = resolve_trust_root(Some(&explicit), Some(&sigstore), None, &state, "rekor.example", false).await;
        assert!(
            matches!(
                result,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::TrustRootUnreadable { .. }
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
        let state = SigningStatePaths::new(tmp.path().join("state"));
        keyless_cache_entry().write_cache(&state).await.unwrap();

        let home = tmp.path().join("trusted-root.json");
        tokio::fs::write(&home, b"not json at all").await.unwrap();
        let sigstore = SigstoreInstances::with_public(SigstoreInstance {
            trusted_root: Some(tmp.path().join("absent.json")),
            trusted_root_json: Some("{}".to_string()),
            ..SigstoreInstance::default()
        });
        let result = resolve_trust_root(None, Some(&sigstore), Some(&home), &state, "rekor.example", false).await;
        assert!(
            matches!(
                result,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::AmbiguousTrustRootConfig
                ))
            ),
            "the table root must be consulted before the convention path, got {result:?}"
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
    /// `OfflineNoPinnedRekorKey`, so a refusal that produced *that*
    /// would be indistinguishable from the absence case. Both halves are
    /// asserted in one test so the discriminator is proved, not assumed.
    #[tokio::test]
    async fn rung_four_falls_through_on_absence_but_not_on_a_present_unusable_file() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));

        let absent = tmp.path().join("absent-trusted-root.json");
        let fell_through = resolve_trust_root(None, None, Some(&absent), &state, "rekor.example", true).await;
        assert!(
            matches!(fell_through, Err(VerifyErrorKind::OfflineNoPinnedRekorKey)),
            "an absent convention file must fall through to the cache rung, got {fell_through:?}"
        );

        let a_directory = tmp.path().join("trusted-root-is-a-directory");
        std::fs::create_dir(&a_directory).unwrap();
        let refused_directory = resolve_trust_root(None, None, Some(&a_directory), &state, "rekor.example", true).await;
        assert!(
            matches!(
                refused_directory,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::TrustRootUnreadable { .. }
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
                    TrustRootLoadReason::TrustRootUnreadable { .. }
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
    /// produces `TrustRootUnreadable`, so this assertion cannot pass on the
    /// pre-change code.
    #[tokio::test]
    async fn the_explicit_override_is_refused_at_the_cap_not_by_the_parser() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let past_the_cap = tmp.path().join("huge-explicit.json");
        std::fs::write(&past_the_cap, vec![b'x'; MAX_TRUSTED_ROOT_BYTES as usize + 1]).unwrap();

        let result = resolve_trust_root(Some(&past_the_cap), None, None, &state, "rekor.example", false).await;
        assert!(
            matches!(
                result,
                Err(VerifyErrorKind::TrustRootLoad(
                    TrustRootLoadReason::TrustRootUnreadable { .. }
                ))
            ),
            "--sigstore-trusted-root past the cap must be refused by the read, got {result:?}"
        );
    }

    /// A the configured instance table carrying neither spelling is not a configured trust
    /// root — it says something about Fulcio/Rekor URLs and nothing about the
    /// anchor — so the ladder continues past it rather than failing.
    #[tokio::test]
    async fn a_url_only_config_tier_does_not_stop_the_ladder() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        keyless_cache_entry().write_cache(&state).await.unwrap();

        let sigstore = SigstoreInstances::with_public(SigstoreInstance {
            fulcio_url: Some("https://fulcio.corp.example".to_string()),
            rekor_url: Some("https://rekor.corp.example".to_string()),
            ..SigstoreInstance::default()
        });
        let resolved = resolve_trust_root(None, Some(&sigstore), None, &state, "rekor.example", false)
            .await
            .expect("a URL-only instance table must not stop the ladder");
        assert_eq!(resolved.der_certs().len(), 1, "the cache rung supplied the anchor");
    }

    const GOLDEN_TRUSTED_ROOT: &str = include_str!("../../../../test/sigstore/trusted_root.json");

    fn corp() -> SigstoreAuthority {
        SigstoreAuthority::parse("fulcio.corp.example").expect("a valid authority")
    }

    fn keyless_on(instance: &SigstoreAuthority) -> CompiledPolicy {
        CompiledPolicy {
            builder: None,
            backends: vec![PolicyBackend::Keyless(ocx_trust::CompiledKeyless {
                identity: Some(ocx_trust::IdentityRule::Exact("someone@example.com".into())),
                issuer: "https://issuer.example".into(),
                claims: Vec::new(),
                instance: instance.clone(),
            })],
        }
    }

    fn corp_table(json: &str) -> SigstoreInstances {
        serde_json::from_value(serde_json::json!({ "fulcio.corp.example": { "trusted_root_json": json } }))
            .expect("a table keyed by an authority parses")
    }

    /// A TUF stand-in that counts its calls and answers like a failed fetch.
    fn counting_tuf(
        calls: &std::sync::atomic::AtomicUsize,
    ) -> impl FnOnce() -> std::future::Ready<Result<TrustRoot, VerifyErrorKind>> + '_ {
        move || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::ready(Err(VerifyErrorKind::TrustRootUnavailable))
        }
    }

    fn no_home(_: &SigstoreAuthority) -> Option<PathBuf> {
        None
    }

    fn state_in(tmp: &tempfile::TempDir) -> SigningStatePaths {
        SigningStatePaths::new(tmp.path().join("state"))
    }

    /// A private instance with none of the first five sources fails with its own slug and never reaches TUF, online
    /// or offline.
    #[tokio::test]
    async fn a_private_instance_with_no_source_fails_missing_and_never_reaches_tuf() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        for offline in [false, true] {
            let calls = std::sync::atomic::AtomicUsize::new(0);
            let result = resolve_instance_with(
                &corp(),
                None,
                None,
                None,
                &state,
                "rekor.example",
                offline,
                counting_tuf(&calls),
            )
            .await;
            assert!(
                matches!(&result, Err(VerifyErrorKind::SigstoreTrustedRootMissing { authority }) if authority == "fulcio.corp.example"),
                "offline={offline}: {result:?}"
            );
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0, "offline={offline}");
        }
    }

    /// The public instance keeps the TUF rung as its last resort.
    #[tokio::test]
    async fn the_public_instance_still_falls_back_to_tuf() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = resolve_instance_with(
            &SigstoreAuthority::public(),
            None,
            None,
            None,
            &state,
            "rekor.example",
            false,
            counting_tuf(&calls),
        )
        .await;
        assert!(
            matches!(result, Err(VerifyErrorKind::TrustRootUnavailable)),
            "{result:?}"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A private instance reads its own table and the root says whose it is.
    #[tokio::test]
    async fn a_private_instance_reads_its_own_table_and_is_tagged_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        let instances = corp_table(GOLDEN_TRUSTED_ROOT);
        let root = resolve_instance_trust_root(&corp(), None, Some(&instances), None, &state, "rekor.example", true)
            .await
            .expect("the table supplies the root");
        assert_eq!(root.instance(), corp());
        assert!(!root.der_certs().is_empty());
    }

    /// A private instance reads its own cache slot, never the public Rekor-keyed one.
    #[tokio::test]
    async fn a_private_instance_reads_its_own_cache_slot_only() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        keyless_cache_entry().write_cache(&state).await.unwrap();
        let from_public_slot = resolve_local_instance_trust_root(&corp(), None, None, None, &state, "rekor.example")
            .await
            .unwrap();
        assert!(
            from_public_slot.is_none(),
            "the public file is not a private instance's cache"
        );

        let mut entry = keyless_cache_entry();
        entry.rekor_authority = corp().to_string();
        entry.write_instance_cache(&corp(), &state).await.unwrap();
        let local = resolve_local_instance_trust_root(&corp(), None, None, None, &state, "rekor.example")
            .await
            .unwrap()
            .expect("its own slot is read");
        assert_eq!(local.instance(), corp());
    }

    /// One identity on an unknown authority and one key entry reach no network and yield no root.
    #[tokio::test]
    async fn an_unknown_authority_and_a_key_entry_make_no_network_call() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        let public_keyless = CompiledPolicy::exact("a".into(), "b".into());
        let key_backend = CompiledPolicy {
            builder: None,
            backends: vec![PolicyBackend::Key(
                sigstore::crypto::CosignVerificationKey::try_from_pem(
                    include_str!("../../../../test/tests/fixtures/golden/keys/cosign.pub").as_bytes(),
                )
                .unwrap(),
            )],
        };
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let roots = resolve_policy_roots_with(
            &[keyless_on(&corp()), key_backend],
            None,
            &no_home,
            &state,
            "rekor.example",
            false,
            counting_tuf(&calls),
        )
        .await
        .expect("an unknown authority is no error of its own");
        assert!(roots.is_empty());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        // The public exact policy is a keyless entry on the public instance: it does reach for TUF, so the counter
        // is shown able to move.
        let reached = resolve_policy_roots_with(
            &[public_keyless],
            None,
            &no_home,
            &state,
            "rekor.example",
            false,
            counting_tuf(&calls),
        )
        .await;
        assert!(reached.is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Each instance named by a usable entry gets its own root, once.
    #[tokio::test]
    async fn each_named_instance_gets_one_root_of_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let state = state_in(&tmp);
        keyless_cache_entry().write_cache(&state).await.unwrap();
        let instances = corp_table(GOLDEN_TRUSTED_ROOT);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let roots = resolve_policy_roots_with(
            &[
                keyless_on(&corp()),
                keyless_on(&SigstoreAuthority::public()),
                keyless_on(&corp()),
            ],
            Some(&instances),
            &no_home,
            &state,
            "rekor.example",
            false,
            counting_tuf(&calls),
        )
        .await
        .expect("both instances resolve");
        let mut named: Vec<_> = roots.iter().map(|resolved| resolved.root.instance()).collect();
        named.sort();
        assert_eq!(named, vec![corp(), SigstoreAuthority::public()]);
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the public cache entry answered"
        );
    }

    /// The public root of a keyless entry, fetched by a stand-in for TUF, counting its calls.
    fn counted_tuf(
        fetches: &std::sync::atomic::AtomicUsize,
    ) -> impl FnOnce() -> std::future::Ready<Result<TrustRoot, VerifyErrorKind>> + '_ {
        move || {
            fetches.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let rekor = std::collections::BTreeMap::from([("aa".to_owned(), vec![1u8, 2, 3])]);
            std::future::ready(Ok(TrustRoot::from_material(
                vec![vec![0x30, 0x00]],
                Default::default(),
                rekor,
            )))
        }
    }

    fn public_keyless_policy() -> CompiledPolicy {
        ocx_trust::parse_publishers(&serde_json::json!([
            { "kind": "keyless", "identity": "me", "oidc_issuer": "https://issuer.example" }
        ]))
        .compile()
        .expect("the entry compiles")
    }

    fn cached_public(state: &SigningStatePaths) -> Option<TrustRootCache> {
        futures_lite_block_on(TrustRootCache::from_cache("rekor.example", state))
    }

    fn futures_lite_block_on<T>(future: impl std::future::Future<Output = std::io::Result<Option<T>>>) -> Option<T> {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future)).unwrap()
    }

    /// The resolver reports the fetch and writes nothing; the writer is [`ResolvedRoot::cache_after_verdict`], run
    /// only after a verdict. A second process on the same state then finds the first one's fetch: one TUF fetch in all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_fetched_public_root_seeds_the_cache_only_through_a_verdict() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let policies = [public_keyless_policy()];
        let fetches = std::sync::atomic::AtomicUsize::new(0);

        let first = resolve_policy_roots_with(
            &policies,
            None,
            &|_| None,
            &state,
            "rekor.example",
            false,
            counted_tuf(&fetches),
        )
        .await
        .expect("the root resolves");
        assert!(first[0].fetched, "the root came from the fetch");
        assert!(
            cached_public(&state).is_none(),
            "resolving alone must cache nothing: no verdict has proven the root yet"
        );

        first[0].cache_after_verdict("rekor.example", &state).await;
        let cached = cached_public(&state).expect("the verdict seeded the cache");
        assert_eq!(cached.rekor_public_key_pem, first[0].root.single_rekor_key_pem());

        let second = resolve_policy_roots_with(
            &policies,
            None,
            &|_| None,
            &state,
            "rekor.example",
            false,
            counted_tuf(&fetches),
        )
        .await
        .expect("the root resolves from the cache");
        assert!(!second[0].fetched, "the cache answered");
        assert_eq!(
            fetches.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the second process must not fetch"
        );
    }

    /// A root read from the convention file is never fetched, so a verdict never seeds the cache with it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_locally_supplied_root_is_not_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let supplied = tmp.path().join("trusted-root.json");
        tokio::fs::write(&supplied, GOLDEN_TRUSTED_ROOT).await.unwrap();
        let fetches = std::sync::atomic::AtomicUsize::new(0);
        let roots = resolve_policy_roots_with(
            &[public_keyless_policy()],
            None,
            &|_| Some(supplied.clone()),
            &state,
            "rekor.example",
            false,
            counted_tuf(&fetches),
        )
        .await
        .expect("the convention-file root resolves");
        let [resolved] = roots.as_slice() else {
            panic!("one public root expected, got {roots:?}");
        };
        assert!(
            resolved.root.single_rekor_key_pem().is_some(),
            "the fixture pins a Rekor key, so only provenance can keep it out of the cache"
        );
        assert!(!resolved.fetched);
        resolved.cache_after_verdict("rekor.example", &state).await;
        assert_eq!(fetches.load(std::sync::atomic::Ordering::SeqCst), 0, "no TUF fetch");
        assert!(cached_public(&state).is_none(), "nothing was cached");
    }

    /// With two pinned Rekor keys the verify may have used either, so no key is recorded.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_fetched_root_with_two_rekor_keys_seeds_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SigningStatePaths::new(tmp.path().join("state"));
        let rekor = std::collections::BTreeMap::from([("aa".to_owned(), vec![1u8]), ("bb".to_owned(), vec![2u8])]);
        let resolved = ResolvedRoot {
            root: TrustRoot::from_material(vec![vec![0x30, 0x00]], Default::default(), rekor),
            fetched: true,
        };
        resolved.cache_after_verdict("rekor.example", &state).await;
        assert!(cached_public(&state).is_none());
    }
}
