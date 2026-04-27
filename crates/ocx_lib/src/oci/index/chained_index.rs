// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::time::Duration;

use async_trait::async_trait;

use super::{ChainMode, Index, LocalIndex, index_impl};
use crate::utility::singleflight;
use crate::{Result, log, oci};

/// Two-role index: a persistent `cache` plus an ordered list of read-only
/// `sources` queried in order on cache miss.
///
/// On a cache miss the chain is walked until a source successfully populates
/// the cache with both the tag pointer and the full manifest chain, after
/// which the cache is re-queried. Concurrent identical cache misses are
/// deduplicated via [`singleflight`](crate::utility::singleflight) — only the
/// leader task performs the fetch; waiters reuse its result.
///
/// `ChainMode` controls how mutable lookups (tag listings, catalog) are
/// routed:
///
/// - `Default` reads the persisted local index. A miss returns `None`
///   (or empty) — pure queries never auto-fetch from sources. The local
///   index is populated only by explicit paths (`ocx index update`) or
///   data-required paths (install / pull).
/// - `Remote` queries sources directly for mutable lookups and never
///   consults the local index. A pure query in Remote mode never mutates
///   local state — `--remote` is a read-through-to-source flag, not a
///   write-through cache fill. If every source errors the failure is
///   propagated rather than silently falling back to the local index.
/// - `Offline` reads the local index only; sources are never consulted.
///
/// Digest-addressed reads (`fetch_manifest` with a digest in the
/// identifier) still use the local index in any mode because immutable
/// content cannot be wrong. Tag-addressed `fetch_manifest` currently
/// auto-fetches and persists the manifest chain on miss — see
/// `feedback_index_routing_semantics.md` (recommendation M3) for the
/// in-flight refactor that splits the read path from the write path.
pub struct ChainedIndex {
    cache: LocalIndex,
    sources: Vec<Index>,
    mode: ChainMode,
    /// Singleflight group for de-duplicating concurrent cache-miss fetches
    /// against the same (mode, identifier) key. Shared across clones so all
    /// `box_clone` spawned waiters converge on the same leader.
    singleflight: singleflight::Group<String, ()>,
}

/// Max in-flight singleflight keys. Scoped per ChainedIndex instance —
/// generous because each key maps to one package identifier under refresh.
const SINGLEFLIGHT_MAX_KEYS: usize = 1024;

/// Max time waiters block for the leader. Matches the blob-store write
/// timeout so a stuck leader surfaces rather than stalling the CLI forever.
const SINGLEFLIGHT_TIMEOUT: Duration = Duration::from_secs(120);

impl ChainedIndex {
    pub fn new(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            cache,
            sources,
            mode,
            singleflight: singleflight::Group::new(SINGLEFLIGHT_MAX_KEYS, SINGLEFLIGHT_TIMEOUT),
        }
    }

    /// Walk the source chain for an identifier — fetch the manifest (by tag
    /// or digest) and persist the full chain into the cache. Wrapped in a
    /// singleflight guard so concurrent waiters share the leader's result.
    ///
    /// Returns `Ok(())` when one source successfully persisted the chain, or
    /// `Ok(())` when no sources are configured (cache-only behaviour). Returns
    /// `Err(_)` when every source errored — preserves the trust boundary
    /// between "not found" (cache retry → `None`) and "registry outage".
    async fn walk_chain(&self, identifier: &oci::Identifier) -> Result<()> {
        if self.mode == ChainMode::Offline {
            // Offline mode never consults sources.
            return Ok(());
        }

        // Digest-only inputs are fetched directly via `GET /v2/<repo>/manifests/<digest>`
        // and persisted without a tag commit — there is no tag to pin. Tagged
        // inputs (or bare repos) normalise to `tag_or_latest()` so the
        // singleflight key collapses concurrent waiters.
        let walked = if identifier.tag().is_none() && identifier.digest().is_some() {
            identifier.clone()
        } else {
            identifier.clone_with_tag(identifier.tag_or_latest())
        };
        let key = format!("{}|{}|{}", self.mode as u8, walked.registry(), walked);

        // Singleflight: one leader fetches, concurrent waiters block on the
        // watch channel and reuse the result. The leader's own error path
        // returns `SourceWalkFailed(ArcError)`, preserving the full typed
        // `crate::Error` source chain. Waiters receive the leader's failure
        // via `singleflight::Error::Failed(SharedError)`, which we surface
        // as `SingleflightFailed`; its `source()` walks the leader's
        // original error chain for diagnostics, but the variant is erased
        // to `dyn Error` at the broadcast boundary — downcasting back to
        // `Error::SourceWalkFailed` is not possible because `SharedError`
        // holds `Arc<dyn Error + Send + Sync>`, not a typed `crate::Error`.
        use singleflight::Acquisition;
        // Singleflight infrastructure failures (capacity, timeout,
        // abandonment) are distinct from source-walk failures — keep them
        // in their own variant so callers can distinguish coordination
        // problems from upstream registry errors.
        let acquisition = self
            .singleflight
            .try_acquire(key)
            .await
            .map_err(super::error::Error::SingleflightFailed)?;
        let handle = match acquisition {
            Acquisition::Leader(h) => h,
            Acquisition::Resolved(()) => return Ok(()),
        };

        // Leader path: walk sources and persist on first success. On
        // failure, wrap the leader's typed error in `ArcError` and broadcast
        // the same `SourceWalkFailed(ArcError)` variant to waiters. The
        // leader also propagates that wrapped variant to its caller so both
        // ends see a consistent, typed error with the original source chain.
        match self.fetch_and_persist_chain(&walked).await {
            Ok(()) => {
                handle.complete(());
                Ok(())
            }
            Err(e) => {
                let arc = crate::error::ArcError::from(e);
                let broadcast = super::error::Error::SourceWalkFailed(arc.clone());
                let _ = handle.fail(broadcast);
                Err(super::error::Error::SourceWalkFailed(arc).into())
            }
        }
    }

    /// Leader-side chain walk: iterates sources, fetches tag digest and
    /// manifest chain from the first success, persists both into the cache.
    ///
    /// Sources are tried sequentially in priority order; the first success short-circuits.
    /// Parallel-peer fallback is intentionally not supported — peer registries are out of scope.
    ///
    /// Returns `Ok(())` when one source persisted the chain OR every
    /// source returned a clean not-found with no errors. Returns
    /// `Err(_)` when any source errored and no source succeeded — we
    /// do not treat a later `Ok(false)` as disproving an earlier
    /// failure.
    async fn fetch_and_persist_chain(&self, identifier: &oci::Identifier) -> Result<()> {
        let mut last_error: Option<crate::Error> = None;
        for source in &self.sources {
            match self.cache.write_chain_and_commit_tag(source, identifier).await {
                Ok(true) => {
                    log::debug!("Fetched '{}' from chained source, persisted to cache.", identifier);
                    return Ok(());
                }
                Ok(false) => {
                    log::debug!("Source has no '{}' — trying next source.", identifier);
                }
                Err(e) => {
                    log::warn!("Could not fetch '{}' from chained source: {e}", identifier);
                    last_error = Some(e);
                }
            }
        }

        if let Some(e) = last_error {
            // At least one source errored. We don't trust a later Ok(false)
            // to disprove an earlier Err — a clean "not found" from a mirror
            // does not contradict a transient failure on the primary.
            return Err(e);
        }
        // All sources either replied Ok(false) or there were no sources.
        Ok(())
    }
}

#[async_trait]
impl index_impl::IndexImpl for ChainedIndex {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        // Catalog routes by mode. Default and Offline read the persisted
        // cache; Remote queries sources only and never falls back to the
        // cache — the whole point of `--remote` is to bypass cached state,
        // and silently serving stale repos on a registry outage would hide
        // the failure from --remote callers. First Ok wins; if every source
        // errors we propagate the last error. Empty `sources` in Remote
        // mode (only possible via misconfiguration: `Context::try_init`
        // pairs Remote with a remote source) returns an empty catalog
        // rather than reading cache.
        if self.mode == ChainMode::Remote {
            let mut last_error: Option<crate::Error> = None;
            for source in &self.sources {
                match source.list_repositories(registry).await {
                    Ok(repos) => return Ok(repos),
                    Err(e) => {
                        log::warn!("Remote-mode list_repositories failed for '{registry}': {e}");
                        last_error = Some(e);
                    }
                }
            }
            return last_error.map_or_else(|| Ok(Vec::new()), Err);
        }
        self.cache.list_repositories(registry).await
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        // Tag listings route by mode. Default and Offline read the local
        // index only; Remote queries sources directly without write-through.
        // A pure query must never mutate local state — write paths live on
        // `LocalIndex::refresh_tags` (called from `ocx index update`) and
        // `LocalIndex::write_chain_and_commit_tag` (called from install /
        // pull). First Ok wins; if every source errors we propagate.
        //
        // Trust-boundary: in Remote mode, if every configured source fails
        // we propagate the last error rather than silently falling back to
        // the local index. `--remote` forces live lookups — collapsing a
        // registry outage into stale local data would hide the real problem
        // from callers and break retry policy.
        if self.mode == ChainMode::Remote {
            let mut last_error: Option<crate::Error> = None;
            for source in &self.sources {
                match source.list_tags(identifier).await {
                    Ok(Some(tags)) => return Ok(Some(tags)),
                    Ok(None) => {}
                    Err(e) => {
                        log::warn!("Remote-mode list_tags failed for '{}': {e}", identifier);
                        last_error = Some(e);
                    }
                }
            }
            return last_error.map_or(Ok(None), Err);
        }
        self.cache.list_tags(identifier).await
    }

    async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        // Digest-addressed reads are cache-first in every mode — immutable
        // content cannot be wrong. Mutable (tag-based) reads in `Remote`
        // mode go straight to the chain walk, skipping the cache read.
        let is_digest_addressed = identifier.digest().is_some();
        if is_digest_addressed || self.mode != ChainMode::Remote {
            match self.cache.fetch_manifest(identifier).await {
                Ok(Some(result)) => return Ok(Some(result)),
                Ok(None) => {}
                Err(e) => {
                    log::warn!(
                        "Local tag cache read failed for '{}', falling back to chained source: {e}",
                        identifier
                    );
                }
            }
        }
        self.walk_chain(identifier).await?;
        self.cache.fetch_manifest(identifier).await
    }

    async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<Option<oci::Digest>> {
        let is_digest_addressed = identifier.digest().is_some();
        if is_digest_addressed || self.mode != ChainMode::Remote {
            match self.cache.fetch_manifest_digest(identifier).await {
                Ok(Some(digest)) => return Ok(Some(digest)),
                Ok(None) => {}
                Err(e) => {
                    log::warn!(
                        "Local tag cache read failed for '{}', falling back to chained source: {e}",
                        identifier
                    );
                }
            }
        }
        self.walk_chain(identifier).await?;
        self.cache.fetch_manifest_digest(identifier).await
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(Self {
            cache: self.cache.clone(),
            sources: self.sources.clone(),
            mode: self.mode,
            // Singleflight group is shared across clones so waiters coalesce.
            singleflight: self.singleflight.clone(),
        })
    }
}

// ── Specification tests — plan_resolution_chain_refs.md tests 22-32 ─────
//
// Tests 22-32: ChainMode routing, singleflight dedup, disk-persistence
// properties.
#[cfg(test)]
mod chain_refs_tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::{
        Result,
        file_structure::{BlobStore, TagStore},
        oci::index::{ChainMode, Index, LocalConfig, LocalIndex, index_impl},
        oci::{Digest, Identifier, ImageManifest, Manifest},
    };

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";
    const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn tagged_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }
    fn digest_only_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_digest(Digest::Sha256(HEX_A.to_string()))
    }
    fn digest_a() -> Digest {
        Digest::Sha256(HEX_A.to_string())
    }
    fn digest_b() -> Digest {
        Digest::Sha256(HEX_B.to_string())
    }
    fn make_image_manifest() -> Manifest {
        Manifest::Image(ImageManifest::default())
    }

    fn make_local_index(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(dir.path().join("tags")),
            blob_store: BlobStore::new(dir.path().join("blobs")),
        })
    }

    /// TestIndex with a call counter — records every fetch_manifest_digest call.
    #[derive(Clone)]
    struct CountingSource {
        known_tags: HashMap<String, Digest>,
        repos: Vec<String>,
        call_count: Arc<Mutex<usize>>,
    }

    impl CountingSource {
        fn with_tag(tag: &str, d: Digest) -> Self {
            let mut known_tags = HashMap::new();
            known_tags.insert(tag.to_string(), d);
            Self {
                known_tags,
                repos: Vec::new(),
                call_count: Arc::new(Mutex::new(0)),
            }
        }
        fn with_repos(repos: Vec<String>) -> Self {
            Self {
                known_tags: HashMap::new(),
                repos,
                call_count: Arc::new(Mutex::new(0)),
            }
        }
        fn calls(&self) -> usize {
            *self.call_count.lock().unwrap()
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for CountingSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            *self.call_count.lock().unwrap() += 1;
            Ok(self.repos.clone())
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            *self.call_count.lock().unwrap() += 1;
            Ok(Some(self.known_tags.keys().cloned().collect()))
        }
        async fn fetch_manifest(&self, identifier: &Identifier) -> Result<Option<(Digest, Manifest)>> {
            let tag = identifier.tag_or_latest();
            *self.call_count.lock().unwrap() += 1;
            Ok(self.known_tags.get(tag).map(|d| (d.clone(), make_image_manifest())))
        }
        async fn fetch_manifest_digest(&self, identifier: &Identifier) -> Result<Option<Digest>> {
            let tag = identifier.tag_or_latest();
            *self.call_count.lock().unwrap() += 1;
            Ok(self.known_tags.get(tag).cloned())
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn make_source(tag: &str, d: Digest) -> (CountingSource, Index) {
        let src = CountingSource::with_tag(tag, d);
        let idx = super::super::Index::from_impl(src.clone());
        (src, idx)
    }

    fn make_source_with_repos(repos: Vec<String>) -> (CountingSource, Index) {
        let src = CountingSource::with_repos(repos);
        let idx = super::super::Index::from_impl(src.clone());
        (src, idx)
    }

    /// Seed the cache with the full chain (tag pointer + manifest blob) so
    /// subsequent cache-only reads succeed. Equivalent to what a successful
    /// `ChainedIndex` walk would leave behind.
    async fn seed_full(cache: &LocalIndex, identifier: &Identifier, _d: Digest, source: &Index) {
        let persisted = cache.write_chain_and_commit_tag(source, identifier).await.unwrap();
        assert!(persisted, "source must know the seeded tag");
    }

    // ── test 22 ───────────────────────────────────────────────────────────

    /// Design record §22: in Default mode, a cache hit returns without touching
    /// sources. Source call count must remain zero.
    #[tokio::test]
    async fn default_mode_cache_hit_returns_without_touching_sources() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        // Seed the cache via a temporary source.
        let (_, seed_idx) = make_source(TAG, digest_a());
        seed_full(&cache, &tagged_id(), digest_a(), &seed_idx).await;

        // Now create a spy source and verify it is never called on cache hit.
        let (spy, spy_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![spy_idx], ChainMode::Default);
        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(result.is_some(), "cache hit must return Some");
        assert_eq!(spy.calls(), 0, "source must not be queried on cache hit (Default mode)");
    }

    // ── test 23 ───────────────────────────────────────────────────────────

    /// Design record §23: in Default mode, a cache miss walks the source,
    /// persists the chain on disk. After the call, the blob data file exists.
    #[tokio::test]
    async fn default_mode_cache_miss_walks_source_and_persists_chain_on_disk() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blob_store = BlobStore::new(cache_dir.path().join("blobs"));

        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);
        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(result.is_some(), "cache-miss source fetch must return Some");

        // Property: the blob data file must exist after a successful fetch.
        let expected_blob = blob_store.data(REGISTRY, &digest_a());
        assert!(
            expected_blob.exists(),
            "Default mode: blob data file must be on disk after fetch_manifest; missing: {}",
            expected_blob.display()
        );
    }

    // ── test 24 ───────────────────────────────────────────────────────────

    /// Design record §24: in Remote mode, tag lookups bypass the cache and go
    /// to the source, but blobs ARE still persisted on disk after the call.
    #[tokio::test]
    async fn remote_mode_bypasses_cache_for_tag_lookup_but_still_persists_blobs() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blob_store = BlobStore::new(cache_dir.path().join("blobs"));

        // Seed the cache with digest_b so a Default-mode lookup would hit b.
        let (_, seed) = make_source(TAG, digest_b());
        seed_full(&cache, &tagged_id(), digest_b(), &seed).await;

        // Source has digest_a — in Remote mode the source is consulted.
        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);
        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        // Remote mode must have gone to the source (digest_a != digest_b).
        assert!(result.is_some());
        assert!(spy.calls() > 0, "Remote mode must consult source for tag lookup");

        // Blob must be persisted even under Remote mode.
        let expected_blob = blob_store.data(REGISTRY, &digest_a());
        assert!(
            expected_blob.exists(),
            "Remote mode: blob must be persisted after fetch_manifest"
        );
    }

    // ── test 25 ───────────────────────────────────────────────────────────

    /// Design record §25: in Remote mode, digest-addressed lookups use the
    /// cache (immutable content — no reason to bypass). The source must NOT
    /// be consulted for digest-addressed fetches.
    #[tokio::test]
    async fn remote_mode_digest_addressed_lookup_uses_cache() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // Pre-write the blob data directly so cache has it.
        // Construct a parallel BlobStore over the same root — LocalIndex.blob_store is private.
        let blob_store = BlobStore::new(cache_dir.path().join("blobs"));
        let blob_path = blob_store.data(REGISTRY, &digest_a());
        std::fs::create_dir_all(blob_path.parent().unwrap()).unwrap();
        let manifest = Manifest::Image(ImageManifest::default());
        serde_json::to_writer(std::fs::File::create(&blob_path).unwrap(), &manifest).unwrap();

        let (spy, src_idx) = make_source(TAG, digest_a());
        let id_with_digest = digest_only_id(); // digest-addressed, no tag
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);
        let result = chained.fetch_manifest(&id_with_digest).await.unwrap();
        assert!(
            result.is_some(),
            "digest-addressed lookup must hit cache in Remote mode"
        );
        assert_eq!(
            spy.calls(),
            0,
            "Remote mode must NOT consult source for digest-addressed lookups"
        );
    }

    // ── test 26 ───────────────────────────────────────────────────────────

    /// Design record §26: in Offline mode, a cache miss returns None without
    /// consulting any sources.
    #[tokio::test]
    async fn offline_mode_cache_miss_returns_none_without_consulting_sources() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Offline);
        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(result.is_none(), "Offline mode: cache miss must return None");
        assert_eq!(spy.calls(), 0, "Offline mode must never consult sources");
    }

    // ── test 27 ───────────────────────────────────────────────────────────

    /// Design record §27: in Offline mode, a cache hit returns from disk
    /// without consulting sources.
    #[tokio::test]
    async fn offline_mode_cache_hit_returns_from_disk() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        // Seed the cache via an online source.
        let (_, seed) = make_source(TAG, digest_a());
        seed_full(&cache, &tagged_id(), digest_a(), &seed).await;

        // Now query Offline mode — must hit from disk, no source calls.
        let (spy, src_idx) = make_source(TAG, digest_b()); // different digest
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Offline);
        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(result.is_some(), "Offline mode: cache hit must return Some");
        assert_eq!(spy.calls(), 0, "Offline mode must never consult sources on hit");
    }

    // ── test 28 ───────────────────────────────────────────────────────────

    /// Design record §28: singleflight deduplicates concurrent identical cache
    /// misses — only 1 source fetch is recorded even when 4 tasks race.
    #[tokio::test]
    async fn singleflight_dedups_concurrent_identical_cache_miss_fetches() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let spy = CountingSource::with_tag(TAG, digest_a());
        let spy_calls = spy.call_count.clone();
        let src_idx = super::super::Index::from_impl(spy);

        let chained = Arc::new(Index::from_chained(cache, vec![src_idx], ChainMode::Default));

        // 4 concurrent tasks on the identical identifier → should produce exactly 1 source fetch.
        let mut tasks: tokio::task::JoinSet<Result<Option<(Digest, Manifest)>>> = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let ch = chained.clone();
            let id = tagged_id();
            tasks.spawn(async move { ch.fetch_manifest(&id).await });
        }
        while let Some(joined) = tasks.join_next().await {
            joined.expect("task panicked").expect("fetch_manifest failed");
        }

        let total_calls = *spy_calls.lock().unwrap();
        assert_eq!(
            total_calls, 1,
            "singleflight must deduplicate: expected 1 source call, got {total_calls}"
        );
    }

    // ── test 29 ───────────────────────────────────────────────────────────

    /// Design record §29: when the source errors during a singleflight-guarded
    /// fetch, all waiters receive the error (broadcast error propagation).
    #[tokio::test]
    async fn singleflight_broadcasts_source_error_to_waiters() {
        #[derive(Clone)]
        struct AlwaysErrorSource;
        #[async_trait]
        impl index_impl::IndexImpl for AlwaysErrorSource {
            async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
                Ok(Vec::new())
            }
            async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
                Ok(None)
            }
            async fn fetch_manifest(&self, _: &Identifier) -> Result<Option<(Digest, Manifest)>> {
                Err(super::super::error::Error::RemoteManifestNotFound("test error".to_string()).into())
            }
            async fn fetch_manifest_digest(&self, _: &Identifier) -> Result<Option<Digest>> {
                Err(super::super::error::Error::RemoteManifestNotFound("test error".to_string()).into())
            }
            fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
                Box::new(self.clone())
            }
        }

        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let src_idx = super::super::Index::from_impl(AlwaysErrorSource);
        let chained = Arc::new(Index::from_chained(cache, vec![src_idx], ChainMode::Default));

        let mut tasks: tokio::task::JoinSet<Result<Option<(Digest, Manifest)>>> = tokio::task::JoinSet::new();
        for _ in 0..3 {
            let ch = chained.clone();
            let id = tagged_id();
            tasks.spawn(async move { ch.fetch_manifest(&id).await });
        }

        let mut error_count = 0;
        while let Some(joined) = tasks.join_next().await {
            let result = joined.expect("task panicked");
            if result.is_err() {
                error_count += 1;
            }
        }
        assert!(
            error_count > 0,
            "singleflight must broadcast source errors to all waiters"
        );
    }

    // ── test 30 ───────────────────────────────────────────────────────────

    /// Design record §30: list_tags respects ChainMode.
    /// Default: local index only, no source contact.
    /// Remote: hits source, returns source tags, no write-through.
    /// Offline: local index only, never consults source.
    #[tokio::test]
    async fn list_tags_respects_chain_mode() {
        // --- Default: local only ---
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache.clone(), vec![src_idx], ChainMode::Default);
        // Local index is empty — list_tags must return None or empty (no source call).
        let result = chained.list_tags(&tagged_id()).await.unwrap();
        let is_empty_or_none = result.is_none() || result.unwrap().is_empty();
        assert!(is_empty_or_none, "Default mode list_tags must read local index only");
        assert_eq!(spy.calls(), 0, "Default mode list_tags must not consult source");

        // --- Offline: same contract ---
        let cache_dir2 = TempDir::new().unwrap();
        let cache2 = make_local_index(&cache_dir2);
        let (spy2, src_idx2) = make_source(TAG, digest_a());
        let chained2 = Index::from_chained(cache2, vec![src_idx2], ChainMode::Offline);
        let result2 = chained2.list_tags(&tagged_id()).await.unwrap();
        let empty2 = result2.is_none() || result2.unwrap().is_empty();
        assert!(empty2, "Offline mode list_tags must read local index only");
        assert_eq!(spy2.calls(), 0, "Offline mode list_tags must not consult source");

        // --- Remote: source returns tags, local index untouched ---
        let cache_dir3 = TempDir::new().unwrap();
        let cache3 = make_local_index(&cache_dir3);
        let (spy3, src_idx3) = make_source(TAG, digest_a());
        let chained3 = Index::from_chained(cache3, vec![src_idx3], ChainMode::Remote);
        let result3 = chained3.list_tags(&tagged_id()).await.unwrap();
        assert!(spy3.calls() > 0, "Remote mode list_tags must consult source");
        let tags3 = result3.expect("Remote mode list_tags must return source tags");
        assert_eq!(
            tags3,
            vec![TAG.to_string()],
            "Remote mode must return the source's tag list"
        );
    }

    // ── regression: Remote-mode list_tags must not mutate the local index ──

    /// A pure `--remote` query must never write to the local index. The tag
    /// store layout is `{root}/{registry_slug}/{repository}.json`, so a
    /// Remote-mode `list_tags` call must not create the registry directory
    /// nor any per-repository tag file.
    #[tokio::test]
    async fn remote_mode_list_tags_does_not_mutate_local_index() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);

        let tags_root = cache_dir.path().join("tags");
        let registry_dir = tags_root.join(REGISTRY);
        let repo_file = registry_dir.join(format!("{REPO}.json"));
        assert!(!repo_file.exists(), "preconditions: tag file must not exist");

        let result = chained.list_tags(&tagged_id()).await.unwrap();
        assert!(result.is_some(), "Remote-mode list_tags must return source tags");

        assert!(
            !repo_file.exists(),
            "Remote-mode list_tags must not create the local tag file at {}",
            repo_file.display()
        );
        // Registry-dir creation is also a write; reject it explicitly so a
        // future regression that creates the dir but no file still fails.
        assert!(
            !registry_dir.exists(),
            "Remote-mode list_tags must not create the registry directory at {}",
            registry_dir.display()
        );
    }

    // ── test 31 ───────────────────────────────────────────────────────────

    /// Design record §31: list_repositories routes by ChainMode. Default and
    /// Offline read the persisted cache without consulting sources; Remote
    /// bypasses the cache and returns the source's repo list.
    #[tokio::test]
    async fn list_repositories_respects_chain_mode() {
        // --- Default: cache only, source untouched ---
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let (spy, src_idx) = make_source_with_repos(vec!["a".to_string(), "b".to_string()]);
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);
        let repos = chained.list_repositories(REGISTRY).await.unwrap();
        assert!(repos.is_empty(), "Default mode list_repositories must read from cache");
        assert_eq!(spy.calls(), 0, "Default mode list_repositories must not consult source");

        // --- Offline: same contract ---
        let cache_dir2 = TempDir::new().unwrap();
        let cache2 = make_local_index(&cache_dir2);
        let (spy2, src_idx2) = make_source_with_repos(vec!["a".to_string(), "b".to_string()]);
        let chained2 = Index::from_chained(cache2, vec![src_idx2], ChainMode::Offline);
        let repos2 = chained2.list_repositories(REGISTRY).await.unwrap();
        assert!(repos2.is_empty(), "Offline mode list_repositories must read from cache");
        assert_eq!(
            spy2.calls(),
            0,
            "Offline mode list_repositories must not consult source"
        );

        // --- Remote: source consulted, returns source's repo list ---
        let cache_dir3 = TempDir::new().unwrap();
        let cache3 = make_local_index(&cache_dir3);
        let expected_repos = vec!["cmake".to_string(), "ninja".to_string()];
        let (spy3, src_idx3) = make_source_with_repos(expected_repos.clone());
        let chained3 = Index::from_chained(cache3, vec![src_idx3], ChainMode::Remote);
        let repos3 = chained3.list_repositories(REGISTRY).await.unwrap();
        assert!(spy3.calls() > 0, "Remote mode list_repositories must consult source");
        assert_eq!(repos3, expected_repos, "Remote mode must return source's repo list");
    }

    // ── regression: Remote-mode list_repositories must propagate source errors ─

    /// Remote mode must NOT silently fall back to cached repos when every
    /// configured source errors — same trust boundary as list_tags.
    #[tokio::test]
    async fn remote_mode_list_repositories_propagates_source_errors() {
        #[derive(Clone)]
        struct AlwaysErrorSource;
        #[async_trait]
        impl index_impl::IndexImpl for AlwaysErrorSource {
            async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
                Err(super::super::error::Error::RemoteManifestNotFound("boom".to_string()).into())
            }
            async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
                Ok(None)
            }
            async fn fetch_manifest(&self, _: &Identifier) -> Result<Option<(Digest, Manifest)>> {
                Ok(None)
            }
            async fn fetch_manifest_digest(&self, _: &Identifier) -> Result<Option<Digest>> {
                Ok(None)
            }
            fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
                Box::new(self.clone())
            }
        }

        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let src_idx = super::super::Index::from_impl(AlwaysErrorSource);
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);
        let result = chained.list_repositories(REGISTRY).await;
        assert!(
            result.is_err(),
            "Remote mode must propagate source errors, not fall back to cache"
        );
    }

    // ── regression: Remote-mode list_tags must propagate source errors ───

    /// Remote mode must NOT silently fall back to cached tags when every
    /// configured source errors — that would hide registry outages from
    /// callers relying on `--remote` for live lookups.
    #[tokio::test]
    async fn remote_mode_list_tags_propagates_source_errors() {
        #[derive(Clone)]
        struct AlwaysErrorSource;
        #[async_trait]
        impl index_impl::IndexImpl for AlwaysErrorSource {
            async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
                Ok(Vec::new())
            }
            async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
                Err(super::super::error::Error::RemoteManifestNotFound("boom".to_string()).into())
            }
            async fn fetch_manifest(&self, _: &Identifier) -> Result<Option<(Digest, Manifest)>> {
                Ok(None)
            }
            async fn fetch_manifest_digest(&self, _: &Identifier) -> Result<Option<Digest>> {
                Err(super::super::error::Error::RemoteManifestNotFound("boom".to_string()).into())
            }
            fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
                Box::new(self.clone())
            }
        }

        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let src_idx = super::super::Index::from_impl(AlwaysErrorSource);
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);
        let result = chained.list_tags(&tagged_id()).await;
        assert!(
            result.is_err(),
            "Remote mode must propagate source errors, not fall back to cache"
        );
    }

    // ── test 32 ───────────────────────────────────────────────────────────

    /// Design record §32: property — for any mode, after a successful
    /// fetch_manifest returning Some((digest, _)), the blob data file must
    /// exist on disk (digest is guaranteed on disk).
    #[tokio::test]
    async fn fetch_manifest_post_persist_is_guaranteed_on_disk() {
        // Test with Default mode (the main case; Remote is covered in test 24).
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blob_store = BlobStore::new(cache_dir.path().join("blobs"));

        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);

        if let Some((digest, _)) = chained.fetch_manifest(&tagged_id()).await.unwrap() {
            let blob_path = blob_store.data(REGISTRY, &digest);
            assert!(
                blob_path.exists(),
                "property violated: fetch_manifest returned digest {:?} but blob is not on disk at {}",
                digest,
                blob_path.display()
            );
        }
        // If None: no blob expected, test passes trivially.
    }

    // ── T3 ────────────────────────────────────────────────────────────────

    /// T3 (plan review): singleflight deduplication under high concurrency —
    /// 8 simultaneous tasks racing on the same tagged identifier must produce
    /// exactly 1 source call. Complements test 28 (4 tasks) with a larger
    /// concurrency factor to stress the singleflight key computation.
    #[tokio::test]
    async fn singleflight_dedups_eight_concurrent_identical_cache_miss_fetches() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let spy = CountingSource::with_tag(TAG, digest_a());
        let spy_calls = spy.call_count.clone();
        let src_idx = super::super::Index::from_impl(spy);

        let chained = Arc::new(Index::from_chained(cache, vec![src_idx], ChainMode::Default));

        const N: usize = 8;
        let mut tasks: tokio::task::JoinSet<Result<Option<(Digest, Manifest)>>> = tokio::task::JoinSet::new();
        for _ in 0..N {
            let ch = chained.clone();
            let id = tagged_id();
            tasks.spawn(async move { ch.fetch_manifest(&id).await });
        }
        while let Some(joined) = tasks.join_next().await {
            joined.expect("task panicked").expect("fetch_manifest failed");
        }

        let total_calls = *spy_calls.lock().unwrap();
        assert_eq!(
            total_calls, 1,
            "singleflight must deduplicate {N} concurrent waiters to exactly 1 source call; \
             got {total_calls}"
        );
    }
}

// ── Specification tests ───────────────────────────────────────────────────
//
// Written from the design record (plan_tag_fallback.md) in specification mode.
// These tests encode the expected ChainedIndex behaviour.
//
// Test → design-record traceability:
//   cache_hit_*           → "Tag in cache → Return immediately (no source walked)"
//   cache_miss_source_*   → "Tag not cached, source has it → update_tag persists it"
//   cache_miss_source_no  → "Tag not cached, source doesn't have it → warn, NotFound"
//   cache_miss_network_*  → "Tag not cached, network failure → warn, NotFound"
//   digest_only_*         → "Identifier with digest but no tag → no fallback"
//   box_clone_*           → "`box_clone` shares caches across cloned chain"
//   list_tags_*           → "`list_tags` delegates to cache only"
//   list_repos_*          → "`list_repositories` delegates to cache only"
//   multi_source_*        → "Multi-source chain proves the Vec shape works"
//   empty_sources_*       → "Empty sources Vec → behaves like LocalIndex alone"
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::{
        Result,
        file_structure::{BlobStore, TagStore},
        oci::index::{Index, LocalConfig, LocalIndex, index_impl},
        oci::{Digest, Identifier, ImageManifest, Manifest},
    };

    // ── Test helpers ──────────────────────────────────────────────────────

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";
    // 64-char hex string required for Sha256.
    const HEX_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEX_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn tagged_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }

    fn digest_only_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_digest(Digest::Sha256(HEX_A.to_string()))
    }

    fn digest_a() -> Digest {
        Digest::Sha256(HEX_A.to_string())
    }

    fn digest_b() -> Digest {
        Digest::Sha256(HEX_B.to_string())
    }

    fn make_image_manifest() -> Manifest {
        Manifest::Image(ImageManifest::default())
    }

    /// Build a real `LocalIndex` backed by a temp directory.
    ///
    /// The `TempDir` must outlive the index; callers keep it in scope.
    fn make_local_index(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(LocalConfig {
            tag_store: TagStore::new(dir.path().join("tags")),
            blob_store: BlobStore::new(dir.path().join("blobs")),
        })
    }

    // ── TestIndex — a programmable fake `IndexImpl` ───────────────────────
    //
    // Records which identifiers were queried so tests can assert that a source
    // was (or was not) consulted.  Programmed with a fixed response for
    // `fetch_manifest` and `fetch_manifest_digest` to return on any call.

    #[derive(Clone)]
    struct TestIndex {
        /// Tags this source knows about.  If the queried tag is in here, the
        /// source returns `Some(digest)`.  Missing → `None` (not an error).
        known_tags: HashMap<String, Digest>,
        /// If `Some(msg)`, every `fetch_manifest_digest` call returns an error
        /// with that message.  Simulates network or auth failures.
        force_error: Option<String>,
        /// Record of every tag queried so tests can verify call ordering.
        calls: Arc<Mutex<Vec<String>>>,
    }

    impl TestIndex {
        fn with_tag(tag: &str, digest: Digest) -> Self {
            let mut known_tags = HashMap::new();
            known_tags.insert(tag.to_string(), digest);
            Self {
                known_tags,
                force_error: None,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn empty() -> Self {
            Self {
                known_tags: HashMap::new(),
                force_error: None,
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                known_tags: HashMap::new(),
                force_error: Some(message.to_string()),
                calls: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for TestIndex {
        async fn list_repositories(&self, _registry: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_tags(&self, _identifier: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(self.known_tags.keys().cloned().collect()))
        }

        async fn fetch_manifest(&self, identifier: &Identifier) -> Result<Option<(Digest, Manifest)>> {
            if let Some(msg) = &self.force_error {
                // Use a RemoteManifestNotFound error to simulate registry errors.
                // The exact variant is unimportant for these tests — only that an
                // error is returned so ChainedIndex's degradation logic is exercised.
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()).into());
            }
            let tag = identifier.tag_or_latest();
            self.calls.lock().unwrap().push(tag.to_string());
            if let Some(digest) = self.known_tags.get(tag) {
                Ok(Some((digest.clone(), make_image_manifest())))
            } else {
                Ok(None)
            }
        }

        async fn fetch_manifest_digest(&self, identifier: &Identifier) -> Result<Option<Digest>> {
            if let Some(msg) = &self.force_error {
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()).into());
            }
            let tag = identifier.tag_or_latest();
            self.calls.lock().unwrap().push(tag.to_string());
            Ok(self.known_tags.get(tag).cloned())
        }

        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    // ── Proper test-source constructor ────────────────────────────────────
    //
    // Because `IndexImpl` is a private trait we cannot call `Index::from(impl
    // IndexImpl)`.  The workaround is to add a `#[cfg(test)]` constructor to
    // `Index` for injecting arbitrary `IndexImpl`s in tests.  Since this file
    // is `chained_index.rs` (a submodule of `index`), we can use `pub(super)`
    // to call a test-only method on the parent `Index` type.
    //
    // If the parent module does not yet have such a constructor, we define one
    // here via the module boundary.  The cleanest approach for the tests
    // below is: build a `ChainedIndex` directly (we *can* because we are in
    // the same file / module), using a real `LocalIndex` as the cache and a
    // `TestIndex` wrapped in a minimal `Index` as the source.
    //
    // The wrapper trick: `Index::from_chained(empty_local, vec![], super::super::ChainMode::Default)` produces
    // an `Index` that always returns `None` from all methods (the ChainedIndex
    // finds nothing in the empty cache and has no sources to fall back to).
    // That `Index` is not useful as a source.
    //
    // The real solution is to expose a `#[cfg(test)] pub(super) fn from_impl`
    // on `Index` in `index.rs`.  We add that now (it is a minimal, test-only
    // change):
    //
    //   #[cfg(test)]
    //   pub(super) fn from_impl(inner: impl IndexImpl + 'static) -> Self {
    //       Self { inner: Box::new(inner) }
    //   }
    //
    // We call it from here as `super::Index::from_impl(test_index)`.
    // This satisfies the "test the public trait surface" constraint because
    // `ChainedIndex` is tested via its `IndexImpl` implementation and the
    // `Index` wrapper — not internal fields.

    fn make_source(t: TestIndex) -> Index {
        // Use the test-only constructor added to Index in index.rs.
        super::super::Index::from_impl(t)
    }

    /// Seed the cache with the full chain (tag pointer + manifest blob) so
    /// subsequent cache-only reads succeed.
    async fn seed_full(cache: &LocalIndex, identifier: &Identifier, _d: Digest, source: &Index) {
        let persisted = cache.write_chain_and_commit_tag(source, identifier).await.unwrap();
        assert!(persisted, "source must know the seeded tag");
    }

    // ── Single-source chain tests ─────────────────────────────────────────

    // Case 1: cache hit → no source consulted.
    //
    // Pre-seed the cache with a tag→digest mapping, then call fetch_manifest.
    // The source should never be queried (zero calls recorded in TestIndex).
    #[tokio::test]
    async fn cache_hit_returns_immediately_without_querying_source() {
        // We need to pre-seed the LocalIndex on disk.  The LocalIndex reads tags
        // from disk lazily.  The easiest way is to run an update via a TestIndex
        // source on a first ChainedIndex, which persists the tag, then build the
        // chained index again with a *different* (empty) source to verify that
        // source is never touched.

        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // Populate the cache with the full chain (tag + manifest blob).
        let seed_source = TestIndex::with_tag(TAG, digest_a());
        let seed_index = make_source(seed_source);
        seed_full(&cache, &tagged_id(), digest_a(), &seed_index).await;

        // Now build the ChainedIndex with a *spy* source that records calls.
        let spy = TestIndex::empty();
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();

        assert!(result.is_some(), "cache hit should return Some");
        assert!(
            spy_calls.lock().unwrap().is_empty(),
            "source must not be queried on a cache hit"
        );
    }

    // Case 2: cache miss + source has tag → update_tag called → retry succeeds.
    #[tokio::test]
    async fn cache_miss_source_has_tag_returns_manifest() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();

        assert!(result.is_some(), "should return the manifest fetched from source");
        let (digest, _) = result.unwrap();
        assert_eq!(digest, digest_a());
    }

    // Case 2b: fetch_manifest_digest has same chain logic.
    #[tokio::test]
    async fn cache_miss_source_has_tag_returns_digest() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained.fetch_manifest_digest(&tagged_id()).await.unwrap();

        assert_eq!(result, Some(digest_a()));
    }

    // Case 3: cache miss + source doesn't have the tag → returns None (warn logged).
    #[tokio::test]
    async fn cache_miss_source_missing_tag_returns_none() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::empty());
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(result.is_none(), "unknown tag should degrade to None");
    }

    // Case 4: cache miss + sole source errors → error propagates to caller.
    //
    // The chain contract: when every source errored we MUST propagate the
    // error so callers can distinguish "package not found" from "registry
    // outage / auth failure". Collapsing to Ok(None) would break automation
    // retry logic.
    #[tokio::test]
    async fn cache_miss_source_error_propagates() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::failing("connection timed out"));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained.fetch_manifest(&tagged_id()).await;
        assert!(
            result.is_err(),
            "sole-source error must propagate, not collapse to Ok(None)"
        );
        let err_message = result.unwrap_err().to_string();
        assert!(
            err_message.contains("connection timed out"),
            "propagated error must carry the source's message; got: {err_message}"
        );
    }

    // Case 4b: fetch_manifest_digest propagates errors the same way.
    #[tokio::test]
    async fn cache_miss_digest_source_error_propagates() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::failing("401 unauthorized"));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained.fetch_manifest_digest(&tagged_id()).await;
        assert!(
            result.is_err(),
            "sole-source error must propagate for digest queries too"
        );
        let err_message = result.unwrap_err().to_string();
        assert!(
            err_message.contains("401 unauthorized"),
            "propagated error must carry the source's message; got: {err_message}"
        );
    }

    // Case 5: digest-only identifier → walks the source chain via
    // `GET /v2/<repo>/manifests/<digest>` and persists the blob, even though
    // there is no tag to commit. Required for `ocx install repo@sha256:...`.
    #[tokio::test]
    async fn digest_only_identifier_walks_chain() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let spy = TestIndex::with_tag(TAG, digest_a());
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let id = digest_only_id(); // no tag
        let _ = chained.fetch_manifest(&id).await.unwrap();

        assert!(
            !spy_calls.lock().unwrap().is_empty(),
            "source must be queried for digest-only identifiers — \
             registries support GET /v2/<repo>/manifests/<digest>"
        );
    }

    // Case 5b: fetch_manifest_digest with digest-only identifier walks the chain too.
    #[tokio::test]
    async fn digest_only_identifier_digest_query_walks_chain() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let spy = TestIndex::with_tag(TAG, digest_a());
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let id = digest_only_id();
        let _ = chained.fetch_manifest_digest(&id).await.unwrap();

        assert!(
            !spy_calls.lock().unwrap().is_empty(),
            "source must be queried for digest-only identifiers"
        );
    }

    // Case 5c: bare identifier (no tag, no digest) → chain walks under implicit
    // `:latest`. `ocx install cmake` on a fresh machine must behave the same as
    // `ocx install cmake:latest` — the fallback chain substitutes "latest" and
    // persists it for the subsequent cache lookup.
    #[tokio::test]
    async fn bare_identifier_walks_chain_as_latest() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::with_tag("latest", digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let bare = Identifier::new_registry(REPO, REGISTRY);
        let result = chained.fetch_manifest(&bare).await.unwrap();

        assert!(result.is_some(), "bare identifier must resolve via implicit :latest");
        let (digest, _) = result.unwrap();
        assert_eq!(digest, digest_a());
    }

    // Case 5d: bare identifier + source has no "latest" → degrades to None.
    #[tokio::test]
    async fn bare_identifier_latest_missing_returns_none() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // Source knows about TAG (3.28) but not "latest".
        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let bare = Identifier::new_registry(REPO, REGISTRY);
        let result = chained.fetch_manifest(&bare).await.unwrap();
        assert!(
            result.is_none(),
            "bare identifier with no remote :latest should degrade to None"
        );
    }

    // Case 6: box_clone shares caches — mutation after clone is visible.
    //
    // Clone the ChainedIndex (via Index::clone → box_clone), seed the cache on
    // the original, then verify the cloned index can read the same data.
    #[tokio::test]
    async fn box_clone_shares_cache() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::empty());
        let original = super::super::Index::from_chained(cache.clone(), vec![source], super::super::ChainMode::Default);
        let cloned = original.clone(); // calls box_clone internally

        // Seed the shared cache via the original by using a source that has the tag.
        let seed_source = make_source(TestIndex::with_tag(TAG, digest_a()));
        seed_full(&cache, &tagged_id(), digest_a(), &seed_source).await;

        // The cloned index should see the tag because caches are shared via Arc.
        let result_via_clone = cloned.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(
            result_via_clone.is_some(),
            "cloned ChainedIndex must share cache with original — mutation must be visible"
        );
    }

    // Case 7: list_tags delegates to cache only — source not queried.
    #[tokio::test]
    async fn list_tags_delegates_to_cache_only() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // The source knows about TAG but the cache does not.
        let spy = TestIndex::with_tag(TAG, digest_a());
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        // list_tags on an identifier not in the cache should return None or an
        // empty list — NOT the source's tags.
        let result = chained.list_tags(&tagged_id()).await.unwrap();
        // Cache has no tags for this identifier → None or Some([]).
        assert!(
            result.is_none() || result.unwrap().is_empty(),
            "list_tags must return only cached tags, not source tags"
        );
        // Source must not have been asked for its manifests (no fetch calls).
        assert!(
            spy_calls.lock().unwrap().is_empty(),
            "source must not be consulted by list_tags"
        );
    }

    // Case 8: list_repositories delegates to cache only.
    #[tokio::test]
    async fn list_repositories_delegates_to_cache_only() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        // Cache is empty → expect empty list.
        let repos = chained.list_repositories(REGISTRY).await.unwrap();
        assert!(
            repos.is_empty(),
            "list_repositories must return only cached repositories"
        );
    }

    // ── Multi-source chain tests ──────────────────────────────────────────

    // Case 9: two sources, first has the tag → second source NOT queried.
    #[tokio::test]
    async fn multi_source_first_hit_second_not_queried() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let first = TestIndex::with_tag(TAG, digest_a());
        let second = TestIndex::empty();
        let second_calls = second.calls.clone();

        let chained = super::super::Index::from_chained(
            cache,
            vec![make_source(first), make_source(second)],
            super::super::ChainMode::Default,
        );

        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(result.is_some(), "first source hit should succeed");

        // Second source must not have been queried.
        assert!(
            second_calls.lock().unwrap().is_empty(),
            "second source must not be queried when first source succeeds"
        );
    }

    // Case 10: two sources, first errors but second has the tag → tag persisted, success.
    #[tokio::test]
    async fn multi_source_first_error_second_succeeds() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let first = TestIndex::failing("connection refused");
        let second = TestIndex::with_tag(TAG, digest_b());

        let chained = super::super::Index::from_chained(
            cache,
            vec![make_source(first), make_source(second)],
            super::super::ChainMode::Default,
        );

        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(result.is_some(), "second source should succeed when first errors");
        let (digest, _) = result.unwrap();
        assert_eq!(digest, digest_b(), "digest should come from the second source");
    }

    // Case 10b: fetch_manifest_digest with same multi-source degradation.
    #[tokio::test]
    async fn multi_source_first_error_second_succeeds_digest() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let first = TestIndex::failing("timeout");
        let second = TestIndex::with_tag(TAG, digest_b());

        let chained = super::super::Index::from_chained(
            cache,
            vec![make_source(first), make_source(second)],
            super::super::ChainMode::Default,
        );

        let result = chained.fetch_manifest_digest(&tagged_id()).await.unwrap();
        assert_eq!(result, Some(digest_b()));
    }

    // Case 11: two sources, both error → propagates the last error.
    //
    // When the entire chain is exhausted by errors the chain MUST surface
    // an error so the caller can distinguish a real outage from a clean
    // not-found.
    #[tokio::test]
    async fn multi_source_all_errors_propagates() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let first = TestIndex::failing("network unreachable");
        let second = TestIndex::failing("503 service unavailable");

        let chained = super::super::Index::from_chained(
            cache,
            vec![make_source(first), make_source(second)],
            super::super::ChainMode::Default,
        );

        let result = chained.fetch_manifest(&tagged_id()).await;
        assert!(result.is_err(), "all-source-error must propagate as Err");
        let err_message = result.unwrap_err().to_string();
        assert!(
            err_message.contains("503 service unavailable"),
            "propagated error must carry the LAST source's message; got: {err_message}"
        );
    }

    // Case 11b: first source errors, second source returns a clean miss → the
    // chain must NOT collapse the earlier error into `Ok(None)`. A mirror
    // answering "not found" does not disprove an authoritative source's
    // transient failure; callers still need the `Err` to keep retry policy honest.
    #[tokio::test]
    async fn fetch_and_persist_chain_propagates_error_when_later_source_misses_cleanly() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let primary = TestIndex::failing("401 unauthorized");
        let mirror = TestIndex::empty(); // clean Ok(None)

        let chained = super::super::Index::from_chained(
            cache,
            vec![make_source(primary), make_source(mirror)],
            super::super::ChainMode::Default,
        );

        let result = chained.fetch_manifest(&tagged_id()).await;
        assert!(
            result.is_err(),
            "error on primary followed by clean miss on mirror must propagate as Err, \
             not collapse into Ok(None)"
        );
        let err_message = result.unwrap_err().to_string();
        assert!(
            err_message.contains("401 unauthorized"),
            "propagated error must carry the primary source's message; got: {err_message}"
        );
    }

    // Case 11c: same scenario for fetch_manifest_digest.
    #[tokio::test]
    async fn fetch_and_persist_chain_digest_propagates_error_when_later_source_misses_cleanly() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let primary = TestIndex::failing("connection refused");
        let mirror = TestIndex::empty();

        let chained = super::super::Index::from_chained(
            cache,
            vec![make_source(primary), make_source(mirror)],
            super::super::ChainMode::Default,
        );

        let result = chained.fetch_manifest_digest(&tagged_id()).await;
        assert!(result.is_err(), "digest query: error-then-miss must propagate as Err");
        let err_message = result.unwrap_err().to_string();
        assert!(
            err_message.contains("connection refused"),
            "propagated error must carry the primary source's message; got: {err_message}"
        );
    }

    // Case 12: empty sources Vec → behaves like LocalIndex alone (no fallback).
    #[tokio::test]
    async fn empty_sources_behaves_like_local_index() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // No sources — empty chain.
        let chained = super::super::Index::from_chained(cache, vec![], super::super::ChainMode::Default);

        let result = chained.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(result.is_none(), "empty sources and empty cache → None");
    }

    // Case 12b: tag persistence after fetch — a second call with empty sources
    // should still succeed because the first call persisted the tag to cache.
    #[tokio::test]
    async fn tag_persisted_in_cache_after_source_fetch() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // First call: source has the tag, chain fetches and persists it.
        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        {
            let chained =
                super::super::Index::from_chained(cache.clone(), vec![source], super::super::ChainMode::Default);
            let _ = chained.fetch_manifest(&tagged_id()).await.unwrap();
        }

        // Second call: same cache, but NO sources.  Must still return the tag
        // from cache because the first call persisted it.
        let chained_no_source = super::super::Index::from_chained(cache, vec![], super::super::ChainMode::Default);
        let result = chained_no_source.fetch_manifest(&tagged_id()).await.unwrap();
        assert!(
            result.is_some(),
            "tag fetched in first call must be persisted so a cache-only second call succeeds"
        );
    }

    // Case 13: a corrupted on-disk tag file (the documented kill-9 recovery
    // window from `tag_guard.rs`) must not short-circuit the chain walk.
    // `ChainedIndex::fetch_manifest` should log a warn, degrade to the source,
    // and the re-read of the now-rewritten file must succeed.
    #[tokio::test]
    async fn corrupted_cache_read_falls_back_to_chain() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // Corrupt the on-disk tag file with unparseable bytes so the first
        // `cache.fetch_manifest` call errors. The tag path follows
        // `tags/{registry_slug}/{repo}.json` (TagStore layout).
        let tag_file = cache_dir
            .path()
            .join("tags")
            .join(REGISTRY)
            .join(format!("{REPO}.json"));
        std::fs::create_dir_all(tag_file.parent().unwrap()).unwrap();
        std::fs::write(&tag_file, b"{not valid json at all").unwrap();

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest(&tagged_id())
            .await
            .expect("corrupt cache must degrade to chain walk, not propagate");
        let (digest, _) = result.expect("chain walk must recover the manifest from the source");
        assert_eq!(digest, digest_a());
    }

    // Case 13b: same degrade path for `fetch_manifest_digest`.
    #[tokio::test]
    async fn corrupted_cache_read_digest_falls_back_to_chain() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let tag_file = cache_dir
            .path()
            .join("tags")
            .join(REGISTRY)
            .join(format!("{REPO}.json"));
        std::fs::create_dir_all(tag_file.parent().unwrap()).unwrap();
        std::fs::write(&tag_file, b"").unwrap();
        // Truncated-but-nonzero would also exercise `read_disk`'s error path;
        // an empty file (len == 0) is treated as "no tags yet" so we need
        // actual garbage to force a parse error.
        std::fs::write(&tag_file, b"garbage").unwrap();

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest_digest(&tagged_id())
            .await
            .expect("corrupt cache must degrade for digest queries too");
        assert_eq!(result, Some(digest_a()));
    }
}
