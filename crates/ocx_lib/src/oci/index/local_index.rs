// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, StreamExt, TryStreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::file_structure::{IndexStore, SOURCE_LOCK_TIMEOUT};
use crate::{Result, log, oci, package::tag::Tag};

use super::wire::{IndexFormatConfig, gate_format_version};
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
///
/// Public because it is one factor of a ceiling stated elsewhere: the CLI's
/// per-package fan-out multiplies by this to state its in-flight bound
/// (`adr_servable_index_snapshot.md` C-024), and a test that hardcodes `64`
/// beside that product reads green when this constant moves.
pub const TAG_REFRESH_CONCURRENCY: usize = 64;

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
    /// Sources whose local `config.json` has already passed the version gate
    /// ([`Self::check_format_version`], C-005) — the memo that makes the check
    /// once-per-source rather than once-per-read. Only a *passing* outcome is
    /// recorded, absence included; a refusal is an error, never a cached state.
    /// Shared across clones, since a clone reads the same index home.
    gated_sources: Arc<RwLock<HashSet<String>>>,
}

impl LocalIndex {
    pub fn new(config: Config) -> Self {
        Self {
            index_store: config.index_store,
            allow_yanked: false,
            gated_sources: Arc::new(RwLock::new(HashSet::new())),
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
    ///   [`Self::persist_published_root`] and persist each referenced dispatch
    ///   object.
    /// - **Derived** (a plain OCI registry — no verbatim root to copy): OCX
    ///   authors the root field-wise through [`Self::commit_root_tag`], bumping
    ///   `observed`, after persisting each tag's dispatch object.
    ///
    /// A tagged identifier (`cmake:3.28`) refreshes only that tag; a bare
    /// identifier (`cmake`) first enumerates the source's tags.
    ///
    /// Jurisdiction-unaware by design: whether `source` can express
    /// `identifier` at all is the **caller's** call. `ocx index update` picks
    /// the source that will answer for each package before calling here, and
    /// reroutes a name the index declines to the registry.
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

    /// Published-source refresh (`adr_index_indirection.md` A2/F1, amended: the
    /// local index is AUTHORED): persist the dispatch objects this write adopts,
    /// then merge those tags into the local root.
    ///
    /// The identifier's shape is the scope. A tagged identifier
    /// (`ocx index update pkg:3.28`) adopts that one tag and touches nothing
    /// else. A bare one (`ocx index update pkg`) adopts every tag the remote
    /// lists plus the package-level fields — and still keeps a tag only the
    /// local copy holds, because merge never deletes.
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
        let scope = match identifier.tag() {
            Some(tag) => RootScope::Tag(tag),
            None => RootScope::Package,
        };
        // Every tag this write adopts needs its dispatch object present, or the
        // pin it lands could not resolve offline (B2). Tags NOT adopted keep
        // whatever object they already had — their pins are untouched, so their
        // objects are too. Dedup by content digest (a dispatch object is
        // content-addressed) so tags a re-push aliased onto one index fetch it
        // once — one representative tag per distinct digest is enough, since
        // `persist_dispatch` fetches the object by tag.
        let mut seen: std::collections::HashSet<oci::Digest> = std::collections::HashSet::new();
        let tags: Vec<String> = root
            .tags
            .iter()
            .filter(|(tag, _)| match scope {
                RootScope::Tag(named) => *tag == named,
                RootScope::Package => true,
            })
            .filter(|(_, entry)| seen.insert(entry.content.clone()))
            .map(|(tag, _)| tag.clone())
            .collect();

        // Persist each distinct tag's dispatch object concurrently — each is a
        // latency-bound fetch + a CAS write to a distinct `o/` path, so the burst
        // is capped at `TAG_REFRESH_CONCURRENCY` (issue #154's polite-citizen
        // contract, carried forward).
        let this = self;
        stream::iter(tags)
            .map(|tag| {
                let tagged = identifier.clone_with_tag(&tag);
                async move {
                    log::debug!("Refreshing published tag '{}' for identifier '{}'.", tag, identifier);
                    // `persist_dispatch` returns the fetched bytes/digest/manifest
                    // — a refresh only needs the write side-effect.
                    this.persist_dispatch(source, &tagged).await.map(|_| ())
                }
            })
            .buffer_unordered(TAG_REFRESH_CONCURRENCY)
            .try_collect::<()>()
            .await?;

        self.commit_published_root(identifier, bytes, scope).await
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

        // D7's half of `records_root_tag`, hoisted: whether a name is reserved is
        // decidable from the name alone. Left downstream it costs a fetch AND
        // stages an image index into `o/` that no root ever names — an orphan in
        // a store outside the GC graph. The bare-manifest half cannot move: that
        // verdict needs the fetched manifest's shape.
        let tags: Vec<String> = tags
            .into_iter()
            .filter(|tag| {
                let indexable = !Tag::is_reserved_str(tag);
                if !indexable {
                    log::debug!("Tag '{tag}' is reserved and is never a version — not fetched.");
                }
                indexable
            })
            .collect();

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
                        Some((_, content, manifest)) => {
                            Ok::<_, crate::Error>(records_root_tag(&tag, &manifest).then_some((tag, content)))
                        }
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
            // There WERE candidate tags; none could carry a version — each
            // resolved to no manifest, was reserved (filtered above), or was a
            // bare manifest (`records_root_tag`). Same per-identifier not-found
            // exit as the empty-tags case above, but a distinct cause and so a
            // distinct message: "no indexable tag" is not "package absent".
            return Err(super::error::Error::NoIndexableTag(identifier.to_string()).into());
        }

        // Author the derived root's tag pointers in ONE lock acquisition + ONE
        // root read-modify-write (`adr_index_indirection.md` A2/F1). Committing
        // each tag separately would re-lock and rewrite the whole root per tag —
        // O(N²) bytes for N tags; the batch merge preserves every other tag.
        self.commit_root_tags(identifier, &fetched).await
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
    ///   The bytes are the OCI image index the tag resolved to, identical in
    ///   shape whether the source is a plain registry or a published
    ///   `index.ocx.sh` copy of one. When the caller has ALREADY fetched the
    ///   bytes (a [`DispatchResolution::AbsentDispatch`] recovery that decoded as
    ///   an image index), it self-heals via [`Self::stage_dispatch_bytes`]
    ///   instead, to avoid the double fetch this method would perform.
    /// - [`oci::Manifest::Image`] ⇒ write **nothing** to `o/`; a single-platform
    ///   tag's `content` is the leaf manifest digest itself, and a leaf platform
    ///   manifest is never copied into the local index (A3/B2) — it is fetched on
    ///   demand from the physical registry.
    ///
    /// Returns the fetched `(bytes, digest, manifest)` verbatim — the dispatch
    /// object's bytes and digest with its decoded shape, or the leaf manifest's
    /// own — or `Ok(None)` when the source has no manifest for `identifier`.
    /// Callers that only need the digest for root growth (`refresh_published`,
    /// `refresh_derived`) discard the rest; `ChainedIndex`'s AbsentDispatch
    /// recovery returns the manifest directly to the caller instead of attempting
    /// a doomed local-storage read-back (a leaf is never written to `o/`, A3),
    /// and caches the bytes in the machine-global blob store, which is where a
    /// leaf belongs.
    pub async fn persist_dispatch(
        &self,
        source: &super::Index,
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
        let Some((bytes, digest, manifest)) = source.fetch_manifest_raw_bytes(identifier).await? else {
            return Ok(None);
        };
        // Dispatch on the decoded manifest shape — NEVER walk child manifests (A3):
        //  - image index ⇒ the dispatch object; write it verbatim into `o/` via
        //    `stage_dispatch_bytes` (recompute-and-verified against the
        //    source-claimed digest, A4) — the bytes are already in hand, so this
        //    never double-fetches.
        //  - single-platform image manifest ⇒ its own digest IS the tag's
        //    `content`, and a leaf platform manifest is never copied into the
        //    local index (A3/B2) — write nothing.
        if let oci::Manifest::ImageIndex(_) = &manifest {
            self.stage_dispatch_bytes(identifier, &digest, &bytes).await?;
        }
        Ok(Some((bytes, digest, manifest)))
    }

    /// Like [`Self::persist_dispatch`], but fetches and decodes the dispatch
    /// object WITHOUT staging an image index into `o/` — the read-only
    /// counterpart used by a [`super::chained_index`] `ReadOnly` resolve
    /// (`ocx package inspect`). Returns the same `(digest, manifest)` so the
    /// caller can display / recurse, while the permanent index stays untouched.
    /// A leaf platform manifest already writes nothing to `o/`, so for that
    /// shape this is identical to `persist_dispatch`; the divergence is only
    /// for an image index, which `persist_dispatch` would stage and this does
    /// not.
    pub async fn fetch_dispatch_only(
        &self,
        source: &super::Index,
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
        source.fetch_manifest_raw_bytes(identifier).await
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
    ///   (`IndexStore::read_dispatch_object`): present and decodable as an image
    ///   index ⇒ [`DispatchResolution::Dispatch`] (via
    ///   [`decode_index_manifest`]); otherwise ⇒
    ///   [`DispatchResolution::AbsentDispatch`], recovered by fetching `content`
    ///   by digest (see that variant).
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
    /// bare miss**, so `ChainedIndex` can drive the fetch-by-digest recovery
    /// ([`DispatchResolution::AbsentDispatch`]). Returns `Ok(None)` only when the
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

        // Dispatch on the `o/` lookup: present ⇒ decode the image index; absent
        // ⇒ `AbsentDispatch` (a leaf platform manifest is never stored in the
        // local index — A3/B2 — so the caller fetches `content` by digest).
        match self
            .index_store
            .read_dispatch_object(source, repository, &content)
            .await?
        {
            // Present but not an image index: a recoverable state, routed as a
            // fetch-by-digest recovery rather than surfaced as corruption.
            Some(bytes) => Ok(Some(match decode_index_manifest(&bytes)? {
                Some(index) => DispatchResolution::Dispatch {
                    content,
                    index: Box::new(index),
                },
                None => DispatchResolution::AbsentDispatch { content },
            })),
            None => Ok(Some(DispatchResolution::AbsentDispatch { content })),
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
        // The version gate runs before any local root is consumed, exactly as
        // it does before a fetched one (C-005) — same rule, same error, either
        // side of the trust boundary.
        self.check_format_version(source).await?;
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

    /// Version-gates this source's local subtree against its `config.json`
    /// (`adr_servable_index_snapshot.md` C-005) — the on-disk twin of
    /// [`OcxIndex::check_format_version`](super::OcxIndex).
    ///
    /// An **absent** `config.json` is [`IndexFormatConfig::assumed_v1`]: a tree
    /// written before ocx wrote configs, or authored by another
    /// implementation, is a valid version-1 index. A present one is parsed and
    /// gated. The rule does not soften because the bytes came off local disk —
    /// a version rule that trusts one provenance and checks the other is the
    /// asymmetry (CWE-501) this deletes.
    ///
    /// Read **once per source per instance**, the absent outcome included.
    /// That is the deliberate difference from the fetched reader, which
    /// re-derives absence every call so a site that publishes a `config.json`
    /// mid-process is picked up: a local subtree is this machine's own tree,
    /// and re-`stat`ing it on every root read buys nothing.
    ///
    /// Only root reads gate here, and that covers every local document this
    /// reader interprets. `c/index.json` carries its own `format_version` and
    /// is gated on read by
    /// [`CatalogDocument::into_packages`](super::wire::CatalogDocument) through
    /// the same [`gate_format_version`], so
    /// [`IndexStore::read_source_catalog`] needs no second check; the derived
    /// half of [`Self::list_local_repositories`] enumerates directories and
    /// parses no document at all, so it has nothing to gate.
    ///
    /// One documented exception, not a universal choke point:
    /// `package_manager::tasks::resolve::recover_base_with_real_registry` reads
    /// a root straight off [`IndexStore`], bypassing this gate. It extracts a
    /// host string and soft-fails to the slug form on any error, so an
    /// ungated read there cannot resolve or install anything — but a reader
    /// added on that route WOULD need the gate.
    ///
    /// A refusal must not be downgraded to a local miss by the layer above:
    /// [`ChainedIndex`](super::chained_index::ChainedIndex)'s
    /// `is_local_read_refusal` propagates it, or the walk would fall through to
    /// a source and then grow the very tree whose version was refused.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat)
    /// (exit 65) on a declared-but-unknown version; a present-but-unreadable or
    /// unparseable `config.json` propagates from
    /// [`IndexStore::read_source_config`] (C-003) — never flattened to absence.
    async fn check_format_version(&self, source: &str) -> Result<()> {
        if self.gated_sources.read().await.contains(source) {
            return Ok(());
        }
        let config = self
            .index_store
            .read_source_config(source)
            .await?
            .unwrap_or_else(IndexFormatConfig::assumed_v1);
        gate_format_version(config.format_version)?;
        self.gated_sources.write().await.insert(source.to_string());
        Ok(())
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

    /// The **physical** transport location the locally-committed root document
    /// points at (`adr_index_indirection.md` C2/C3) — the offline counterpart to
    /// [`OcxIndex::physical_identifier`](super::OcxIndex).
    ///
    /// Every root the local copy holds already carries the answer: `repository`
    /// is the `oci://host/path` pointer, copied verbatim from a published site
    /// or OCX-authored for a derived source. Reading it here is what lets a
    /// resolve derive the physical address with zero network; without it the
    /// [`index_impl::IndexImpl::physical_reference`] default answers `Ok(None)`,
    /// which the caller reads as "no rewrite" and turns into the LOGICAL
    /// identifier — an indirected package silently reported as its own
    /// transport.
    ///
    /// The logical digest is carried onto the physical location and the tag is
    /// dropped — the exact shape [`super::OcxIndex`] mints — so a local answer
    /// and a source answer for one identifier can never disagree. The physical
    /// value is transport-only routing (C2), never a storage key.
    ///
    /// `Ok(None)` = no root known locally. See
    /// [`ChainedIndex::physical_reference`](super::chained_index::ChainedIndex)
    /// for why that stays indistinguishable from "registry-backed, no rewrite".
    ///
    /// # Errors
    ///
    /// [`Error::MalformedPhysicalRef`](super::error::Error::MalformedPhysicalRef)
    /// when the committed root's `repository` is not a well-formed `oci://`
    /// pointer — the same strict C3 parse the root-read hook applies.
    pub(super) async fn physical_reference(
        &self,
        identifier: &oci::Identifier,
        kind: SourceKind,
    ) -> Result<Option<oci::Identifier>> {
        let Some(result) = self
            .read_root_by_kind(identifier.registry(), identifier.repository(), kind)
            .await?
        else {
            return Ok(None);
        };
        let (registry, repository) = super::parse_physical_repository(&result.root.repository)?;
        let physical = oci::Identifier::new_registry(repository, registry);
        Ok(Some(match identifier.digest() {
            Some(digest) => physical.clone_with_digest(digest),
            None => physical,
        }))
    }

    /// Merge a fetched published root into the local copy
    /// (`adr_index_indirection.md` A2, amended: the local index is AUTHORED,
    /// not mirrored).
    ///
    /// **Merge is the only write verb.** The local copy is never replaced by a
    /// fetched document, because it is not a mirror of one: it is the record of
    /// what this machine snapshotted. So a write adds and updates within
    /// `scope`, and never deletes — a tag only the local copy holds survives
    /// every update, however the remote's own tag list has changed.
    ///
    /// [`RootScope`] decides how much is in scope: one named tag, or the whole
    /// package. Nothing outside it is touched, which is what keeps
    /// `ocx index update pkg:3.28` from moving a sibling pin, or the routing
    /// pointer, on behalf of a user who named one version.
    ///
    /// Drift is adopted **silently**: no warning, no comparison, no diagnostic.
    /// Whatever the fetched root says about tags outside the scope is not this
    /// call's business, and reporting it here would make every resolve a
    /// staleness check against the network. Staleness surfaces exactly once,
    /// where it was asked for — `ocx index update`'s report.
    ///
    /// The merge runs on an order-preserving [`serde_json::Value`] and is
    /// re-emitted through [`super::serialize_root`], the one canonical root
    /// serializer, so every field OCX does not model rides through untouched and
    /// the result stays in the hosted site's normal form.
    ///
    /// Once the transaction has committed, the source's `config.json` is
    /// written **if absent** (`adr_servable_index_snapshot.md` C-023): the
    /// update path is the sole writer of that document, so a tree OCX grows
    /// declares itself an index while a tree OCX only reads stays untouched
    /// (C-022). That write failing to get its lock in time is logged and
    /// swallowed — its catalog work has already committed and the next update
    /// writes the config — but every other failure propagates.
    pub(super) async fn commit_published_root(
        &self,
        identifier: &oci::Identifier,
        fetched_bytes: &[u8],
        scope: RootScope<'_>,
    ) -> Result<()> {
        let source = identifier.registry();
        let repository = identifier.repository();
        let repository_check =
            |root: &super::wire::IndexRoot| super::parse_physical_repository(&root.repository).map(|_| ());

        // The whole read-merge-write runs under the source's catalog lock: the
        // root and its `c/index.json` entry are one unit (F1), and a pre-lock
        // read would let a concurrent writer's root be clobbered by this merge.
        let mut transaction = self.index_store.begin_catalog_transaction(source).await?;

        let committed = self.index_store.read_root_document_bytes(source, repository).await?;
        // `None` = nothing in scope changed — the fetched root does not carry
        // the named tag, or the copy already holds exactly these entries. A
        // no-op write would churn the mtime of a tree people commit and rsync (A2).
        if let Some(bytes) = merge_root(committed.as_deref(), fetched_bytes, scope) {
            transaction.write_root(repository, &bytes, repository_check).await?;
        }
        transaction.commit().await?;

        // The tree ocx just published declares itself an index at the version
        // this binary speaks (C-023). Two things fix this statement's position.
        //
        // It is AFTER the commit because `commit(self)` consumes the
        // transaction and drops its lock guard, and `ensure_source_config`
        // RE-acquires that same `index-catalog` / `c/index.json` lock rather
        // than inheriting it — inverted, this blocks on itself for the full
        // `SOURCE_LOCK_TIMEOUT` and then errors.
        //
        // It is after the catalog for crash order too: a crash between the two
        // leaves a tree with content and no config, which is the pre-change
        // status quo and is repaired by the next update. The other order would
        // leave a config-only tree claiming to be an index with nothing in it.
        match self.index_store.ensure_source_config(source).await {
            // Losing the race for that second lock (a concurrent `regenerate`
            // holds it across its whole run) must not fail an update whose
            // catalog write already committed. Nothing is corrupted — the tree
            // is left content-complete and config-less, the same state the
            // crash case leaves, and the next update writes the config. Only
            // the timeout is absorbed; a genuine I/O failure still propagates.
            Err(error) if is_lock_timeout(&error) => {
                log::warn!(
                    "Index source '{source}' was published without a 'config.json': its catalog lock \
                     stayed held for {SOURCE_LOCK_TIMEOUT:?} ({}). The next index update writes it.",
                    crate::error::render_chain(&error)
                );
                Ok(())
            }
            result => result,
        }
    }

    /// Stage already-fetched dispatch-object bytes into the wire-grammar object
    /// CAS under the object's own digest — the no-double-fetch self-heal write
    /// (`adr_index_indirection.md` A3). When [`ChainedIndex`](super::chained_index::ChainedIndex)
    /// already holds the bytes of a [`DispatchResolution::AbsentDispatch`] recovery
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

#[cfg(test)]
impl LocalIndex {
    /// Seed `bytes` as `identifier`'s committed root, with the catalog entry
    /// `CatalogTransaction::write_root` derives from them.
    ///
    /// Test scaffolding only. Production has no verbatim-replace writer any
    /// more — every real write merges ([`LocalIndex::commit_published_root`]) —
    /// but a test needs a way to put a package into a known committed state
    /// without going through the code under test.
    pub(super) async fn seed_root_document(&self, identifier: &oci::Identifier, bytes: &[u8]) -> Result<()> {
        let mut transaction = self
            .index_store
            .begin_catalog_transaction(identifier.registry())
            .await?;
        transaction
            .write_root(identifier.repository(), bytes, |root| {
                super::parse_physical_repository(&root.repository).map(|_| ())
            })
            .await?;
        transaction.commit().await
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
    /// dispatch objects are copied verbatim, and the source carries a
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
    /// `content` names a dispatch object present in `o/`: the OCI image index
    /// the observed tag referenced, verbatim as the registry served it. `content`
    /// is the head digest the root tag pointed at. The manifest is boxed so this
    /// variant does not dwarf the digest-only [`Self::AbsentDispatch`] (clippy
    /// `large_enum_variant`).
    Dispatch {
        content: oci::Digest,
        index: Box<oci::ImageIndex>,
    },
    /// `content` is absent from `o/`. Recovery is the same for every source:
    /// fetch `content` by digest — from the machine-global blob store first
    /// (installed content, A3 step 2 / B2), then the physical registry.
    ///
    /// Both shapes `content` can name are digest-addressable at the registry:
    /// a leaf platform manifest the local index never copies (A3/B2), or an
    /// image index whose `o/` copy is missing from an incomplete snapshot. In
    /// the latter case the fetched bytes self-heal back into `o/`
    /// ([`LocalIndex::stage_dispatch_bytes`]) and dispatch continues.
    AbsentDispatch { content: oci::Digest },
}

/// How much of a fetched published root a write may adopt.
///
/// Both scopes only ever add and update. Neither deletes: the local index is
/// authored, so a tag it holds is a snapshot this machine took, not a row that
/// disappears because the site stopped listing it.
#[derive(Debug, Clone, Copy)]
pub(super) enum RootScope<'a> {
    /// One named tag's entry. Every sibling pin and every package-level field —
    /// `repository` included — stays exactly as committed. This is
    /// `ocx index update pkg:3.28`, and every grow-on-resolve.
    Tag(&'a str),
    /// Every tag the remote lists, plus the package-level fields. This is
    /// `ocx index update pkg` — the sanctioned point to take a routing
    /// migration, because the user named the package and nothing narrower.
    Package,
}

/// Whether `error` is a lock acquisition that ran out of patience rather than a
/// genuine I/O failure — the one outcome
/// [`LocalIndex::commit_published_root`]'s `config.json` hook absorbs (C-023).
///
/// `LockedFile::open_exclusive_with_timeout` reports an expired wait as
/// [`std::io::ErrorKind::TimedOut`], wrapped by
/// [`file_error`](crate::error::file_error) like every other I/O failure on that
/// path — so the *kind* is the discriminator, not the variant.
///
/// The kind alone is not enough: an `ETIMEDOUT` from a network filesystem
/// (NFS/CIFS) maps to the same kind, and every syscall on this path wraps
/// through `file_error` too. The wait timeout is synthesized by ocx with
/// `io::Error::new`, so it carries **no** `raw_os_error`, while an OS
/// `ETIMEDOUT` always carries one — which is what keeps a real write failure
/// from being absorbed as a lost lock race.
fn is_lock_timeout(error: &crate::Error) -> bool {
    matches!(
        error,
        crate::Error::InternalFile(_, io)
            if io.kind() == std::io::ErrorKind::TimedOut && io.raw_os_error().is_none()
    )
}

/// Merge a fetched published root into the `committed` one within `scope`,
/// returning the bytes to write — or `None` when nothing in scope changed.
///
/// The document is walked as an order-preserving [`serde_json::Value`] rather
/// than the typed [`super::wire::IndexRoot`], which is parse-only and models a
/// subset: a typed round-trip would silently drop every human-governed field a
/// newer index writer added. The emitted bytes come from
/// [`super::serialize_root`], the canonical root serializer, so an untouched
/// field cannot drift and the result stays in the site's normal form.
///
/// With no committed root — a package first seen — the merge runs against the
/// fetched document with its `tags` emptied, so a first-sight `Tag` write lands
/// exactly the tag it resolved rather than the site's whole tag list, and the
/// package-level fields come along because there is nothing yet to protect.
/// Committed bytes no reader accepts get the same treatment: recovering from a
/// crashed write is not overwriting committed state, because bytes that do not
/// parse hold no pin.
fn merge_root(committed: Option<&[u8]>, fetched: &[u8], scope: RootScope<'_>) -> Option<Vec<u8>> {
    let fetched_root: serde_json::Value = serde_json::from_slice(fetched).ok()?;
    let adopted: Vec<(String, serde_json::Value)> = match scope {
        RootScope::Tag(tag) => {
            let Some(entry) = fetched_root.get("tags").and_then(|tags| tags.get(tag)).cloned() else {
                // Publish skew: the source resolved the tag but its root does
                // not list it. Inventing the pointer is not this path's business.
                log::debug!("fetched root does not carry '{tag}' — leaving the committed root alone");
                return None;
            };
            vec![(tag.to_string(), entry)]
        }
        RootScope::Package => fetched_root
            .get("tags")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .map(|(tag, entry)| (tag.clone(), entry.clone()))
            .collect(),
    };

    // The TYPED parse, not merely "is it JSON": a document missing `repository`
    // merges cleanly and is then refused by `write_root`'s own `IndexRoot`
    // parse, so every later update would re-merge and re-fail instead of healing.
    let usable = committed.is_some_and(|bytes| serde_json::from_slice::<super::wire::IndexRoot>(bytes).is_ok());
    let mut root: serde_json::Value = match committed.filter(|_| usable).map(serde_json::from_slice) {
        Some(Ok(root)) => root,
        _ => {
            // Start from the fetched document with an empty tag map: its
            // package-level fields are adopted (nothing to protect) while the
            // scope still decides which tags land.
            let mut base = fetched_root.clone();
            if let Some(object) = base.as_object_mut() {
                object.insert("tags".to_string(), serde_json::Value::Object(serde_json::Map::new()));
            }
            base
        }
    };

    let Some(object) = root.as_object_mut() else {
        return Some(fetched.to_vec());
    };
    let mut changed = false;
    if let RootScope::Package = scope {
        // Package-level adoption: routing and every human-governed field the
        // remote carries. Overwrite-only — a field the remote dropped stays,
        // because merge never deletes.
        for (key, value) in fetched_root.as_object().into_iter().flatten() {
            if key != "tags" && object.get(key) != Some(value) {
                object.insert(key.clone(), value.clone());
                changed = true;
            }
        }
    }
    let tags = object
        .entry("tags")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(tags) = tags.as_object_mut() else {
        return Some(fetched.to_vec());
    };
    for (tag, entry) in adopted {
        if tags.get(&tag) != Some(&entry) {
            tags.insert(tag, entry);
            changed = true;
        }
    }
    // A first-sight package always writes: there is no committed document for
    // an unchanged merge to leave alone.
    (changed || !usable).then(|| super::serialize_root(&root))
}

/// Whether `tag` may be recorded as a version in an OCX-authored derived root
/// (`adr_index_indirection.md` D2/D7). The two write paths — the update path
/// ([`LocalIndex::refresh_derived`]) and the resolve path
/// (`ChainedIndex::fetch_and_persist_chain`'s grow branch) — both bypass
/// [`super::Index::list_tags`] when the identifier already carries a tag, so the
/// listing filters cannot stand in for this: without it a violating entry is
/// committed and then *hidden* by the listing filter, which is invisible rather
/// than absent.
///
/// Two rules, one gate, exclude rather than refuse (an unusable tag is not a
/// reason to fail the other tags of the same package):
///
/// - **D2 — the root must never point at a bare manifest.** A tag whose
///   `content` is a leaf platform-manifest digest writes nothing to `o/`
///   ([`LocalIndex::persist_dispatch`]), so recording it would create exactly
///   the tag-without-an-object absence D1 abolished.
/// - **D7 — a reserved tag is not a version.** The `__ocx` namespace and
///   `sha256.<hex>` digest aliases ([`Tag::is_reserved_str`]) are not version
///   pointers and must never appear as ones.
pub(super) fn records_root_tag(tag: &str, manifest: &oci::Manifest) -> bool {
    if !matches!(manifest, oci::Manifest::ImageIndex(_)) {
        log::debug!("tag '{tag}' resolves to a bare manifest, not an image index — not recorded in the root");
        return false;
    }
    if Tag::is_reserved_str(tag) {
        log::debug!("tag '{tag}' is reserved and is never a version — not recorded in the root");
        return false;
    }
    true
}

/// Decodes a verified dispatch object into the OCI image index it is
/// (`adr_index_indirection.md` A2, `adr_oci_index_only_dispatch.md` D1).
///
/// One shape is ever written to the dispatch-object CAS: the OCI image index
/// the observed tag referenced, byte-for-byte as the registry served it. There
/// is no second codec and no fallback — the index has no business defining
/// object shapes of its own, so the only parse is the OCI one.
///
/// `Ok(None)` = the bytes are not an image index. That is the fail-closed
/// shape: [`oci::ImageIndex`] requires `schemaVersion` and `manifests`, so a
/// leaf platform manifest, a truncated file, or any other payload is refused
/// here and surfaced as [`DispatchResolution::AbsentDispatch`] — a recoverable
/// cache miss the caller heals by fetching `content` by digest, never a silent
/// load of the wrong shape. Unknown sibling fields (`subject`, keys a newer
/// writer adds) are tolerated: the fleet reads one another's documents, and the
/// bytes are stored verbatim and never re-serialised, so nothing is lost by
/// ignoring them (A4 is load-bearing exactly here).
///
/// # Errors
///
/// [`Error::InvalidImageIndex`](super::error::Error::InvalidImageIndex) when the
/// bytes *are* an image index but an invalid one. Deserialisation proves shape
/// only — `schemaVersion` is an unconstrained `u8` — so the semantics are
/// checked on read-back too, not just at the boundary that admitted the bytes.
/// This is not the recoverable-miss case: a document carrying `manifests` can
/// never be a leaf, so it is malformed index data and is refused, never healed.
fn decode_index_manifest(bytes: &[u8]) -> Result<Option<oci::ImageIndex>> {
    let Ok(index) = serde_json::from_slice::<oci::ImageIndex>(bytes) else {
        return Ok(None);
    };
    crate::oci::manifest::validate_image_index(&index).map_err(super::error::Error::from)?;
    Ok(Some(index))
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
            .map(|tags| tags.into_iter().filter(|t| !Tag::is_reserved_str(t)).collect()))
    }

    async fn fetch_manifest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        log::trace!("Fetching manifest for identifier '{}'.", identifier);
        match self.resolve_dispatch(identifier, SourceKind::Derived).await? {
            Some(DispatchResolution::Dispatch { content, index }) => {
                Ok(Some((content, oci::Manifest::ImageIndex(*index))))
            }
            // The digest/tag is known but its bytes are not locally cached
            // (a leaf platform manifest, A3) — a bare local read cannot
            // produce it; `ChainedIndex` drives the source-kind-routed
            // recovery instead.
            Some(DispatchResolution::AbsentDispatch { .. }) | None => Ok(None),
        }
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<oci::Digest>> {
        match self.resolve_dispatch(identifier, SourceKind::Derived).await? {
            // The digest is known regardless of whether the dispatch bytes
            // are locally cached — `AbsentDispatch` still carries it.
            Some(DispatchResolution::Dispatch { content, .. })
            | Some(DispatchResolution::AbsentDispatch { content }) => Ok(Some(content)),
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

    async fn physical_reference(&self, identifier: &oci::Identifier) -> Result<Option<oci::Identifier>> {
        // Never take the trait default here: it answers `Ok(None)` = "no
        // rewrite", which a caller turns into the logical identifier even
        // though the committed root names a different physical location.
        LocalIndex::physical_reference(self, identifier, SourceKind::Derived).await
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

    /// The inner factor of the ceiling `ocx index sync` states (C-024: ≤ 512
    /// in-flight requests, `INDEX_REFRESH_CONCURRENCY` × this constant).
    ///
    /// The CLI asserts the product of the two constants; that leaves the
    /// constant's **call sites** unguarded, and they are what make the ceiling
    /// real. `buffer_unordered(500)` here keeps the constant at 64, keeps the
    /// CLI's product assertion green, and makes the true ceiling 4000 — and the
    /// acceptance measurement cannot see it either, because its fixture
    /// publishes one tag per repository, so this nested fan-out runs at width 1.
    #[test]
    fn the_per_tag_fan_out_is_sized_by_the_constant_at_every_site() {
        let source: String = include_str!("local_index.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            source.matches("buffer_unordered(").count(),
            2,
            "two per-tag fan-outs live here, one per provenance kind; a third needs its own \
             review against C-024's ceiling"
        );
        assert_eq!(
            source.matches("buffer_unordered(TAG_REFRESH_CONCURRENCY)").count(),
            2,
            "both must be sized by the constant: a literal leaves the constant true and the \
             ceiling false"
        );
    }

    fn make_index(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(Config {
            index_store: IndexStore::new(dir.path().join("index")),
        })
    }

    fn store(dir: &TempDir) -> IndexStore {
        IndexStore::new(dir.path().join("index"))
    }

    /// Decodes a persisted `c/index.json` straight off disk into its `packages`
    /// map — asserting on the writer's own bytes rather than round-tripping
    /// through [`IndexStore::read_source_catalog`], which would hide a matched
    /// read/write pair of bugs.
    fn catalog_on_disk(path: &std::path::Path) -> crate::oci::index::CatalogIndex {
        let document: crate::oci::index::CatalogDocument =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        document.into_packages().unwrap()
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

    /// Verbatim registry bytes for an OCI image index over `leaves`
    /// (`(architecture, leaf-manifest digest)` pairs), plus their own digest.
    ///
    /// **Deliberately NOT the canonical serde encoding of what it parses to.**
    /// The document is pretty-printed and carries a `subject` field
    /// `oci::ImageIndex` does not model. A fixture that served exactly
    /// `serde_json::to_vec(&parsed)` could not tell a byte-copying
    /// implementation from a re-serialising one, so every verbatim-bytes and
    /// digest-stability assertion built on it would be vacuous — the whole
    /// point of storing registry bytes verbatim (D1/A4) would be pinned by
    /// nothing.
    fn image_index_bytes(leaves: &[(&str, &oci::Digest)]) -> (Vec<u8>, oci::Digest) {
        let manifests = leaves
            .iter()
            .map(|(architecture, digest)| {
                format!(
                    "    {{ \"mediaType\": \"application/vnd.oci.image.manifest.v1+json\", \"digest\": \"{digest}\", \
                     \"size\": 42, \"platform\": {{ \"architecture\": \"{architecture}\", \"os\": \"linux\" }} }}"
                )
            })
            .collect::<Vec<_>>()
            .join(",\n");
        let subject = leaves[0].1;
        let json = format!(
            "{{\n  \"schemaVersion\": 2,\n  \"mediaType\": \"application/vnd.oci.image.index.v1+json\",\n  \
             \"subject\": {{ \"mediaType\": \"application/vnd.oci.image.manifest.v1+json\", \"digest\": \"{subject}\", \
             \"size\": 3 }},\n  \"manifests\": [\n{manifests}\n  ]\n}}\n"
        );
        let bytes = json.into_bytes();
        let digest = Algorithm::Sha256.hash(&bytes);
        (bytes, digest)
    }

    /// A minimal fake DERIVED source: one tag → the verbatim OCI image index a
    /// registry would serve for it, plus that index and the flat platform
    /// manifest it names, each addressable by its own digest. Because it
    /// overrides `fetch_manifest_raw_bytes` with matching `(bytes, digest)`, the
    /// index store's A3 verify accepts the persisted objects.
    ///
    /// A digest request is answered with the bytes that hash to THAT digest, as
    /// a registry would — a fixture serving one fixed document for every digest
    /// cannot tell a resolve that honours a committed pin from one that quietly
    /// re-asks the floating tag.
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
            identifier: &oci::Identifier,
            _op: IndexOperation,
        ) -> Result<Option<(oci::Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
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
            let (leaf_bytes, leaf_digest) = image_manifest_bytes();
            let (index_object_bytes, index_digest) = index_bytes();
            let (bytes, digest) = match id.digest() {
                Some(requested) if requested == index_digest => (index_object_bytes, index_digest),
                // The physical platform-manifest leaf the index names.
                Some(requested) if requested == leaf_digest => (leaf_bytes, leaf_digest),
                Some(_) => return Ok(None),
                None => (index_object_bytes, index_digest),
            };
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

    /// A derived source whose tag resolves to a BARE platform manifest — the
    /// shape D2 refuses to record as a root version.
    #[derive(Clone)]
    struct BareManifestSource {
        tag: String,
    }

    #[async_trait]
    impl super::super::index_impl::IndexImpl for BareManifestSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![self.tag.clone()]))
        }
        async fn fetch_manifest(
            &self,
            id: &oci::Identifier,
            _op: IndexOperation,
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

    fn bare_manifest_source_for_tag(tag: &str) -> super::super::Index {
        super::super::Index::from_impl(BareManifestSource { tag: tag.to_string() })
    }

    // ── derived source authors a root document (A2/A3) ───────────────────────
    //
    // `refresh_tags` grows the hosted wire grammar. A registry (derived) source
    // resolves the tag to the OCI image index the registry serves, so
    // `refresh_tags` authors a root document with `tag → content` AND writes
    // that index verbatim into the dispatch object CAS. The platform manifests
    // it names are never copied (A3/B2).

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

        let (object_bytes, content) = index_bytes();
        let (_, leaf) = image_manifest_bytes();
        let root = read_root_value(&dir);
        assert_eq!(
            root["repository"].as_str(),
            Some(format!("oci://{REGISTRY}/{REPO}").as_str()),
            "a derived refresh authors the oci:// physical pointer from the identifier"
        );
        assert_eq!(
            root["tags"]["3.28"]["content"].as_str(),
            Some(content.to_string().as_str()),
            "the refreshed tag's content is the image index the tag resolved to"
        );
        // The index travels with the pointer, verbatim (D1) — that is what makes
        // a hosted subtree copy-pasteable into a local index.
        assert_eq!(
            std::fs::read(store(&dir).dispatch_object_path(REGISTRY, REPO, &content)).unwrap(),
            object_bytes,
            "the dispatch object must be the registry's own bytes, not a re-serialisation"
        );
        // The platform manifests it names are never copied (A3/B2).
        assert!(
            !store(&dir).dispatch_object_path(REGISTRY, REPO, &leaf).exists(),
            "a leaf platform manifest must never enter the dispatch object CAS (A3/B2)"
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
            let (bytes, digest) = index_bytes();
            Ok(Some((digest, serde_json::from_slice(&bytes).unwrap())))
        }
        async fn fetch_manifest_digest(&self, _: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            let (_, digest) = index_bytes();
            Ok(Some(digest))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            let (bytes, digest) = index_bytes();
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

    // ── published refresh fans distinct sibling dispatch objects into o/ (B2) ─

    /// A single-platform image index naming `leaf`, plus its own digest.
    /// Varying `leaf` yields a DISTINCT index (distinct dispatch digest).
    fn index_for_leaf(leaf: &oci::Digest) -> (Vec<u8>, oci::Digest) {
        image_index_bytes(&[("amd64", leaf)])
    }

    /// A PUBLISHED source serving a verbatim root document whose two tags point
    /// at two DISTINCT dispatch objects — so `refresh_published`'s fan-out
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
            // Each tag resolves to a DISTINCT single-platform image index.
            let leaf_char = match id.tag() {
                Some("1.0") => "a",
                Some("2.0") => "b",
                _ => return Ok(None),
            };
            let leaf = oci::Digest::Sha256(leaf_char.repeat(64));
            let (bytes, digest) = index_for_leaf(&leaf);
            let manifest = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, digest, manifest)))
        }
        async fn fetch_root_document(
            &self,
            _: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, super::super::wire::IndexRoot)>> {
            let (_, obs1) = index_for_leaf(&oci::Digest::Sha256("a".repeat(64)));
            let (_, obs2) = index_for_leaf(&oci::Digest::Sha256("b".repeat(64)));
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
    async fn refresh_published_persists_both_distinct_sibling_dispatch_objects() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = super::super::Index::from_impl(PublishedTwoTagSource);

        index.refresh_tags(&repo_id(), &source).await.unwrap();

        // The root's two tags name two DISTINCT dispatch digests, so the
        // content-digest dedup keeps both — each must land as its own o/ object
        // (a sibling tag pointing at an obs absent from o/ could not resolve
        // offline, B2).
        let (_, obs1) = index_for_leaf(&oci::Digest::Sha256("a".repeat(64)));
        let (_, obs2) = index_for_leaf(&oci::Digest::Sha256("b".repeat(64)));
        assert_ne!(obs1, obs2, "prerequisite: the two dispatch objects must be distinct");
        assert!(
            store(&dir).dispatch_object_path(REGISTRY, REPO, &obs1).exists(),
            "the first tag's dispatch object must be persisted under o/"
        );
        assert!(
            store(&dir).dispatch_object_path(REGISTRY, REPO, &obs2).exists(),
            "the second tag's distinct dispatch object must be persisted under o/"
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
            let (bytes, digest) = index_bytes();
            Ok(Some((digest, serde_json::from_slice(&bytes).unwrap())))
        }
        async fn fetch_manifest_digest(&self, _: &oci::Identifier, _: IndexOperation) -> Result<Option<oci::Digest>> {
            let (_, digest) = index_bytes();
            Ok(Some(digest))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &oci::Identifier,
        ) -> Result<Option<(Vec<u8>, oci::Digest, Manifest)>> {
            let (bytes, digest) = index_bytes();
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
        // The derived source resolves the tag to an OCI image index, so a
        // dispatch object IS written alongside the root.
        let source = source_for_tag("3.28");
        index.refresh_tags(&tagged_id("3.28"), &source).await.unwrap();

        let home = dir.path().join("index");
        // The authored root document lands at <home>/<source>/p/<repo>.json.
        assert!(
            home.join(REGISTRY).join("p").join(format!("{REPO}.json")).exists(),
            "the derived root document must land under the wire-grammar home"
        );
        // The dispatch object lands at <home>/<source>/p/<repo>/o/sha256/<hex>.json.
        let (_, dispatch_digest) = index_bytes();
        assert!(
            store(&dir)
                .dispatch_object_path(REGISTRY, REPO, &dispatch_digest)
                .exists(),
            "the multi-platform image index must be persisted as a dispatch object under the home"
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
        let source = source_for_tag("3.28");
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

        let (_, dispatch_digest) = index_bytes();
        let dispatch_path = store(&dir).dispatch_object_path(REGISTRY, REPO, &dispatch_digest);
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
        let (_, dispatch_digest) = index_bytes();

        // Seed only the root's tag pointer; leave the dispatch object absent.
        cache.commit_root_tag(&id, &dispatch_digest).await.unwrap();
        let dispatch_path = store(&dir).dispatch_object_path(REGISTRY, REPO, &dispatch_digest);
        assert!(!dispatch_path.exists(), "prerequisite: dispatch object must be absent");

        let chained =
            super::super::Index::from_chained(cache, vec![source_for_tag("3.28")], super::super::ChainMode::Default);
        let result = chained
            .fetch_manifest(&id, super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result.is_some(),
            "tag cached but dispatch object missing must re-fetch via the chain and return Some"
        );
        assert_eq!(result.unwrap().0, dispatch_digest);
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
        let source = source_for_tag("3.28");
        let (_bytes, head, _manifest) = index
            .persist_dispatch(&source, &id)
            .await
            .unwrap()
            .expect("source has a manifest to persist");
        index.commit_root_tag(&id, &head).await.unwrap();
        let (_, dispatch_digest) = index_bytes();
        let dispatch_path = store(dir).dispatch_object_path(REGISTRY, REPO, &dispatch_digest);
        assert!(
            dispatch_path.exists(),
            "prerequisite: the dispatch object must be persisted"
        );
        std::fs::write(&dispatch_path, b"tampered garbage").unwrap();
        dispatch_digest
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
            vec![source_for_tag("3.28")],
            super::super::ChainMode::Default,
        );
        let result = chained
            .fetch_manifest(&tagged_id("3.28"), super::IndexOperation::Resolve)
            .await
            .expect("online Resolve must heal a corrupt object, not error");
        assert!(result.is_some(), "healed Resolve must return the manifest");

        let healed = std::fs::read(store(&dir).dispatch_object_path(REGISTRY, REPO, &digest)).unwrap();
        let (expected, _) = index_bytes();
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
                Err(crate::Error::OciIndex(super::super::error::Error::NoIndexableTag(_)))
            ),
            "tagged nonexistent package must report not-found, got {tagged:?}"
        );
    }

    // ── dispatch-object decode: one OCI parse, fail-closed (D1) ──────────────

    /// The verbatim image index a derived source serves for its tag: one
    /// descriptor naming the flat image manifest, plus the index's own digest.
    fn index_bytes() -> (Vec<u8>, oci::Digest) {
        let (_, leaf) = image_manifest_bytes();
        index_for_leaf(&leaf)
    }

    #[test]
    fn decode_index_manifest_returns_the_image_index_it_was_given() {
        let (index_object_bytes, _) = index_bytes();
        let index = decode_index_manifest(&index_object_bytes)
            .expect("a valid image index is not a refusal")
            .expect("an image index must decode");
        assert_eq!(index.manifests.len(), 1);
    }

    #[test]
    fn decode_index_manifest_returns_none_for_non_oci_bytes() {
        // Fail-closed, by type: there is no second codec. A bare platform
        // manifest, a `{"platforms":[...]}` projection, and plain garbage are
        // all simply "not a dispatch object" — surfaced as `AbsentDispatch` and
        // healed by a fetch-by-digest, never loaded as something they are not.
        // The `Err` arm is reserved for bytes that ARE an image index but an
        // invalid one; none of these are.
        let (manifest_bytes, _) = image_manifest_bytes();
        assert!(
            decode_index_manifest(&manifest_bytes).expect("not a refusal").is_none(),
            "a bare platform manifest is not a dispatch object"
        );
        assert!(
            decode_index_manifest(br#"{"platforms":[]}"#)
                .expect("not a refusal")
                .is_none(),
            "a document with no schemaVersion and no manifests is not a dispatch object"
        );
        assert!(
            decode_index_manifest(b"not a manifest at all")
                .expect("not a refusal")
                .is_none()
        );
    }

    /// A locally stored dispatch object that IS an image index but declares
    /// `schemaVersion: 1` is refused, not reported as an absent dispatch.
    ///
    /// The distinction is the whole point: a document carrying `manifests` can
    /// never be a leaf manifest, so "heal it by fetching `content` by digest"
    /// is not available — the bytes are malformed index data and the only
    /// honest outcome is `DataError` (65). The fixture is a byte literal: no
    /// serialisation of `oci::ImageIndex` can emit `schemaVersion: 1`.
    #[test]
    fn decode_index_manifest_refuses_an_index_with_the_wrong_schema_version() {
        let error = decode_index_manifest(br#"{"schemaVersion":1,"manifests":[]}"#)
            .expect_err("an invalid image index must be refused, never reported as absent");
        assert_eq!(
            crate::cli::ClassifyExitCode::classify(&error),
            Some(crate::cli::ExitCode::DataError),
            "malformed index data is a data error, not a generic failure"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dispatch_object_chain_persists_and_resolves_offline() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let id = tagged_id("3.28");
        let source = source_for_tag("3.28");

        // Persist the dispatch object and author the tag → content root pointer,
        // exactly as a chain walk would.
        let (_bytes, head, _manifest) = index.persist_dispatch(&source, &id).await.unwrap().unwrap();
        let (object_bytes, content) = index_bytes();
        assert_eq!(
            head, content,
            "persist_dispatch returns the dispatch object's own digest"
        );
        index.commit_root_tag(&id, &content).await.unwrap();

        // The stored object is the registry's bytes, not a re-serialisation of
        // the parse — the copy-pasteable property (D1/A4). The fixture is
        // pretty-printed and carries a field `oci::ImageIndex` does not model,
        // so a `serde_json::to_vec(&manifest)` write cannot pass this.
        assert_eq!(
            std::fs::read(store(&dir).dispatch_object_path(REGISTRY, REPO, &content)).unwrap(),
            object_bytes,
            "the dispatch object must be the verbatim bytes the source served"
        );

        // Fresh index resolves the tag offline through the local index.
        let fresh = make_index(&dir);
        let (digest, manifest) = fresh
            .fetch_manifest(&id, IndexOperation::Query)
            .await
            .unwrap()
            .expect("tag resolves from the persisted dispatch object");
        assert_eq!(digest, content, "the resolved digest is the dispatch-object digest");
        match manifest {
            Manifest::ImageIndex(index) => assert_eq!(index.manifests.len(), 1),
            other => panic!("expected the stored image index, got {other:?}"),
        }

        // The physical leaf is never copied into the local index (A3/B2) — a
        // digest-addressed query for it is a clean local miss, not an error;
        // fetching it is a registry concern, covered by
        // `resolve_dispatch_returns_absent_dispatch_when_object_missing`.
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

    /// A two-platform image index as verbatim registry bytes, paired with its
    /// digest — a DISPATCH object, never a bare leaf manifest.
    fn two_platform_index() -> (Vec<u8>, oci::Digest) {
        let leaf_a = oci::Digest::Sha256("a".repeat(64));
        let leaf_b = oci::Digest::Sha256("b".repeat(64));
        image_index_bytes(&[("amd64", &leaf_a), ("arm64", &leaf_b)])
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

    /// A fetch-counting source: a tag resolves to a verbatim two-platform OCI
    /// image index (a dispatch object). Every `fetch_manifest_raw_bytes` bumps
    /// a shared counter, so a
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
            let (bytes, digest) = two_platform_index();
            let manifest = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, digest, manifest)))
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

        let (_bytes, head, head_manifest) = index.persist_dispatch(&source, &id).await.unwrap().unwrap();
        let (dispatch_bytes, dispatch_digest) = two_platform_index();
        assert_eq!(
            head, dispatch_digest,
            "persist_dispatch returns the dispatch object's own digest"
        );
        assert!(
            matches!(head_manifest, Manifest::ImageIndex(_)),
            "persist_dispatch returns the decoded dispatch manifest alongside the digest"
        );

        // Exactly ONE dispatch object, at the `.json` wire path, byte-identical.
        let dispatch_path = store(&dir).dispatch_object_path(REGISTRY, REPO, &dispatch_digest);
        assert!(
            dispatch_path.exists(),
            "the dispatch object must exist at {dispatch_path:?}"
        );
        assert_eq!(
            std::fs::read(&dispatch_path).unwrap(),
            dispatch_bytes,
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
        // A source whose tag resolves to a flat single-platform image MANIFEST.
        let source = bare_manifest_source_for_tag("3.28");

        let (_bytes, head, head_manifest) = index.persist_dispatch(&source, &id).await.unwrap().unwrap();
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

    /// D7 at the LOCAL listing boundary. `commit_root_tags` is a pure writer —
    /// the callers enforce D7, not it — so a root that already carries a
    /// reserved entry (an older copy, a hand-edited shipped tree) must still
    /// never surface one as a version.
    #[tokio::test(flavor = "multi_thread")]
    async fn list_tags_filters_reserved_tags() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let content = oci::Digest::Sha256("a".repeat(64));
        for tag in [
            "3.28",
            "latest",
            "__ocx.desc",
            "__OCX.future",
            &format!("sha256.{}", "a".repeat(64)),
        ] {
            index.commit_root_tag(&tagged_id(tag), &content).await.unwrap();
        }

        let mut tags = IndexImpl::list_tags(&index, &repo_id()).await.unwrap().unwrap();
        tags.sort();
        assert_eq!(
            tags,
            vec!["3.28".to_string(), "latest".to_string()],
            "reserved tags must never be listed as versions"
        );
    }

    // ── D2/D7 at BOTH derived write boundaries (F2, N-1, N-16) ───────────────
    //
    // The three `list_tags` filters cannot stand in for these: both write paths
    // bypass `list_tags` entirely when the identifier already carries a tag, so
    // a violating entry used to be committed and then merely HIDDEN by the
    // listing filter — invisible, not absent. Every assertion below is on the
    // COMMITTED ROOT for exactly that reason.

    /// The root document's tag map, or an empty map when no root was written.
    fn root_tag_names(dir: &TempDir) -> Vec<String> {
        let path = store(dir).root_document_path(REGISTRY, REPO);
        if !path.exists() {
            return Vec::new();
        }
        let root: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        root["tags"]
            .as_object()
            .map(|tags| tags.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// F2 — the UPDATE path. A tag resolving to a bare manifest writes nothing
    /// to `o/` (`persist_dispatch`), so recording it would create exactly the
    /// tag-without-an-object absence D2 abolishes.
    #[tokio::test(flavor = "multi_thread")]
    async fn derived_refresh_skips_a_bare_manifest_tag() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = bare_manifest_source_for_tag("3.28");

        // Nothing indexable: the refresh reports not-found rather than
        // committing a pointer to an object that does not exist.
        let result = index.refresh_tags(&tagged_id("3.28"), &source).await;
        assert!(
            matches!(
                result,
                Err(crate::Error::OciIndex(super::super::error::Error::NoIndexableTag(_)))
            ),
            "a bare-manifest-only refresh records nothing, got {result:?}"
        );
        assert!(
            root_tag_names(&dir).is_empty(),
            "no root tag entry may be committed for a bare-manifest tag"
        );
    }

    /// N-16 — the UPDATE path, reserved-tag half. The tag resolves to a genuine
    /// IMAGE INDEX, so the kind gate above passes and only the reserved verdict
    /// can exclude it. A `sha256.<hex>` or `__ocx*` name is not a version.
    #[tokio::test(flavor = "multi_thread")]
    async fn derived_refresh_skips_a_reserved_tag() {
        for reserved in ["__ocxfoo", "__OCX.future", &format!("sha256.{}", "a".repeat(64))] {
            let dir = TempDir::new().unwrap();
            let index = make_index(&dir);
            let source = source_for_tag(reserved);

            let result = index.refresh_tags(&tagged_id(reserved), &source).await;
            assert!(
                matches!(
                    result,
                    Err(crate::Error::OciIndex(super::super::error::Error::NoIndexableTag(_)))
                ),
                "reserved tag {reserved} must record nothing, got {result:?}"
            );
            assert!(
                root_tag_names(&dir).is_empty(),
                "reserved tag {reserved} must not appear in the committed root"
            );
            // D7's hoisted pre-filter (`refresh_derived`) exists to avoid
            // fetching AND staging an orphan image index into `o/` for a name
            // that is never a version — prove the directory stays empty, not
            // just the root tag map.
            let object_dir = store(&dir)
                .dispatch_object_path(REGISTRY, REPO, &oci::Digest::Sha256("0".repeat(64)))
                .parent()
                .unwrap()
                .to_path_buf();
            assert!(
                !object_dir.exists() || std::fs::read_dir(&object_dir).unwrap().next().is_none(),
                "reserved tag {reserved} must not stage any dispatch object into o/"
            );
        }
    }

    /// N-1 — the RESOLVE path, the more common one. A Default-mode
    /// `Op::Resolve` of `cmake:1.0` against a plain registry serving a bare
    /// manifest persisted nothing to `o/` yet still committed
    /// `tags["1.0"].content = <leaf digest>`. Fixing only the update path left
    /// this wide open.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_grow_skips_a_bare_manifest_tag() {
        let dir = TempDir::new().unwrap();
        let id = tagged_id("1.0");
        let chained = super::super::Index::from_chained(
            make_index(&dir),
            vec![bare_manifest_source_for_tag("1.0")],
            super::super::ChainMode::Default,
        );

        // The resolve itself still succeeds — the manifest is returned to the
        // caller; only the root write is refused (exclude, never refuse).
        let resolved = chained
            .fetch_manifest(&id, super::IndexOperation::Resolve)
            .await
            .expect("the resolve must succeed");
        assert!(resolved.is_some(), "the bare manifest is still resolved for the caller");
        assert!(
            root_tag_names(&dir).is_empty(),
            "the grow branch must not commit a root tag pointing at a bare manifest"
        );
    }

    /// N-16 — the RESOLVE path, reserved-tag half. Both tags resolve to a
    /// genuine image index, so the kind gate passes and the reserved verdict is
    /// the only thing that can exclude them.
    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_grow_skips_a_reserved_tag() {
        for reserved in ["__ocxfoo", "__OCX.future", &format!("sha256.{}", "a".repeat(64))] {
            let dir = TempDir::new().unwrap();
            let chained = super::super::Index::from_chained(
                make_index(&dir),
                vec![source_for_tag(reserved)],
                super::super::ChainMode::Default,
            );

            let resolved = chained
                .fetch_manifest(&tagged_id(reserved), super::IndexOperation::Resolve)
                .await
                .expect("the resolve must succeed");
            assert!(resolved.is_some(), "the index is still resolved for the caller");
            assert!(
                root_tag_names(&dir).is_empty(),
                "reserved tag {reserved} must not be committed into the OCX-authored root"
            );
        }
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

    // ── resolve_dispatch (A3 read path): typed Dispatch / AbsentDispatch / None ──

    /// Seed a wire-grammar root doc (tag `3.28` → `dispatch_digest`) plus its
    /// dispatch object directly on disk, so `resolve_dispatch` (the method under
    /// test) is the only code exercised.
    async fn seed_root_and_dispatch(dir: &TempDir) -> oci::Digest {
        let store = store(dir);
        let (dispatch_bytes, dispatch_digest) = two_platform_index();
        store
            .write_dispatch_object(REGISTRY, REPO, &dispatch_digest, &dispatch_bytes)
            .await
            .unwrap();
        let root_path = store.root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        std::fs::write(&root_path, root_bytes_for(&dispatch_digest)).unwrap();
        dispatch_digest
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_returns_dispatch_for_derived_and_never_creates_catalog() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let dispatch_digest = seed_root_and_dispatch(&dir).await;

        let root_path = store(&dir).root_document_path(REGISTRY, REPO);
        let root_before = std::fs::read(&root_path).unwrap();

        let resolution = index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .unwrap()
            .expect("a present root + dispatch object resolves");
        match resolution {
            DispatchResolution::Dispatch { content, index } => {
                assert_eq!(content, dispatch_digest, "Dispatch carries the tag's content digest");
                assert_eq!(
                    index.manifests.len(),
                    2,
                    "a dispatch object decodes to the image index it is"
                );
            }
            DispatchResolution::AbsentDispatch { .. } => panic!("expected Dispatch, got AbsentDispatch"),
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
    async fn resolve_dispatch_returns_absent_dispatch_when_object_missing() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // Seed only the root (tag → content), NOT the dispatch object.
        let (_, content) = two_platform_index();
        let root_path = store(&dir).root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_path.parent().unwrap()).unwrap();
        std::fs::write(&root_path, root_bytes_for(&content)).unwrap();

        let resolution = index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .unwrap()
            .expect("a present root resolves to a typed outcome, never a bare miss");
        match resolution {
            DispatchResolution::AbsentDispatch { content: resolved } => assert_eq!(
                resolved, content,
                "AbsentDispatch preserves the tag's content digest for source-kind-routed recovery"
            ),
            DispatchResolution::Dispatch { .. } => panic!("expected AbsentDispatch (object absent), got Dispatch"),
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
            r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"3.28":{{"content":"{content}","observed":"2026-07-18T09:00:00Z","yanked":{{"reason":"critical security issue","at":"2026-02-01T00:00:00Z"}}}}}}}}"#
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
        // AbsentDispatch), never refused.
        let allowing = make_index(&dir).with_allow_yanked(true);
        let resolution = allowing
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .expect("allow_yanked must not refuse a yanked tag")
            .expect("a present root resolves to a typed outcome");
        assert!(
            matches!(resolution, DispatchResolution::AbsentDispatch { content: c } if c == content),
            "allow_yanked must resolve the yanked tag's content as AbsentDispatch (its object is unseeded)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_published_crosschecks_catalog() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let dispatch_digest = seed_root_and_dispatch(&dir).await;
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
            matches!(resolution, DispatchResolution::Dispatch { ref content, .. } if *content == dispatch_digest),
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
        let (dispatch_bytes, dispatch_digest) = two_platform_index();
        store(&dir)
            .write_dispatch_object(REGISTRY, REPO, &dispatch_digest, &dispatch_bytes)
            .await
            .unwrap();

        // A digest-addressed identifier looks the digest up directly in `o/`,
        // never reading a root document — so the source kind is irrelevant.
        let id = repo_id().clone_with_digest(dispatch_digest.clone());
        let resolution = index
            .resolve_dispatch(&id, SourceKind::Derived)
            .await
            .unwrap()
            .expect("a present dispatch object resolves");
        match resolution {
            DispatchResolution::Dispatch { content, index } => {
                assert_eq!(content, dispatch_digest, "Dispatch carries the addressed digest");
                assert_eq!(
                    index.manifests.len(),
                    2,
                    "a dispatch object decodes to the image index it is"
                );
            }
            DispatchResolution::AbsentDispatch { .. } => {
                panic!("expected Dispatch for a present digest-addressed object")
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_digest_addressed_absent_object_is_absent_dispatch() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let (_, digest) = two_platform_index();

        // Nothing on disk — the digest-addressed lookup misses in `o/` and
        // surfaces a typed AbsentDispatch, never a bare None (A3).
        let id = repo_id().clone_with_digest(digest.clone());
        let resolution = index
            .resolve_dispatch(&id, SourceKind::Derived)
            .await
            .unwrap()
            .expect("a digest-addressed miss is a typed AbsentDispatch, never a bare miss");
        match resolution {
            DispatchResolution::AbsentDispatch { content } => assert_eq!(content, digest),
            DispatchResolution::Dispatch { .. } => {
                panic!("expected AbsentDispatch for an absent digest-addressed object")
            }
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

        index.seed_root_document(&tagged_id("3.28"), &bytes).await.unwrap();

        let store = store(&dir);
        let on_disk = std::fs::read(store.root_document_path(REGISTRY, REPO)).unwrap();
        assert_eq!(
            on_disk, bytes,
            "the published root must land byte-identical, never re-serialized"
        );

        let catalog = catalog_on_disk(&store.source_catalog_path(REGISTRY));
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
        seed.commit().await.unwrap();

        let bytes = br#"{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{}}"#.to_vec();
        index.seed_root_document(&tagged_id("3.28"), &bytes).await.unwrap();

        let catalog = catalog_on_disk(&store.source_catalog_path(REGISTRY));
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
        let (dispatch_bytes, dispatch_digest) = two_platform_index();

        index
            .stage_dispatch_bytes(&repo_id(), &dispatch_digest, &dispatch_bytes)
            .await
            .unwrap();

        let path = store(&dir).dispatch_object_path(REGISTRY, REPO, &dispatch_digest);
        assert!(
            path.exists(),
            "the staged dispatch object must land at the wire .json path"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            dispatch_bytes,
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

    // ── physical_reference: the local root IS the physical pointer ────────

    /// A published root document whose `repository` names a DIFFERENT physical
    /// location than the logical identifier — the `index.ocx.sh` indirection
    /// this method exists to read (`adr_index_indirection.md` C2).
    fn indirected_root_bytes() -> Vec<u8> {
        br#"{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{}}"#.to_vec()
    }

    #[tokio::test]
    async fn physical_reference_dereferences_the_committed_root_pointer() {
        let dir = TempDir::new().unwrap();
        let local = make_index(&dir);
        local
            .seed_root_document(&repo_id(), &indirected_root_bytes())
            .await
            .unwrap();

        // Tag AND digest on the input: the physical value must carry the digest
        // (content addressing at the physical registry) and drop the tag — the
        // exact shape `OcxIndex::physical_identifier` mints, so a local answer
        // and a source answer can never disagree.
        let (_, digest) = image_manifest_bytes();
        let logical = tagged_id("3.28").clone_with_digest(digest.clone());
        let physical = local
            .physical_reference(&logical, SourceKind::Published)
            .await
            .unwrap()
            .expect("a committed root's `repository` pointer is the physical address");

        assert_eq!(physical.registry(), "ghcr.io");
        assert_eq!(physical.repository(), "ocx-contrib/cmake");
        assert_eq!(physical.digest(), Some(digest));
        assert_eq!(
            physical.tag(),
            None,
            "the physical reference is digest-addressed, never tagged"
        );
        assert_ne!(
            physical.registry(),
            REGISTRY,
            "reporting the LOGICAL registry as its own transport is the defect"
        );
    }

    #[tokio::test]
    async fn physical_reference_carries_no_digest_when_the_logical_reference_has_none() {
        let dir = TempDir::new().unwrap();
        let local = make_index(&dir);
        local
            .seed_root_document(&repo_id(), &indirected_root_bytes())
            .await
            .unwrap();

        let physical = local
            .physical_reference(&tagged_id("3.28"), SourceKind::Published)
            .await
            .unwrap()
            .expect("the root is present");
        assert_eq!(physical.digest(), None);
        assert_eq!(physical.tag(), None);
        assert_eq!(physical.to_string(), "ghcr.io/ocx-contrib/cmake");
    }

    #[tokio::test]
    async fn physical_reference_of_a_derived_root_equals_the_logical_identifier() {
        // A plain OCI registry publishes no index, so OCX authors the root with
        // `oci://<logical registry>/<logical repository>` — physical == logical.
        // The rewrite is a no-op there, which is exactly why `Ok(None)` (no root)
        // and `Some(physical)` (a derived root) are indistinguishable downstream.
        let dir = TempDir::new().unwrap();
        let local = make_index(&dir);
        let (_, digest) = image_manifest_bytes();
        local.commit_root_tag(&tagged_id("3.28"), &digest).await.unwrap();

        let logical = repo_id().clone_with_digest(digest.clone());
        let physical = local
            .physical_reference(&logical, SourceKind::Derived)
            .await
            .unwrap()
            .expect("the derived root is present");
        assert_eq!(physical, logical, "a derived rewrite must be the identity");
    }

    #[tokio::test]
    async fn physical_reference_is_none_without_a_local_root() {
        let dir = TempDir::new().unwrap();
        let local = make_index(&dir);
        assert!(
            local
                .physical_reference(&repo_id(), SourceKind::Derived)
                .await
                .unwrap()
                .is_none(),
            "no local root means no rewrite is known — the registry-backed answer"
        );
    }

    #[tokio::test]
    async fn physical_reference_trait_surface_does_not_fall_through_to_the_none_default() {
        // The trait default returns `Ok(None)` for every identifier; that default
        // reaching `LocalIndex` is the whole defect. Drive the trait surface, not
        // the inherent method, so a deleted `impl` reds here.
        let dir = TempDir::new().unwrap();
        let local = make_index(&dir);
        local
            .seed_root_document(&repo_id(), &indirected_root_bytes())
            .await
            .unwrap();

        let physical = IndexImpl::physical_reference(&local, &repo_id())
            .await
            .unwrap()
            .expect("the trait surface must answer from the committed root, not the None default");
        assert_eq!(physical.registry(), "ghcr.io");
    }

    // ── C-005 (local half): one version rule, local bytes included ──────────

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_subtree_with_no_config_json_resolves() {
        // The inverse of a fail-closed reading of absence: a tree written
        // before ocx wrote configs — or by another implementation — is a valid
        // version-1 index, so the root read goes through (C-005).
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let dispatch_digest = seed_root_and_dispatch(&dir).await;
        assert!(
            !store(&dir).source_config_path(REGISTRY).exists(),
            "prerequisite: the subtree carries no config.json"
        );

        let resolution = index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .expect("an absent config.json is version 1, never a refusal")
            .expect("a present root + dispatch object resolves");
        assert!(
            matches!(resolution, DispatchResolution::Dispatch { ref content, .. } if *content == dispatch_digest),
            "the config-less subtree must resolve its tag"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_subtree_declaring_an_unknown_format_version_is_refused() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        seed_root_and_dispatch(&dir).await;
        let config_path = store(&dir).source_config_path(REGISTRY);
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, br#"{"format_version":2}"#).unwrap();

        let error = index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .expect_err("a declared-but-unknown format_version must fail closed on disk too");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::UnsupportedIndexFormat { version: 2 })
            ),
            "expected UnsupportedIndexFormat{{2}}, got {error:?}"
        );
        assert_eq!(
            crate::cli::ClassifyExitCode::classify(&error),
            Some(crate::cli::ExitCode::DataError),
            "an unsupported format_version is a data error (65) at the local reader as much as the fetched one"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_local_reader_memoizes_the_absent_config_outcome() {
        // C-005's local row differs from the fetched one here: the local reader
        // memoizes absence too, once per source per instance. Pinning it needs
        // both halves — the memoized instance keeps resolving, and a fresh one
        // reads the same tree and refuses, which is what proves the first half
        // is memoization rather than a gate that never ran.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        seed_root_and_dispatch(&dir).await;
        index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .expect("the config-less subtree resolves, memoizing the absent outcome");

        let config_path = store(&dir).source_config_path(REGISTRY);
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, br#"{"format_version":2}"#).unwrap();

        index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .expect("the memoized absent outcome must not be re-read within one instance");
        make_index(&dir)
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Derived)
            .await
            .expect_err("a fresh instance reads the published config.json and refuses version 2");
    }

    // ── C-023: the config.json hook on the update path ─────────────────────

    /// Exactly what C-023 writes: the two-space, trailing-newline Python form
    /// of `{"format_version": 1}`, with no `name_segments` — ocx cannot derive
    /// a name shape from a tree and never guesses one.
    const CONFIG_ON_DISK: &str = "{\n  \"format_version\": 1\n}\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn a_first_publish_writes_the_version_pin(/* S-001 */) {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let (_, content) = two_platform_index();

        index
            .commit_published_root(&tagged_id("3.28"), &root_bytes_for(&content), RootScope::Tag("3.28"))
            .await
            .unwrap();

        let on_disk = std::fs::read(store(&dir).source_config_path(REGISTRY)).unwrap();
        assert_eq!(
            String::from_utf8(on_disk).unwrap(),
            CONFIG_ON_DISK,
            "the first publish declares the tree an index at the version this binary speaks"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_update_that_changes_nothing_still_writes_the_version_pin() {
        // C-023's reachability clause: `commit_published_root` commits
        // unconditionally, so the config write must NOT be gated on the merge
        // having produced bytes. The tree that meets this is the one the whole
        // ADR is about — an rsync'd published copy whose roots are already
        // current, so its first `ocx index update` merges nothing. Gating the
        // write on the merge result (which this module's own "never churn a
        // tree people commit and rsync" comments invite) would leave that tree
        // config-less and unservable: S-002/S-016, reintroduced.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let (_, content) = two_platform_index();
        let bytes = root_bytes_for(&content);
        index.seed_root_document(&tagged_id("3.28"), &bytes).await.unwrap();
        let config_path = store(&dir).source_config_path(REGISTRY);
        assert!(!config_path.exists(), "prerequisite: the seeded tree carries no config");

        index
            .commit_published_root(&tagged_id("3.28"), &bytes, RootScope::Tag("3.28"))
            .await
            .unwrap();

        // `serialize_root` would re-emit these compact bytes pretty-printed, so
        // an unchanged root file is the proof that the merge wrote nothing.
        assert_eq!(
            std::fs::read(store(&dir).root_document_path(REGISTRY, REPO)).unwrap(),
            bytes,
            "prerequisite: this update really is the no-op merge"
        );
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            CONFIG_ON_DISK.as_bytes(),
            "an update with nothing to merge still declares the tree an index"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_publish_leaves_config_json_byte_and_mtime_identical(/* S-008 */) {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let (_, content) = two_platform_index();
        let bytes = root_bytes_for(&content);

        index
            .commit_published_root(&tagged_id("3.28"), &bytes, RootScope::Tag("3.28"))
            .await
            .unwrap();
        let config_path = store(&dir).source_config_path(REGISTRY);
        let first = std::fs::read(&config_path).unwrap();
        let stamped = std::fs::metadata(&config_path).unwrap().modified().unwrap();

        index
            .commit_published_root(&tagged_id("3.28"), &bytes, RootScope::Tag("3.28"))
            .await
            .unwrap();

        // The writer publishes by atomic rename, so a re-write would carry the
        // replacement's own stamp — an unchanged mtime is the no-write claim.
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            first,
            "write-if-absent, never update"
        );
        assert_eq!(
            std::fs::metadata(&config_path).unwrap().modified().unwrap(),
            stamped,
            "a second update must not churn the mtime of a tree people commit and rsync"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pre_seeded_config_json_is_untouched(/* S-015 */) {
        // A tree rsync'd from a hosted index carries the renderer's own config,
        // including the `name_segments` an operator declared. Write-if-absent
        // is what keeps ocx from replacing it with its own narrower document.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let config_path = store(&dir).source_config_path(REGISTRY);
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        let hosted = br#"{"format_version": 1, "name_segments": 2}"#;
        std::fs::write(&config_path, hosted).unwrap();

        let (_, content) = two_platform_index();
        index
            .commit_published_root(&tagged_id("3.28"), &root_bytes_for(&content), RootScope::Tag("3.28"))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            hosted,
            "an existing config.json is left byte-identical, name_segments included"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_read_root_self_heal_leaves_a_config_less_tree_config_less(/* S-019 */) {
        // C-022's containment claim, and the reason the hook is not in
        // `CatalogTransaction::commit`: the self-heal shares that primitive, so
        // a hook there would make a plain resolve create `config.json` in a
        // tree ocx may not own.
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        seed_root_and_dispatch(&dir).await;

        index
            .resolve_dispatch(&tagged_id("3.28"), SourceKind::Published)
            .await
            .unwrap()
            .expect("a published root + dispatch object resolves");

        let store = store(&dir);
        assert!(
            store.source_catalog_path(REGISTRY).exists(),
            "prerequisite: the resolve really did drive the catalog self-heal"
        );
        assert!(
            !store.source_config_path(REGISTRY).exists(),
            "a read path must never write config.json (C-022)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_real_contended_acquire_is_what_the_hook_absorbs() {
        // Coupled to the lock path rather than to an error the test builds
        // itself: if `FileLock`'s synthesized timeout ever changes shape, the
        // predicate stops matching and a lost lock race becomes a hard failure
        // in the update path — silently, if the only pin is hand-built.
        //
        // A short timeout stands in for `SOURCE_LOCK_TIMEOUT`; the error is the
        // same one the 60s wait produces, and the test costs 50ms.
        let dir = TempDir::new().unwrap();
        let store = store(&dir);
        let _held = store
            .lock_source("index-catalog", REGISTRY, "c/index.json", SOURCE_LOCK_TIMEOUT)
            .await
            .unwrap();

        let error = store
            .lock_source(
                "index-catalog",
                REGISTRY,
                "c/index.json",
                std::time::Duration::from_millis(50),
            )
            .await
            .expect_err("the guard above still holds this lock");
        assert!(
            is_lock_timeout(&error),
            "the hook must recognize the error the lock path really produces, got {error:?}"
        );
    }

    #[test]
    fn a_genuine_io_failure_is_never_absorbed_by_the_config_hook() {
        // The lenient direction of the same discrimination: absorbing one of
        // these would report a failed write as a successful update.
        let denied = crate::error::file_error(
            std::path::Path::new("/x/config.json"),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert!(!is_lock_timeout(&denied), "a genuine I/O failure still propagates");

        // ETIMEDOUT from a network filesystem carries the same kind. The OS
        // error number is what separates it from ocx's own synthesized wait
        // timeout — absorbing it would turn a failed NFS write into a success.
        //
        // The numeric code for that timeout is per-platform (ETIMEDOUT is 110
        // on Linux, 60 on Darwin; Windows reaches the kind through its own
        // codes), so the candidate that actually maps to `TimedOut` here is
        // discovered rather than assumed — a hardcoded 110 lands on
        // `Uncategorized` off Linux and fails the prerequisite below for a
        // reason that has nothing to do with the behaviour under test.
        let os_timeout = [110, 60, 10060, 121]
            .into_iter()
            .find(|code| std::io::Error::from_raw_os_error(*code).kind() == std::io::ErrorKind::TimedOut)
            .expect("this platform must map some OS error number to ErrorKind::TimedOut");
        let nfs = crate::error::file_error(
            std::path::Path::new("/x/config.json"),
            std::io::Error::from_raw_os_error(os_timeout),
        );
        assert_eq!(nfs_kind(&nfs), std::io::ErrorKind::TimedOut, "prerequisite: same kind");
        assert!(
            !is_lock_timeout(&nfs),
            "an OS ETIMEDOUT is a write failure, not a lost race"
        );
    }

    /// The `io::ErrorKind` inside an `InternalFile`, for the prerequisite
    /// assertion above — the test is only meaningful if the OS error really
    /// does collapse to `TimedOut`.
    fn nfs_kind(error: &crate::Error) -> std::io::ErrorKind {
        match error {
            crate::Error::InternalFile(_, io) => io.kind(),
            other => panic!("expected InternalFile, got {other:?}"),
        }
    }
}
