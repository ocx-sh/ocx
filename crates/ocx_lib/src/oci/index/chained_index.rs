// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::time::Duration;

use async_trait::async_trait;

use super::local_index::{DispatchResolution, SourceKind};
use super::{ChainMode, Index, IndexOperation, LocalIndex, index_impl};
use crate::file_structure::BlobStore;
use crate::utility::singleflight;
use crate::{Result, log, oci};

/// Whether `err` reports a present-but-corrupt dispatch object — its on-disk
/// bytes no longer hash to the digest that names them (`adr_index_indirection.md`
/// A3, CWE-345). Distinct from an absent object, which the local read path
/// reports as `Ok(None)`. `ChainedIndex` uses this to decide whether a corrupt
/// local read can be healed by a source re-fetch (online `Resolve`) or must be
/// escalated to a hard `DataError` (`--offline`, or any pure `Query`).
fn is_corrupt_index_object(err: &crate::Error) -> bool {
    matches!(
        err,
        crate::Error::FileStructure(crate::file_structure::error::Error::DigestMismatch { .. })
    )
}

/// Whether `err` is a human-lane refusal surfaced from the COMMITTED local root
/// — a tag resolving to a yanked entry without the opt-in
/// (`adr_index_indirection.md` F3). Unlike a corrupt object (healable by a
/// source re-fetch) or a plain local miss (fall through to a source), a yank is
/// an authoritative publisher signal: it must propagate as a hard `DataError`
/// straight out of the local read, never fall through to a source that would
/// serve the same name and silently bypass the refusal. This is the offline
/// counterpart to the authoritative-source refusal `fetch_and_persist_chain`
/// already stops the walk on.
fn is_local_status_refusal(err: &crate::Error) -> bool {
    matches!(err, crate::Error::OciIndex(super::error::Error::YankedRefused { .. }))
}

/// Recompute-verify: does `bytes` genuinely hash to `digest`?
///
/// `BlobStore` is a stateless CAS — it does not self-verify on read or write
/// (see its own doc comment's "Trust" section) — so every caller reading
/// content out of it is responsible for checking the bytes against the digest
/// that names them (CWE-345 trust-boundary check). Shared by
/// [`ChainedIndex::recover_absent_leaf`] (leaf-manifest recovery) and
/// [`index_impl::IndexImpl::fetch_blob`] (config-blob cache-first read and
/// post-fetch verify) so the recompute logic lives in exactly one place.
fn digest_matches(bytes: &[u8], digest: &oci::Digest) -> bool {
    digest.algorithm().hash(bytes) == *digest
}

/// A curated local index plus an ordered list of upstream sources
/// queried on miss for `Resolve` callers.
///
/// The local index is **not** a transparent cache: it is populated only
/// by explicit paths (`ocx index update`) or `Resolve` callers
/// (install / pull). Concurrent identical cache misses are deduplicated
/// via [`singleflight`](crate::utility::singleflight) — only the leader
/// task performs the fetch; waiters reuse its result.
///
/// `ChainMode` controls how mutable lookups (tag listings, catalog) are
/// routed:
///
/// - `Default` reads the persisted local index. `Resolve` callers walk
///   the chain and persist on miss; `Query` callers return `None` and
///   never contact a source.
/// - `Remote` queries sources directly for mutable lookups and never
///   consults the local index. A pure query in Remote mode never mutates
///   local state — `--remote` is a read-through-to-source flag, not a
///   write-through cache fill. If every source errors the failure is
///   propagated rather than silently falling back to the local index.
/// - `Offline` reads the local index only; sources are never consulted.
///
/// Digest-addressed reads still consult the local index first in any
/// mode because immutable content cannot be wrong. The query/resolve
/// split is encoded by the [`super::IndexOperation`] argument:
/// `Query` callers never trigger a chain walk, `Resolve` callers do.
pub struct ChainedIndex {
    /// The curated, persisted local index. Not a transparent cache — see
    /// the struct doc for the lifecycle. Renamed from `cache` to make
    /// "this is data we deliberately wrote, not opportunistic cache fill"
    /// the obvious mental model at every access site.
    local_index: LocalIndex,
    sources: Vec<Index>,
    mode: ChainMode,
    /// Singleflight group for de-duplicating concurrent cache-miss fetches
    /// against the same (mode, identifier) key. Shared across clones so all
    /// `box_clone` spawned waiters converge on the same leader. Carries the
    /// full resolved `(digest, manifest)` — not just the digest — because a
    /// leaf platform manifest is never written into the local index (A3), so
    /// a post-walk local read-back cannot recover it; the walk's own fetch is
    /// the only place the manifest bytes ever exist locally.
    singleflight: singleflight::Group<String, Option<(oci::Digest, oci::Manifest)>>,
    /// When set, `fetch_and_persist_chain` never commits a tag pointer into
    /// the local index — resolution lands only in the caller's own record
    /// (e.g. `ocx.lock`). Content-addressed blob writes still happen. Set by
    /// [`Self::new_lock_scoped`] for the update-verb family; see
    /// `adr_toolchain_update_family.md`.
    suppress_tag_commit: bool,
    /// The machine-global blob store (`$OCX_HOME/blobs`) that holds installed
    /// **content** — leaf platform manifests and their layers — distinct from
    /// the local index, which holds resolution **dispatch** only
    /// (`adr_index_indirection.md` B2). A leaf platform manifest is never
    /// written into the local index (A3), but it is cached here at install time
    /// (`stage_and_link_chain_blobs`), so a [`DispatchResolution::AbsentLeaf`]
    /// (content absent from `o/`) is recovered from this store **before** any
    /// source walk — the step that makes offline exec of an installed tool
    /// resolve with zero network (A3 step 2). `None` for constructions that
    /// have no machine-global content to consult (unit fakes, the lock-scoped
    /// update index); the store lives here rather than on [`LocalIndex`] because
    /// it is machine-global content the chain orchestrates over, not part of the
    /// travels-with-a-copy local index (Decision H).
    ///
    /// Also the sole route [`index_impl::IndexImpl::fetch_blob`] uses for
    /// config-blob content — the index-home flat blob CAS has been retired
    /// (`adr_index_indirection.md` B2). `content_store: None` is a **test-only
    /// affordance**: production always attaches `fs.blobs` via
    /// [`super::Index::from_chained_with_content_store`] (`context.rs`).
    content_store: Option<BlobStore>,
}

/// Max in-flight singleflight keys. Scoped per ChainedIndex instance —
/// generous because each key maps to one package identifier under refresh.
const SINGLEFLIGHT_MAX_KEYS: usize = 1024;

/// Max time waiters block for the leader. Matches the blob-store write
/// timeout so a stuck leader surfaces rather than stalling the CLI forever.
const SINGLEFLIGHT_TIMEOUT: Duration = Duration::from_secs(120);

impl ChainedIndex {
    pub fn new(local_index: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            local_index,
            sources,
            mode,
            singleflight: singleflight::Group::new(SINGLEFLIGHT_MAX_KEYS, SINGLEFLIGHT_TIMEOUT),
            suppress_tag_commit: false,
            content_store: None,
        }
    }

    /// Like [`Self::new`], but tag resolution never commits a tag pointer
    /// into the local index — the caller's lock is the canonical record.
    /// Used by the update-verb family (`ocx update`), which resolves tags
    /// live without mutating the shared tag store.
    pub fn new_lock_scoped(local_index: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            suppress_tag_commit: true,
            ..Self::new(local_index, sources, mode)
        }
    }

    /// Attach the machine-global blob store so an [`DispatchResolution::AbsentLeaf`]
    /// recovers its leaf platform manifest from `$OCX_HOME/blobs` before any
    /// source walk (`adr_index_indirection.md` A3 step 2 / B2). Consuming
    /// builder — keeps the `new` / `from_chained` signatures unchanged so the
    /// blob store is opt-in at the one production construction site
    /// (`context.rs`) without churning every caller.
    pub fn with_content_store(mut self, content_store: BlobStore) -> Self {
        self.content_store = Some(content_store);
        self
    }

    /// Provenance for `identifier`'s namespace (`adr_index_indirection.md`
    /// A2/H) — the configured source authoritative for it, if any, asked for
    /// its cheap, synchronous [`Index::source_kind`]. No configured source
    /// claims the namespace (e.g. `--offline`, where `self.sources` is empty
    /// by construction) defaults to [`SourceKind::Derived`]: the uncatalogued
    /// read shares the exact root-document path with the catalogued one and
    /// only skips the catalog cross-check/self-heal, never resolution
    /// correctness.
    fn kind_for(&self, identifier: &oci::Identifier) -> SourceKind {
        self.sources
            .iter()
            .find(|source| source.is_authoritative_for(identifier))
            .map(Index::source_kind)
            .unwrap_or(SourceKind::Derived)
    }

    /// [`Self::kind_for`] for a bare registry (no repository) —
    /// `list_repositories`'s namespace-only query. `is_authoritative_for`
    /// inspects only `.registry()`, so the placeholder repository segment is
    /// never actually read.
    fn kind_for_registry(&self, registry: &str) -> SourceKind {
        self.kind_for(&oci::Identifier::new_registry("_", registry))
    }

    /// Policy probe for no-resolve modes: an unpinned identifier whose
    /// tag/digest is absent from the local index raises
    /// `PolicyResolutionBlocked`. Genuine local-index I/O / parse errors
    /// propagate (must not be masked as a policy block).
    ///
    /// Called from `walk_chain` for both `Offline` (unpinned path) and
    /// `Frozen` (all unpinned paths). Each mode decides what to do after
    /// the probe returns `Ok(())` — Offline early-returns; Frozen falls
    /// through to the source walk. A dispatch object being present is not
    /// required — [`DispatchResolution::AbsentLeaf`] still names a known
    /// digest, so it counts as locally resolvable too; only a genuinely
    /// unknown root/tag (`Ok(None)`) is a policy block.
    async fn ensure_locally_resolvable(&self, identifier: &oci::Identifier) -> Result<()> {
        let kind = self.kind_for(identifier);
        let locally_resolvable = self.local_index.resolve_dispatch(identifier, kind).await?.is_some();
        if !locally_resolvable {
            return Err(super::error::Error::PolicyResolutionBlocked {
                identifier: identifier.to_string(),
                policy: self.mode.policy_label(),
            }
            .into());
        }
        Ok(())
    }

    /// Recover an [`DispatchResolution::AbsentLeaf`]'s `content` from the
    /// machine-global blob store (`adr_index_indirection.md` A3 step 2 / B2).
    ///
    /// A leaf platform manifest is never written into the local index (A3), so a
    /// tag/digest whose `content` is absent from `o/` reports `AbsentLeaf`. That
    /// content is not lost: it was cached into `$OCX_HOME/blobs` at install time
    /// (`stage_and_link_chain_blobs`, `ChainRole::Manifest`). This read is tried
    /// **before** the source walk so an installed tool resolves offline with
    /// zero network — the "installed-tool offline exec is unaffected by A3"
    /// guarantee (B2).
    ///
    /// The read is digest-verified (`sha256(bytes) == content`, A4): a
    /// content-addressed store should hash to its key, and re-verifying keeps a
    /// corrupt or truncated blob from masquerading as a valid leaf. A verify or
    /// decode failure returns `Ok(None)` (a clean recovery miss) so the caller
    /// falls through to the source walk (online) or the offline/policy path,
    /// exactly as if the blob were absent — never a hard error swallowing a
    /// genuine miss.
    ///
    /// If the recovered bytes decode as an **image index** rather than a leaf
    /// (an incomplete snapshot whose dispatch object was evicted from `o/` but
    /// still lingers in the blob store), the object self-heals back into the
    /// local index (`stage_dispatch_bytes`) so the next dispatch reads it
    /// locally (A3 step 2 fallback), then is returned so the current resolve
    /// proceeds without a network round-trip.
    ///
    /// Returns `Ok(None)` when no blob store is attached (unit fakes, the
    /// lock-scoped update index) or the content is not cached locally.
    async fn recover_absent_leaf(
        &self,
        identifier: &oci::Identifier,
        content: &oci::Digest,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        let Some(content_store) = &self.content_store else {
            return Ok(None);
        };
        let Some(bytes) = content_store.read_blob(identifier.registry(), content).await? else {
            return Ok(None);
        };
        // Digest-verify the recovered bytes against the content digest they are
        // keyed by (A4). A published-source AbsentLeaf names an observation-object
        // digest that no registry blob endpoint serves, so this store misses for
        // it (returns above) — the leaf-trap the ADR warns against cannot fire here.
        if !digest_matches(&bytes, content) {
            log::warn!(
                "blob-store manifest for '{content}' failed digest verification (recomputed {}); \
                 removing the corrupt object and falling through to the source walk",
                content.algorithm().hash(&bytes)
            );
            // Remove the present-but-corrupt blob before returning the miss
            // (subsystem-oci.md: a corrupt entry is removed before the source
            // walk so the write-through that follows a successful fetch heals
            // it). Leaving it in place would let `write_blob`'s check-first fast
            // path re-accept the corrupt file on the next resolve, so an offline
            // resolve would keep loading tampered bytes. Best-effort — a removal
            // failure must not fail the resolve, whose manifest the source walk
            // still supplies; the install-staging shortcut re-verifies too.
            if let Err(error) = content_store.remove_blob(identifier.registry(), content).await {
                log::warn!("failed to remove corrupt blob-store object '{content}': {error}");
            }
            return Ok(None);
        }
        let manifest: oci::Manifest = match serde_json::from_slice(&bytes) {
            Ok(manifest) => manifest,
            Err(error) => {
                // Not an OCI manifest — e.g. a published-source obs digest that
                // happens to name a cached blob of another shape. Not a leaf
                // recovery; fall through so the source-kind-routed walk decides.
                log::debug!("blob-store object for '{content}' is not an OCI manifest ({error}); not a leaf recovery");
                return Ok(None);
            }
        };
        // Self-heal an incomplete snapshot: an image index present in the blob
        // store but missing from `o/` is staged back so the next dispatch reads
        // it locally (A3 step 2 fallback). Best-effort — a heal failure must not
        // fail the resolve, whose manifest is already in hand.
        if matches!(manifest, oci::Manifest::ImageIndex(_))
            && let Err(error) = self.local_index.stage_dispatch_bytes(identifier, content, &bytes).await
        {
            log::warn!("failed to self-heal dispatch object '{content}' into the local index: {error}");
        }
        Ok(Some((content.clone(), manifest)))
    }

    /// Walk the source chain for an identifier — fetch the manifest (by tag
    /// or digest) and persist the dispatch object into the local index.
    /// Wrapped in a singleflight guard so concurrent waiters share the
    /// leader's result.
    ///
    /// `grow_root` distinguishes the two miss shapes a caller can observe
    /// locally before walking (`adr_index_indirection.md` "Grow ≠ refresh"):
    /// a genuinely unknown root/tag (`true` — the walk also grows the local
    /// copy, symmetric across published/derived) versus an already-known
    /// root whose dispatch object is merely absent
    /// ([`DispatchResolution::AbsentLeaf`], `false` — recovery only, the
    /// root is never re-copied). Invariant 1 (a published root is never
    /// auto-refreshed under Default) is preserved because `grow_root` is
    /// only ever `true` on a genuine first-time miss, never on an
    /// AbsentLeaf recovery of an already-present root.
    ///
    /// Returns `Ok(Some((digest, manifest)))` when one source successfully
    /// resolved the identifier — the manifest is returned directly, not
    /// re-read from local storage, because a leaf platform manifest is never
    /// written into the local index (A3) and a read-back would find nothing
    /// for that shape — or `Ok(None)` when nothing was fetched (not found,
    /// no sources, or an Offline early-return). Returns `Err(_)` when every
    /// source errored — preserves the trust boundary between "not found"
    /// (cache retry → `None`) and "registry outage".
    async fn walk_chain(
        &self,
        identifier: &oci::Identifier,
        grow_root: bool,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        // No-resolve policies (offline, frozen) refuse to resolve an unpinned
        // (tag-only) reference whose tag→digest mapping is genuinely absent from
        // the local index. A digest-bearing identifier is an already-known
        // version, so it is exempt here — Offline still blocks its *content*
        // fetch just below; Frozen lets it walk the chain like Default.
        //
        // The block fires only on a true *resolution* miss. A tag pointer that
        // DOES resolve locally — even when its manifest blob is missing from the
        // cache — is not a policy block: that is a content-fetch concern handled
        // downstream (offline → `OfflineManifestMissing` naming the digest).
        // `fetch_manifest` reaches `walk_chain` for both "tag absent" and "tag
        // present, blob missing"; probing the tag pointer (no source contact —
        // `LocalIndex::fetch_manifest_digest` needs only the tag store) tells the
        // two apart.
        //
        // Errors from the probe are propagated with `?` — a corrupt or unreadable
        // local index must surface as a real I/O error, not be silently treated
        // as "tag absent" which would incorrectly raise PolicyResolutionBlocked.
        match self.mode {
            // Offline never contacts a source. For unpinned (tag-only) identifiers,
            // first check whether the tag IS locally resolvable — a true miss raises
            // PolicyResolutionBlocked; a hit lets through and falls to the early-return.
            // For digest-bearing identifiers the check is skipped: the early-return fires
            // immediately (content stays unfetched; the resulting `None` is a policy error
            // at the content boundary).
            ChainMode::Offline => {
                if identifier.digest().is_none() {
                    // Probe the local tag store; raises PolicyResolutionBlocked if
                    // the tag is genuinely absent. I/O errors propagate unchanged.
                    self.ensure_locally_resolvable(identifier).await?;
                }
                // Digest-addressed offline miss: no source contact, content stays
                // unfetched (the resulting `None` becomes a policy error at the
                // content boundary).
                return Ok(None);
            }
            // Frozen refuses to resolve an unpinned reference from a source, but
            // digest-addressed (already-known) content is still fetched like Default.
            ChainMode::Frozen if identifier.digest().is_none() => {
                // Probe the local tag store; raises PolicyResolutionBlocked if
                // the tag is genuinely absent. I/O errors propagate unchanged.
                self.ensure_locally_resolvable(identifier).await?;
                // Tag is locally resolvable: fall through to source walk so the
                // missing content blob is fetched (only unknown-tag *resolution* is
                // blocked; content fetches for known tags are allowed).
            }
            ChainMode::Default | ChainMode::Remote | ChainMode::Frozen => {}
        }

        // Digest-bearing inputs (digest-only OR tag+digest pinned-id pulls)
        // are fetched directly via `GET /v2/<repo>/manifests/<digest>`. The
        // tag-pointer commit decision lives in `fetch_and_persist_chain`:
        // tag+digest skips the commit because `ocx.lock` is canonical.
        // Bare or tag-only inputs normalise to `tag_or_latest()` so the
        // singleflight key collapses concurrent waiters.
        let walked = if identifier.digest().is_some() {
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
            Acquisition::Resolved(head) => return Ok(head),
        };

        // Leader path: walk sources and persist on first success. On
        // failure, wrap the leader's typed error in `ArcError` and broadcast
        // the same `SourceWalkFailed(ArcError)` variant to waiters. The
        // leader also propagates that wrapped variant to its caller so both
        // ends see a consistent, typed error with the original source chain.
        match self.fetch_and_persist_chain(&walked, grow_root).await {
            Ok(head) => {
                handle.complete(head.clone());
                Ok(head)
            }
            Err(e) => {
                let arc = crate::error::ArcError::from(e);
                let broadcast = super::error::Error::SourceWalkFailed(arc.clone());
                let _ = handle.fail(broadcast);
                Err(super::error::Error::SourceWalkFailed(arc).into())
            }
        }
    }

    /// Leader-side chain walk: iterates sources, fetches + persists the
    /// dispatch object from the first success, then (kind-routed, `grow_root`
    /// gated) grows the local root.
    ///
    /// Sources are tried sequentially in priority order; the first success short-circuits.
    /// Parallel-peer fallback is intentionally not supported — peer registries are out of scope.
    ///
    /// Returns `Ok(Some((digest, manifest)))` when one source resolved the
    /// identifier, or `Ok(None)` when every source returned a clean
    /// not-found with no errors. Returns `Err(_)` when any source errored
    /// and no source succeeded — we do not treat a later `Ok(false)` as
    /// disproving an earlier failure.
    async fn fetch_and_persist_chain(
        &self,
        identifier: &oci::Identifier,
        grow_root: bool,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        let mut last_error: Option<crate::Error> = None;
        for source in &self.sources {
            match self.local_index.persist_dispatch(source, identifier).await {
                Ok(Some((digest, manifest))) => {
                    // Root growth gated on identifier shape (same contract as
                    // the legacy tag-pointer commit):
                    //   - tag-only (`cmake:1.0`)            → grow
                    //   - digest-only (`cmake@sha256:...`)  → skip (no tag to pin)
                    //   - tag+digest (`cmake:1.0@sha256:`)  → skip (pinned-id pull;
                    //                                        `ocx.lock` is canonical)
                    //   - bare repo (`cmake`)               → normalised to `latest`
                    //                                        in walk_chain → grows
                    // The `tag+digest` skip is the post-pin contract change: the
                    // caller already has the digest pinned in `ocx.lock`, so a
                    // root write here is redundant and silently shadows the lock.
                    // See `adr_index_routing_semantics.md`.
                    // `suppress_tag_commit` extends the same principle to
                    // tag-only resolution performed on behalf of a lock
                    // (`ocx update`); see `adr_toolchain_update_family.md`.
                    // `grow_root` further gates on the miss shape the caller
                    // observed locally — an AbsentLeaf recovery of an
                    // already-known root must never re-copy it (Invariant 1).
                    if grow_root
                        && !self.suppress_tag_commit
                        && identifier.tag().is_some()
                        && identifier.digest().is_none()
                    {
                        match source.source_kind() {
                            // C2 policy: a Default write-through resolve of a
                            // published package ALSO grows the local copy —
                            // symmetric with derived, removing the
                            // install→offline asymmetry.
                            SourceKind::Published => {
                                if let Some((root_bytes, _root)) = source.fetch_root_document(identifier).await? {
                                    self.local_index.persist_published_root(identifier, &root_bytes).await?;
                                }
                            }
                            SourceKind::Derived => {
                                self.local_index.commit_root_tag(identifier, &digest).await?;
                            }
                        }
                    }
                    log::debug!("Fetched '{}' from chained source, persisted to cache.", identifier);
                    return Ok(Some((digest, manifest)));
                }
                Ok(None) => {
                    // A clean miss from the one source authoritative for this
                    // identifier's namespace (Decision H) is terminal — it must
                    // never fall through to a lower, non-authoritative source
                    // (the `OciIndex` catch-all) that could answer the same
                    // name over a different protocol. Falling through here
                    // would re-introduce the index->OCI-tags fallback chain
                    // `adr_index_indirection.md` Decision H dissolved. Mirrors
                    // the `Err` arm's authoritative-stop just below; a
                    // non-authoritative source's miss keeps the fall-through
                    // behaviour so foreign-namespace routing is unaffected.
                    if source.is_authoritative_for(identifier) {
                        log::debug!("Authoritative source has no '{}' — stopping.", identifier);
                        return Ok(None);
                    }
                    log::debug!("Source has no '{}' — trying next source.", identifier);
                }
                Err(e) => {
                    // An authoritative source's refusal (yanked tag, obs tamper,
                    // fail-closed format) must STOP the walk — never fall through
                    // to a lower source that could answer the same name and both
                    // bypass the refusal and leak induced-error traffic to it
                    // (`adr_index_indirection.md` F3). Transient errors from a
                    // non-authoritative source keep the fall-through behaviour.
                    if source.is_authoritative_for(identifier) {
                        log::warn!("Authoritative source refused '{}': {e}", identifier);
                        return Err(e);
                    }
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
        Ok(None)
    }

    /// Remote-mode pure-query manifest read: consult the source chain directly
    /// and return the first hit **without persisting**.
    ///
    /// `--remote` is a read-through-to-source flag, not a write-through cache
    /// fill, so a tag-addressed `Query` must reach the live registry (the same
    /// routing `list_tags` already uses in Remote mode) yet never mutate the
    /// local index. First `Some` wins; if every source errors the failure is
    /// propagated rather than masked as a clean miss (trust boundary — a
    /// registry outage must not look like "not found"). A clean miss from the
    /// source authoritative for `identifier`'s namespace is likewise terminal —
    /// it never falls through to a lower, non-authoritative source (Decision
    /// H: exactly one remote per namespace). See `adr_index_routing_semantics.md`.
    async fn query_sources_manifest(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        let mut last_error: Option<crate::Error> = None;
        for source in &self.sources {
            match source.fetch_manifest(identifier, IndexOperation::Query).await {
                Ok(Some(result)) => return Ok(Some(result)),
                // Same authoritative-stop as `fetch_and_persist_chain`: a clean
                // miss from the namespace's one authoritative source is
                // terminal, never a fall-through to the `OciIndex` catch-all.
                Ok(None) if source.is_authoritative_for(identifier) => return Ok(None),
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Remote-mode fetch_manifest failed for '{}': {e}", identifier);
                    last_error = Some(e);
                }
            }
        }
        last_error.map_or(Ok(None), Err)
    }

    /// Digest counterpart to [`Self::query_sources_manifest`] — same Remote-mode
    /// read-through-without-persist contract.
    async fn query_sources_manifest_digest(&self, identifier: &oci::Identifier) -> Result<Option<oci::Digest>> {
        let mut last_error: Option<crate::Error> = None;
        for source in &self.sources {
            match source.fetch_manifest_digest(identifier, IndexOperation::Query).await {
                Ok(Some(digest)) => return Ok(Some(digest)),
                // Same authoritative-stop as `query_sources_manifest`.
                Ok(None) if source.is_authoritative_for(identifier) => return Ok(None),
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Remote-mode fetch_manifest_digest failed for '{}': {e}", identifier);
                    last_error = Some(e);
                }
            }
        }
        last_error.map_or(Ok(None), Err)
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
        self.local_index
            .list_local_repositories(registry, self.kind_for_registry(registry))
            .await
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        // Tag listings route by mode. Default and Offline read the local
        // index only; Remote queries sources directly without write-through.
        // A pure query must never mutate local state — write paths live on
        // `LocalIndex::refresh_tags` (called from `ocx index update`) and
        // the `persist_dispatch` + root-growth pair driven by
        // `fetch_and_persist_chain` (called from install / pull). First Ok
        // wins; if every source errors we propagate.
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
        self.local_index
            .list_local_tags(identifier, self.kind_for(identifier))
            .await
    }

    async fn fetch_manifest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        // Digest-addressed reads are local-first in every mode — immutable
        // content cannot be wrong. Mutable (tag-based) reads in `Remote`
        // mode go straight to the chain walk, skipping the local read.
        let is_digest_addressed = identifier.digest().is_some();
        let kind = self.kind_for(identifier);
        // Set only when the local read hit a *recoverable* corrupt dispatch
        // object (root/tag already known, only the object is tampered) — the
        // walk below must heal ONLY that object, never re-copy/re-grow an
        // already-known root (Invariant 1, F1 never-auto-refreshed).
        let mut corrupt_known = false;
        let local = if is_digest_addressed || self.mode != ChainMode::Remote {
            match self.local_index.resolve_dispatch(identifier, kind).await {
                Ok(resolution) => resolution,
                Err(e) => {
                    // A yanked-tag refusal from the committed local root is an
                    // authoritative publisher signal (F3) — propagate it straight
                    // out rather than fall through to a source that would serve the
                    // same name and bypass the refusal.
                    if is_local_status_refusal(&e) {
                        return Err(e);
                    }
                    // A present-but-corrupt dispatch object is only recoverable
                    // by a Resolve walk that actually re-fetches and overwrites
                    // it. A pure Query never persists, and Offline consults no
                    // source — surface the corruption as a hard error (DataError)
                    // rather than a silent cache miss. Default/Remote/Frozen
                    // Resolve fall through: the walk re-fetches and self-heals
                    // the object.
                    let corrupt = is_corrupt_index_object(&e);
                    if corrupt && !(op == IndexOperation::Resolve && self.mode != ChainMode::Offline) {
                        return Err(e);
                    }
                    corrupt_known = corrupt;
                    log::warn!(
                        "Local index read failed for '{}', falling back to chained source: {e}",
                        identifier
                    );
                    None
                }
            }
        } else {
            None
        };
        // A locally-cached dispatch object answers the read directly; an
        // `AbsentLeaf` (the digest/tag is known but its bytes are not
        // locally cached, A3) falls through to source recovery below, same
        // as a genuine local miss.
        if let Some(DispatchResolution::Dispatch { content, manifest }) = local {
            return Ok(Some((content, *manifest)));
        }
        // Order: local index dispatch → blob store → sources
        // (`adr_index_indirection.md` A3 step 2). An `AbsentLeaf` names a known
        // `content` digest whose bytes are absent from `o/` — a leaf platform
        // manifest, which is CONTENT cached into `$OCX_HOME/blobs` at install
        // (B2), never the local index. Consult that store before any source
        // walk so an installed tool resolves offline with zero network. A miss
        // (or no attached store) falls through unchanged.
        if let Some(DispatchResolution::AbsentLeaf { content }) = &local
            && let Some(recovered) = self.recover_absent_leaf(identifier, content).await?
        {
            return Ok(Some(recovered));
        }
        // Remote-mode pure queries read through to the source without
        // persisting — `--remote` forces a live lookup for mutable
        // (tag-addressed) reads, the same as `list_tags`, but a query must
        // never write the local index. Digest-addressed reads stay local-first
        // (handled above) because immutable content cannot be wrong.
        if self.mode == ChainMode::Remote && op == IndexOperation::Query && !is_digest_addressed {
            return self.query_sources_manifest(identifier).await;
        }
        // Pure queries never walk the chain. The local index's role is to
        // cache resolved data; populating it on a query call would silently
        // mutate state from a read-only command.
        match op {
            IndexOperation::Query => Ok(None),
            IndexOperation::Resolve => {
                // `AbsentLeaf` or a recoverable corrupt object both mean the
                // root/tag is already known locally — recover ONLY the
                // dispatch content (`grow_root = false`, Invariant 1: never
                // re-copy an already-present published root). Only a genuine
                // miss (`None`, no corruption) grows the local root on
                // success (C2 policy).
                let grow_root = !corrupt_known && !matches!(local, Some(DispatchResolution::AbsentLeaf { .. }));
                self.walk_chain(identifier, grow_root).await
            }
        }
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<oci::Digest>> {
        let is_digest_addressed = identifier.digest().is_some();
        let kind = self.kind_for(identifier);
        // See `fetch_manifest`'s identical flag — a recoverable corrupt
        // dispatch object means the root/tag is already known, so the walk
        // must not re-grow the root.
        let mut corrupt_known = false;
        if is_digest_addressed || self.mode != ChainMode::Remote {
            match self.local_index.resolve_dispatch(identifier, kind).await {
                Ok(Some(DispatchResolution::Dispatch { content, .. })) => return Ok(Some(content)),
                // Unlike `fetch_manifest`, a TAG-addressed digest read is
                // answerable from `AbsentLeaf` too — the root lookup already
                // confirmed the tag exists and names this content, only the
                // dispatch bytes are uncached. A DIGEST-addressed `AbsentLeaf`
                // is just the caller's own input echoed back with no existence
                // confirmation, so it needs the object locally present, a
                // cached leaf blob, or a source to confirm existence.
                Ok(Some(DispatchResolution::AbsentLeaf { content })) if !is_digest_addressed => {
                    return Ok(Some(content));
                }
                // A DIGEST-addressed `AbsentLeaf`: confirm existence from the
                // machine-global blob store (installed content, A3 step 2 / B2)
                // before falling through to the source walk, so an offline
                // digest query resolves with zero network when the leaf is
                // cached. A miss falls through unchanged.
                Ok(Some(DispatchResolution::AbsentLeaf { content })) => {
                    if let Some((digest, _)) = self.recover_absent_leaf(identifier, &content).await? {
                        return Ok(Some(digest));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    // Same authoritative-refusal propagation as `fetch_manifest`:
                    // a yanked-tag refusal from the committed local root is a hard
                    // `DataError`, never a fall-through to a source (F3).
                    if is_local_status_refusal(&e) {
                        return Err(e);
                    }
                    // Same corrupt-object routing as `fetch_manifest`: escalate a
                    // present-but-corrupt local object to a hard error unless an
                    // online Resolve walk can re-fetch and heal it.
                    let corrupt = is_corrupt_index_object(&e);
                    if corrupt && !(op == IndexOperation::Resolve && self.mode != ChainMode::Offline) {
                        return Err(e);
                    }
                    corrupt_known = corrupt;
                    log::warn!(
                        "Local index read failed for '{}', falling back to chained source: {e}",
                        identifier
                    );
                }
            }
        }
        if self.mode == ChainMode::Remote && op == IndexOperation::Query && !is_digest_addressed {
            return self.query_sources_manifest_digest(identifier).await;
        }
        match op {
            IndexOperation::Query => Ok(None),
            // Reaches the walk either on a genuine local miss (`AbsentLeaf`
            // already answered above for the tag-addressed case) or a
            // recoverable corrupt object — root growth applies only for the
            // former (C2 policy); a corrupt-object recovery must not re-grow
            // an already-known root (Invariant 1).
            IndexOperation::Resolve => Ok(self
                .walk_chain(identifier, !corrupt_known)
                .await?
                .map(|(digest, _)| digest)),
        }
    }

    /// Fetches genuine content-addressed blob bytes (config blobs) through the
    /// machine-global blob store (`$OCX_HOME/blobs`), never the local index —
    /// the index-home flat blob CAS has been retired (`adr_index_indirection.md`
    /// B2). `content_store: None` (unit-test constructions via [`super::Index::from_chained`])
    /// skips the local read and write-through silently, matching the pre-B2
    /// no-cache contract for those callers.
    ///
    /// - **Cache-first**: an attached store's local hit is digest-verified
    ///   (`BlobStore` does not self-verify — [`digest_matches`]). A verified
    ///   hit returns immediately; a corrupt hit escalates to a hard
    ///   `DigestMismatch` under `--offline` (no source to heal with), or online
    ///   warns and falls through to the source walk, marking the entry for an
    ///   atomic in-place heal (`BlobStore::replace_blob` — see below).
    /// - **Offline + local miss** → `Ok(None)` (the `OfflineManifestMissing` →
    ///   exit 81 contract lives downstream at the policy boundary).
    /// - **Source walk (online)**: first `Some` wins. Fetched bytes are
    ///   digest-verified against `blob_ref.digest()` BEFORE being returned
    ///   (CWE-345 — never trust unverified remote bytes), then written
    ///   through to the content store — `BlobStore::replace_blob` (unconditional
    ///   atomic tempfile+rename, no existence check) when the cache-first read
    ///   found a corrupt entry, `BlobStore::write_blob` (idempotent fast path)
    ///   otherwise. A **remove-then-`write_blob`** two-step was tried and
    ///   rejected in review: a removal failure would leave the corrupt file in
    ///   place while the subsequent `write_blob` fast path re-accepts it
    ///   unchanged, so the heal silently never happens. `replace_blob` has no
    ///   such window — one atomic rename replaces whatever is there. A
    ///   write-through/replace failure is logged, not fatal — the fetch still
    ///   returns the verified bytes to the caller; a stuck heal is retried on
    ///   the next online fetch. Propagates the last error if every source
    ///   erred (trust boundary).
    async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
        let digest = blob_ref.digest();
        // Set when the cache-first read found a present-but-corrupt entry —
        // the write-through below must heal it via an unconditional atomic
        // replace, not `write_blob`'s existence-checked fast path (which would
        // silently re-accept the still-corrupt file untouched).
        let mut heal_corrupt = false;
        if let Some(content_store) = &self.content_store
            && let Some(bytes) = content_store.read_blob(blob_ref.registry(), &digest).await?
        {
            if digest_matches(&bytes, &digest) {
                return Ok(Some(bytes));
            }
            // Present-but-corrupt: only a source re-fetch can heal it. Offline
            // has no source, so escalate to a hard error rather than silently
            // discarding tampered content.
            if self.mode == ChainMode::Offline {
                return Err(crate::file_structure::error::Error::DigestMismatch {
                    claimed: digest.clone(),
                    computed: digest.algorithm().hash(&bytes),
                }
                .into());
            }
            log::warn!("Blob-store copy of '{blob_ref}' is corrupt, re-fetching from source.");
            heal_corrupt = true;
        }
        if self.mode == ChainMode::Offline {
            return Ok(None);
        }
        // Walk sources; first `Some` wins. Verify BEFORE returning or
        // write-through — a source must never smuggle unverified bytes past
        // this boundary. Propagate last error if every source erred (trust
        // boundary).
        let mut last_error: Option<crate::Error> = None;
        for source in &self.sources {
            match source.fetch_blob(blob_ref).await {
                Ok(Some(bytes)) => {
                    if !digest_matches(&bytes, &digest) {
                        return Err(crate::file_structure::error::Error::DigestMismatch {
                            claimed: digest.clone(),
                            computed: digest.algorithm().hash(&bytes),
                        }
                        .into());
                    }
                    if let Some(content_store) = &self.content_store {
                        // `replace_blob` (unconditional atomic rename) heals a
                        // known-corrupt entry in one step; `write_blob`
                        // (existence-checked fast path) is the ordinary,
                        // cheaper write-through for a genuine cache miss.
                        let write_through = if heal_corrupt {
                            content_store.replace_blob(blob_ref.registry(), &digest, &bytes).await
                        } else {
                            content_store.write_blob(blob_ref.registry(), &digest, &bytes).await
                        };
                        if let Err(e) = write_through {
                            log::warn!("Write-through to blob store failed for '{blob_ref}': {e}");
                        }
                    }
                    return Ok(Some(bytes));
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Source fetch_blob failed for '{blob_ref}': {e}");
                    last_error = Some(e);
                }
            }
        }
        last_error.map_or(Ok(None), Err)
    }

    /// Fetches verbatim manifest bytes straight from the source chain —
    /// never through the local dispatch-object cache. That cache holds
    /// bytes only for dispatch-shaped digests (image index / observation
    /// object); a leaf platform manifest is never copied into it
    /// (`adr_index_indirection.md` A3/B2 — leaf manifests are content,
    /// fetched on demand). The trait default re-serialises the parsed
    /// manifest instead of returning wire-exact bytes, which is wrong for a
    /// caller (chain-blob staging) that persists the result under the
    /// source-claimed digest — a re-serialised JSON body will not, in
    /// general, hash back to that digest.
    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
        if self.mode == ChainMode::Offline {
            return Ok(None);
        }
        let mut last_error: Option<crate::Error> = None;
        for source in &self.sources {
            match source.fetch_manifest_raw_bytes(identifier).await {
                Ok(Some(result)) => return Ok(Some(result)),
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Source fetch_manifest_raw_bytes failed for '{}': {e}", identifier);
                    last_error = Some(e);
                }
            }
        }
        last_error.map_or(Ok(None), Err)
    }

    async fn physical_reference(&self, identifier: &oci::Identifier) -> Result<Option<oci::Identifier>> {
        // Delegate to the sources in priority order; the first that maps the
        // identifier to a physical location wins (only `OcxIndex` does).
        for source in &self.sources {
            if let Some(physical) = source.physical_reference(identifier).await? {
                return Ok(Some(physical));
            }
        }
        Ok(None)
    }

    fn is_authoritative_for(&self, identifier: &oci::Identifier) -> bool {
        self.sources
            .iter()
            .any(|source| source.is_authoritative_for(identifier))
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(Self {
            local_index: self.local_index.clone(),
            sources: self.sources.clone(),
            mode: self.mode,
            // Singleflight group is shared across clones so waiters coalesce.
            singleflight: self.singleflight.clone(),
            suppress_tag_commit: self.suppress_tag_commit,
            content_store: self.content_store.clone(),
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
        file_structure::{BlobStore, IndexStore},
        oci::index::{ChainMode, Index, IndexOperation, LocalConfig, LocalIndex, index_impl},
        oci::{Algorithm, Digest, Identifier, ImageManifest, Manifest},
    };

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";

    fn tagged_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }
    fn digest_only_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_digest(digest_a())
    }
    // Two distinct single-child image INDEXES — distinct bytes so their
    // digests differ, and (A3) the bytes genuinely hash to the digest the
    // source serves. Dispatch-shaped (never a bare leaf manifest) so a
    // routing test's "cache hit" fixtures are genuinely locally-cacheable
    // under the dispatch-only local index (A3 headline: a leaf platform
    // manifest is never written to `o/`, so a flat-manifest fixture could
    // never be a real local hit here) — see `plan_one_index.md` WP-C2's
    // persist-shape rewrite note. The routing assertions these fixtures back
    // (mode gates, singleflight, authoritative-stop) are unchanged; only the
    // wire shape backing them moved from a legacy flat manifest to a dispatch
    // object.
    fn manifest_a_bytes() -> &'static [u8] {
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2,"platform":{"os":"linux","architecture":"amd64"}}]}"#
    }
    fn manifest_b_bytes() -> &'static [u8] {
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","size":3,"platform":{"os":"linux","architecture":"amd64"}}]}"#
    }
    fn digest_a() -> Digest {
        Algorithm::Sha256.hash(manifest_a_bytes())
    }
    fn digest_b() -> Digest {
        Algorithm::Sha256.hash(manifest_b_bytes())
    }
    fn bytes_for(digest: &Digest) -> Vec<u8> {
        if *digest == digest_a() {
            manifest_a_bytes().to_vec()
        } else if *digest == digest_b() {
            manifest_b_bytes().to_vec()
        } else {
            panic!("unknown test digest {digest}")
        }
    }
    fn manifest_for(digest: &Digest) -> Manifest {
        serde_json::from_slice(&bytes_for(digest)).unwrap()
    }

    /// The index store `make_local_index` reads/writes (the default
    /// machine-local home under the temp root). Persisted objects and tags land
    /// here — the index manifest CAS, not `$OCX_HOME/blobs` (A1).
    fn index_store(dir: &TempDir) -> IndexStore {
        IndexStore::new(dir.path().join("index"))
    }

    fn make_local_index(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(LocalConfig {
            index_store: index_store(dir),
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
        async fn fetch_manifest(
            &self,
            identifier: &Identifier,
            _op: super::super::IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            let tag = identifier.tag_or_latest();
            *self.call_count.lock().unwrap() += 1;
            Ok(self.known_tags.get(tag).map(|d| (d.clone(), manifest_for(d))))
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &Identifier,
            _op: super::super::IndexOperation,
        ) -> Result<Option<Digest>> {
            let tag = identifier.tag_or_latest();
            *self.call_count.lock().unwrap() += 1;
            Ok(self.known_tags.get(tag).cloned())
        }
        async fn fetch_blob(&self, _blob_ref: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            *self.call_count.lock().unwrap() += 1;
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &Identifier,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            let tag = identifier.tag_or_latest();
            *self.call_count.lock().unwrap() += 1;
            Ok(self
                .known_tags
                .get(tag)
                .map(|d| (bytes_for(d), d.clone(), manifest_for(d))))
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

    /// Seed the cache with the full dispatch chain (root tag pointer +
    /// dispatch object) so subsequent cache-only reads succeed. Equivalent to
    /// what a successful `ChainedIndex` walk would leave behind
    /// (`adr_index_indirection.md` A3 — `persist_dispatch` + `commit_root_tag`).
    async fn seed_full(cache: &LocalIndex, identifier: &Identifier, _d: Digest, source: &Index) {
        let (digest, _manifest) = cache
            .persist_dispatch(source, identifier)
            .await
            .unwrap()
            .expect("source must know the seeded tag");
        if identifier.tag().is_some() {
            cache.commit_root_tag(identifier, &digest).await.unwrap();
        }
    }

    // ── test 22 ───────────────────────────────────────────────────────────

    /// Design record §22: in Default mode, a cache hit returns without touching
    /// sources. Source call count must remain zero.
    #[tokio::test(flavor = "multi_thread")]
    async fn default_mode_cache_hit_returns_without_touching_sources() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        // Seed the cache via a temporary source.
        let (_, seed_idx) = make_source(TAG, digest_a());
        seed_full(&cache, &tagged_id(), digest_a(), &seed_idx).await;

        // Now create a spy source and verify it is never called on cache hit.
        let (spy, spy_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![spy_idx], ChainMode::Default);
        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "cache hit must return Some");
        assert_eq!(spy.calls(), 0, "source must not be queried on cache hit (Default mode)");
    }

    // ── test 23 ───────────────────────────────────────────────────────────

    /// Design record §23: in Default mode, a cache miss walks the source,
    /// persists the chain on disk. After the call, the blob data file exists.
    #[tokio::test(flavor = "multi_thread")]
    async fn default_mode_cache_miss_walks_source_and_persists_chain_on_disk() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blob_store = index_store(&cache_dir);

        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);
        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "cache-miss source fetch must return Some");

        // Property: the dispatch object must exist on disk after a successful fetch.
        let expected_blob = blob_store.dispatch_object_path(REGISTRY, REPO, &digest_a());
        assert!(
            expected_blob.exists(),
            "Default mode: dispatch object must be on disk after fetch_manifest; missing: {}",
            expected_blob.display()
        );
    }

    // ── test 24 ───────────────────────────────────────────────────────────

    /// Design record §24: in Remote mode, tag lookups bypass the cache and go
    /// to the source, but blobs ARE still persisted on disk after the call.
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_mode_bypasses_cache_for_tag_lookup_but_still_persists_blobs() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blob_store = index_store(&cache_dir);

        // Seed the cache with digest_b so a Default-mode lookup would hit b.
        let (_, seed) = make_source(TAG, digest_b());
        seed_full(&cache, &tagged_id(), digest_b(), &seed).await;

        // Source has digest_a — in Remote mode the source is consulted.
        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);
        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        // Remote mode must have gone to the source (digest_a != digest_b).
        assert!(result.is_some());
        assert!(spy.calls() > 0, "Remote mode must consult source for tag lookup");

        // The dispatch object must be persisted even under Remote mode.
        let expected_blob = blob_store.dispatch_object_path(REGISTRY, REPO, &digest_a());
        assert!(
            expected_blob.exists(),
            "Remote mode: dispatch object must be persisted after fetch_manifest"
        );
    }

    // ── test 25 ───────────────────────────────────────────────────────────

    /// Design record §25: in Remote mode, digest-addressed lookups use the
    /// cache (immutable content — no reason to bypass). The source must NOT
    /// be consulted for digest-addressed fetches.
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_mode_digest_addressed_lookup_uses_cache() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // Pre-write the dispatch object directly into the dispatch-object CAS
        // so the cache has it. The bytes must hash to `digest_a` (A3 verify on read).
        index_store(&cache_dir)
            .write_dispatch_object(REGISTRY, REPO, &digest_a(), manifest_a_bytes())
            .await
            .unwrap();

        let (spy, src_idx) = make_source(TAG, digest_a());
        let id_with_digest = digest_only_id(); // digest-addressed, no tag
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);
        let result = chained
            .fetch_manifest(&id_with_digest, super::IndexOperation::Resolve)
            .await
            .unwrap();
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

    // ── update family: lock-scoped resolution ───────────────────────────

    /// `from_chained_lock_scoped` (the `ocx update` index): a tag-addressed
    /// `Resolve` walks the source and persists the manifest blob, but never
    /// commits a tag pointer into the local index — the caller's lock is the
    /// canonical record (`adr_toolchain_update_family.md`).
    #[tokio::test(flavor = "multi_thread")]
    async fn lock_scoped_resolve_persists_blobs_but_never_commits_tag_pointer() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blob_store = index_store(&cache_dir);

        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained_lock_scoped(cache, vec![src_idx], ChainMode::Remote);
        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "lock-scoped resolve must reach the source");
        assert!(spy.calls() > 0, "lock-scoped Remote resolve must consult the source");

        // Dispatch object still persisted — content-addressed, immutable,
        // pre-warms materialization.
        let expected_blob = blob_store.dispatch_object_path(REGISTRY, REPO, &digest_a());
        assert!(
            expected_blob.exists(),
            "lock-scoped resolve must still persist the dispatch object"
        );

        // Tag pointer must NOT have been committed: an offline probe over the
        // same directory cannot resolve the tag (the blob alone is not enough
        // for a tag-addressed query — it needs the tag pointer).
        let probe = Index::from_chained(make_local_index(&cache_dir), Vec::new(), ChainMode::Offline);
        let tag_pointer = probe
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            tag_pointer.is_none(),
            "lock-scoped resolve must never commit a tag pointer; got a manifest for the tag"
        );
    }

    /// The suppress-tag-commit flag must survive `box_clone`: resolving
    /// through a clone of a lock-scoped index still commits no tag pointer.
    #[tokio::test(flavor = "multi_thread")]
    async fn lock_scoped_suppression_survives_box_clone() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained_lock_scoped(cache, vec![src_idx], ChainMode::Remote);
        let cloned = chained.clone();
        let result = cloned
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "cloned lock-scoped resolve must reach the source");

        let probe = Index::from_chained(make_local_index(&cache_dir), Vec::new(), ChainMode::Offline);
        let tag_pointer = probe
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            tag_pointer.is_none(),
            "suppress_tag_commit must be carried through box_clone; got a manifest for the tag"
        );
    }

    /// Concurrent same-tag resolves through a lock-scoped index: every
    /// waiter — singleflight leader and followers alike — must land on the
    /// identical head digest (the followers receive it via the shared
    /// `Option<Digest>` singleflight value), and the tag pointer must stay
    /// absent afterwards. The source call count is deliberately not asserted:
    /// without a committed tag pointer a straggler that misses the
    /// singleflight in-flight window legitimately re-walks the source.
    #[tokio::test(flavor = "multi_thread")]
    async fn lock_scoped_concurrent_resolves_share_head_digest_without_tag_commit() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Arc::new(Index::from_chained_lock_scoped(cache, vec![src_idx], ChainMode::Remote));

        let mut tasks: tokio::task::JoinSet<Result<Option<Digest>>> = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let ch = chained.clone();
            let id = tagged_id();
            tasks.spawn(async move { ch.fetch_manifest_digest(&id, super::IndexOperation::Resolve).await });
        }
        let mut digests = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let resolved = joined.expect("task panicked").expect("fetch_manifest_digest failed");
            digests.push(resolved.expect("every concurrent waiter must resolve the tag"));
        }
        assert_eq!(digests.len(), 4);
        assert!(
            digests.iter().all(|digest| *digest == digest_a()),
            "all concurrent waiters must share the leader's head digest; got {digests:?}"
        );

        // The concurrent resolves must not have committed a tag pointer.
        let probe = Index::from_chained(make_local_index(&cache_dir), Vec::new(), ChainMode::Offline);
        let tag_pointer = probe
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            tag_pointer.is_none(),
            "concurrent lock-scoped resolves must never commit a tag pointer"
        );
    }

    // ── test 26 ───────────────────────────────────────────────────────────

    /// Design record §26 (revised for #155): in Offline mode an unpinned
    /// (tag-only) cache miss is a **policy block** — it errors with
    /// `PolicyResolutionBlocked{policy:"offline"}` and never consults a
    /// source. This unifies offline with frozen: under either policy the
    /// resolver was forbidden from checking, so "policy blocked" is the honest
    /// answer (previously this surfaced as `Ok(None)` → not-found exit 79).
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_mode_tag_only_miss_blocks_without_consulting_sources() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Offline);
        let err = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect_err("Offline mode: unpinned-tag miss must be a policy block");
        assert_policy_blocked(&err, "offline");
        assert_eq!(spy.calls(), 0, "Offline mode must never consult sources");
    }

    // ── test 27 ───────────────────────────────────────────────────────────

    /// Design record §27: in Offline mode, a cache hit returns from disk
    /// without consulting sources.
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_mode_cache_hit_returns_from_disk() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        // Seed the cache via an online source.
        let (_, seed) = make_source(TAG, digest_a());
        seed_full(&cache, &tagged_id(), digest_a(), &seed).await;

        // Now query Offline mode — must hit from disk, no source calls.
        let (spy, src_idx) = make_source(TAG, digest_b()); // different digest
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Offline);
        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "Offline mode: cache hit must return Some");
        assert_eq!(spy.calls(), 0, "Offline mode must never consult sources on hit");
    }

    // ── #155 frozen / no-resolve-policy routing ─────────────────────────────

    /// Assert `err` is the chained-index `PolicyResolutionBlocked` variant with
    /// the expected lowercase policy label.
    fn assert_policy_blocked(err: &crate::Error, expected_policy: &str) {
        match err {
            crate::Error::OciIndex(super::super::error::Error::PolicyResolutionBlocked { policy, identifier }) => {
                assert_eq!(
                    *policy, expected_policy,
                    "policy label mismatch (identifier={identifier})"
                );
            }
            other => panic!("expected PolicyResolutionBlocked{{policy:{expected_policy:?}}}, got: {other:?}"),
        }
    }

    /// Frozen + unpinned (tag-only) miss → `PolicyResolutionBlocked{policy:"frozen"}`,
    /// source never contacted (the spy records zero calls).
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_mode_tag_only_miss_blocks_without_consulting_sources() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Frozen);
        let err = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect_err("Frozen mode: unpinned-tag miss must be a policy block");
        assert_policy_blocked(&err, "frozen");
        assert_eq!(
            spy.calls(),
            0,
            "Frozen mode must not consult sources for an unpinned-tag miss"
        );
    }

    /// Frozen + tag-only HIT (already in the local index) → resolves from
    /// cache, no source contact. Frozen never costs anything on the hit path.
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_mode_tag_only_hit_returns_from_local_index() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        // Seed the cache via a temporary source so the tag is locally known.
        let (_, seed) = make_source(TAG, digest_a());
        seed_full(&cache, &tagged_id(), digest_a(), &seed).await;

        let (spy, src_idx) = make_source(TAG, digest_b()); // different digest
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Frozen);
        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect("Frozen mode: tag-only hit must resolve from cache");
        assert!(result.is_some(), "Frozen mode: tag-only hit must return Some");
        assert_eq!(spy.calls(), 0, "Frozen mode must not consult sources on a local hit");
    }

    /// Tag pointer present in the local index but manifest blob missing (the
    /// "tag-present, blob-missing" state that arises after `ocx index update`
    /// without a subsequent pull) must NOT be treated as a policy block under
    /// Frozen or Offline modes.  The policy gate probes `fetch_manifest_digest`
    /// which queries only the tag store — returning `Some(digest)` means the
    /// tag IS locally resolvable — so the gate lets the call through.  The
    /// blob-absent path is a content-fetch concern handled downstream, not a
    /// resolution policy violation.
    ///
    /// The two modes differ in what happens *after* the gate passes:
    ///   - `Offline`: early-return at the `ChainMode::Offline` guard; no source
    ///     contact at all (gate + early-return together mean zero spy calls).
    ///   - `Frozen`: the gate passes, then the chain walks normally to fetch the
    ///     missing content — spy IS called (content fetch is allowed for a
    ///     locally-resolved tag; only resolution of *unknown* tags is blocked).
    ///
    /// Regression: the probe formerly used `.ok().flatten()` which would silently
    /// mask a real I/O error from a corrupt index as "tag absent", raising a
    /// spurious `PolicyResolutionBlocked`.
    #[tokio::test(flavor = "multi_thread")]
    async fn tag_present_blob_missing_does_not_trigger_policy_block() {
        // ── Offline: gate passes + early-return → zero spy calls ────────────
        {
            let cache_dir = TempDir::new().unwrap();
            let cache = make_local_index(&cache_dir);

            // Seed only the root's tag pointer — skip `persist_dispatch` so the
            // dispatch object is never written (the AbsentLeaf shape).
            // `commit_root_tag` is `pub(super)` and accessible here because
            // `chain_refs_tests` lives in the same `index` parent module.
            cache.commit_root_tag(&tagged_id(), &digest_a()).await.unwrap();

            let (spy, src_idx) = make_source(TAG, digest_a());
            let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Offline);

            let result = chained
                .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
                .await;

            // (a) Must NOT be PolicyResolutionBlocked.
            assert!(
                !matches!(
                    result,
                    Err(crate::Error::OciIndex(
                        super::super::error::Error::PolicyResolutionBlocked { .. }
                    ))
                ),
                "Offline: tag-present/blob-missing must not raise PolicyResolutionBlocked; \
                 got {result:?}"
            );
            // (b) Offline early-return fires after the gate: zero source calls.
            assert_eq!(
                spy.calls(),
                0,
                "Offline: spy must receive zero calls (early-return before source walk); \
                 got {} call(s)",
                spy.calls()
            );
        }

        // ── Frozen: gate passes, chain walks to fetch missing content ────────
        {
            let cache_dir = TempDir::new().unwrap();
            let cache = make_local_index(&cache_dir);

            // Same tag-only seed; Frozen permits content fetches for known tags.
            cache.commit_root_tag(&tagged_id(), &digest_a()).await.unwrap();

            let (spy, src_idx) = make_source(TAG, digest_a());
            let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Frozen);

            let result = chained
                .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
                .await;

            // (a) Must NOT be PolicyResolutionBlocked.
            assert!(
                !matches!(
                    result,
                    Err(crate::Error::OciIndex(
                        super::super::error::Error::PolicyResolutionBlocked { .. }
                    ))
                ),
                "Frozen: tag-present/blob-missing must not raise PolicyResolutionBlocked; \
                 got {result:?}"
            );
            // (b) Frozen walks the chain for content: spy IS called.
            assert!(
                spy.calls() > 0,
                "Frozen: spy must be called for content fetch after gate passes; \
                 got {} call(s)",
                spy.calls()
            );
        }
    }

    /// Frozen + digest-addressed miss → walks the source (content fetch is
    /// allowed for an already-known version); Offline + the same input → no
    /// source contact. The digest axis is exactly what makes frozen distinct
    /// from offline.
    #[tokio::test(flavor = "multi_thread")]
    async fn digest_addressed_miss_walks_under_frozen_but_not_offline() {
        // Frozen: source IS consulted (not a policy block).
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Frozen);
        let result = chained
            .fetch_manifest(&digest_only_id(), super::super::IndexOperation::Resolve)
            .await;
        assert!(
            result.is_ok(),
            "Frozen mode must not policy-block a digest-addressed resolve; got {result:?}"
        );
        assert!(
            spy.calls() > 0,
            "Frozen mode must walk the source for a digest-addressed miss"
        );

        // Offline: source is NOT consulted; the miss stays a clean None.
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Offline);
        let result = chained
            .fetch_manifest(&digest_only_id(), super::super::IndexOperation::Resolve)
            .await
            .expect("Offline digest-addressed miss must be a clean None, not an error");
        assert!(result.is_none(), "Offline digest-addressed miss must return None");
        assert_eq!(
            spy.calls(),
            0,
            "Offline mode must never consult sources for a digest miss"
        );
    }

    // ── test 28 ───────────────────────────────────────────────────────────

    /// Design record §28: singleflight deduplicates concurrent identical cache
    /// misses — only 1 source fetch is recorded even when 4 tasks race.
    #[tokio::test(flavor = "multi_thread")]
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
            tasks.spawn(async move { ch.fetch_manifest(&id, super::IndexOperation::Resolve).await });
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
    #[tokio::test(flavor = "multi_thread")]
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
            async fn fetch_manifest(
                &self,
                _: &Identifier,
                _op: super::super::IndexOperation,
            ) -> Result<Option<(Digest, Manifest)>> {
                Err(super::super::error::Error::RemoteManifestNotFound("test error".to_string()).into())
            }
            async fn fetch_manifest_digest(
                &self,
                _: &Identifier,
                _op: super::super::IndexOperation,
            ) -> Result<Option<Digest>> {
                Err(super::super::error::Error::RemoteManifestNotFound("test error".to_string()).into())
            }
            async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
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
            tasks.spawn(async move { ch.fetch_manifest(&id, super::IndexOperation::Resolve).await });
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
    #[tokio::test(flavor = "multi_thread")]
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

    // ── routing invariant: Op::Query never walks the source chain ────────

    /// `IndexOperation::Query` is the contract for pure-read callers
    /// (`index list`, `index catalog`, `package info`). The invariant this
    /// routing split exists to protect is *no query-path writes to the local
    /// index* in any mode — never a chain walk, never a tag-pointer commit.
    ///
    /// In `Default` and `Offline` modes a tag-addressed cache miss returns
    /// `None` without touching the source. In `Remote` mode a `Query` reads
    /// *through* to the source — a live `--remote` lookup, the same routing
    /// `list_tags` uses — and returns `Some`, but still must not persist: the
    /// tag store stays untouched. A spy source records every invocation and
    /// the tag-store directory must never be created. See
    /// `adr_index_routing_semantics.md`.
    #[tokio::test(flavor = "multi_thread")]
    async fn op_query_never_writes_local_index_in_any_mode() {
        // Default / Offline / Frozen: tag-addressed cache miss → None, source untouched.
        for mode in [ChainMode::Default, ChainMode::Offline, ChainMode::Frozen] {
            let cache_dir = TempDir::new().unwrap();
            let cache = make_local_index(&cache_dir);
            let (spy, src_idx) = make_source(TAG, digest_a());
            let chained = Index::from_chained(cache, vec![src_idx], mode);

            let result = chained
                .fetch_manifest(&tagged_id(), super::IndexOperation::Query)
                .await
                .unwrap();
            assert!(
                result.is_none(),
                "Op::Query cache miss must return None in mode {mode:?}, got Some"
            );
            assert_eq!(
                spy.calls(),
                0,
                "Op::Query must not call source in mode {mode:?}; got {} call(s)",
                spy.calls()
            );
            assert!(
                !index_store(&cache_dir).root_document_path(REGISTRY, REPO).exists(),
                "Op::Query in mode {mode:?} must not create the root document"
            );
        }

        // Remote: a Query reads through to the source (Some) but never persists.
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);

        let manifest = chained
            .fetch_manifest(&tagged_id(), super::IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            manifest.is_some(),
            "Op::Query in Remote mode must read through to the source"
        );
        let digest = chained
            .fetch_manifest_digest(&tagged_id(), super::IndexOperation::Query)
            .await
            .unwrap();
        assert_eq!(
            digest,
            Some(digest_a()),
            "Op::Query digest in Remote mode must read through to the source"
        );
        assert!(spy.calls() > 0, "Op::Query in Remote mode must consult the source");
        assert!(
            !index_store(&cache_dir).root_document_path(REGISTRY, REPO).exists(),
            "Op::Query in Remote mode must not create the root document (no write-through)"
        );
    }

    // ── pinned-id pull: tag+digest identifier must skip tag-pointer commit ──

    /// A pinned-id pull (`cmake:1.0@sha256:...`) carries both tag and
    /// digest. Persisting the dispatch object is fine — content-addressed
    /// objects are immutable — but growing the root would silently shadow
    /// `ocx.lock` (which is the canonical record). The post-pin contract is
    /// to skip the root growth and let the lock own the tag→digest mapping.
    /// Asserted by checking that the root document is never created.
    #[tokio::test(flavor = "multi_thread")]
    async fn pinned_id_pull_skips_tag_pointer_commit() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);

        // tag+digest identifier — what `command/pull.rs` produces from a
        // `PinnedIdentifier` via `clone_with_digest` after `lock` resolved
        // the tag.
        let pinned_id = tagged_id().clone_with_digest(digest_a());
        assert!(pinned_id.tag().is_some() && pinned_id.digest().is_some());

        let result = chained
            .fetch_manifest(&pinned_id, super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "pinned-id resolve must succeed and return manifest");

        // Dispatch objects are persisted (content-addressed), but the root
        // document must not be written.
        let root_path = index_store(&cache_dir).root_document_path(REGISTRY, REPO);
        assert!(
            !root_path.exists(),
            "tag+digest pull must not create the root document at {}",
            root_path.display()
        );
    }

    // ── regression: Remote-mode list_tags must not mutate the local index ──

    /// A pure `--remote` query must never write to the local index. A
    /// Remote-mode `list_tags` call must not create the repository's root
    /// document.
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_mode_list_tags_does_not_mutate_local_index() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Remote);

        let root_path = index_store(&cache_dir).root_document_path(REGISTRY, REPO);
        assert!(!root_path.exists(), "preconditions: root document must not exist");

        let result = chained.list_tags(&tagged_id()).await.unwrap();
        assert!(result.is_some(), "Remote-mode list_tags must return source tags");

        assert!(
            !root_path.exists(),
            "Remote-mode list_tags must not create the local root document at {}",
            root_path.display()
        );
    }

    // ── test 31 ───────────────────────────────────────────────────────────

    /// Design record §31: list_repositories routes by ChainMode. Default and
    /// Offline read the persisted cache without consulting sources; Remote
    /// bypasses the cache and returns the source's repo list.
    #[tokio::test(flavor = "multi_thread")]
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
    #[tokio::test(flavor = "multi_thread")]
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
            async fn fetch_manifest(
                &self,
                _: &Identifier,
                _op: super::super::IndexOperation,
            ) -> Result<Option<(Digest, Manifest)>> {
                Ok(None)
            }
            async fn fetch_manifest_digest(
                &self,
                _: &Identifier,
                _op: super::super::IndexOperation,
            ) -> Result<Option<Digest>> {
                Ok(None)
            }
            async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
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
    #[tokio::test(flavor = "multi_thread")]
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
            async fn fetch_manifest(
                &self,
                _: &Identifier,
                _op: super::super::IndexOperation,
            ) -> Result<Option<(Digest, Manifest)>> {
                Ok(None)
            }
            async fn fetch_manifest_digest(
                &self,
                _: &Identifier,
                _op: super::super::IndexOperation,
            ) -> Result<Option<Digest>> {
                Err(super::super::error::Error::RemoteManifestNotFound("boom".to_string()).into())
            }
            async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
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
    /// fetch_manifest returning Some((digest, _)) for a dispatch-shaped
    /// fixture, the dispatch object must exist on disk (digest is
    /// guaranteed on disk).
    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_manifest_post_persist_is_guaranteed_on_disk() {
        // Test with Default mode (the main case; Remote is covered in test 24).
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blob_store = index_store(&cache_dir);

        let (_, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);

        if let Some((digest, _)) = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap()
        {
            let blob_path = blob_store.dispatch_object_path(REGISTRY, REPO, &digest);
            assert!(
                blob_path.exists(),
                "property violated: fetch_manifest returned digest {:?} but the dispatch object is not on disk at {}",
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
    #[tokio::test(flavor = "multi_thread")]
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
            tasks.spawn(async move { ch.fetch_manifest(&id, super::IndexOperation::Resolve).await });
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

    // ── fetch_blob — config-blob routing through ChainedIndex ─────

    /// Source stub that serves a single fixed `(digest → bytes)` mapping
    /// from `fetch_blob`. Records call count so tests can assert
    /// cache-first behaviour.
    #[derive(Clone)]
    struct BlobOnlySource {
        digest: Digest,
        bytes: Vec<u8>,
        call_count: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl index_impl::IndexImpl for BlobOnlySource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, blob_ref: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            *self.call_count.lock().unwrap() += 1;
            if blob_ref.digest() == self.digest {
                Ok(Some(self.bytes.clone()))
            } else {
                Ok(None)
            }
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn pinned_for_test() -> crate::oci::PinnedIdentifier {
        crate::oci::PinnedIdentifier::try_from(digest_only_id()).unwrap()
    }

    /// Cache hit: blob already in the machine-global blob store (`fs.blobs`)
    /// — returns the bytes without consulting any source. Proves the
    /// offline-rehydration path works when the local CAS already holds the
    /// blob (the index-home flat blob CAS has been retired, `adr_index_indirection.md` B2).
    #[tokio::test(flavor = "multi_thread")]
    async fn chained_fetch_blob_cache_hit_no_source_call() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static). This test seeds the
        // cache via `BlobStore::write_blob` directly, which increments it; holding
        // this lock prevents our call from inflating the coalescing-test delta.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blobs = BlobStore::new(cache_dir.path().join("blobs"));
        let pinned = pinned_for_test();
        let bytes = b"cached config blob".to_vec();
        // A staged blob must hash to its digest (`BlobStore::write_blob` trusts
        // the caller to have verified this upstream).
        let blob_digest = Algorithm::Sha256.hash(&bytes);
        let blob_ref = pinned.clone_with_digest(blob_digest.clone());
        blobs
            .write_blob(blob_ref.registry(), &blob_digest, &bytes)
            .await
            .expect("write_blob must succeed");

        let spy = BlobOnlySource {
            digest: blob_digest.clone(),
            bytes: b"should-not-be-served".to_vec(),
            call_count: Arc::new(Mutex::new(0)),
        };
        let spy_calls = spy.call_count.clone();
        let src_idx = super::super::Index::from_impl(spy);
        let chained = Index::from_chained_with_content_store(cache, vec![src_idx], ChainMode::Default, blobs);

        let got = chained
            .fetch_blob(&blob_ref)
            .await
            .expect("fetch_blob must succeed")
            .expect("cache hit must return Some(bytes)");
        assert_eq!(got, bytes, "cache hit must return the on-disk bytes");
        assert_eq!(*spy_calls.lock().unwrap(), 0, "cache hit must not consult sources");
    }

    /// Offline mode + local miss → `Ok(None)`. Caller maps `None` to
    /// `Error::OfflineMode` at the policy boundary (see `pull::setup_owned`).
    #[tokio::test(flavor = "multi_thread")]
    async fn chained_fetch_blob_offline_miss_returns_none() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let pinned = pinned_for_test();
        let blob_digest = digest_b();

        // A source is configured but must never be consulted in Offline mode.
        let spy = BlobOnlySource {
            digest: blob_digest.clone(),
            bytes: b"forbidden in offline mode".to_vec(),
            call_count: Arc::new(Mutex::new(0)),
        };
        let spy_calls = spy.call_count.clone();
        let src_idx = super::super::Index::from_impl(spy);
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Offline);

        let blob_ref = pinned.clone_with_digest(blob_digest.clone());
        let got = chained.fetch_blob(&blob_ref).await.expect("fetch_blob must succeed");
        assert!(got.is_none(), "Offline-mode local miss must return None");
        assert_eq!(*spy_calls.lock().unwrap(), 0, "Offline mode must not consult sources");
    }

    /// Default mode + local miss → walks the source chain, returns the
    /// bytes, AND persists them into the machine-global blob store (`fs.blobs`)
    /// so a subsequent offline read hits without a network round-trip. This is
    /// the regression guarantee for `ocx clean; rm -rf packages installs;
    /// --offline install`.
    #[tokio::test(flavor = "multi_thread")]
    async fn chained_fetch_blob_walks_chain_and_persists() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blobs = BlobStore::new(cache_dir.path().join("blobs"));
        let pinned = pinned_for_test();
        let bytes = b"freshly fetched config blob".to_vec();
        // The write-through verifies sha256(bytes) == digest before persisting
        // (CWE-345 — never trust unverified remote bytes).
        let blob_digest = Algorithm::Sha256.hash(&bytes);

        let spy = BlobOnlySource {
            digest: blob_digest.clone(),
            bytes: bytes.clone(),
            call_count: Arc::new(Mutex::new(0)),
        };
        let spy_calls = spy.call_count.clone();
        let src_idx = super::super::Index::from_impl(spy);
        let blob_ref = pinned.clone_with_digest(blob_digest.clone());

        // Pre-condition: not on disk yet.
        let on_disk = blobs.data(blob_ref.registry(), &blob_digest);
        assert!(!on_disk.exists(), "blob must be absent before the chain walk");

        let chained = Index::from_chained_with_content_store(cache, vec![src_idx], ChainMode::Default, blobs);
        let got = chained
            .fetch_blob(&blob_ref)
            .await
            .expect("fetch_blob must succeed")
            .expect("source hit must return Some(bytes)");
        assert_eq!(got, bytes);
        assert_eq!(*spy_calls.lock().unwrap(), 1, "source must be called exactly once");

        // Post-condition: blob persisted into the machine-global blob store
        // for offline rehydration.
        assert!(
            on_disk.exists(),
            "write-through must persist the blob at {}",
            on_disk.display()
        );
        let staged = std::fs::read(&on_disk).unwrap();
        assert_eq!(staged, bytes, "staged bytes must match fetched bytes");
    }

    /// Corrupt-online heal, end-to-end: a tampered blob-store entry (bytes
    /// that do NOT hash to the digest naming them — written directly at the
    /// CAS `data` path, bypassing `write_blob`'s own verify-free contract) is
    /// atomically replaced by a subsequent source re-fetch (`replace_blob`),
    /// not left immortal by `write_blob`'s check-first fast path (which
    /// short-circuits on any existing non-empty target without re-hashing).
    #[tokio::test(flavor = "multi_thread")]
    async fn chained_fetch_blob_corrupt_online_heals_via_source_refetch() {
        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static) — this test's
        // write-through calls `BlobStore::write_blob` directly.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blobs = BlobStore::new(cache_dir.path().join("blobs"));
        let pinned = pinned_for_test();
        let good_bytes = b"the genuine config blob".to_vec();
        let blob_digest = Algorithm::Sha256.hash(&good_bytes);
        let blob_ref = pinned.clone_with_digest(blob_digest.clone());

        // Tamper: write WRONG bytes directly at the CAS data path, bypassing
        // `write_blob` (which would refuse to overwrite this non-empty file on
        // a later legitimate call — exactly the immortality this test guards
        // against).
        let on_disk = blobs.data(blob_ref.registry(), &blob_digest);
        std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
        std::fs::write(&on_disk, b"tampered bytes that do not hash to blob_digest").unwrap();

        let spy = BlobOnlySource {
            digest: blob_digest.clone(),
            bytes: good_bytes.clone(),
            call_count: Arc::new(Mutex::new(0)),
        };
        let spy_calls = spy.call_count.clone();
        let src_idx = super::super::Index::from_impl(spy);
        let chained = Index::from_chained_with_content_store(cache, vec![src_idx], ChainMode::Default, blobs);

        let got = chained
            .fetch_blob(&blob_ref)
            .await
            .expect("fetch_blob must succeed")
            .expect("corrupt-online must fall through to the source and return Some(bytes)");
        assert_eq!(
            got, good_bytes,
            "the returned bytes must be the genuine, source-fetched content"
        );
        assert_eq!(*spy_calls.lock().unwrap(), 1, "source must be consulted exactly once");

        // The on-disk copy must now genuinely hash to the digest — proving
        // the corrupt entry was actually replaced, not left in place under a
        // no-op `write_blob` fast path.
        let healed = std::fs::read(&on_disk).unwrap();
        assert_eq!(
            healed, good_bytes,
            "the on-disk blob must be healed to the genuine bytes"
        );
        assert_eq!(
            Algorithm::Sha256.hash(&healed),
            blob_digest,
            "the healed on-disk bytes must hash to the digest naming them"
        );
    }

    /// Corrupt-offline: a tampered blob-store entry with no source to heal
    /// from is a hard `DigestMismatch` error, never a silent miss or a
    /// silently-served tampered read.
    #[tokio::test(flavor = "multi_thread")]
    async fn chained_fetch_blob_corrupt_offline_hard_errors() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blobs = BlobStore::new(cache_dir.path().join("blobs"));
        let pinned = pinned_for_test();
        let claimed_digest = digest_a();
        let blob_ref = pinned.clone_with_digest(claimed_digest.clone());

        let on_disk = blobs.data(blob_ref.registry(), &claimed_digest);
        std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
        std::fs::write(&on_disk, b"tampered bytes that do not hash to claimed_digest").unwrap();

        // A source is configured but must never be consulted in Offline mode.
        let (spy, src_idx) = make_source(TAG, digest_b());
        let chained = Index::from_chained_with_content_store(cache, vec![src_idx], ChainMode::Offline, blobs);

        let err = chained
            .fetch_blob(&blob_ref)
            .await
            .expect_err("a corrupt local blob under --offline must hard-error, not return Ok");
        assert!(
            matches!(
                err,
                crate::Error::FileStructure(crate::file_structure::error::Error::DigestMismatch { .. })
            ),
            "expected DigestMismatch, got {err:?}"
        );
        assert_eq!(spy.calls(), 0, "Offline mode must never consult sources");
    }

    /// Terra-gate regression: if the atomic-replace heal write ITSELF fails
    /// (permission denied — reproduced here by chmod'ing the blob's parent
    /// directory read-only, so `tempfile::NamedTempFile::new_in` cannot create
    /// the replacement file), `fetch_blob` must still return the genuine,
    /// verified bytes to the caller — a stuck heal must not fail the fetch,
    /// only leave the on-disk copy corrupt for the next attempt to retry.
    /// Pins the fix for the review finding that a naive remove-then-write
    /// two-step (or a `write_blob` re-accept of a still-corrupt file) could
    /// silently leave the tampered blob in place forever while claiming success.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn chained_fetch_blob_corrupt_online_heal_write_failure_still_returns_verified_bytes() {
        use std::os::unix::fs::PermissionsExt;

        // Serialise against `pull_coordinator_coalesces_concurrent_same_digest_writers`
        // (WRITE_BLOB_CALL_COUNT is a process-global static) — this test's
        // (failed) write-through attempt still calls the shared `persist_bytes`
        // helper.
        let _serialize = crate::file_structure::WRITE_BLOB_TEST_LOCK.lock().await;

        /// RAII guard that restores directory permissions on drop so a test
        /// failure doesn't leave a read-only dir behind and break `TempDir`
        /// cleanup (mirrors `project::lock::tests::save_preserves_original_on_write_failure`).
        struct RestorePerms {
            dir: std::path::PathBuf,
            original: std::fs::Permissions,
        }
        impl Drop for RestorePerms {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(&self.dir, self.original.clone());
            }
        }

        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let blobs = BlobStore::new(cache_dir.path().join("blobs"));
        let pinned = pinned_for_test();
        let good_bytes = b"the genuine config blob, heal write will fail".to_vec();
        let blob_digest = Algorithm::Sha256.hash(&good_bytes);
        let blob_ref = pinned.clone_with_digest(blob_digest.clone());

        // Tamper: wrong bytes directly at the CAS data path.
        let on_disk = blobs.data(blob_ref.registry(), &blob_digest);
        let blob_dir = on_disk.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&blob_dir).unwrap();
        let corrupt_bytes = b"tampered bytes that do not hash to blob_digest".to_vec();
        std::fs::write(&on_disk, &corrupt_bytes).unwrap();

        // Make the blob's own directory read-only (no write bit) so
        // `tempfile::NamedTempFile::new_in` cannot create the replacement file
        // there — the replace attempt fails with a permission error.
        let original_perms = std::fs::metadata(&blob_dir).unwrap().permissions();
        let _restore = RestorePerms {
            dir: blob_dir.clone(),
            original: original_perms.clone(),
        };
        std::fs::set_permissions(&blob_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let spy = BlobOnlySource {
            digest: blob_digest.clone(),
            bytes: good_bytes.clone(),
            call_count: Arc::new(Mutex::new(0)),
        };
        let spy_calls = spy.call_count.clone();
        let src_idx = super::super::Index::from_impl(spy);
        let chained = Index::from_chained_with_content_store(cache, vec![src_idx], ChainMode::Default, blobs);

        let got = chained
            .fetch_blob(&blob_ref)
            .await
            .expect("a failed heal write must not fail the fetch itself")
            .expect("corrupt-online must still fall through to the source and return Some(bytes)");
        assert_eq!(
            got, good_bytes,
            "the returned bytes must be the genuine, source-fetched content even though the heal write failed"
        );
        assert_eq!(*spy_calls.lock().unwrap(), 1, "source must be consulted exactly once");

        // Restore write access before reading back, so the assertion itself
        // doesn't depend on read-only semantics.
        std::fs::set_permissions(&blob_dir, original_perms).unwrap();
        let still_on_disk = std::fs::read(&on_disk).unwrap();
        assert_eq!(
            still_on_disk, corrupt_bytes,
            "the on-disk copy must remain the corrupt bytes — the heal write failed and must not be masked as success"
        );
    }

    // ── corrupt-known recovery must never re-grow an already-known root ────

    /// A fake PUBLISHED-kind source (`is_authoritative_for` claims `REGISTRY`,
    /// `source_kind() == Published`) serving a fixed dispatch object for
    /// `TAG`. Records `fetch_root_document` calls so a test can assert
    /// corrupt-object recovery never re-fetches/re-copies an already-known
    /// root.
    #[derive(Clone)]
    struct PublishedSource {
        fetch_root_document_calls: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl index_impl::IndexImpl for PublishedSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![TAG.to_string()]))
        }
        async fn fetch_manifest(
            &self,
            identifier: &Identifier,
            _op: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(&self, identifier: &Identifier, _op: IndexOperation) -> Result<Option<Digest>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _identifier: &Identifier,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            Ok(Some((
                manifest_a_bytes().to_vec(),
                digest_a(),
                manifest_for(&digest_a()),
            )))
        }
        async fn fetch_root_document(
            &self,
            _identifier: &Identifier,
        ) -> Result<Option<(Vec<u8>, super::super::IndexRoot)>> {
            *self.fetch_root_document_calls.lock().unwrap() += 1;
            Ok(None)
        }
        fn is_authoritative_for(&self, identifier: &Identifier) -> bool {
            identifier.registry() == REGISTRY
        }
        fn source_kind(&self) -> super::super::local_index::SourceKind {
            super::super::local_index::SourceKind::Published
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Regression for a Block finding: a corrupt-but-known dispatch object
    /// (the root/tag is already resolved locally, only the `o/` object is
    /// tampered) must self-heal ONLY the object. It must never re-fetch or
    /// re-copy an already-known published root — that would violate the
    /// binding "Resolve never overwrites an existing published root" ruling
    /// and F1's never-auto-refreshed invariant.
    #[tokio::test(flavor = "multi_thread")]
    async fn corrupt_known_published_root_recovers_object_without_regrowing_root() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let store = index_store(&cache_dir);

        // Seed a KNOWN published root + its dispatch object, exactly as
        // `persist_published_root`/`persist_dispatch` would leave behind.
        let root_bytes = format!(
            r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"{TAG}":{{"content":"{}","observed":"2026-07-18T00:00:00Z"}}}}}}"#,
            digest_a()
        )
        .into_bytes();
        store
            .write_dispatch_object(REGISTRY, REPO, &digest_a(), manifest_a_bytes())
            .await
            .unwrap();
        store.write_root_document(REGISTRY, REPO, &root_bytes).await.unwrap();

        // Tamper the dispatch object so `resolve_dispatch` reports a
        // recoverable `DigestMismatch` — the root itself stays untouched.
        let dispatch_path = store.dispatch_object_path(REGISTRY, REPO, &digest_a());
        std::fs::write(&dispatch_path, b"tampered garbage").unwrap();

        let fetch_root_document_calls = Arc::new(Mutex::new(0));
        let source = PublishedSource {
            fetch_root_document_calls: fetch_root_document_calls.clone(),
        };
        let src_idx = super::super::Index::from_impl(source);
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect("corrupt dispatch object must self-heal, not error");
        assert!(result.is_some(), "healed Resolve must return the manifest");

        // The dispatch object was healed back to the correct verbatim bytes.
        let healed = std::fs::read(&dispatch_path).unwrap();
        assert_eq!(
            healed,
            manifest_a_bytes(),
            "the recovery must have re-persisted the correct dispatch bytes"
        );

        // The root document was NEVER rewritten...
        let root_after = std::fs::read(store.root_document_path(REGISTRY, REPO)).unwrap();
        assert_eq!(
            root_after, root_bytes,
            "corrupt-object recovery must not rewrite an already-known published root"
        );
        // ...and the source's fetch_root_document was never even called.
        assert_eq!(
            *fetch_root_document_calls.lock().unwrap(),
            0,
            "corrupt-object recovery must never re-fetch the published root"
        );
    }

    // ── flat / single-platform-tag routing (A3: no dispatch object) ────────

    /// A fake DERIVED-kind source serving a fixed flat (single-platform)
    /// `Manifest::Image` for `TAG`. Records call count so a test can assert
    /// the source is never consulted under Offline.
    #[derive(Clone)]
    struct FlatManifestSource {
        bytes: &'static [u8],
        calls: Arc<Mutex<usize>>,
    }

    impl FlatManifestSource {
        fn digest(&self) -> Digest {
            Algorithm::Sha256.hash(self.bytes)
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for FlatManifestSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![TAG.to_string()]))
        }
        async fn fetch_manifest(
            &self,
            _identifier: &Identifier,
            _op: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some((self.digest(), serde_json::from_slice(self.bytes).unwrap())))
        }
        async fn fetch_manifest_digest(&self, _identifier: &Identifier, _op: IndexOperation) -> Result<Option<Digest>> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some(self.digest()))
        }
        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _identifier: &Identifier,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some((
                self.bytes.to_vec(),
                self.digest(),
                serde_json::from_slice(self.bytes).unwrap(),
            )))
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    const FLAT_MANIFEST_JSON: &[u8] = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;

    /// Warn-tier coverage: a flat (single-platform) tag never gains a
    /// dispatch object (A3/B2) but its root still grows with `content` set
    /// to the leaf manifest digest itself (Default mode); Offline mode
    /// policy-blocks an unknown flat-manifest tag before ever consulting the
    /// source.
    #[tokio::test(flavor = "multi_thread")]
    async fn flat_manifest_tag_routing() {
        let flat_digest = Algorithm::Sha256.hash(FLAT_MANIFEST_JSON);

        // (a) + (b): Default-mode Resolve writes nothing to `o/` but grows
        // the root with `content` = the leaf digest.
        {
            let cache_dir = TempDir::new().unwrap();
            let cache = make_local_index(&cache_dir);
            let store = index_store(&cache_dir);

            let source = FlatManifestSource {
                bytes: FLAT_MANIFEST_JSON,
                calls: Arc::new(Mutex::new(0)),
            };
            let src_idx = super::super::Index::from_impl(source);
            let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);

            let (digest, manifest) = chained
                .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
                .await
                .expect("flat manifest resolve must succeed")
                .expect("source has the tag");
            assert_eq!(digest, flat_digest);
            assert!(
                matches!(manifest, Manifest::Image(_)),
                "a flat single-platform tag must resolve to Manifest::Image"
            );

            let dispatch_path = store.dispatch_object_path(REGISTRY, REPO, &flat_digest);
            assert!(
                !dispatch_path.exists(),
                "a single-platform tag must write nothing to the dispatch object CAS (A3/B2)"
            );

            let root_bytes = std::fs::read(store.root_document_path(REGISTRY, REPO)).unwrap();
            let root: serde_json::Value = serde_json::from_slice(&root_bytes).unwrap();
            assert_eq!(
                root["tags"][TAG]["content"].as_str(),
                Some(flat_digest.to_string()).as_deref(),
                "the root's tag content must be the leaf manifest digest itself"
            );
        }

        // (c) Offline mode policy-blocks before any source call.
        {
            let cache_dir = TempDir::new().unwrap();
            let cache = make_local_index(&cache_dir);

            let source = FlatManifestSource {
                bytes: FLAT_MANIFEST_JSON,
                calls: Arc::new(Mutex::new(0)),
            };
            let calls = source.calls.clone();
            let src_idx = super::super::Index::from_impl(source);
            let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Offline);

            let err = chained
                .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
                .await
                .expect_err("offline resolve of an unknown flat-manifest tag must be policy-blocked");
            assert_policy_blocked(&err, "offline");
            assert_eq!(*calls.lock().unwrap(), 0, "Offline mode must never consult sources");
        }
    }

    // ── blob-store leaf recovery (A3 step 2 / B2) ────────────────────────────
    //
    // A leaf platform manifest is never written into the local index (A3); it
    // is CONTENT cached into `$OCX_HOME/blobs` at install (B2). These tests pin
    // the regression: an `AbsentLeaf` (content absent from `o/`) is recovered
    // from the machine-global blob store BEFORE any source walk, so an
    // installed tool resolves offline with zero network — the regression that
    // left `test_offline.py` / `test_pinned_offline.py` red.

    /// A flat single-platform (leaf) image manifest and the digest its bytes
    /// hash to. A leaf is never written to the dispatch-object CAS (A3), so a
    /// tag or digest pointing at it reports `DispatchResolution::AbsentLeaf`;
    /// the bytes live only in the machine-global blob store (B2).
    fn leaf_manifest_bytes() -> (Vec<u8>, Digest) {
        let manifest = Manifest::Image(ImageManifest::default());
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let digest = Algorithm::Sha256.hash(&bytes);
        (bytes, digest)
    }

    /// A `BlobStore` rooted under the temp dir, seeded with `bytes` under
    /// `(REGISTRY, digest)` — the shape `stage_and_link_chain_blobs` leaves
    /// behind at install for a leaf platform manifest.
    async fn seeded_blob_store(dir: &TempDir, digest: &Digest, bytes: &[u8]) -> BlobStore {
        let blobs = BlobStore::new(dir.path().join("blobs"));
        blobs.write_blob(REGISTRY, digest, bytes).await.unwrap();
        blobs
    }

    /// Regression (#215-family, `test_offline.py`): a tag-addressed `AbsentLeaf`
    /// resolves offline from the blob store with zero sources. The tag pointer
    /// is locally known (root committed), the leaf is absent from `o/`, and its
    /// bytes sit in `$OCX_HOME/blobs` — exactly the post-install offline-exec
    /// state.
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_tag_absent_leaf_recovers_from_blob_store() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let (leaf_bytes, leaf_digest) = leaf_manifest_bytes();
        // Root tag → leaf content; a single-platform tag writes nothing to `o/`
        // (AbsentLeaf).
        cache.commit_root_tag(&tagged_id(), &leaf_digest).await.unwrap();
        let blobs = seeded_blob_store(&dir, &leaf_digest, &leaf_bytes).await;

        // Offline, zero sources: recovery must come from the blob store alone.
        let chained = Index::from_chained_with_content_store(cache, vec![], ChainMode::Offline, blobs);
        let (digest, manifest) = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("offline AbsentLeaf must recover the leaf manifest from the blob store");
        assert_eq!(digest, leaf_digest);
        assert!(matches!(manifest, Manifest::Image(_)), "recovered a flat leaf manifest");
    }

    /// A digest-addressed `AbsentLeaf` (a pinned pull's leaf) recovers offline
    /// from the blob store through both `fetch_manifest` and
    /// `fetch_manifest_digest` (`test_pinned_offline.py`).
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_digest_absent_leaf_recovers_from_blob_store() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let (leaf_bytes, leaf_digest) = leaf_manifest_bytes();
        let blobs = seeded_blob_store(&dir, &leaf_digest, &leaf_bytes).await;
        let id = Identifier::new_registry(REPO, REGISTRY).clone_with_digest(leaf_digest.clone());
        let chained = Index::from_chained_with_content_store(cache, vec![], ChainMode::Offline, blobs);

        let (digest, _manifest) = chained
            .fetch_manifest(&id, IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("digest-addressed offline leaf must recover from the blob store");
        assert_eq!(digest, leaf_digest);

        let confirmed = chained
            .fetch_manifest_digest(&id, IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("digest-addressed offline leaf digest must be confirmed from the blob store");
        assert_eq!(confirmed, leaf_digest);
    }

    /// The blob-store recovery is opt-in: without an attached content store
    /// (the `from_chained` seam every unit test uses), an offline `AbsentLeaf`
    /// stays a clean `None` — proving the fix changes nothing for the
    /// no-content-store construction and cannot mask a genuine offline miss.
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_absent_leaf_without_content_store_returns_none() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let (leaf_bytes, leaf_digest) = leaf_manifest_bytes();
        cache.commit_root_tag(&tagged_id(), &leaf_digest).await.unwrap();
        // Bytes exist on disk, but no content store is wired into this chain.
        let _ = seeded_blob_store(&dir, &leaf_digest, &leaf_bytes).await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Offline);
        let result = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "no content store → offline AbsentLeaf stays a clean None"
        );
    }

    /// The blob-store recovery must NOT mask the no-resolve policy block: an
    /// unindexed tag (no root → a genuine `None` miss, not `AbsentLeaf`) still
    /// exits with `PolicyResolutionBlocked` under Offline even with a blob store
    /// attached, and never contacts a source. Pins the pre-C2 policy contract
    /// against the new content-store seam (`test_frozen.py` /
    /// `test_offline.py::test_exit_code_on_offline_blocks_fetch`).
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_unindexed_tag_blocks_even_with_content_store() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        // Blob store present but the tag was never indexed — resolve_dispatch is
        // a genuine miss (None), so recovery cannot fire.
        let (leaf_bytes, leaf_digest) = leaf_manifest_bytes();
        let blobs = seeded_blob_store(&dir, &leaf_digest, &leaf_bytes).await;
        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained_with_content_store(cache, vec![src_idx], ChainMode::Offline, blobs);

        let err = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("offline unindexed tag must policy-block even with a content store attached");
        assert_policy_blocked(&err, "offline");
        assert_eq!(spy.calls(), 0, "policy block must fire before any source contact");
    }

    // ── authoritative-stop on a clean miss (no silent fallthrough) ─────────

    /// A fake source claiming authoritative ownership of `REGISTRY`'s
    /// namespace (mirrors `OcxIndex::is_authoritative_for`) but reporting a
    /// clean miss for every identifier — the case where the one configured
    /// ocx-index for a namespace genuinely has no such package.
    #[derive(Clone)]
    struct AuthoritativeMissSource;

    #[async_trait]
    impl index_impl::IndexImpl for AuthoritativeMissSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _: &Identifier, _op: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _op: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn is_authoritative_for(&self, identifier: &Identifier) -> bool {
            identifier.registry() == REGISTRY
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Regression: an index-authoritative source's clean miss for a package
    /// in its own namespace must be terminal — it must never fall through to
    /// the registry catch-all (`adr_index_indirection.md` F5a / Decision H:
    /// exactly one remote per namespace, no index→OCI-tags fallback chain).
    /// Exercises the `Default`+`Resolve` chain walk (`fetch_and_persist_chain`).
    #[tokio::test(flavor = "multi_thread")]
    async fn authoritative_clean_miss_does_not_fall_through_to_registry_resolve() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let authoritative_idx = Index::from_impl(AuthoritativeMissSource);
        let (registry_spy, registry_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![authoritative_idx, registry_idx], ChainMode::Default);

        let result = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "authoritative source's clean miss must be a terminal None"
        );
        assert_eq!(
            registry_spy.calls(),
            0,
            "the registry source must never be queried once the authoritative source reported a clean miss"
        );
    }

    /// Same authoritative-stop invariant on the `--remote` pure-query path
    /// (`query_sources_manifest`).
    #[tokio::test(flavor = "multi_thread")]
    async fn authoritative_clean_miss_does_not_fall_through_to_registry_remote_query() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let authoritative_idx = Index::from_impl(AuthoritativeMissSource);
        let (registry_spy, registry_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![authoritative_idx, registry_idx], ChainMode::Remote);

        let result = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "authoritative source's clean miss must be a terminal None under a --remote query too"
        );
        assert_eq!(
            registry_spy.calls(),
            0,
            "the registry source must never be queried once the authoritative source reported a clean miss"
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
        file_structure::IndexStore,
        oci::index::{Index, LocalConfig, LocalIndex, index_impl},
        oci::{Algorithm, Digest, Identifier, Manifest},
    };

    // ── Test helpers ──────────────────────────────────────────────────────

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";

    fn tagged_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }

    fn digest_only_id() -> Identifier {
        Identifier::new_registry(REPO, REGISTRY).clone_with_digest(digest_a())
    }

    // Two distinct single-child image INDEXES — distinct bytes so digests
    // differ, and (A3) the bytes genuinely hash to the digest the source
    // serves. Dispatch-shaped (never a bare leaf manifest) so a routing
    // test's "cache hit" fixtures are genuinely locally-cacheable under the
    // dispatch-only local index — see the identical note on the sibling copy
    // of these fixtures in `chain_refs_tests`.
    fn manifest_a_bytes() -> &'static [u8] {
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2,"platform":{"os":"linux","architecture":"amd64"}}]}"#
    }
    fn manifest_b_bytes() -> &'static [u8] {
        br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","size":3,"platform":{"os":"linux","architecture":"amd64"}}]}"#
    }
    fn digest_a() -> Digest {
        Algorithm::Sha256.hash(manifest_a_bytes())
    }
    fn digest_b() -> Digest {
        Algorithm::Sha256.hash(manifest_b_bytes())
    }
    fn bytes_for(digest: &Digest) -> Vec<u8> {
        if *digest == digest_a() {
            manifest_a_bytes().to_vec()
        } else if *digest == digest_b() {
            manifest_b_bytes().to_vec()
        } else {
            panic!("unknown test digest {digest}")
        }
    }
    fn manifest_for(digest: &Digest) -> Manifest {
        serde_json::from_slice(&bytes_for(digest)).unwrap()
    }

    fn index_store(dir: &TempDir) -> IndexStore {
        IndexStore::new(dir.path().join("index"))
    }

    /// Build a real `LocalIndex` backed by a temp directory's index home.
    ///
    /// The `TempDir` must outlive the index; callers keep it in scope.
    fn make_local_index(dir: &TempDir) -> LocalIndex {
        LocalIndex::new(LocalConfig {
            index_store: index_store(dir),
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

        async fn fetch_manifest(
            &self,
            identifier: &Identifier,
            _op: super::super::IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            if let Some(msg) = &self.force_error {
                // Use a RemoteManifestNotFound error to simulate registry errors.
                // The exact variant is unimportant for these tests — only that an
                // error is returned so ChainedIndex's degradation logic is exercised.
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()).into());
            }
            let tag = identifier.tag_or_latest();
            self.calls.lock().unwrap().push(tag.to_string());
            if let Some(digest) = self.known_tags.get(tag) {
                Ok(Some((digest.clone(), manifest_for(digest))))
            } else {
                Ok(None)
            }
        }

        async fn fetch_manifest_digest(
            &self,
            identifier: &Identifier,
            _op: super::super::IndexOperation,
        ) -> Result<Option<Digest>> {
            if let Some(msg) = &self.force_error {
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()).into());
            }
            let tag = identifier.tag_or_latest();
            self.calls.lock().unwrap().push(tag.to_string());
            Ok(self.known_tags.get(tag).cloned())
        }

        async fn fetch_blob(&self, _blob_ref: &crate::oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            if let Some(msg) = &self.force_error {
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()).into());
            }
            Ok(None)
        }

        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &Identifier,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            if let Some(msg) = &self.force_error {
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()).into());
            }
            let tag = identifier.tag_or_latest();
            self.calls.lock().unwrap().push(tag.to_string());
            Ok(self
                .known_tags
                .get(tag)
                .map(|digest| (bytes_for(digest), digest.clone(), manifest_for(digest))))
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

    /// Seed the cache with the full dispatch chain (root tag pointer +
    /// dispatch object) so subsequent cache-only reads succeed.
    async fn seed_full(cache: &LocalIndex, identifier: &Identifier, _d: Digest, source: &Index) {
        let (digest, _manifest) = cache
            .persist_dispatch(source, identifier)
            .await
            .unwrap()
            .expect("source must know the seeded tag");
        if identifier.tag().is_some() {
            cache.commit_root_tag(identifier, &digest).await.unwrap();
        }
    }

    // ── Single-source chain tests ─────────────────────────────────────────

    // Case 1: cache hit → no source consulted.
    //
    // Pre-seed the cache with a tag→digest mapping, then call fetch_manifest.
    // The source should never be queried (zero calls recorded in TestIndex).
    #[tokio::test(flavor = "multi_thread")]
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

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();

        assert!(result.is_some(), "cache hit should return Some");
        assert!(
            spy_calls.lock().unwrap().is_empty(),
            "source must not be queried on a cache hit"
        );
    }

    // Case 2: cache miss + source has tag → update_tag called → retry succeeds.
    #[tokio::test(flavor = "multi_thread")]
    async fn cache_miss_source_has_tag_returns_manifest() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();

        assert!(result.is_some(), "should return the manifest fetched from source");
        let (digest, _) = result.unwrap();
        assert_eq!(digest, digest_a());
    }

    // Case 2b: fetch_manifest_digest has same chain logic.
    #[tokio::test(flavor = "multi_thread")]
    async fn cache_miss_source_has_tag_returns_digest() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest_digest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();

        assert_eq!(result, Some(digest_a()));
    }

    // Case 3: cache miss + source doesn't have the tag → returns None (warn logged).
    #[tokio::test(flavor = "multi_thread")]
    async fn cache_miss_source_missing_tag_returns_none() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::empty());
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_none(), "unknown tag should degrade to None");
    }

    // Case 4: cache miss + sole source errors → error propagates to caller.
    //
    // The chain contract: when every source errored we MUST propagate the
    // error so callers can distinguish "package not found" from "registry
    // outage / auth failure". Collapsing to Ok(None) would break automation
    // retry logic.
    #[tokio::test(flavor = "multi_thread")]
    async fn cache_miss_source_error_propagates() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::failing("connection timed out"));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await;
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
    #[tokio::test(flavor = "multi_thread")]
    async fn cache_miss_digest_source_error_propagates() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::failing("401 unauthorized"));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest_digest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await;
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
    #[tokio::test(flavor = "multi_thread")]
    async fn digest_only_identifier_walks_chain() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let spy = TestIndex::with_tag(TAG, digest_a());
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let id = digest_only_id(); // no tag
        let _ = chained
            .fetch_manifest(&id, super::IndexOperation::Resolve)
            .await
            .unwrap();

        assert!(
            !spy_calls.lock().unwrap().is_empty(),
            "source must be queried for digest-only identifiers — \
             registries support GET /v2/<repo>/manifests/<digest>"
        );
    }

    // Case 5b: fetch_manifest_digest with digest-only identifier walks the chain too.
    #[tokio::test(flavor = "multi_thread")]
    async fn digest_only_identifier_digest_query_walks_chain() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let spy = TestIndex::with_tag(TAG, digest_a());
        let spy_calls = spy.calls.clone();
        let source = make_source(spy);
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let id = digest_only_id();
        let _ = chained
            .fetch_manifest_digest(&id, super::IndexOperation::Resolve)
            .await
            .unwrap();

        assert!(
            !spy_calls.lock().unwrap().is_empty(),
            "source must be queried for digest-only identifiers"
        );
    }

    // Case 5c: bare identifier (no tag, no digest) → chain walks under implicit
    // `:latest`. `ocx install cmake` on a fresh machine must behave the same as
    // `ocx install cmake:latest` — the fallback chain substitutes "latest" and
    // persists it for the subsequent cache lookup.
    #[tokio::test(flavor = "multi_thread")]
    async fn bare_identifier_walks_chain_as_latest() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let source = make_source(TestIndex::with_tag("latest", digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let bare = Identifier::new_registry(REPO, REGISTRY);
        let result = chained
            .fetch_manifest(&bare, super::IndexOperation::Resolve)
            .await
            .unwrap();

        assert!(result.is_some(), "bare identifier must resolve via implicit :latest");
        let (digest, _) = result.unwrap();
        assert_eq!(digest, digest_a());
    }

    // Case 5d: bare identifier + source has no "latest" → degrades to None.
    #[tokio::test(flavor = "multi_thread")]
    async fn bare_identifier_latest_missing_returns_none() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // Source knows about TAG (3.28) but not "latest".
        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let bare = Identifier::new_registry(REPO, REGISTRY);
        let result = chained
            .fetch_manifest(&bare, super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "bare identifier with no remote :latest should degrade to None"
        );
    }

    // Case 6: box_clone shares caches — mutation after clone is visible.
    //
    // Clone the ChainedIndex (via Index::clone → box_clone), seed the cache on
    // the original, then verify the cloned index can read the same data.
    #[tokio::test(flavor = "multi_thread")]
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
        let result_via_clone = cloned
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result_via_clone.is_some(),
            "cloned ChainedIndex must share cache with original — mutation must be visible"
        );
    }

    // Case 7: list_tags delegates to cache only — source not queried.
    #[tokio::test(flavor = "multi_thread")]
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
    #[tokio::test(flavor = "multi_thread")]
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
    #[tokio::test(flavor = "multi_thread")]
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

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "first source hit should succeed");

        // Second source must not have been queried.
        assert!(
            second_calls.lock().unwrap().is_empty(),
            "second source must not be queried when first source succeeds"
        );
    }

    // Case 10: two sources, first errors but second has the tag → tag persisted, success.
    #[tokio::test(flavor = "multi_thread")]
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

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "second source should succeed when first errors");
        let (digest, _) = result.unwrap();
        assert_eq!(digest, digest_b(), "digest should come from the second source");
    }

    // Case 10b: fetch_manifest_digest with same multi-source degradation.
    #[tokio::test(flavor = "multi_thread")]
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

        let result = chained
            .fetch_manifest_digest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert_eq!(result, Some(digest_b()));
    }

    // Case 11: two sources, both error → propagates the last error.
    //
    // When the entire chain is exhausted by errors the chain MUST surface
    // an error so the caller can distinguish a real outage from a clean
    // not-found.
    #[tokio::test(flavor = "multi_thread")]
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

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await;
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
    #[tokio::test(flavor = "multi_thread")]
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

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await;
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
    #[tokio::test(flavor = "multi_thread")]
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

        let result = chained
            .fetch_manifest_digest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await;
        assert!(result.is_err(), "digest query: error-then-miss must propagate as Err");
        let err_message = result.unwrap_err().to_string();
        assert!(
            err_message.contains("connection refused"),
            "propagated error must carry the primary source's message; got: {err_message}"
        );
    }

    // Case 12: empty sources Vec → behaves like LocalIndex alone (no fallback).
    #[tokio::test(flavor = "multi_thread")]
    async fn empty_sources_behaves_like_local_index() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // No sources — empty chain.
        let chained = super::super::Index::from_chained(cache, vec![], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_none(), "empty sources and empty cache → None");
    }

    // Case 12b: tag persistence after fetch — a second call with empty sources
    // should still succeed because the first call persisted the tag to cache.
    #[tokio::test(flavor = "multi_thread")]
    async fn tag_persisted_in_cache_after_source_fetch() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // First call: source has the tag, chain fetches and persists it.
        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        {
            let chained =
                super::super::Index::from_chained(cache.clone(), vec![source], super::super::ChainMode::Default);
            let _ = chained
                .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
                .await
                .unwrap();
        }

        // Second call: same cache, but NO sources.  Must still return the tag
        // from cache because the first call persisted it.
        let chained_no_source = super::super::Index::from_chained(cache, vec![], super::super::ChainMode::Default);
        let result = chained_no_source
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result.is_some(),
            "tag fetched in first call must be persisted so a cache-only second call succeeds"
        );
    }

    // Case 13: a corrupted on-disk root document must not short-circuit the
    // chain walk. `ChainedIndex::fetch_manifest` should log a warn, degrade
    // to the source, and the walk must recover the manifest. An unparseable
    // root raises `MalformedRootDocument` (a hard error per F1 — genuine
    // corruption, not a bare root/catalog digest disagreement), but that
    // variant is not `is_corrupt_index_object` (which matches only
    // `DigestMismatch`), so it takes the same generic warn-and-degrade path
    // as any other local-read error.
    #[tokio::test(flavor = "multi_thread")]
    async fn corrupted_cache_read_falls_back_to_chain() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        // Corrupt the on-disk root document with unparseable bytes so the
        // first local read errors. The path is the index store's
        // `<home>/<source>/p/<ns>/<pkg>.json`.
        let root_file = index_store(&cache_dir).root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_file.parent().unwrap()).unwrap();
        std::fs::write(&root_file, b"{not valid json at all").unwrap();

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect("corrupt cache must degrade to chain walk, not propagate");
        let (digest, _) = result.expect("chain walk must recover the manifest from the source");
        assert_eq!(digest, digest_a());
    }

    // Case 13b: same degrade path for `fetch_manifest_digest`.
    #[tokio::test(flavor = "multi_thread")]
    async fn corrupted_cache_read_digest_falls_back_to_chain() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);

        let root_file = index_store(&cache_dir).root_document_path(REGISTRY, REPO);
        std::fs::create_dir_all(root_file.parent().unwrap()).unwrap();
        // Garbage bytes force a parse error in the root read so the local
        // read errors and `ChainedIndex` degrades to the source chain.
        std::fs::write(&root_file, b"garbage").unwrap();

        let source = make_source(TestIndex::with_tag(TAG, digest_a()));
        let chained = super::super::Index::from_chained(cache, vec![source], super::super::ChainMode::Default);

        let result = chained
            .fetch_manifest_digest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect("corrupt cache must degrade for digest queries too");
        assert_eq!(result, Some(digest_a()));
    }
}
