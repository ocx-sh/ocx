// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::BTreeMap;

use async_trait::async_trait;
use futures::stream::{self, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};

use crate::file_structure::{IndexStore, SOURCE_LOCK_TIMEOUT};
use crate::{Result, log, oci, package::tag::Tag};

use super::{IndexOperation, index_impl};

mod config;

pub use config::Config;

/// Maximum number of per-tag manifest-chain persists to run concurrently in a
/// single [`LocalIndex::refresh_tags`].
///
/// Each persist is a small, latency-bound registry round-trip (fetch the
/// verbatim manifest bytes) plus a CAS write — not a memory-bound transfer.
/// 64 resolves a typical many-tagged package in a single round while capping
/// the simultaneous request burst a registry might answer with `429`.
const TAG_REFRESH_CONCURRENCY: usize = 64;

/// An OCX-authored **derived** root document (`adr_index_indirection.md` A2). A
/// derived index (a plain OCI registry) publishes no index of its own, so OCX
/// authors the root doc field-wise in the wire grammar. Unlike the read-only
/// wire [`IndexRoot`](super::wire::IndexRoot), this derives `Serialize` and
/// carries each tag's `observed` timestamp, so a re-authored root round-trips
/// every existing tag's stamp instead of dropping it.
#[derive(Debug, Default, Serialize, Deserialize)]
struct DerivedRoot {
    /// Physical `oci://host/path` pointer. For a derived index the logical and
    /// physical locations coincide — it is authored from the identifier itself.
    repository: String,
    #[serde(default)]
    tags: BTreeMap<String, DerivedTag>,
}

/// A single tag pointer inside a [`DerivedRoot`] (`adr_index_indirection.md` A2).
#[derive(Debug, Serialize, Deserialize)]
struct DerivedTag {
    /// The dispatch-object (image-index) or leaf-manifest digest this tag points
    /// at. `oci::Digest`'s serde is exact-wire, so a malformed on-disk value
    /// fails the whole [`DerivedRoot`] deserialize — the trigger for
    /// `commit_root_tag`'s kill-9 "start fresh" recovery branch below.
    content: oci::Digest,
    /// RFC3339 timestamp the pointer was last confirmed against the source —
    /// bumped only on refresh, never a freshness gate for local resolution.
    #[serde(default)]
    observed: String,
}

/// File-backed collection of registry metadata, rooted at the index home.
///
/// **Wire grammar only** — this is a `IndexStore`-backed collection of
/// per-repository root documents plus the verbatim, digest-verified
/// dispatch-object CAS (`o/sha256/<hex>.json`, `adr_index_indirection.md`
/// Decision A2/A3), so a committed `.ocx/index/` resolves a version choice
/// offline with zero dependence on machine-global state. It never holds
/// genuine content-addressed blob bytes (config blobs, leaf platform
/// manifests) — those live exclusively in the machine-global `BlobStore`
/// (`$OCX_HOME/blobs`, Decision B2), which `ChainedIndex` routes through
/// directly.
#[derive(Clone)]
pub struct LocalIndex {
    index_store: IndexStore,
    /// When false, a tag resolving to a yanked entry in the committed root is
    /// refused (`adr_index_indirection.md` F3) — the OFFLINE counterpart to
    /// [`OcxIndex::allow_yanked`](super::OcxIndex). Reads `OCX_ALLOW_YANKED`;
    /// defaults to false so every construction site (tests, `IndexSync`) that
    /// does not opt in keeps the safe refusal.
    allow_yanked: bool,
}

impl LocalIndex {
    pub fn new(config: Config) -> Self {
        Self {
            index_store: config.index_store,
            allow_yanked: false,
        }
    }

    /// Sets the yanked opt-in (`OCX_ALLOW_YANKED`) for offline status surfacing
    /// (`adr_index_indirection.md` F3). Consuming builder so existing
    /// construction sites stay a single `new(..)` call; only `context.rs` opts
    /// in from the resolved env flag.
    pub fn with_allow_yanked(mut self, allow_yanked: bool) -> Self {
        self.allow_yanked = allow_yanked;
        self
    }

    /// The index store backing this local index — the effective index home
    /// (`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index`) this copy reads and writes.
    /// Exposed so a derived manager (e.g. `ocx patch test`'s scratch manager,
    /// which reuses the running context's local index) can route its
    /// guaranteed-local companion / site-patch lookups through
    /// `PackageManager::with_index` to the **same** home, rather than a divergent
    /// default that a `pull`-committed tag pointer never lands in.
    pub fn index_store(&self) -> &IndexStore {
        &self.index_store
    }

    /// Grow the local index copy for `identifier` from `source`, writing the
    /// hosted wire grammar (`adr_index_indirection.md` A2/A3): per-tag dispatch
    /// objects into `o/` (multi-platform only) and the package root document.
    ///
    /// This is the write path for `ocx index update`. It never walks the
    /// image-index → platform-manifest chain (A3): the single dispatch object is
    /// the whole per-tag write, and a single-platform tag writes nothing to `o/`
    /// (its `content` is the leaf digest, fetched on demand).
    ///
    /// The two provenance kinds diverge in exactly who authors the root
    /// (Decision H's "two ifs"):
    ///
    /// - **Published** (an `index.ocx.sh` copy — [`super::Index::fetch_root_document`]
    ///   returns the verbatim root): copy the root byte-for-byte through
    ///   [`Self::persist_published_root`] and persist each referenced observation
    ///   object.
    /// - **Derived** (a plain OCI registry — no verbatim root to copy): OCX
    ///   authors the root field-wise through [`Self::commit_root_tag`], bumping
    ///   `observed`, after persisting each tag's dispatch object.
    ///
    /// A tagged identifier (`cmake:3.28`) refreshes only that tag; a bare
    /// identifier (`cmake`) first enumerates the source's tags.
    pub async fn refresh_tags(&self, identifier: &oci::Identifier, source: &super::Index) -> Result<()> {
        // One info line per identifier; per-tag detail is debug-only so an index
        // update over a many-tagged package does not flood info logs.
        log::info!("Refreshing tags for identifier '{}'.", identifier);

        // A published source serves a verbatim root document; a derived (plain
        // OCI-registry) source does not — that presence is the provenance switch.
        if let Some((bytes, root)) = source.fetch_root_document(identifier).await? {
            self.refresh_published(identifier, source, &bytes, &root).await
        } else {
            self.refresh_derived(identifier, source).await
        }
    }

    /// Published-source refresh (`adr_index_indirection.md` A2/F1): persist each
    /// referenced observation object, then copy the verbatim root document.
    ///
    /// F1 write order — dispatch objects first (harmless orphans if interrupted),
    /// then the root plus its catalog entry — so a crash never leaves a root
    /// pointing at an absent `o/` object.
    async fn refresh_published(
        &self,
        identifier: &oci::Identifier,
        source: &super::Index,
        bytes: &[u8],
        root: &super::wire::IndexRoot,
    ) -> Result<()> {
        // The full published root is persisted below (copy-a-mirror, A2), so
        // EVERY distinct observation object it references must travel with the
        // copy — not only the named tag's (B2). A tag-scoped update
        // (`ocx index update pkg:1.0`) still writes the whole root, so a sibling
        // tag left pointing at an obs absent from `o/` could not resolve offline.
        // Dedup by content digest (an obs is content-addressed) so tags a re-push
        // aliased onto one observation fetch it once — one representative tag per
        // distinct obs is enough, since `persist_dispatch` fetches the obs by tag.
        let mut seen: std::collections::HashSet<oci::Digest> = std::collections::HashSet::new();
        let tags: Vec<String> = root
            .tags
            .iter()
            .filter(|(_, entry)| seen.insert(entry.content.clone()))
            .map(|(tag, _)| tag.clone())
            .collect();

        // Persist each distinct tag's observation object concurrently — each is a
        // latency-bound fetch + a CAS write to a distinct `o/` path, so the burst
        // is capped at `TAG_REFRESH_CONCURRENCY` (issue #154's polite-citizen
        // contract, carried forward).
        let this = self;
        stream::iter(tags)
            .map(|tag| {
                let tagged = identifier.clone_with_tag(&tag);
                async move {
                    log::debug!("Refreshing published tag '{}' for identifier '{}'.", tag, identifier);
                    // `persist_dispatch` returns `(digest, manifest)` — a refresh
                    // only needs the write side-effect.
                    this.persist_dispatch(source, &tagged).await.map(|_| ())
                }
            })
            .buffer_unordered(TAG_REFRESH_CONCURRENCY)
            .try_collect::<()>()
            .await?;

        self.persist_published_root(identifier, bytes).await
    }

    /// Derived-source refresh (`adr_index_indirection.md` A2/A3): persist each
    /// tag's dispatch object, then author the root document field-wise.
    async fn refresh_derived(&self, identifier: &oci::Identifier, source: &super::Index) -> Result<()> {
        let tags = match identifier.tag() {
            Some(tag) => vec![tag.to_owned()],
            None => source.list_tags(identifier).await?.unwrap_or_default(),
        };

        if tags.is_empty() {
            // A bare identifier the source lists no tags for — the package does
            // not exist (or has no published versions). Report it per-identifier
            // (NotFound → exit 79) so `ocx index update` aggregates a nonzero
            // exit while still refreshing the other requested identifiers.
            return Err(super::error::Error::RemoteManifestNotFound(identifier.to_string()).into());
        }

        // Fan the per-tag dispatch persists out concurrently (issue #154); each
        // returns `(tag, content)`. The commit step below serializes on the root
        // file lock, so the concurrency lives here, on the fetches.
        let this = self;
        let fetched: Vec<(String, oci::Digest)> = stream::iter(tags)
            .map(|tag| {
                let tagged = identifier.clone_with_tag(&tag);
                async move {
                    log::debug!("Refreshing derived tag '{}' for identifier '{}'.", tag, identifier);
                    match this.persist_dispatch(source, &tagged).await? {
                        Some((content, _manifest)) => Ok::<_, crate::Error>(Some((tag, content))),
                        None => {
                            log::debug!("Source has no manifest for tag '{}' — skipping.", tag);
                            Ok(None)
                        }
                    }
                }
            })
            .buffer_unordered(TAG_REFRESH_CONCURRENCY)
            .try_filter_map(|entry| async move { Ok(entry) })
            .try_collect()
            .await?;

        if fetched.is_empty() {
            // Every requested tag resolved to no manifest at the source — nothing
            // was persisted. Same per-identifier not-found signal as the
            // empty-tags case above.
            return Err(super::error::Error::RemoteManifestNotFound(identifier.to_string()).into());
        }

        // Author the derived root's tag pointers in ONE lock acquisition + ONE
        // root read-modify-write (`adr_index_indirection.md` A2/F1). Committing
        // each tag separately would re-lock and rewrite the whole root per tag —
        // O(N²) bytes for N tags; the batch merge preserves every other tag.
        self.commit_root_tags(identifier, &fetched).await
    }

    /// Sync one source's `c/index.json` catalog and re-snapshot the packages
    /// whose root moved (`adr_index_indirection.md` F2).
    ///
    /// The catalog is **per-source** (A2): the diff basis and the persisted map
    /// live at `<home>/<source>/c/index.json`, keyed by this source's namespace.
    /// The flow, in F2's contract order:
    ///
    /// 1. Read the persisted per-source catalog + ETag as the diff basis.
    /// 2. Conditional-GET the remote catalog **outside** any lock — a `304`
    ///    short-circuits with nothing to do; a `200` diffs the per-package root
    ///    digests against the persisted map.
    /// 3. Re-snapshot only the moved (or new) packages via [`Self::refresh_tags`]
    ///    — the published grow path (root + dispatch objects).
    /// 4. Reconcile-commit the catalog + ETag coherently under the source lock:
    ///    [`IndexStore::begin_catalog_transaction`](crate::file_structure::IndexStore::begin_catalog_transaction)
    ///    re-reads the on-disk map first, then the fetched entries are merged in
    ///    — **never** a wholesale replace from the pre-lock read, so a concurrent
    ///    per-package upsert is never clobbered.
    ///
    /// Returns the sync outcome (moved set, unchanged flag) for reporting;
    /// remote-catalog drift a caller did not re-snapshot is staleness ("update
    /// available"), never an error (F1).
    pub async fn sync_catalog(&self, source: &super::OcxIndex) -> Result<super::CatalogSyncOutcome> {
        let namespace = source.namespace();

        // Step 1 — the per-source catalog + ETag are the diff basis (F2). The
        // reconcile-commit below re-reads under the lock, so this pre-lock read is
        // only a basis for the network diff, never the map that gets written.
        let previous = self
            .index_store
            .read_source_catalog(namespace)
            .await?
            .unwrap_or_default();
        let previous_etag = self.index_store.read_source_catalog_etag(namespace).await?;

        // Step 2 — network work happens OUTSIDE the transaction lock.
        let outcome = source.sync_catalog(&previous, previous_etag.as_deref()).await?;

        // Boundary validation (CWE-22, F2 "surfaces, never silently acts"): a
        // published index's `c/index.json` keys are attacker-controlled for a
        // mirrored or compromised source, and each key becomes a `repository`
        // that `IndexStore` splits into path segments verbatim — so a key
        // like `../../victim` would write outside the index home. Reject the
        // WHOLE sync fail-closed if ANY key is not a well-formed repository
        // path, before building a single filesystem path from one. `moved` is a
        // subset of `catalog` (it is diffed from it), so this one pass guards
        // both the re-snapshot loop below AND the persisted catalog map.
        for key in outcome.catalog.keys() {
            if let Err(error) = Self::validate_catalog_key(namespace, key) {
                log::warn!("refusing catalog sync for index source '{namespace}': {error}");
                return Err(error);
            }
        }

        // Step 3 — re-snapshot only packages that ALREADY have a local root and
        // whose digest moved (F2). `outcome.moved` is already restricted to
        // entries present in the previous local catalog whose digest changed
        // (`diff_moved`); this filter narrows it further to the MATERIALIZED
        // subset. A package new to the local catalog, or a listing row (a catalog
        // entry with no root document on disk), is NEVER auto-materialized here —
        // it stays a listing row until first `update`d, and its catalog value is
        // adopted in the batched merge below WITHOUT any fetch. The two filters
        // together turn the old whole-catalog re-snapshot storm into
        // O(changed-materialized) fetches. Each catalog key is the
        // source-relative `<ns>/<pkg>` repository path; the published grow path
        // routes through `fetch_root_document`.
        let source_index = super::Index::from_source(source.clone());
        let mut refreshed: std::collections::HashSet<&String> = std::collections::HashSet::new();
        for repository in &outcome.moved {
            if !crate::utility::fs::path_exists_lossy(&self.index_store.root_document_path(namespace, repository)).await
            {
                continue;
            }
            let identifier = oci::Identifier::new_registry(repository.clone(), namespace);
            self.refresh_tags(&identifier, &source_index).await?;
            refreshed.insert(repository);
        }

        // Step 4 — reconcile-commit the fetched catalog + ETag in ONE transaction
        // under the source lock. A `304` (`unchanged`) has nothing to persist.
        if !outcome.unchanged {
            let mut transaction = self.index_store.begin_catalog_transaction(namespace).await?;
            // Merge the fetched entries into the freshly re-read on-disk map, but
            // SKIP the re-snapshotted packages: step 3's refresh already committed
            // their entries derived from the actually-persisted root bytes (F2
            // "rewrite the root AND re-upsert its entry together"). Re-applying the
            // fetched catalog value here would clobber that fresher, root-derived
            // entry back to the pre-fetch remote claim — a manufactured
            // root/catalog straddle under CDN skew. Every other fetched entry —
            // unchanged rows, new listing rows, and changed listing rows — adopts
            // its fetched value here in one batched write; an entry only in the
            // on-disk map (a concurrent per-package upsert, or a package the remote
            // dropped) survives. This is the reconcile, not a wholesale replace.
            for (repository, catalog_entry) in &outcome.catalog {
                if refreshed.contains(repository) {
                    continue;
                }
                transaction.catalog().insert(repository.clone(), catalog_entry.clone());
            }
            transaction.commit(outcome.etag.as_deref()).await?;
        }
        Ok(outcome)
    }

    /// Validates that a remote catalog `key` is a well-formed OCI repository
    /// path under `namespace` before it is used to build a filesystem path
    /// (CWE-22 boundary validation).
    ///
    /// A published index's `c/index.json` keys are attacker-controlled for a
    /// mirrored or compromised source; each key becomes the `repository` of an
    /// [`oci::Identifier`] (`new_registry`) and then a path via
    /// `IndexStore::root_document_path` (`repository_path` splits it on `/`
    /// verbatim), so a key like `../../victim` would escape the index home.
    /// Rather than re-implement the grammar, reuse the canonical identifier
    /// parser: parse `<namespace>/<key>` (which runs the directory-traversal +
    /// repository-charclass checks) and require it round-trips to exactly this
    /// namespace and repository with **no** tag or digest — so a `:` or `@`
    /// smuggled into a key cannot slip through split as a tag/digest.
    fn validate_catalog_key(namespace: &str, key: &str) -> Result<()> {
        let malformed = |reason: String| super::error::Error::MalformedCatalogKey {
            index_source: namespace.to_string(),
            key: key.to_string(),
            reason,
        };
        // The `reason` string IS this variant's design: two of its three call
        // sites (the namespace/tag checks below) carry no underlying error at
        // all, only a descriptive reason. For the parse case the parser's Display
        // is exactly that reason, and nothing downstream matches on the parse
        // error's structure (the boundary only needs "malformed → refuse
        // fail-closed"), so flattening it into `reason` is intentional, not
        // source erasure. A dedicated `#[source]`-bearing arm would have to live
        // in `oci/index/error.rs`.
        let identifier = oci::Identifier::parse(&format!("{namespace}/{key}"))
            .map_err(|parse_error| malformed(parse_error.to_string()))?;
        if identifier.registry() != namespace || identifier.repository() != key {
            return Err(malformed("not a bare repository path under the source namespace".to_string()).into());
        }
        if identifier.tag().is_some() || identifier.digest().is_some() {
            return Err(malformed("must not carry a tag or digest".to_string()).into());
        }
        Ok(())
    }

    // ── Dispatch-only reads/writes (A3) ───────────────────────────────────────

    /// Persist the single dispatch object for `identifier` from `source` and
    /// return the head digest (`adr_index_indirection.md` A3 — dispatch-only:
    /// never walks child manifests).
    ///
    /// Fetches the verbatim response bytes exactly once
    /// ([`super::Index::fetch_manifest_raw_bytes`]) and dispatches on the decoded
    /// manifest shape — **never** walking child manifests:
    ///
    /// - [`oci::Manifest::ImageIndex`] ⇒ write the verbatim bytes into the
    ///   dispatch-object CAS (`IndexStore::write_dispatch_object`, which
    ///   recompute-and-verifies the digest against the source-claimed one, A4).
    ///   The bytes are the source's dispatch object — an OCI image index (a
    ///   derived / OCI-registry source) or an observation object (a published
    ///   `index.ocx.sh` source, which [`decode_index_manifest`] presents as a
    ///   synthetic image index). When the caller has ALREADY fetched the bytes
    ///   (a [`DispatchResolution::AbsentLeaf`] recovery that decoded as an image
    ///   index), it self-heals via [`Self::stage_dispatch_bytes`] instead, to
    ///   avoid the double fetch this method would perform.
    /// - [`oci::Manifest::Image`] ⇒ write **nothing** to `o/`; a single-platform
    ///   tag's `content` is the leaf manifest digest itself, and a leaf platform
    ///   manifest is never copied into the local index (A3/B2) — it is fetched on
    ///   demand from the physical registry.
    ///
    /// Returns the fetched `(digest, manifest)` — the dispatch object's digest
    /// with its decoded shape, or the leaf manifest's own digest with the leaf
    /// itself — or `Ok(None)` when the source has no manifest for `identifier`.
    /// Callers that only need the digest for root growth (`refresh_published`,
    /// `refresh_derived`) discard the manifest; `ChainedIndex`'s AbsentLeaf
    /// recovery returns it directly to the caller instead of attempting a
    /// doomed local-storage read-back (a leaf is never written to `o/`, A3).
    pub async fn persist_dispatch(
        &self,
        source: &super::Index,
        identifier: &oci::Identifier,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        let Some((bytes, digest, manifest)) = source.fetch_manifest_raw_bytes(identifier).await? else {
            return Ok(None);
        };
        // Dispatch on the decoded manifest shape — NEVER walk child manifests (A3):
        //  - image index (an OCI multi-platform index, or an observation object
        //    decoded to a synthetic index) ⇒ the dispatch object; self-heal it
        //    verbatim into `o/` via `stage_dispatch_bytes` (recompute-and-verified
        //    against the source-claimed digest, A4) — the bytes are already in
        //    hand, so this never double-fetches.
        //  - single-platform image manifest ⇒ its own digest IS the tag's
        //    `content`, and a leaf platform manifest is never copied into the
        //    local index (A3/B2) — write nothing.
        if let oci::Manifest::ImageIndex(_) = &manifest {
            self.stage_dispatch_bytes(identifier, &digest, &bytes).await?;
        }
        Ok(Some((digest, manifest)))
    }

    /// Commit a single tag → `content` pointer for `identifier` into a DERIVED
    /// (OCX-authored) root document (`adr_index_indirection.md` A2/F1),
    /// read-modify-written under an exclusive lock on the root document's own
    /// `.lock` sidecar.
    ///
    /// **Derived index (a plain OCI registry, which publishes no index of its
    /// own).** OCX authors the root doc itself, field-wise — `{ "repository":
    /// "oci://<physical>", "tags": { "<tag>": { "content": "<content>",
    /// "observed": "<iso8601>" } } }`. The write is a read-modify-write under the
    /// lock: read the existing authored root (if any), upsert the target tag's
    /// entry preserving every other tag, re-serialize, and write it through
    /// `IndexStore::write_root_document`. The physical `repository` is
    /// derived from `identifier` — for a derived index the logical and
    /// physical locations coincide. `observed` is an ISO-8601 timestamp
    /// bumped **only** on this refresh; it is never a freshness gate for
    /// local resolution.
    ///
    /// **A published index (an `index.ocx.sh` copy) is never authored here.** A
    /// published root travels verbatim with the copy and is updated only by
    /// **re-snapshot** — a whole new set of verbatim bytes fetched from the site
    /// and written through `CatalogTransaction::write_root` by the
    /// `ocx index update` / catalog-sync path (F1/F2, see [`Self::persist_published_root`]).
    /// OCX never edits a published root field-wise, so the local copy stays
    /// byte-identical to the site (copy-a-mirror, A2) and keeps verifying
    /// against its `c/index.json` catalog entry.
    ///
    /// Caller must ensure `identifier.tag()` is `Some`. Visibility is
    /// `pub(super)` so `ChainedIndex::fetch_and_persist_chain` stays the sole
    /// caller outside the refresh path — the same narrow root-writer surface a
    /// structural test could guard, mirroring the pre-C2 tag-pointer writer's
    /// contract.
    pub(super) async fn commit_root_tag(&self, identifier: &oci::Identifier, content: &oci::Digest) -> Result<()> {
        let tag = identifier
            .tag()
            .expect("commit_root_tag invariant: identifier must carry a tag");
        self.commit_root_tags(identifier, &[(tag.to_owned(), content.clone())])
            .await
    }

    /// Batch counterpart to [`Self::commit_root_tag`]: upsert MANY `tag →
    /// content` pointers into a DERIVED (OCX-authored) root document under a
    /// SINGLE lock acquisition and a SINGLE root read-modify-write
    /// (`adr_index_indirection.md` A2/F1). `identifier` supplies the shared
    /// source + repository (its own tag, if any, is ignored); `entries` is the
    /// `(tag, content)` set to upsert.
    ///
    /// This is the write step of a **derived** [`Self::refresh_tags`]: committing
    /// N tags one at a time through [`Self::commit_root_tag`] would take the
    /// source lock and re-read + rewrite the whole root N times — O(N²) bytes for
    /// N tags. Merging every upsert into one read-modify-write keeps the single
    /// lock / read / write while preserving the same crash-safety, repository
    /// cross-check, and "preserve every other tag" merge. All batched tags share
    /// one `observed` stamp — they were confirmed against the source together.
    async fn commit_root_tags(&self, identifier: &oci::Identifier, entries: &[(String, oci::Digest)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let source = identifier.registry();
        let repository = identifier.repository();
        // A derived index's logical and physical locations coincide (there is no
        // separate index site to point elsewhere), so the `oci://` pointer is
        // authored straight from the identifier.
        let expected_repository = format!("oci://{source}/{repository}");

        // The derived root is a shared multi-writer file (concurrent
        // `commit_root_tag(s)` for distinct tags of one repository), so the
        // read-modify-write runs under an exclusive lock. The lock is keyed on
        // the per-source directory's file identity and lives in the
        // machine-global `$OCX_HOME/locks` — never a sidecar in the index home,
        // which may be a read-only shipped copy — discriminated by `repository`
        // so distinct repositories of one source do not serialize
        // (`IndexStore::lock_source`).
        let _guard = self
            .index_store
            .lock_source("index-root", source, repository, SOURCE_LOCK_TIMEOUT)
            .await?;

        let mut doc = match self.index_store.read_root_document_bytes(source, repository).await? {
            Some(bytes) => match serde_json::from_slice::<DerivedRoot>(&bytes) {
                Ok(doc) => {
                    // Repository cross-check: an existing authored root that names a
                    // different physical host is corruption — a hard `DataError`
                    // (F1), never a silent overwrite.
                    if doc.repository != expected_repository {
                        return Err(super::error::Error::RootRepositoryMismatch {
                            repository: repository.to_string(),
                            expected: expected_repository,
                            found: doc.repository,
                        }
                        .into());
                    }
                    doc
                }
                // Kill-9 recovery: this root is always OCX's own prior write
                // (a derived root is never externally supplied), so an
                // unparseable existing document is a crashed-write artifact,
                // not a trust-boundary concern — treated as "not yet
                // written" so the upsert below rewrites it cleanly.
                Err(e) => {
                    log::warn!(
                        "derived root for '{source}/{repository}' is unparseable ({e}) — starting fresh for recovery."
                    );
                    DerivedRoot {
                        repository: expected_repository,
                        tags: BTreeMap::new(),
                    }
                }
            },
            None => DerivedRoot {
                repository: expected_repository,
                tags: BTreeMap::new(),
            },
        };

        // Upsert every requested tag, preserving each other tag's pointer and
        // stamp. One `observed` for the whole batch — confirmed together.
        let observed = chrono::Utc::now().to_rfc3339();
        for (tag, content) in entries {
            doc.tags.insert(
                tag.clone(),
                DerivedTag {
                    content: content.clone(),
                    observed: observed.clone(),
                },
            );
        }

        let bytes = serde_json::to_vec_pretty(&doc)?;
        self.index_store.write_root_document(source, repository, &bytes).await?;
        Ok(())
    }

    /// Resolve `identifier` against the dispatch-only object store to a typed
    /// [`DispatchResolution`] (`adr_index_indirection.md` A3 read path — the
    /// root-doc counterpart to [`Self::get_manifest`] / [`Self::get_tags`]).
    ///
    /// - **Digest-addressed** `identifier` — look the digest up directly in `o/`
    ///   (`IndexStore::read_dispatch_object`): present ⇒
    ///   [`DispatchResolution::Dispatch`] (decoded via
    ///   [`decode_index_manifest`]); absent ⇒
    ///   [`DispatchResolution::AbsentLeaf`], whose recovery is
    ///   **source-kind-routed** (see that variant — never an unconditional leaf).
    /// - **Tag-addressed** `identifier` — read the root document per `kind`:
    ///   `IndexStore::read_root` for [`SourceKind::Published`] (cross-checks
    ///   the `c/index.json` catalog entry) or `IndexStore::read_root_uncatalogued`
    ///   for [`SourceKind::Derived`] (no catalog → `CatalogEntryStatus::NoCatalog`),
    ///   both passing the C3 `oci://` strict-parse
    ///   [`super::parse_physical_repository`] as the `repository_check` hook.
    ///   Resolve `tag → content` from the root's machine lane, then dispatch on
    ///   the `o/` lookup exactly as the digest case.
    ///
    /// The absent-object case is a **typed outcome, never an error and never a
    /// bare miss**, so `ChainedIndex` can drive the source-kind-routed recovery
    /// ([`DispatchResolution::AbsentLeaf`]). Returns `Ok(None)` only when the
    /// root document or the requested tag is unknown locally — the clean miss the
    /// caller turns into a chain walk.
    pub(super) async fn resolve_dispatch(
        &self,
        identifier: &oci::Identifier,
        kind: SourceKind,
    ) -> Result<Option<DispatchResolution>> {
        let source = identifier.registry();
        let repository = identifier.repository();

        // Resolve the `content` digest to dispatch on.
        let content = match identifier.digest() {
            // Digest-addressed: the digest IS the object to look up in `o/`.
            Some(digest) => digest,
            // Tag-addressed: read the root per source kind, then `tag → content`.
            None => {
                let Some(result) = self.read_root_by_kind(source, repository, kind).await? else {
                    // Unknown root — the clean miss the caller turns into a chain walk.
                    return Ok(None);
                };
                let tag = identifier.tag_or_latest();
                let Some(tag_entry) = result.root.tags.get(tag) else {
                    // Root present, tag absent — likewise a clean miss.
                    return Ok(None);
                };
                // Surface the human-governed lane straight from the COMMITTED root
                // (F3): warn on deprecation / supersession, and warn + refuse a
                // yanked tag unless opted in — the OFFLINE counterpart to
                // `OcxIndex::surface_status`, so a committed yank/deprecation is
                // honored with zero network. The digest-addressed branch above
                // skips this deliberately: a yank is a tag-lane publisher signal,
                // never checked on an immutable digest pin.
                super::ocx_index::surface_root_status(identifier, &result.root, tag_entry, self.allow_yanked)?;
                tag_entry.content.clone()
            }
        };

        // Dispatch on the `o/` lookup: present ⇒ decode to the dispatch manifest;
        // absent ⇒ `AbsentLeaf` (a leaf platform manifest is never stored in the
        // local index — A3/B2 — so the caller drives the source-kind-routed
        // recovery).
        match self
            .index_store
            .read_dispatch_object(source, repository, &content)
            .await?
        {
            Some(bytes) => match decode_index_manifest(&bytes)? {
                Some(manifest) => Ok(Some(DispatchResolution::Dispatch {
                    content,
                    manifest: Box::new(manifest),
                })),
                // Present but neither codec: a recoverable state, routed as a
                // fetch-by-digest recovery rather than surfaced as corruption.
                None => Ok(Some(DispatchResolution::AbsentLeaf { content })),
            },
            None => Ok(Some(DispatchResolution::AbsentLeaf { content })),
        }
    }

    /// Read a repository's root document by source kind, sharing the C3
    /// `oci://` repository-check hook (`adr_index_indirection.md` A2/H "two
    /// ifs" — published cross-checks the `c/index.json` catalog entry and
    /// self-heals a straddle, F1; derived has no catalog to cross-check).
    /// `Ok(None)` when the root is not known locally.
    async fn read_root_by_kind(
        &self,
        source: &str,
        repository: &str,
        kind: SourceKind,
    ) -> Result<Option<crate::file_structure::RootReadResult>> {
        let repository_check =
            |root: &super::wire::IndexRoot| super::parse_physical_repository(&root.repository).map(|_| ());
        match kind {
            SourceKind::Published => self.index_store.read_root(source, repository, repository_check).await,
            SourceKind::Derived => {
                self.index_store
                    .read_root_uncatalogued(source, repository, repository_check)
                    .await
            }
        }
    }

    /// List locally-known tags for `identifier`'s repository, by source kind
    /// (`adr_index_indirection.md` A2/H) — reads the root document's `tags`
    /// map. `Ok(None)` when the root is not known locally.
    pub(super) async fn list_local_tags(
        &self,
        identifier: &oci::Identifier,
        kind: SourceKind,
    ) -> Result<Option<Vec<String>>> {
        let Some(result) = self
            .read_root_by_kind(identifier.registry(), identifier.repository(), kind)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(result.root.tags.keys().cloned().collect()))
    }

    /// List repositories known locally under `source`, by source kind
    /// (`adr_index_indirection.md` A2/H) — the per-source `c/index.json`
    /// catalog for a published source, directory enumeration of `p/` for a
    /// derived one (a derived index's catalog IS the directory enumeration,
    /// A2).
    pub(super) async fn list_local_repositories(&self, source: &str, kind: SourceKind) -> Result<Vec<String>> {
        match kind {
            SourceKind::Published => Ok(self
                .index_store
                .read_source_catalog(source)
                .await?
                .map(|catalog| catalog.into_keys().collect())
                .unwrap_or_default()),
            SourceKind::Derived => self.index_store.list_wire_repositories(source).await,
        }
    }

    /// Persist a PUBLISHED (verbatim-copied) root document — the copy-a-mirror
    /// counterpart to [`Self::commit_root_tag`] (`adr_index_indirection.md`
    /// A2/F1/H). Takes the verbatim `bytes` a published source served
    /// ([`super::Index::fetch_root_document`]) and writes them through the
    /// source-scoped catalog transaction (`IndexStore::begin_catalog_transaction`
    /// → `CatalogTransaction::write_root` → `commit`), so the local copy stays
    /// byte-identical to the site and its `c/index.json` catalog entry
    /// (`sha256(bytes)`) is upserted atomically alongside the root (F1).
    ///
    /// Unlike [`Self::commit_root_tag`], OCX never edits the bytes field-wise — a
    /// published root is replaced whole, verbatim (copy-a-mirror). This is the
    /// path `refresh_tags` / `sync_catalog` route published sources through, and
    /// the grow-on-resolve path a Default write-through takes for a published
    /// package (C2, per the plan's grow ≠ refresh ruling). `write_root` re-parses
    /// `bytes` for its C3 cross-check and derives the catalog entry from those raw
    /// bytes, so the caller passes no parsed form — the write is byte-verbatim.
    pub(super) async fn persist_published_root(&self, identifier: &oci::Identifier, bytes: &[u8]) -> Result<()> {
        let source = identifier.registry();
        let repository = identifier.repository();

        // Author the root + its `c/index.json` catalog entry atomically through
        // the source-scoped transaction (F1): re-read + reconcile under the lock,
        // then commit both together, so a concurrent per-package upsert of a
        // DIFFERENT package is never clobbered.
        let mut transaction = self.index_store.begin_catalog_transaction(source).await?;
        transaction
            .write_root(repository, bytes, |root| {
                super::parse_physical_repository(&root.repository).map(|_| ())
            })
            .await?;
        transaction.commit(None).await?;
        Ok(())
    }

    /// Stage already-fetched dispatch-object bytes into the wire-grammar object
    /// CAS under the object's own digest — the no-double-fetch self-heal write
    /// (`adr_index_indirection.md` A3). When [`ChainedIndex`](super::chained_index::ChainedIndex)
    /// already holds the bytes of a [`DispatchResolution::AbsentLeaf`] recovery
    /// that decoded as an image index (an incomplete snapshot), it heals `o/`
    /// here instead of re-fetching through [`Self::persist_dispatch`]. The store
    /// recompute-and-verifies the digest before the write commits (A4);
    /// re-staging the same digest is idempotent.
    pub async fn stage_dispatch_bytes(
        &self,
        identifier: &oci::Identifier,
        digest: &oci::Digest,
        bytes: &[u8],
    ) -> Result<()> {
        self.index_store
            .write_dispatch_object(identifier.registry(), identifier.repository(), digest, bytes)
            .await
    }
}

/// The provenance of an index source (`adr_index_indirection.md` Decision A2/H)
/// — the "two ifs" that distinguish a **published** (`index.ocx.sh`) copy from a
/// **derived** (OCI-registry) one. Threaded through [`LocalIndex::resolve_dispatch`]
/// and the write path (`ChainedIndex::fetch_and_persist_chain`) so
/// `IndexStore::read_root` knows whether a `c/index.json` catalog
/// cross-check applies. Deliberately minimal per Decision H's "two ifs, keep it
/// minimal": catalog source (file vs directory enumeration) and root authorship
/// (verbatim copy vs OCX-authored field-wise).
///
/// `pub(crate)` (not `pub(super)`): [`index_impl::IndexImpl::source_kind`]
/// returns this type and the trait itself is re-exported `pub(crate)` for
/// sibling-module tests, so the return type must be at least as visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// A published ocx-index (`index.ocx.sh` or a mirror of it): roots and
    /// observation objects are copied verbatim, and the source carries a
    /// `c/index.json` catalog, so a root read cross-checks its catalog entry
    /// (`CatalogEntryStatus::Consistent` / `CatalogEntryStatus::Recovered`).
    Published,
    /// A derived index over a plain OCI registry: OCX authors the root doc
    /// field-wise and there is no catalog — directory enumeration lists it, so a
    /// root read carries `CatalogEntryStatus::NoCatalog`.
    Derived,
}

/// Outcome of resolving a root tag / content digest against the dispatch-only
/// object store (`adr_index_indirection.md` A3). The seam
/// [`LocalIndex::resolve_dispatch`] surfaces to `ChainedIndex`: the
/// absent-object case is a **typed outcome, not an error**, so the caller can
/// drive the fallback fetch-by-digest — a leaf platform manifest is never stored
/// in the local index (A3/B2).
#[derive(Debug)]
pub(super) enum DispatchResolution {
    /// `content` names a dispatch object present in `o/`: an image index
    /// (derived source) or an observation object (published source), decoded to
    /// an [`oci::Manifest::ImageIndex`]. `content` is the head digest the root
    /// tag pointed at. The manifest is boxed so this variant does not dwarf the
    /// digest-only [`Self::AbsentLeaf`] (clippy `large_enum_variant`).
    Dispatch {
        content: oci::Digest,
        manifest: Box<oci::Manifest>,
    },
    /// `content` is absent from `o/`. Recovery is **source-kind-routed** — this
    /// variant asserts nothing about what `content` names; the caller decides by
    /// [`SourceKind`]:
    ///
    /// - [`SourceKind::Published`] — an obs object that should have traveled with
    ///   the copy is missing (a damaged or incomplete copy). Re-fetch it from the
    ///   index site (the `OcxIndex` remote, via
    ///   [`super::Index::fetch_manifest_raw_bytes`]), verify `sha256`, and
    ///   self-heal it into `o/` ([`LocalIndex::stage_dispatch_bytes`]). **Never**
    ///   a physical-registry fetch-by-digest — an obs digest is not a registry
    ///   manifest digest, so that would 404 (the leaf-trap).
    /// - [`SourceKind::Derived`] — `content` names a leaf platform manifest the
    ///   local index does not hold (A3/B2). Fetch it by digest from the blob
    ///   store or the physical registry.
    /// - **Either kind** — if the fetched bytes decode as an image index (an
    ///   incomplete snapshot rather than a leaf), self-heal it into `o/`
    ///   ([`LocalIndex::stage_dispatch_bytes`]) and continue dispatch.
    AbsentLeaf { content: oci::Digest },
}

/// Decodes a verified dispatch object into an [`oci::Manifest`], dispatching on
/// the per-source payload codec (`adr_index_indirection.md` A2).
///
/// Exactly two object kinds are ever written to the dispatch-object CAS:
/// verbatim OCI manifests (registry sources) and `index.ocx.sh` observation
/// objects (native codec). The untagged [`oci::Manifest`] requires
/// `schemaVersion`, which an observation object lacks, so the OCI parse fails
/// cleanly for an observation and the fall-through is unambiguous. An
/// observation is decoded to the same synthetic image index a live
/// [`super::OcxIndex`] presents, so an offline resolve walks the local index
/// first without re-fetching.
///
/// `Ok(None)` = the bytes are neither codec (corruption → recoverable cache
/// miss); `Err` = a well-formed observation carrying a malformed leaf digest
/// (a trust-boundary data error, not silently swallowed).
fn decode_index_manifest(bytes: &[u8]) -> Result<Option<oci::Manifest>> {
    if let Ok(manifest) = serde_json::from_slice::<oci::Manifest>(bytes) {
        return Ok(Some(manifest));
    }
    match serde_json::from_slice::<super::wire::Observation>(bytes) {
        Ok(observation) => Ok(Some(oci::Manifest::ImageIndex(super::ocx_index::observation_to_index(
            &observation,
        )?))),
        Err(_) => Ok(None),
    }
}

#[async_trait]
impl index_impl::IndexImpl for LocalIndex {
    // This bare trait surface is never reached in PRODUCTION — `LocalIndex`
    // is always the `cache` field of a `ChainedIndex`, which calls the
    // kind-routed inherent methods (`resolve_dispatch`, `list_local_tags`,
    // `list_local_repositories`) directly so a `Published` source's catalog
    // cross-check applies. It is retained as the TEST-facing trait surface:
    // the module's own unit tests drive a bare `LocalIndex` through
    // `IndexImpl` (`list_repositories`, `fetch_manifest`) to exercise offline
    // resolution of the persisted wire grammar. Absent any external kind
    // context, these trait-level implementations default to
    // `SourceKind::Derived` — the uncatalogued read shares the exact
    // root-document path with the catalogued one and only skips the catalog
    // cross-check/self-heal, never resolution correctness
    // (`adr_index_indirection.md` A2/H).
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        self.list_local_repositories(registry, SourceKind::Derived).await
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        Ok(self
            .list_local_tags(identifier, SourceKind::Derived)
            .await?
            .map(|tags| tags.into_iter().filter(|t| !Tag::is_internal_str(t)).collect()))
    }

    async fn fetch_manifest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        log::trace!("Fetching manifest for identifier '{}'.", identifier);
        match self.resolve_dispatch(identifier, SourceKind::Derived).await? {
            Some(DispatchResolution::Dispatch { content, manifest }) => Ok(Some((content, *manifest))),
            // The digest/tag is known but its bytes are not locally cached
            // (a leaf platform manifest, A3) — a bare local read cannot
            // produce it; `ChainedIndex` drives the source-kind-routed
            // recovery instead.
            Some(DispatchResolution::AbsentLeaf { .. }) | None => Ok(None),
        }
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<oci::Digest>> {
        match self.resolve_dispatch(identifier, SourceKind::Derived).await? {
            // The digest is known regardless of whether the dispatch bytes
            // are locally cached — `AbsentLeaf` still carries it.
            Some(DispatchResolution::Dispatch { content, .. }) | Some(DispatchResolution::AbsentLeaf { content }) => {
                Ok(Some(content))
            }
            None => Ok(None),
        }
    }

    async fn fetch_blob(&self, _blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
        // `LocalIndex` serves the wire grammar only (root documents + dispatch
        // objects) — genuine content-addressed blobs (config blobs) live
        // exclusively in the machine-global blob store (`$OCX_HOME/blobs`),
        // never here. `ChainedIndex::fetch_blob` routes cache-first reads and
        // write-through directly through its attached `BlobStore`
        // (`content_store`); this trait method is never reached in production
        // (see the bare-trait-surface note above) and always reports a clean
        // miss.
        Ok(None)
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::super::index_impl::IndexImpl;
    use super::*;

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::oci::{Algorithm, ImageManifest, Manifest};

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";

    fn make_index(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(Config {
            index_store: IndexStore::new(dir.path().join("index")),
        })
    }

    fn store(dir: &TempDir) -> IndexStore {
        IndexStore::new(dir.path().join("index"))
    }

    fn repo_id() -> oci::Identifier {
        oci::Identifier::new_registry(REPO, REGISTRY)
    }

    fn tagged_id(tag: &str) -> oci::Identifier {
        repo_id().clone_with_tag(tag)
    }

    /// Serialise a flat image manifest and return `(bytes, digest)` so the
    /// bytes genuinely hash to the digest — the A3 write invariant.
    fn image_manifest_bytes() -> (Vec<u8>, oci::Digest) {
        let manifest = Manifest::Image(ImageManifest::default());
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let digest = Algorithm::Sha256.hash(&bytes);
        (bytes, digest)
    }

    /// A minimal fake source: serves one tag → a verbatim flat image manifest
    /// whose bytes hash to the returned digest. Because it overrides
    /// `fetch_manifest_raw_bytes` with matching `(bytes, digest)`, the index
    /// store's A3 verify accepts the persisted objects.
    #[derive(Clone)]
    struct FakeSource {
        tag: String,
    }

    #[async_trait]
    impl super::super::index_impl::IndexImpl for FakeSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![self.tag.clone()]))
        }
        async fn fetch_manifest(
            &self,
            _identifier: &oci::Identifier,
            _op: IndexOperation,
        ) -> Result<Option<(oci::Digest, Manifest)>> {
            let (bytes, digest) = image_manifest_bytes();
            let manifest = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((digest, manifest)))
        }
        async fn fetch_manifest_digest(&self, _: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            let (_, digest) = image_manifest_bytes();
            Ok(Some(digest))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            let (bytes, digest) = image_manifest_bytes();
            let manifest = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, digest, manifest)))
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn source_for_tag(tag: &str) -> super::super::Index {
        super::super::Index::from_impl(FakeSource { tag: tag.to_string() })
    }

    // ── derived source authors a root document (A2/A3) ───────────────────────
    //
    // `refresh_tags` grows the hosted wire grammar. A registry (derived)
    // source resolves the tag to a single-platform image MANIFEST, so
    // `refresh_tags` authors a root document with `tag → content` and writes
    // NOTHING to the dispatch object CAS (a leaf manifest is never copied,
    // A3/B2).

    /// Read the authored root document for `(REGISTRY, REPO)` as a JSON value.
    fn read_root_value(dir: &TempDir) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(store(dir).root_document_path(REGISTRY, REPO)).unwrap()).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_derived_authors_root_with_tag_content() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = source_for_tag("3.28");

        index.refresh_tags(&tagged_id("3.28"), &source).await.unwrap();

        let (_, digest) = image_manifest_bytes();
        let root = read_root_value(&dir);
        assert_eq!(
            root["repository"].as_str(),
            Some(format!("oci://{REGISTRY}/{REPO}").as_str()),
            "a derived refresh authors the oci:// physical pointer from the identifier"
        );
        assert_eq!(
            root["tags"]["3.28"]["content"].as_str(),
            Some(digest.to_string().as_str()),
            "the refreshed tag's content is the resolved manifest digest"
        );
        // A single-platform tag copies no leaf manifest into the dispatch CAS.
        assert!(
            !store(&dir).dispatch_object_path(REGISTRY, REPO, &digest).exists(),
            "a single-platform tag must write nothing to the dispatch object CAS (A3/B2)"
        );
    }

    // ── the authored root's tag carries an RFC3339 observed timestamp ────────

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_derived_stamps_observed_timestamp_on_tags() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        index
            .refresh_tags(&tagged_id("3.28"), &source_for_tag("3.28"))
            .await
            .unwrap();

        let root = read_root_value(&dir);
        let observed = root["tags"]["3.28"]["observed"].as_str().expect("observed present");
        assert!(
            chrono::DateTime::parse_from_rfc3339(observed).is_ok(),
            "observed must be an RFC3339 timestamp, got {observed:?}"
        );
    }

    // ── merge: a second refresh preserves the first tag in the root ──────────

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_derived_merges_new_tag_preserving_existing() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);

        index
            .refresh_tags(&tagged_id("1.0"), &source_for_tag("1.0"))
            .await
            .unwrap();
        index
            .refresh_tags(&tagged_id("2.0"), &source_for_tag("2.0"))
            .await
            .unwrap();

        let root = read_root_value(&dir);
        let tags = root["tags"].as_object().expect("tags object present");
        assert!(tags.contains_key("1.0"), "tag 1.0 must survive the merge");
        assert!(tags.contains_key("2.0"), "tag 2.0 must be present after merge");
    }

    // ── batched derived refresh: N tags land in ONE root read-modify-write ────

    /// A derived source listing several tags, each resolving to a single-platform
    /// image manifest — so a bare `refresh_tags` fans the per-tag fetches out and
    /// then authors ALL tag pointers in one batched commit.
    #[derive(Clone)]
    struct MultiTagSource {
        tags: Vec<String>,
    }

    #[async_trait]
    impl super::super::index_impl::IndexImpl for MultiTagSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(self.tags.clone()))
        }
        async fn fetch_manifest(
            &self,
            _: &oci::Identifier,
            _: IndexOperation,
        ) -> Result<Option<(oci::Digest, Manifest)>> {
            let (bytes, digest) = image_manifest_bytes();
            Ok(Some((digest, serde_json::from_slice(&bytes).unwrap())))
        }
        async fn fetch_manifest_digest(&self, _: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            let (_, digest) = image_manifest_bytes();
            Ok(Some(digest))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            let (bytes, digest) = image_manifest_bytes();
            let manifest = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, digest, manifest)))
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_derived_commits_all_tags_in_one_root_write() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let tags = ["1.0", "2.0", "3.0", "4.0"];
        let source = super::super::Index::from_impl(MultiTagSource {
            tags: tags.iter().map(|t| t.to_string()).collect(),
        });

        // Bare identifier → enumerate the source's tags, fetch each, then author
        // the whole root in a single batched read-modify-write.
        index.refresh_tags(&repo_id(), &source).await.unwrap();

        let root = read_root_value(&dir);
        let tag_map = root["tags"].as_object().expect("tags object present");
        assert_eq!(
            tag_map.len(),
            tags.len(),
            "every listed tag must land in the authored root"
        );
        for tag in tags {
            assert!(
                tag_map.contains_key(tag),
                "tag {tag} must be present after the batched refresh"
            );
        }

        // The batch signature: every tag shares ONE `observed` stamp because the
        // whole root is authored in a single read-modify-write, not one commit
        // per tag (each of which stamps its own `now()`, distinct at sub-second
        // resolution — so this assertion fails against the old O(N²) per-tag loop).
        let observed: std::collections::HashSet<&str> = tag_map
            .values()
            .map(|entry| entry["observed"].as_str().expect("observed present"))
            .collect();
        assert_eq!(
            observed.len(),
            1,
            "all tags must carry one shared observed stamp — proof of a single batched commit, got {observed:?}"
        );
    }

    // ── published refresh fans distinct sibling observations into o/ (B2) ─────

    /// A single-platform observation pointing at `leaf`, plus its own digest.
    /// Varying `leaf` yields a DISTINCT observation (distinct obs digest).
    fn observation_for_leaf(leaf: &oci::Digest) -> (Vec<u8>, oci::Digest) {
        let json =
            format!(r#"{{"platforms":[{{"platform":{{"architecture":"amd64","os":"linux"}},"digest":"{leaf}"}}]}}"#);
        let bytes = json.into_bytes();
        let digest = Algorithm::Sha256.hash(&bytes);
        (bytes, digest)
    }

    /// A PUBLISHED source serving a verbatim root document whose two tags point
    /// at two DISTINCT observation objects — so `refresh_published`'s fan-out
    /// (deduped by content digest) keeps and persists both.
    #[derive(Clone)]
    struct PublishedTwoTagSource;

    #[async_trait]
    impl super::super::index_impl::IndexImpl for PublishedTwoTagSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string(), "2.0".to_string()]))
        }
        async fn fetch_manifest(
            &self,
            id: &oci::Identifier,
            _: IndexOperation,
        ) -> Result<Option<(oci::Digest, Manifest)>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, d, m)| (d, m)))
        }
        async fn fetch_manifest_digest(&self, id: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, d, _)| d))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            id: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            // Each tag resolves to a DISTINCT single-platform observation.
            let leaf_char = match id.tag() {
                Some("1.0") => "a",
                Some("2.0") => "b",
                _ => return Ok(None),
            };
            let leaf = oci::Digest::Sha256(leaf_char.repeat(64));
            let (bytes, digest) = observation_for_leaf(&leaf);
            let observation: super::super::wire::Observation = serde_json::from_slice(&bytes).unwrap();
            let index = super::super::ocx_index::observation_to_index(&observation).unwrap();
            Ok(Some((bytes, digest, Manifest::ImageIndex(index))))
        }
        async fn fetch_root_document(
            &self,
            _: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, super::super::wire::IndexRoot)>> {
            let (_, obs1) = observation_for_leaf(&oci::Digest::Sha256("a".repeat(64)));
            let (_, obs2) = observation_for_leaf(&oci::Digest::Sha256("b".repeat(64)));
            let bytes = format!(
                r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"1.0":{{"content":"{obs1}"}},"2.0":{{"content":"{obs2}"}}}}}}"#
            )
            .into_bytes();
            let root = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, root)))
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_published_persists_both_distinct_sibling_observations() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = super::super::Index::from_impl(PublishedTwoTagSource);

        index.refresh_tags(&repo_id(), &source).await.unwrap();

        // The root's two tags name two DISTINCT observation digests, so the
        // content-digest dedup keeps both — each must land as its own o/ object
        // (a sibling tag pointing at an obs absent from o/ could not resolve
        // offline, B2).
        let (_, obs1) = observation_for_leaf(&oci::Digest::Sha256("a".repeat(64)));
        let (_, obs2) = observation_for_leaf(&oci::Digest::Sha256("b".repeat(64)));
        assert_ne!(obs1, obs2, "prerequisite: the two observations must be distinct");
        assert!(
            store(&dir).dispatch_object_path(REGISTRY, REPO, &obs1).exists(),
            "the first tag's observation object must be persisted under o/"
        );
        assert!(
            store(&dir).dispatch_object_path(REGISTRY, REPO, &obs2).exists(),
            "the second tag's distinct observation object must be persisted under o/"
        );
    }

    // ── concurrent distinct-tag writers all survive (root-file lock) ─────────

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_refresh_different_tags_preserves_all() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = repo_id();

        let mut set: tokio::task::JoinSet<Result<()>> = tokio::task::JoinSet::new();
        for i in 0u8..8 {
            let index = index.clone();
            let ident = id.clone();
            set.spawn(async move {
                let tag = format!("v{i}");
                let source = source_for_tag(&tag);
                index.refresh_tags(&ident.clone_with_tag(&tag), &source).await
            });
        }
        while let Some(joined) = set.join_next().await {
            joined.expect("task panicked").expect("refresh failed");
        }

        let root = read_root_value(&dir);
        assert_eq!(
            root["tags"].as_object().unwrap().len(),
            8,
            "all 8 concurrent writers' tags must survive in the authored root (root-file lock)"
        );
    }

    // ── refresh fans tag persists out concurrently (issue #154) ──────────────

    /// Source whose every `fetch_manifest_raw_bytes` blocks on a shared barrier
    /// sized to the tag count. A concurrent refresh has all fetches in flight
    /// at once, releasing the barrier; a sequential refresh deadlocks on the
    /// first fetch.
    #[derive(Clone)]
    struct BarrierSource {
        tags: Vec<String>,
        barrier: std::sync::Arc<tokio::sync::Barrier>,
    }

    #[async_trait]
    impl super::super::index_impl::IndexImpl for BarrierSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(self.tags.clone()))
        }
        async fn fetch_manifest(
            &self,
            _: &oci::Identifier,
            _: IndexOperation,
        ) -> Result<Option<(oci::Digest, Manifest)>> {
            let (bytes, digest) = image_manifest_bytes();
            Ok(Some((digest, serde_json::from_slice(&bytes).unwrap())))
        }
        async fn fetch_manifest_digest(&self, _: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            let (_, digest) = image_manifest_bytes();
            Ok(Some(digest))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            let (bytes, digest) = image_manifest_bytes();
            let manifest = serde_json::from_slice(&bytes).unwrap();
            // Block until every concurrent persist reaches this point. Releases
            // only if `refresh` fans the persists out in parallel.
            self.barrier.wait().await;
            Ok(Some((bytes, digest, manifest)))
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_persists_tags_concurrently() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = repo_id();

        let tags: Vec<String> = ["1.0", "2.0", "3.0", "4.0", "5.0"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(tags.len()));
        let source = super::super::Index::from_impl(BarrierSource {
            tags: tags.clone(),
            barrier,
        });

        tokio::time::timeout(std::time::Duration::from_secs(5), index.refresh_tags(&id, &source))
            .await
            .expect("refresh must persist tags concurrently; a sequential persist deadlocks on the barrier")
            .expect("refresh failed");

        let root = read_root_value(&dir);
        assert_eq!(
            root["tags"].as_object().unwrap().len(),
            tags.len(),
            "every persisted tag must be recorded in the authored root"
        );
    }

    // ── home routing: refresh writes the wire grammar under its home ─────────

    #[tokio::test(flavor = "multi_thread")]
    async fn root_and_dispatch_land_under_the_wire_grammar_home() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // ObservationSource resolves the tag to a multi-platform observation (an
        // image index), so a dispatch object IS written alongside the root.
        let source = super::super::Index::from_impl(ObservationSource);
        index.refresh_tags(&tagged_id("3.28"), &source).await.unwrap();

        let home = dir.path().join("index");
        // The authored root document lands at <home>/<source>/p/<repo>.json.
        assert!(
            home.join(REGISTRY).join("p").join(format!("{REPO}.json")).exists(),
            "the derived root document must land under the wire-grammar home"
        );
        // The observation object lands at <home>/<source>/p/<repo>/o/sha256/<hex>.json.
        let (_, obs_digest) = observation_bytes();
        assert!(
            store(&dir).dispatch_object_path(REGISTRY, REPO, &obs_digest).exists(),
            "the multi-platform observation must be persisted as a dispatch object under the home"
        );
    }

    // ── list_repositories reads the wire-grammar layout (directory enumeration) ─

    #[tokio::test(flavor = "multi_thread")]
    async fn list_repositories_reflects_persisted_tags() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // A derived source's catalog IS the directory enumeration of `p/` (A2) —
        // seed a root doc via `commit_root_tag`.
        let (_, digest) = image_manifest_bytes();
        index.commit_root_tag(&tagged_id("3.28"), &digest).await.unwrap();

        let repos = index.list_repositories(REGISTRY).await.unwrap();
        assert_eq!(repos, vec![REPO.to_string()]);
    }

    // ── ChainedIndex integration: cache-miss persists a dispatch object ───────

    #[tokio::test(flavor = "multi_thread")]
    async fn chained_fetch_manifest_persists_object_into_local_index() {
        let dir = TempDir::new().unwrap();
        let cache = make_index(&dir);
        let source = super::super::Index::from_impl(ObservationSource);
        let id = tagged_id("3.28");

        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);
        let result = chained
            .fetch_manifest(&id, super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result.is_some(),
            "chained fetch must resolve via the source and persist"
        );

        let (_, obs_digest) = observation_bytes();
        let dispatch_path = store(&dir).dispatch_object_path(REGISTRY, REPO, &obs_digest);
        assert!(
            dispatch_path.exists(),
            "chained fetch_manifest must persist the dispatch object at {dispatch_path:?}"
        );
    }

    // ── latent-bug fix: tag present but dispatch object missing → re-fetch ───

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_object_with_present_tag_refetches_via_chain() {
        let dir = TempDir::new().unwrap();
        let cache = make_index(&dir);
        let id = tagged_id("3.28");
        let (_, obs_digest) = observation_bytes();

        // Seed only the root's tag pointer; leave the dispatch object absent.
        cache.commit_root_tag(&id, &obs_digest).await.unwrap();
        let dispatch_path = store(&dir).dispatch_object_path(REGISTRY, REPO, &obs_digest);
        assert!(!dispatch_path.exists(), "prerequisite: dispatch object must be absent");

        let chained = super::super::Index::from_chained(
            cache,
            vec![super::super::Index::from_impl(ObservationSource)],
            super::super::ChainMode::Default,
        );
        let result = chained
            .fetch_manifest(&id, super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result.is_some(),
            "tag cached but dispatch object missing must re-fetch via the chain and return Some"
        );
        assert_eq!(result.unwrap().0, obs_digest);
        assert!(
            dispatch_path.exists(),
            "the chain walk must have re-persisted the dispatch object"
        );
    }

    // ── corrupt object routing: offline escalates, online Resolve self-heals ──

    /// Seed a valid `(tag → content, dispatch object)` pair, then overwrite
    /// the dispatch-object file with bytes that no longer hash to the digest
    /// — the offline-tamper scenario
    /// (`test_index_selfcontained.py::test_tampered_dispatch_object_
    /// fails_offline_read_with_dataerror`, replicated at the lib layer).
    async fn seed_then_tamper_object(dir: &TempDir) -> oci::Digest {
        let index = make_index(dir);
        let id = tagged_id("3.28");
        let source = super::super::Index::from_impl(ObservationSource);
        let (head, _manifest) = index
            .persist_dispatch(&source, &id)
            .await
            .unwrap()
            .expect("source has a manifest to persist");
        index.commit_root_tag(&id, &head).await.unwrap();
        let (_, obs_digest) = observation_bytes();
        let dispatch_path = store(dir).dispatch_object_path(REGISTRY, REPO, &obs_digest);
        assert!(
            dispatch_path.exists(),
            "prerequisite: the dispatch object must be persisted"
        );
        std::fs::write(&dispatch_path, b"tampered garbage").unwrap();
        obs_digest
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn chained_offline_query_on_corrupt_object_surfaces_dataerror() {
        let dir = TempDir::new().unwrap();
        seed_then_tamper_object(&dir).await;

        // Offline (no source can heal) + a pure Query must NOT read the tampered
        // object as an empty miss (exit 0). It surfaces the corruption as a
        // `DigestMismatch`, which `classify` maps to `DataError` (65).
        let chained = super::super::Index::from_chained(make_index(&dir), vec![], super::super::ChainMode::Offline);
        let result = chained
            .fetch_manifest(&tagged_id("3.28"), super::IndexOperation::Query)
            .await;
        assert!(
            matches!(
                result,
                Err(crate::Error::FileStructure(
                    crate::file_structure::error::Error::DigestMismatch { .. }
                ))
            ),
            "offline query over a tampered object must fail with DigestMismatch, got {result:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn chained_online_resolve_on_corrupt_object_self_heals() {
        let dir = TempDir::new().unwrap();
        let digest = seed_then_tamper_object(&dir).await;

        // Online Resolve: the corrupt local read falls through to the chain
        // walk, which re-fetches and self-heals the tampered dispatch object —
        // resolution succeeds and the object is correct on disk again.
        let chained = super::super::Index::from_chained(
            make_index(&dir),
            vec![super::super::Index::from_impl(ObservationSource)],
            super::super::ChainMode::Default,
        );
        let result = chained
            .fetch_manifest(&tagged_id("3.28"), super::IndexOperation::Resolve)
            .await
            .expect("online Resolve must heal a corrupt object, not error");
        assert!(result.is_some(), "healed Resolve must return the manifest");

        let healed = std::fs::read(store(&dir).dispatch_object_path(REGISTRY, REPO, &digest)).unwrap();
        let (expected, _) = observation_bytes();
        assert_eq!(healed, expected, "the walk must have re-persisted the correct bytes");
    }

    // ── index update reports not-found for an absent package (aggregation) ───

    /// A source that knows no tags and serves no manifests — the `ocx index
    /// update <nonexistent>` case.
    #[derive(Clone)]
    struct EmptySource;

    #[async_trait]
    impl super::super::index_impl::IndexImpl for EmptySource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &oci::Identifier,
            _: IndexOperation,
        ) -> Result<Option<(oci::Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_tags_reports_not_found_for_absent_package() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let empty = super::super::Index::from_impl(EmptySource);

        // Bare identifier: the source lists no tags at all.
        let bare = index.refresh_tags(&repo_id(), &empty).await;
        assert!(
            matches!(
                bare,
                Err(crate::Error::OciIndex(
                    super::super::error::Error::RemoteManifestNotFound(_)
                ))
            ),
            "bare nonexistent package must report not-found, got {bare:?}"
        );

        // Tagged identifier: the tag exists in the request but the source serves
        // no manifest for it — nothing persists, so it must not silently succeed.
        let tagged = index.refresh_tags(&tagged_id("9.9"), &empty).await;
        assert!(
            matches!(
                tagged,
                Err(crate::Error::OciIndex(
                    super::super::error::Error::RemoteManifestNotFound(_)
                ))
            ),
            "tagged nonexistent package must report not-found, got {tagged:?}"
        );
    }

    // ── per-source payload codec: index.ocx.sh observation read-back (A2) ────

    /// Build a valid observation document pointing at the flat image manifest's
    /// digest, returning its verbatim bytes and their digest.
    fn observation_bytes() -> (Vec<u8>, oci::Digest) {
        let (_, leaf) = image_manifest_bytes();
        let json =
            format!(r#"{{"platforms":[{{"platform":{{"architecture":"amd64","os":"linux"}},"digest":"{leaf}"}}]}}"#);
        let bytes = json.into_bytes();
        let digest = Algorithm::Sha256.hash(&bytes);
        (bytes, digest)
    }

    #[test]
    fn decode_index_manifest_dispatches_oci_and_observation() {
        // OCI manifest bytes → parsed manifest (the registry-source codec).
        let (manifest_bytes, _) = image_manifest_bytes();
        assert!(matches!(
            decode_index_manifest(&manifest_bytes).unwrap(),
            Some(oci::Manifest::Image(_))
        ));

        // Observation bytes (no schemaVersion) → synthetic image index.
        let (obs_bytes, _) = observation_bytes();
        match decode_index_manifest(&obs_bytes).unwrap() {
            Some(oci::Manifest::ImageIndex(index)) => assert_eq!(index.manifests.len(), 1),
            other => panic!("observation must decode to a synthetic image index, got {other:?}"),
        }

        // Neither codec → recoverable cache miss, not an error.
        assert!(decode_index_manifest(b"not a manifest at all").unwrap().is_none());
    }

    /// A fake source shaped like [`super::super::OcxIndex`]: a tag resolves to
    /// a verbatim observation (bytes hash to the obs digest) whose single leaf is
    /// the flat image manifest; a digest resolves to that physical manifest.
    #[derive(Clone)]
    struct ObservationSource;

    #[async_trait]
    impl super::super::index_impl::IndexImpl for ObservationSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["3.28".to_string()]))
        }
        async fn fetch_manifest(
            &self,
            id: &oci::Identifier,
            _: IndexOperation,
        ) -> Result<Option<(oci::Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(id)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(&self, id: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            id: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            if id.digest().is_some() {
                // The physical platform-manifest leaf.
                let (bytes, digest) = image_manifest_bytes();
                let manifest = serde_json::from_slice(&bytes).unwrap();
                return Ok(Some((bytes, digest, manifest)));
            }
            // The tag: verbatim observation bytes + the synthetic index the
            // persist recursion walks over.
            let (bytes, digest) = observation_bytes();
            let observation: super::super::wire::Observation = serde_json::from_slice(&bytes).unwrap();
            let index = super::super::ocx_index::observation_to_index(&observation).unwrap();
            Ok(Some((bytes, digest, Manifest::ImageIndex(index))))
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn observation_chain_persists_and_resolves_offline() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = tagged_id("3.28");
        let source = super::super::Index::from_impl(ObservationSource);

        // Persist the dispatch (observation) object and author the tag →
        // obs-digest root pointer, exactly as a chain walk would.
        let (head, _manifest) = index.persist_dispatch(&source, &id).await.unwrap().unwrap();
        let (_, obs_digest) = observation_bytes();
        assert_eq!(
            head, obs_digest,
            "persist_dispatch returns the observation's own digest"
        );
        index.commit_root_tag(&id, &obs_digest).await.unwrap();

        // Fresh index resolves the tag offline through the local index: the obs
        // object decodes to the synthetic image index.
        let fresh = make_index(&dir);
        let (digest, manifest) = fresh
            .fetch_manifest(&id, IndexOperation::Query)
            .await
            .unwrap()
            .expect("tag resolves from the persisted observation");
        assert_eq!(digest, obs_digest, "the resolved digest is the observation digest");
        match manifest {
            Manifest::ImageIndex(index) => assert_eq!(index.manifests.len(), 1),
            other => panic!("expected a synthetic image index from the observation, got {other:?}"),
        }

        // The physical leaf is never copied into the local index (A3/B2) — a
        // digest-addressed query for it is a clean local miss, not an error;
        // fetching it is a registry concern, covered by
        // `resolve_dispatch_returns_absent_leaf_when_object_missing`.
        let (_, leaf) = image_manifest_bytes();
        let leaf_manifest = fresh
            .fetch_manifest(&id.clone_with_digest(leaf), IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            leaf_manifest.is_none(),
            "a leaf platform manifest is never locally cached (A3), so a query for it must miss"
        );
    }

    // ── C1 dispatch-only rework — specification tests (A2/A3/F1) ──────────────
    //
    // Written from the ADR contracts (`adr_index_indirection.md` Decisions A2/A3,
    // arch-verify rulings in plan_one_index), NOT the stub bodies. The C1 stub
    // surface — `persist_dispatch`, `commit_root_tag`, `resolve_dispatch`,
    // `persist_published_root` — is `unimplemented!()`, so every test that drives
    // it is EXPECTED TO PANIC until C1 lands; that panic is the passing signal
    // for this phase. `stage_dispatch_bytes` and the Index-wrapper
    // `fetch_root_document` default are already implemented and pass now
    // (regression coverage).

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A two-platform observation as verbatim wire bytes, paired with its
    /// digest — an index.ocx.sh-style DISPATCH object (decodes to a synthetic
    /// image index, never a bare leaf manifest).
    fn two_platform_observation() -> (Vec<u8>, oci::Digest) {
        let leaf_a = format!("sha256:{}", "a".repeat(64));
        let leaf_b = format!("sha256:{}", "b".repeat(64));
        let json = format!(
            r#"{{"platforms":[{{"platform":{{"architecture":"amd64","os":"linux"}},"digest":"{leaf_a}"}},{{"platform":{{"architecture":"arm64","os":"linux"}},"digest":"{leaf_b}"}}]}}"#
        );
        let bytes = json.into_bytes();
        let digest = Algorithm::Sha256.hash(&bytes);
        (bytes, digest)
    }

    /// Root-document bytes (wire grammar) that point tag `3.28` at `content` and
    /// carry an `oci://<REGISTRY>/<REPO>` physical pointer (passes the C3
    /// `parse_physical_repository` cross-check).
    fn root_bytes_for(content: &oci::Digest) -> Vec<u8> {
        format!(
            r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"3.28":{{"content":"{content}","observed":"2026-07-18T09:00:00Z"}}}}}}"#
        )
        .into_bytes()
    }

    /// A fetch-counting source: a tag resolves to a verbatim two-platform
    /// observation decoded as a synthetic image index (an index.ocx.sh dispatch
    /// object). Every `fetch_manifest_raw_bytes` bumps a shared counter, so a
    /// test can prove `persist_dispatch` fetches exactly once — never walking
    /// child manifests (A3).
    #[derive(Clone)]
    struct CountingDispatchSource {
        fetches: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl super::super::index_impl::IndexImpl for CountingDispatchSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["3.28".to_string()]))
        }
        async fn fetch_manifest(
            &self,
            id: &oci::Identifier,
            _: IndexOperation,
        ) -> Result<Option<(oci::Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(id)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(&self, id: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            id: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            self.fetches.fetch_add(1, Ordering::SeqCst);
            if id.digest().is_some() {
                // A child leaf — reached ONLY if the caller wrongly walks the
                // image index's children. The counter catches that recursion.
                let (bytes, digest) = image_manifest_bytes();
                let manifest = serde_json::from_slice(&bytes).unwrap();
                return Ok(Some((bytes, digest, manifest)));
            }
            let (bytes, digest) = two_platform_observation();
            let observation: super::super::wire::Observation = serde_json::from_slice(&bytes).unwrap();
            let index = super::super::ocx_index::observation_to_index(&observation).unwrap();
            Ok(Some((bytes, digest, Manifest::ImageIndex(index))))
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    // ── persist_dispatch (A3): one dispatch object, no child walk ────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn persist_dispatch_writes_one_object_for_multi_platform_tag_without_recursion() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = tagged_id("3.28");
        let fetches = Arc::new(AtomicUsize::new(0));
        let source = super::super::Index::from_impl(CountingDispatchSource {
            fetches: fetches.clone(),
        });

        let (head, head_manifest) = index.persist_dispatch(&source, &id).await.unwrap().unwrap();
        let (obs_bytes, obs_digest) = two_platform_observation();
        assert_eq!(
            head, obs_digest,
            "persist_dispatch returns the dispatch object's own digest"
        );
        assert!(
            matches!(head_manifest, Manifest::ImageIndex(_)),
            "persist_dispatch returns the decoded dispatch manifest alongside the digest"
        );

        // Exactly ONE dispatch object, at the `.json` wire path, byte-identical.
        let dispatch_path = store(&dir).dispatch_object_path(REGISTRY, REPO, &obs_digest);
        assert!(
            dispatch_path.exists(),
            "the dispatch object must exist at {dispatch_path:?}"
        );
        assert_eq!(
            std::fs::read(&dispatch_path).unwrap(),
            obs_bytes,
            "the dispatch object's bytes must be written verbatim"
        );

        // Zero child manifests: the package's o/sha256 dir holds exactly one file.
        let object_dir = dispatch_path.parent().unwrap();
        let object_count = std::fs::read_dir(object_dir).unwrap().count();
        assert_eq!(
            object_count, 1,
            "a dispatch persist writes exactly one o/ object, never child manifests"
        );

        // No child-walk recursion: the source was fetched exactly once.
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "persist_dispatch must fetch the dispatch object once, never walk its children"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn persist_dispatch_writes_nothing_for_single_platform_tag() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = tagged_id("3.28");
        // FakeSource resolves the tag to a flat single-platform image MANIFEST.
        let source = source_for_tag("3.28");

        let (head, head_manifest) = index.persist_dispatch(&source, &id).await.unwrap().unwrap();
        let (_, manifest_digest) = image_manifest_bytes();
        assert_eq!(
            head, manifest_digest,
            "a single-platform tag's content is the leaf manifest digest itself"
        );
        assert!(
            matches!(head_manifest, Manifest::Image(_)),
            "persist_dispatch returns the decoded leaf manifest alongside its digest"
        );

        // A leaf platform manifest is never copied into the local index (A3/B2).
        let dispatch_path = store(&dir).dispatch_object_path(REGISTRY, REPO, &manifest_digest);
        assert!(
            !dispatch_path.exists(),
            "a single-platform tag must write nothing to the dispatch object CAS"
        );
        let object_dir = dispatch_path.parent().unwrap().parent().unwrap(); // .../o/
        assert!(
            !object_dir.exists() || std::fs::read_dir(object_dir).unwrap().next().is_none(),
            "the dispatch object directory must be absent or empty for a single-platform tag"
        );
    }

    // ── commit_root_tag (A2/F1): OCX-authored derived root ────────────────────

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_root_tag_authors_derived_root_with_oci_repository_and_observed() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let content = oci::Digest::Sha256("a".repeat(64));

        index.commit_root_tag(&tagged_id("3.28"), &content).await.unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store(&dir).root_document_path(REGISTRY, REPO)).unwrap()).unwrap();
        assert!(
            raw["repository"].as_str().unwrap().starts_with("oci://"),
            "a derived root's repository must be an oci:// physical pointer, got {:?}",
            raw["repository"]
        );
        let tag = &raw["tags"]["3.28"];
        assert_eq!(
            tag["content"].as_str().unwrap(),
            content.to_string(),
            "the authored tag's content must be the committed digest"
        );
        let observed = tag["observed"]
            .as_str()
            .expect("an authored tag carries an observed timestamp");
        assert!(
            chrono::DateTime::parse_from_rfc3339(observed).is_ok(),
            "observed must be an RFC3339 timestamp bumped on this refresh, got {observed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_root_tag_upsert_preserves_existing_tags() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);

        index
            .commit_root_tag(&tagged_id("3.28"), &oci::Digest::Sha256("a".repeat(64)))
            .await
            .unwrap();
        index
            .commit_root_tag(&tagged_id("3.27"), &oci::Digest::Sha256("b".repeat(64)))
            .await
            .unwrap();

        let raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store(&dir).root_document_path(REGISTRY, REPO)).unwrap()).unwrap();
        let tags = raw["tags"].as_object().expect("tags object present");
        assert!(
            tags.contains_key("3.28"),
            "the first-committed tag must survive the second upsert"
        );
        assert!(
            tags.contains_key("3.27"),
            "the second-committed tag must be present (merge, not overwrite)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_root_tag_rejects_repository_mismatched_existing_root() {
        use crate::cli::{ClassifyExitCode, ExitCode};
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);

        // Seed an existing derived root whose repository names a DIFFERENT
        // physical host than the one this identifier implies — a repository
        // cross-check failure is a hard DataError (F1), never a silent overwrite.
        let store = store(&dir);
        let root_path = store.root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        std::fs::write(
            &root_path,
            br#"{"repository":"oci://wrong.example.com/cmake","tags":{}}"#,
        )
        .unwrap();

        let result = index
            .commit_root_tag(&tagged_id("3.28"), &oci::Digest::Sha256("a".repeat(64)))
            .await;
        let err = result.expect_err("a repository-mismatched existing root must be rejected, never overwritten");
        assert_eq!(
            err.classify(),
            Some(ExitCode::DataError),
            "a repository cross-check failure must classify as DataError, got {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_root_tag_recovery_starts_fresh_and_drops_every_prior_tag_on_malformed_content_digest() {
        // Locks the accepted data-loss-on-corruption behavior
        // (`adr_index_indirection.md` amendment 2026-07-19): `DerivedTag::content`
        // is an `oci::Digest`, whose exact-wire deserialize fails the whole
        // `DerivedRoot` parse on a malformed value — the ONLY trigger for the
        // kill-9 "start fresh" recovery branch in `commit_root_tag` (a derived
        // root is always OCX's own prior write, so an unparseable existing
        // document is treated as a crashed-write artifact, never a
        // trust-boundary concern). "Starting fresh" REPLACES the whole tags
        // map, so committing a NEW tag against a malformed root silently
        // drops every OTHER tag that root held — a deliberate, accepted
        // tradeoff, not a partial-merge recovery.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);

        let store = store(&dir);
        let root_path = store.root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        std::fs::write(
            &root_path,
            format!(
                r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"3.27":{{"content":"not-a-digest","observed":"2026-01-01T00:00:00Z"}}}}}}"#
            ),
        )
        .unwrap();

        index
            .commit_root_tag(&tagged_id("3.28"), &oci::Digest::Sha256("c".repeat(64)))
            .await
            .unwrap();

        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&root_path).unwrap()).unwrap();
        let tags = raw["tags"].as_object().expect("tags object present");
        assert_eq!(
            tags.len(),
            1,
            "the malformed prior root must be replaced wholesale — only the newly committed tag survives"
        );
        assert!(
            tags.contains_key("3.28"),
            "the newly committed tag must be present after the fresh-start recovery"
        );
        assert!(
            !tags.contains_key("3.27"),
            "the prior tag from the unparseable root is GONE — accepted data loss on corruption, not a merge"
        );
    }

    // ── resolve_dispatch (A3 read path): typed Dispatch / AbsentLeaf / None ──

    /// Seed a wire-grammar root doc (tag `3.28` → `obs_digest`) plus its
    /// dispatch object directly on disk, so `resolve_dispatch` (the method under
    /// test) is the only code exercised.
    async fn seed_root_and_dispatch(dir: &TempDir) -> oci::Digest {
        let store = store(dir);
        let (obs_bytes, obs_digest) = two_platform_observation();
        store
            .write_dispatch_object(REGISTRY, REPO, &obs_digest, &obs_bytes)
            .await
            .unwrap();
        let root_path = store.root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        std::fs::write(&root_path, root_bytes_for(&obs_digest)).unwrap();
        obs_digest
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_returns_dispatch_for_derived_and_never_creates_catalog() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let obs_digest = seed_root_and_dispatch(&dir).await;

        let root_path = store(&dir).root_document_path(REGISTRY, REPO);
        let root_before = std::fs::read(&root_path).unwrap();

        let resolution = index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .unwrap()
            .expect("a present root + dispatch object resolves");
        match resolution {
            DispatchResolution::Dispatch { content, manifest } => {
                assert_eq!(content, obs_digest, "Dispatch carries the tag's content digest");
                assert!(
                    matches!(&*manifest, oci::Manifest::ImageIndex(_)),
                    "a dispatch object decodes to an image index"
                );
            }
            DispatchResolution::AbsentLeaf { .. } => panic!("expected Dispatch, got AbsentLeaf"),
        }

        // Derived resolve routes through read_root_uncatalogued (A2 "two ifs"):
        // it must NEVER materialize a c/index.json on a catalog-less source.
        assert!(
            !store(&dir).source_catalog_path(REGISTRY).exists(),
            "a derived resolve must never create c/index.json"
        );
        // A read never rewrites the root (observed bumped on refresh only).
        assert_eq!(
            std::fs::read(&root_path).unwrap(),
            root_before,
            "resolve must not rewrite the root document"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_returns_absent_leaf_when_object_missing() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // Seed only the root (tag → content), NOT the dispatch object.
        let (_, content) = two_platform_observation();
        let root_path = store(&dir).root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        std::fs::write(&root_path, root_bytes_for(&content)).unwrap();

        let resolution = index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .unwrap()
            .expect("a present root resolves to a typed outcome, never a bare miss");
        match resolution {
            DispatchResolution::AbsentLeaf { content: resolved } => assert_eq!(
                resolved, content,
                "AbsentLeaf preserves the tag's content digest for source-kind-routed recovery"
            ),
            DispatchResolution::Dispatch { .. } => panic!("expected AbsentLeaf (object absent), got Dispatch"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_returns_none_when_root_absent() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let result = index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "an unknown root is a clean miss the caller turns into a chain walk"
        );
    }

    // ── offline yank refusal wiring (F3): surface_root_status via resolve_dispatch ─

    /// A wire-grammar root document whose tag `3.28` is marked `yanked`, physical
    /// pointer `oci://<REGISTRY>/<REPO>` so the C3 cross-check passes.
    fn yanked_root_bytes(content: &oci::Digest) -> Vec<u8> {
        format!(
            r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"3.28":{{"content":"{content}","observed":"2026-07-18T09:00:00Z","yanked":true}}}}}}"#
        )
        .into_bytes()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_refuses_yanked_tag_offline_unless_allowed() {
        use crate::cli::{ClassifyExitCode, ExitCode};
        let dir = TempDir::new().unwrap();
        let content = oci::Digest::Sha256("a".repeat(64));

        // Seed only the yanked root — no dispatch object needed: the refusal
        // fires at surface_root_status, before the o/ lookup.
        let store = store(&dir);
        let root_path = store.root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        std::fs::write(&root_path, yanked_root_bytes(&content)).unwrap();

        // Default (allow_yanked = false): an offline read of a yanked tag is
        // refused with zero network — the OFFLINE counterpart to OcxIndex's
        // surface_status (F3). Catches an `allow_yanked` mis-wire in resolve_dispatch.
        let refusing = make_index(&dir);
        let refused = refusing.resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived).await;
        let err = refused.expect_err("a yanked tag must be refused offline when allow_yanked is false");
        assert!(
            matches!(
                err,
                crate::Error::OciIndex(super::super::error::Error::YankedRefused { .. })
            ),
            "expected YankedRefused, got {err:?}"
        );
        assert_eq!(
            err.classify(),
            Some(ExitCode::DataError),
            "a yank refusal classifies as DataError"
        );

        // Opting in (OCX_ALLOW_YANKED, threaded via with_allow_yanked) passes the
        // surface check — the tag resolves (its dispatch object is unseeded, so
        // AbsentLeaf), never refused.
        let allowing = make_index(&dir).with_allow_yanked(true);
        let resolution = allowing
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .expect("allow_yanked must not refuse a yanked tag")
            .expect("a present root resolves to a typed outcome");
        assert!(
            matches!(resolution, DispatchResolution::AbsentLeaf { content: c } if c == content),
            "allow_yanked must resolve the yanked tag's content as AbsentLeaf (its object is unseeded)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_published_crosschecks_catalog() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let obs_digest = seed_root_and_dispatch(&dir).await;
        // No c/index.json seeded — a Published read cross-checks the catalog and
        // self-heals a missing entry (F1), so a published resolve MUST create
        // c/index.json. That materialization is the observable difference from a
        // Derived resolve, which routes through the catalog-free read.
        assert!(
            !store(&dir).source_catalog_path(REGISTRY).exists(),
            "prerequisite: no catalog on disk yet"
        );

        let resolution = index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Published)
            .await
            .unwrap()
            .expect("a published root + dispatch object resolves");
        assert!(
            matches!(resolution, DispatchResolution::Dispatch { ref content, .. } if *content == obs_digest),
            "a published resolve returns the dispatch object"
        );
        assert!(
            store(&dir).source_catalog_path(REGISTRY).exists(),
            "a published resolve routes through read_root, self-healing its c/index.json catalog entry"
        );
    }

    // ── resolve_dispatch digest-addressed branch: o/ present ⇒ Dispatch ──────

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_digest_addressed_present_object_is_dispatch() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let (obs_bytes, obs_digest) = two_platform_observation();
        store(&dir)
            .write_dispatch_object(REGISTRY, REPO, &obs_digest, &obs_bytes)
            .await
            .unwrap();

        // A digest-addressed identifier looks the digest up directly in `o/`,
        // never reading a root document — so the source kind is irrelevant.
        let id = repo_id().clone_with_digest(obs_digest.clone());
        let resolution = index
            .resolve_dispatch(&id, SourceKind::Derived)
            .await
            .unwrap()
            .expect("a present dispatch object resolves");
        match resolution {
            DispatchResolution::Dispatch { content, manifest } => {
                assert_eq!(content, obs_digest, "Dispatch carries the addressed digest");
                assert!(
                    matches!(&*manifest, oci::Manifest::ImageIndex(_)),
                    "a dispatch object decodes to an image index"
                );
            }
            DispatchResolution::AbsentLeaf { .. } => panic!("expected Dispatch for a present digest-addressed object"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_digest_addressed_absent_object_is_absent_leaf() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let (_, digest) = two_platform_observation();

        // Nothing on disk — the digest-addressed lookup misses in `o/` and
        // surfaces a typed AbsentLeaf, never a bare None (A3).
        let id = repo_id().clone_with_digest(digest.clone());
        let resolution = index
            .resolve_dispatch(&id, SourceKind::Derived)
            .await
            .unwrap()
            .expect("a digest-addressed miss is a typed AbsentLeaf, never a bare miss");
        match resolution {
            DispatchResolution::AbsentLeaf { content } => assert_eq!(content, digest),
            DispatchResolution::Dispatch { .. } => panic!("expected AbsentLeaf for an absent digest-addressed object"),
        }
    }

    // ── persist_published_root (A2/F1): verbatim copy + derived catalog entry ─

    #[tokio::test(flavor = "multi_thread")]
    async fn persist_published_root_lands_verbatim_bytes_and_derives_catalog_entry() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // Non-canonical whitespace: a re-serialization would change the bytes,
        // so a verbatim write is what keeps the copy byte-identical to the site
        // and its catalog entry == sha256(bytes) (copy-a-mirror, A2/F1).
        let bytes = br#"{  "repository" : "oci://ghcr.io/ocx-contrib/cmake" ,  "tags" : { }  }"#.to_vec();

        index.persist_published_root(&tagged_id("3.28"), &bytes).await.unwrap();

        let store = store(&dir);
        let on_disk = std::fs::read(store.root_document_path(REGISTRY, REPO)).unwrap();
        assert_eq!(
            on_disk, bytes,
            "the published root must land byte-identical, never re-serialized"
        );

        let catalog: crate::oci::index::CatalogIndex =
            serde_json::from_slice(&std::fs::read(store.source_catalog_path(REGISTRY)).unwrap()).unwrap();
        assert_eq!(
            catalog.get(REPO),
            Some(&IndexStore::root_catalog_entry(&bytes)),
            "the catalog entry must be exactly sha256(root bytes), committed alongside the root"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn persist_published_root_transaction_preserves_other_catalog_entries() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let store = store(&dir);

        // A prior catalog entry for a DIFFERENT package of the same source.
        let mut seed = store.begin_catalog_transaction(REGISTRY).await.unwrap();
        seed.catalog()
            .insert("other/tool".to_string(), "sha256:existing".to_string());
        seed.commit(None).await.unwrap();

        let bytes = br#"{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{}}"#.to_vec();
        index.persist_published_root(&tagged_id("3.28"), &bytes).await.unwrap();

        let catalog: crate::oci::index::CatalogIndex =
            serde_json::from_slice(&std::fs::read(store.source_catalog_path(REGISTRY)).unwrap()).unwrap();
        assert_eq!(
            catalog.get("other/tool"),
            Some(&"sha256:existing".to_string()),
            "the transaction must re-read + reconcile, never clobber a pre-existing catalog entry"
        );
        assert_eq!(
            catalog.get(REPO),
            Some(&IndexStore::root_catalog_entry(&bytes)),
            "this package's own entry must be committed alongside"
        );
    }

    // ── stage_dispatch_bytes: verified dispatch write (implemented, passes) ──

    #[tokio::test(flavor = "multi_thread")]
    async fn stage_dispatch_bytes_writes_verified_object() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let (obs_bytes, obs_digest) = two_platform_observation();

        index
            .stage_dispatch_bytes(&repo_id(), &obs_digest, &obs_bytes)
            .await
            .unwrap();

        let path = store(&dir).dispatch_object_path(REGISTRY, REPO, &obs_digest);
        assert!(
            path.exists(),
            "the staged dispatch object must land at the wire .json path"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            obs_bytes,
            "the staged bytes must be verbatim"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stage_dispatch_bytes_rejects_wrong_digest_and_writes_nothing() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // A well-formed digest the bytes do NOT hash to.
        let wrong = oci::Digest::Sha256("a".repeat(64));
        let bytes = b"these bytes do not hash to the claimed digest";

        let result = index.stage_dispatch_bytes(&repo_id(), &wrong, bytes).await;
        assert!(
            matches!(
                result,
                Err(crate::Error::FileStructure(
                    crate::file_structure::error::Error::DigestMismatch { .. }
                ))
            ),
            "a digest mismatch must be a hard error (A4), got {result:?}"
        );
        assert!(
            !store(&dir).dispatch_object_path(REGISTRY, REPO, &wrong).exists(),
            "a rejected stage must leave nothing on disk"
        );
    }

    // ── Index wrapper forwards fetch_root_document; default ⇒ None ───────────

    #[tokio::test(flavor = "multi_thread")]
    async fn index_wrapper_fetch_root_document_defaults_to_none_for_registry_source() {
        // A derived / plain-OCI source publishes no verbatim root: the IndexImpl
        // default returns Ok(None), and the Index wrapper forwards it (A2/H).
        let source = super::super::Index::from_impl(EmptySource);
        assert!(
            source.fetch_root_document(&repo_id()).await.unwrap().is_none(),
            "a registry-backed source serves no verbatim root document"
        );
    }

    // ── sync_catalog: catalog-key boundary validation (CWE-22, F2) ────────────
    //
    // A published index's `c/index.json` keys are attacker-controlled for a
    // mirrored / compromised source. A key that is not a well-formed repository
    // path — `../../victim`, a bare `..`, an absolute `/tmp/x`, an internal
    // `a/../b`, a Windows-style `..\victim` — must fail the whole sync closed
    // (`MalformedCatalogKey` → DataError) BEFORE any filesystem path is built,
    // so nothing is written outside the index home.

    /// A minimal [`IndexTransport`](super::super::IndexTransport) serving a
    /// `format_version: 1` config and a caller-supplied `c/index.json` body, so
    /// `LocalIndex::sync_catalog` reaches the catalog-key validation with an
    /// attacker-chosen key set.
    #[derive(Clone)]
    struct CatalogTransport {
        catalog_json: String,
    }

    #[async_trait]
    impl super::super::IndexTransport for CatalogTransport {
        async fn get(&self, url: &str, _if_none_match: Option<&str>) -> Result<super::super::IndexFetch> {
            if url.ends_with("/config.json") {
                return Ok(super::super::IndexFetch::Found {
                    bytes: br#"{"format_version":1}"#.to_vec(),
                    etag: None,
                });
            }
            if url.ends_with("/c/index.json") {
                return Ok(super::super::IndexFetch::Found {
                    bytes: self.catalog_json.clone().into_bytes(),
                    etag: Some("etag-1".to_string()),
                });
            }
            Ok(super::super::IndexFetch::NotFound)
        }

        fn box_clone(&self) -> Box<dyn super::super::IndexTransport> {
            Box::new(self.clone())
        }
    }

    /// Serialises a catalog with `key` (untrusted) plus one always-valid key so
    /// the map is non-trivial. `serde_json` handles all escaping, so a
    /// backslash in `key` round-trips into the JSON body verbatim.
    fn catalog_with_key(key: &str) -> String {
        let mut map = serde_json::Map::new();
        map.insert(
            key.to_string(),
            serde_json::Value::String(format!("sha256:{}", "a".repeat(64))),
        );
        map.insert(
            "kitware/cmake".to_string(),
            serde_json::Value::String(format!("sha256:{}", "b".repeat(64))),
        );
        serde_json::Value::Object(map).to_string()
    }

    fn ocx_source_with_catalog(catalog_json: String) -> super::super::OcxIndex {
        use crate::oci::client::test_transport::{StubTransport, StubTransportData};
        super::super::OcxIndex::new(super::super::OcxIndexConfig {
            transport: Box::new(CatalogTransport { catalog_json }),
            base_url: "https://index.test".to_string(),
            namespace: REGISTRY.to_string(),
            client: oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new()))),
            allow_yanked: false,
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sync_catalog_refuses_traversing_catalog_keys_and_writes_nothing() {
        // `REGISTRY` is `example.com`, a valid registry host — `<ns>/<key>`
        // therefore parses through the canonical grammar.
        let malicious_keys = ["../../victim", "..", "/tmp/victim", "a/../b", "..\\victim"];

        for key in malicious_keys {
            let outside = TempDir::new().unwrap();
            // A sentinel a sibling of the index home; its survival proves the
            // refused sync never wrote outside the home.
            let sentinel = outside.path().join("victim.json");
            std::fs::write(&sentinel, b"original").unwrap();
            let home = outside.path().join("index");

            let local = LocalIndex::new(Config {
                index_store: IndexStore::new(&home),
            });
            let source = ocx_source_with_catalog(catalog_with_key(key));

            let error = local
                .sync_catalog(&source)
                .await
                .expect_err(&format!("a malformed catalog key {key:?} must fail the sync closed"));
            assert!(
                matches!(
                    error,
                    crate::Error::OciIndex(super::super::error::Error::MalformedCatalogKey { .. })
                ),
                "expected MalformedCatalogKey for {key:?}, got {error:?}"
            );

            assert_eq!(
                std::fs::read(&sentinel).unwrap(),
                b"original",
                "a refused catalog sync ({key:?}) must never write outside the index home"
            );
            // Nothing was snapshotted — no per-source catalog landed on disk.
            assert!(
                !home.join(REGISTRY).join("c").join("index.json").exists(),
                "a refused catalog sync ({key:?}) must not persist a catalog"
            );
        }
    }

    #[test]
    fn validate_catalog_key_accepts_well_formed_repositories() {
        for key in ["kitware/cmake", "cmake", "org/sub/tool", "foo.bar_baz-qux/a1"] {
            LocalIndex::validate_catalog_key(REGISTRY, key)
                .unwrap_or_else(|error| panic!("well-formed catalog key {key:?} must pass: {error}"));
        }
    }

    #[test]
    fn validate_catalog_key_rejects_traversal_and_malformed_keys() {
        // Traversal escapes, an absolute path, a Windows-style backslash escape,
        // an internal `..`, plus grammar violations (empty, uppercase, a
        // tag-smuggling `:`), all refused as MalformedCatalogKey.
        for key in [
            "../../victim",
            "..",
            "/tmp/victim",
            "a/../b",
            "..\\victim",
            "",
            "Foo",
            "pkg:tag",
        ] {
            assert!(
                matches!(
                    LocalIndex::validate_catalog_key(REGISTRY, key),
                    Err(crate::Error::OciIndex(
                        super::super::error::Error::MalformedCatalogKey { .. }
                    ))
                ),
                "malformed catalog key {key:?} must be rejected as MalformedCatalogKey"
            );
        }
    }
}
