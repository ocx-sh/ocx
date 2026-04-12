// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use super::{Index, LocalIndex, index_impl};
use crate::{Result, log, oci};

/// Two-role index: a persistent `cache` plus an ordered list of read-only
/// `sources` queried in order on cache miss.
///
/// On a cache miss the chain is walked until a source successfully syncs the
/// requested tag into `cache`, after which the cache is re-queried. Successful
/// fetches are persisted; source errors are logged at `warn` and the chain
/// continues. For #41 `sources` holds a single remote index, but the `Vec`
/// shape leaves room for future N-source scenarios (CI cache, org mirror).
pub(super) struct ChainedIndex {
    cache: LocalIndex,
    sources: Vec<Index>,
}

impl ChainedIndex {
    pub(super) fn new(cache: LocalIndex, sources: Vec<Index>) -> Self {
        Self { cache, sources }
    }

    /// Walk the source chain for a tagged identifier, allowing the first source
    /// that responds without error to populate the cache.
    ///
    /// Returns `Ok(())` when at least one source ran cleanly (whether it
    /// actually persisted a tag or not — `update_tag` returning `Ok(())` is
    /// the "no error" signal). The caller is expected to retry the cache
    /// afterwards: a real hit yields `Some`, a clean miss yields `None` (a
    /// legitimate not-found).
    ///
    /// Returns `Err(_)` only when *every* source returned an error and no
    /// source ever ran cleanly. This preserves the trust boundary between
    /// "package does not exist" (cache retry → `None`) and "registry outage
    /// or auth failure" (`Err` propagated to the caller). Collapsing source
    /// errors into `Ok(None)` would make a 401 indistinguishable from a 404
    /// and silently break automation retry logic.
    ///
    /// An empty `sources` Vec returns `Ok(())` (cache-only behavior).
    async fn walk_chain(&self, identifier: &oci::Identifier) -> Result<()> {
        // Digest-only identifiers (`pkg@sha256:…`) cannot be discovered via
        // tag fallback — bail out. For every other shape, fall back under
        // `tag_or_latest()`, which returns the explicit tag when present and
        // `"latest"` for bare identifiers. That keeps `ocx install cmake`
        // behaving the same as `ocx install cmake:latest` on a fresh machine;
        // full tag discovery stays the job of `ocx index update`.
        if identifier.tag().is_none() && identifier.digest().is_some() {
            return Ok(());
        }
        let tagged = identifier.clone_with_tag(identifier.tag_or_latest());

        let mut last_error: Option<crate::Error> = None;
        for source in &self.sources {
            match self.cache.update(source, &tagged).await {
                Ok(()) => {
                    log::debug!("Fetched tag '{}' from chained source, retrying cache lookup.", tagged);
                    return Ok(());
                }
                Err(e) => {
                    // AC6: surface the underlying cause so users can distinguish
                    // "manifest not found" vs "connection timed out" vs "401 unauthorized".
                    log::warn!("Could not fetch tag '{}' from chained source: {e}", tagged);
                    last_error = Some(e);
                }
            }
        }

        // Every source errored. Propagate the last error so the caller can
        // distinguish "package not found" from "registry outage / auth failure".
        match last_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl index_impl::IndexImpl for ChainedIndex {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        self.cache.list_repositories(registry).await
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        self.cache.list_tags(identifier).await
    }

    async fn fetch_manifest(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        // Cache read errors (e.g. a truncated tag file from a kill-9 during
        // `TagGuard::write_disk`) must not short-circuit the chain walk —
        // degrading to the source is the whole point of the fallback.
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
        self.walk_chain(identifier).await?;
        self.cache.fetch_manifest(identifier).await
    }

    async fn fetch_manifest_digest(&self, identifier: &oci::Identifier) -> Result<Option<oci::Digest>> {
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
        self.walk_chain(identifier).await?;
        self.cache.fetch_manifest_digest(identifier).await
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(Self {
            cache: self.cache.clone(),
            sources: self.sources.clone(),
        })
    }
}

// ── Specification tests ───────────────────────────────────────────────────
//
// Written from the design record (plan_tag_fallback.md) in specification mode.
// These tests encode the expected ChainedIndex behaviour and MUST fail with
// `unimplemented!()` panics against the current stub bodies.  They are the
// executable specification that Phase 4 (implementation) must satisfy.
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
    // The wrapper trick: `Index::from_chained(empty_local, vec![])` produces
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

        // Populate the cache by running update_tag via a source that has the tag.
        let seed_source = TestIndex::with_tag(TAG, digest_a());
        let seed_index = make_source(seed_source);
        cache.update(&seed_index, &tagged_id()).await.unwrap();

        // Now build the ChainedIndex with a *spy* source that records calls.
        let spy = TestIndex::empty();
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source]);

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
        let chained = super::super::Index::from_chained(cache, vec![source]);

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
        let chained = super::super::Index::from_chained(cache, vec![source]);

        let result = chained.fetch_manifest_digest(&tagged_id()).await.unwrap();

        assert_eq!(result, Some(digest_a()));
    }

    // Case 3: cache miss + source doesn't have the tag → returns None (warn logged).
    #[tokio::test]
    async fn cache_miss_source_missing_tag_returns_none() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::empty());
        let chained = super::super::Index::from_chained(cache, vec![source]);

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
        let chained = super::super::Index::from_chained(cache, vec![source]);

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
        let chained = super::super::Index::from_chained(cache, vec![source]);

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

    // Case 5: digest-only identifier → no chain walk, returns cache result (None if not cached).
    #[tokio::test]
    async fn digest_only_identifier_no_chain_walk() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // Source has the HEX_A digest but the identifier has no tag,
        // so the chain must NOT walk the source.
        let spy = TestIndex::with_tag(TAG, digest_a());
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source]);

        let id = digest_only_id(); // no tag
        let result = chained.fetch_manifest(&id).await.unwrap();

        assert!(result.is_none(), "digest-only id with empty cache should return None");
        assert!(
            spy_calls.lock().unwrap().is_empty(),
            "source must NOT be queried for digest-only identifiers"
        );
    }

    // Case 5b: fetch_manifest_digest with digest-only identifier.
    #[tokio::test]
    async fn digest_only_identifier_digest_query_no_chain_walk() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let spy = TestIndex::with_tag(TAG, digest_a());
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source]);

        let id = digest_only_id();
        let result = chained.fetch_manifest_digest(&id).await.unwrap();

        assert!(result.is_none());
        assert!(
            spy_calls.lock().unwrap().is_empty(),
            "source must NOT be queried for digest-only identifiers"
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
        let chained = super::super::Index::from_chained(cache, vec![source]);

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
        let chained = super::super::Index::from_chained(cache, vec![source]);

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
        let original = super::super::Index::from_chained(cache.clone(), vec![source]);
        let cloned = original.clone(); // calls box_clone internally

        // Seed the shared cache via the original by using a source that has the tag.
        let seed_source = make_source(TestIndex::with_tag(TAG, digest_a()));
        cache.update(&seed_source, &tagged_id()).await.unwrap();

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
        let chained = super::super::Index::from_chained(cache, vec![source]);

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
        let chained = super::super::Index::from_chained(cache, vec![source]);

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

        let chained = super::super::Index::from_chained(cache, vec![make_source(first), make_source(second)]);

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

        let chained = super::super::Index::from_chained(cache, vec![make_source(first), make_source(second)]);

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

        let chained = super::super::Index::from_chained(cache, vec![make_source(first), make_source(second)]);

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

        let chained = super::super::Index::from_chained(cache, vec![make_source(first), make_source(second)]);

        let result = chained.fetch_manifest(&tagged_id()).await;
        assert!(result.is_err(), "all-source-error must propagate as Err");
        let err_message = result.unwrap_err().to_string();
        assert!(
            err_message.contains("503 service unavailable"),
            "propagated error must carry the LAST source's message; got: {err_message}"
        );
    }

    // Case 12: empty sources Vec → behaves like LocalIndex alone (no fallback).
    #[tokio::test]
    async fn empty_sources_behaves_like_local_index() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // No sources — empty chain.
        let chained = super::super::Index::from_chained(cache, vec![]);

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
            let chained = super::super::Index::from_chained(cache.clone(), vec![source]);
            let _ = chained.fetch_manifest(&tagged_id()).await.unwrap();
        }

        // Second call: same cache, but NO sources.  Must still return the tag
        // from cache because the first call persisted it.
        let chained_no_source = super::super::Index::from_chained(cache, vec![]);
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
        let chained = super::super::Index::from_chained(cache, vec![source]);

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
        let chained = super::super::Index::from_chained(cache, vec![source]);

        let result = chained
            .fetch_manifest_digest(&tagged_id())
            .await
            .expect("corrupt cache must degrade for digest queries too");
        assert_eq!(result, Some(digest_a()));
    }
}
