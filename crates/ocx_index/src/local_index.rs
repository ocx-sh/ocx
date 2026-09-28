// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use super::error::Result;
use crate::{IndexStore, SOURCE_LOCK_TIMEOUT};
use ocx_oci::tag::is_reserved_tag;

use super::wire::{IndexFormatConfig, gate_format_version};
use super::{IndexOperation, index_impl};

mod config;

pub use config::Config;

/// Maximum per-tag dispatch persists run concurrently in one [`LocalIndex::refresh_tags`].
///
/// Public because the CLI multiplies its per-package fan-out by this to state its in-flight
/// bound (`adr_servable_index_snapshot.md`); a test hardcoding `64` there stays green when this moves.
pub const TAG_REFRESH_CONCURRENCY: usize = 64;

/// An OCX-authored root document for a plain OCI registry (`adr_index_indirection.md#a2`).
///
/// Carries each tag's `observed` stamp, which the read-only wire
/// [`IndexRoot`](super::wire::IndexRoot) drops, so a re-authored root keeps every stamp.
#[derive(Debug, Default, Serialize, Deserialize)]
struct DerivedRoot {
    /// `oci://host/path`, authored from the identifier itself.
    repository: String,
    #[serde(default)]
    tags: BTreeMap<String, DerivedTag>,
}

/// A single tag pointer inside a [`DerivedRoot`] (`adr_index_indirection.md#a2`).
#[derive(Debug, Serialize, Deserialize)]
struct DerivedTag {
    content: ocx_oci::Digest,
    /// RFC3339 time of the last refresh; never a freshness gate for local resolution.
    #[serde(default)]
    observed: String,
}

/// The pins one root commit moved past: what the local copy pinned going in, and coming out.
///
/// The sweep removes `previous \ current` only, never a walk over `o/`, or it deletes an object
/// a concurrent sibling-tag refresh wrote before its lock and has not pinned yet.
#[derive(Debug, Default, Clone)]
pub(super) struct RootPins {
    previous: Vec<ocx_oci::Digest>,
    current: Vec<ocx_oci::Digest>,
}

/// The local index collection: per-repository root documents plus the digest-verified
/// dispatch-object CAS (`o/sha256/<hex>.json`, `adr_index_indirection.md#a2`).
///
/// It never holds blob bytes (config blobs, leaf manifests); those live in `$OCX_HOME/blobs`.
#[derive(Clone)]
pub struct LocalIndex {
    index_store: IndexStore,
    /// When false, a yanked tag in the committed root is refused (`adr_index_indirection.md#f3`).
    allow_yanked: bool,
    /// Sources whose `config.json` passed [`Self::check_format_version`]; a refusal is never cached.
    gated_sources: Arc<RwLock<HashSet<String>>>,
    /// Per-namespace `trusted_hosts` SSRF exemptions that
    /// [`ChainedIndex::guard_local_physical`](super::chained_index) judges this copy's
    /// remote-controlled `repository` pointers against (`adr_index_indirection.md#a2`).
    trusted_hosts: std::collections::HashMap<String, Vec<String>>,
    /// `OCX_INSECURE_REGISTRIES` authorities (`host[:port]`) this index may dial over plain HTTP.
    insecure_hosts: Vec<String>,
    /// Registries an index owns, read from config because `--offline` builds no sources and such
    /// a name must still never be read at the host it spells.
    index_namespaces: HashSet<String>,
}

impl LocalIndex {
    pub fn new(config: Config) -> Self {
        Self {
            index_store: config.index_store,
            allow_yanked: false,
            gated_sources: Arc::new(RwLock::new(HashSet::new())),
            trusted_hosts: std::collections::HashMap::new(),
            insecure_hosts: Vec::new(),
            index_namespaces: HashSet::new(),
        }
    }

    /// Sets the registries an index owns.
    pub fn with_index_namespaces(mut self, index_namespaces: HashSet<String>) -> Self {
        self.index_namespaces = index_namespaces;
        self
    }

    /// Whether config names an index as the owner of `registry`, whatever the chain mode.
    pub fn is_index_namespace(&self, registry: &str) -> bool {
        self.index_namespaces.contains(registry)
    }

    /// Sets the yanked opt-in (`OCX_ALLOW_YANKED`) (`adr_index_indirection.md#f3`).
    pub fn with_allow_yanked(mut self, allow_yanked: bool) -> Self {
        self.allow_yanked = allow_yanked;
        self
    }

    /// Sets the per-namespace `trusted_hosts` sets.
    pub fn with_trusted_hosts(mut self, trusted_hosts: std::collections::HashMap<String, Vec<String>>) -> Self {
        self.trusted_hosts = trusted_hosts;
        self
    }

    /// Sets the `OCX_INSECURE_REGISTRIES` authorities.
    pub fn with_insecure_hosts(mut self, insecure_hosts: Vec<String>) -> Self {
        self.insecure_hosts = insecure_hosts;
        self
    }

    /// The authorities this index dials over plain HTTP; empty when none were configured.
    pub fn insecure_hosts(&self) -> &[String] {
        &self.insecure_hosts
    }

    /// The `trusted_hosts` SSRF exemption configured for `registry`, or empty.
    pub fn trusted_hosts_for(&self, registry: &str) -> &[String] {
        self.trusted_hosts.get(registry).map_or(&[], Vec::as_slice)
    }

    /// The effective index home (`--index` ▸ `OCX_INDEX` ▸ `$OCX_HOME/index`) this copy reads and writes.
    pub fn index_store(&self) -> &IndexStore {
        &self.index_store
    }

    /// Grow the local copy for `identifier` from `source`: dispatch objects into `o/`, then the
    /// root document (`adr_index_indirection.md#a2`). A tagged identifier refreshes only that tag.
    ///
    /// Jurisdiction-unaware: the caller picks the source.
    pub async fn refresh_tags(&self, identifier: &ocx_oci::PackageRef, source: &super::Index) -> Result<()> {
        log::info!("Refreshing tags for identifier '{}'.", identifier);

        // A served root document is what marks a published source.
        if let Some((bytes, root)) = source.fetch_root_document(identifier).await? {
            self.refresh_published(identifier, source, &bytes, &root).await
        } else {
            self.refresh_derived(identifier, source).await
        }
    }

    /// Published-source refresh (`adr_index_indirection.md#f1`): persist the adopted tags'
    /// dispatch objects, then merge those tags into the local root.
    ///
    /// Objects are written before the root, so a crash never leaves a root pointing at an absent object.
    async fn refresh_published(
        &self,
        identifier: &ocx_oci::PackageRef,
        source: &super::Index,
        bytes: &[u8],
        root: &super::wire::IndexRoot,
    ) -> Result<()> {
        let named = identifier.tag();
        let in_scope = |tag: &str| named.is_none_or(|only| tag == only);

        // One representative tag per content digest, so aliased tags fetch the index once.
        let mut seen: std::collections::HashSet<ocx_oci::Digest> = std::collections::HashSet::new();
        let representatives: Vec<(String, ocx_oci::Digest)> = root
            .tags
            .iter()
            .filter(|(tag, _)| in_scope(tag))
            .filter(|(_, entry)| seen.insert(entry.content.clone()))
            .map(|(tag, entry)| (tag.clone(), entry.content.clone()))
            .collect();

        let this = self;
        let registry = identifier.registry();
        let repository = identifier.repository();
        // `collect`, not `try_collect`, or one tag's failure discards every sibling's completed work
        // (`adr_index_sync_performance.md`); the input index keeps the reported failure deterministic.
        let outcomes: Vec<(usize, ocx_oci::Digest, Result<()>)> = stream::iter(representatives.into_iter().enumerate())
            .map(|(position, (tag, content))| {
                let tagged = identifier.clone_with_tag(&tag);
                async move {
                    log::debug!("Refreshing published tag '{}' for identifier '{}'.", tag, identifier);
                    // Skip the fetch only for a hash-verified object, never a bare `exists()`, or a
                    // zero-byte crash artifact or tampered file is kept forever.
                    let outcome = match this
                        .index_store
                        .read_dispatch_object(registry, repository, &content)
                        .await
                    {
                        Ok(Some(_)) => Ok(()),
                        Ok(None) | Err(_) => {
                            // `Err` refetches on purpose: nothing else repairs a corrupt object, since
                            // `resolve_dispatch` treats a `DigestMismatch` as fatal.
                            this.persist_dispatch(source, &tagged).await.map(|_| ())
                        }
                    };
                    (position, content, outcome)
                }
            })
            .buffer_unordered(TAG_REFRESH_CONCURRENCY)
            .collect()
            .await;

        // Failure is per content digest, not per tag: every tag aliasing a failed digest is unpersisted.
        let mut unpersisted: std::collections::HashSet<ocx_oci::Digest> = std::collections::HashSet::new();
        let mut failure: Option<(usize, super::error::Error)> = None;
        for (position, content, outcome) in outcomes {
            let Err(error) = outcome else { continue };
            unpersisted.insert(content);
            if failure.as_ref().is_none_or(|(lowest, _)| position < *lowest) {
                failure = Some((position, error));
            }
        }

        let Some((_, error)) = failure else {
            let scope = match &named {
                Some(tag) => RootScope::Tags(std::slice::from_ref(tag)),
                None => RootScope::Package,
            };
            let pins = self.commit_published_root(identifier, bytes, scope).await?;
            self.sweep_dispatch_orphans(identifier, &pins).await;
            return Ok(());
        };

        // Partial success commits only persisted tags under a tag scope: `RootScope::Package` would
        // pin a tag whose object was never written and migrate `repository` on a partial observation.
        let adopted: Vec<&str> = root
            .tags
            .iter()
            .filter(|(tag, entry)| in_scope(tag) && !unpersisted.contains(&entry.content))
            .map(|(tag, _)| tag.as_str())
            .collect();
        // Matched, never `?`-ed, or the tag failure in `error` is dropped unreported.
        if !adopted.is_empty() {
            match self
                .commit_published_root(identifier, bytes, RootScope::Tags(&adopted))
                .await
            {
                Ok(pins) => self.sweep_dispatch_orphans(identifier, &pins).await,
                Err(commit) => return Err(withheld_by_commit_failure(identifier, &error, commit)),
            }
        }
        Err(error)
    }

    /// Derived-source refresh (`adr_index_indirection.md#a2`): persist each
    /// tag's dispatch object, then author the root document field-wise.
    async fn refresh_derived(&self, identifier: &ocx_oci::PackageRef, source: &super::Index) -> Result<()> {
        let tags = match identifier.tag() {
            Some(tag) => vec![tag.to_owned()],
            None => source.list_tags(identifier).await?.unwrap_or_default(),
        };

        if tags.is_empty() {
            return Err(super::error::Error::RemoteManifestNotFound(identifier.to_string()));
        }

        // Reserved names are filtered before the fetch, or each stages an image index into `o/`
        // that no root names, an orphan in a store GC never walks.
        let tags: Vec<String> = tags
            .into_iter()
            .filter(|tag| {
                let indexable = !is_reserved_tag(tag);
                if !indexable {
                    log::debug!("Tag '{tag}' is reserved and is never a version — not fetched.");
                }
                indexable
            })
            .collect();

        let this = self;
        // `collect`, not `try_collect`, or one tag's failure discards every sibling's completed work;
        // the input index keeps the reported failure deterministic.
        let outcomes = stream::iter(tags.into_iter().enumerate())
            .map(|(position, tag)| {
                let tagged = identifier.clone_with_tag(&tag);
                async move {
                    log::debug!("Refreshing derived tag '{}' for identifier '{}'.", tag, identifier);
                    let outcome = match this.persist_dispatch(source, &tagged).await {
                        Ok(Some((_, content, manifest))) => {
                            Ok(records_root_tag(&tag, &manifest).then_some((tag, content)))
                        }
                        Ok(None) => {
                            log::debug!("Source has no manifest for tag '{}' — skipping.", tag);
                            Ok(None)
                        }
                        Err(error) => Err(error),
                    };
                    (position, outcome)
                }
            })
            .buffer_unordered(TAG_REFRESH_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;

        let mut fetched: Vec<(String, ocx_oci::Digest)> = Vec::new();
        let mut failure: Option<(usize, super::error::Error)> = None;
        for (position, outcome) in outcomes {
            match outcome {
                Ok(Some(entry)) => fetched.push(entry),
                Ok(None) => {}
                Err(error) => {
                    if failure.as_ref().is_none_or(|(lowest, _)| position < *lowest) {
                        failure = Some((position, error));
                    }
                }
            }
        }

        if let Some((_, error)) = failure {
            // Matched, never `?`-ed, or the tag failure in `error` is dropped unreported.
            if !fetched.is_empty() {
                match self.commit_root_tags(identifier, &fetched).await {
                    Ok(pins) => self.sweep_dispatch_orphans(identifier, &pins).await,
                    Err(commit) => return Err(withheld_by_commit_failure(identifier, &error, commit)),
                }
            }
            return Err(error);
        }

        if fetched.is_empty() {
            return Err(super::error::Error::NoIndexableTag(identifier.to_string()));
        }

        // One batched commit (`adr_index_indirection.md#a2`); per-tag commits rewrite the root N times.
        let pins = self.commit_root_tags(identifier, &fetched).await?;
        self.sweep_dispatch_orphans(identifier, &pins).await;
        Ok(())
    }

    /// Remove `pins.previous \ pins.current` from `o/`; infallible, a failure only leaves an orphan.
    ///
    /// Call it after every commit that moves a pin, partial ones included, or the abandoned object
    /// is stranded forever: the next run reads `previous` from the already-moved root.
    async fn sweep_dispatch_orphans(&self, identifier: &ocx_oci::PackageRef, pins: &RootPins) {
        // An object whose tag never committed is in neither pin set, so it survives for the next run
        // (`test_orphan_dispatch_object_without_root_self_heals_on_next_online_update`).
        let source = identifier.registry();
        let repository = identifier.repository();
        // `debug!` only: this runs per package across a whole catalog on `ocx index sync`.
        match super::regenerate::sweep_orphan_objects(
            &self.index_store,
            source,
            repository,
            &pins.previous,
            &pins.current,
        )
        .await
        {
            Ok(removed) if removed.is_empty() => {}
            Ok(removed) => log::debug!(
                "Removed {} dispatch object(s) '{source}/{repository}' no longer pins: {}.",
                removed.len(),
                removed.join(", ")
            ),
            Err(error) => log::debug!(
                "Could not sweep the dispatch objects '{source}/{repository}' moved off ({}) — the \
                 refresh itself committed; the next pin movement past them sweeps them.",
                ocx_util::error::render_chain(&error)
            ),
        }
    }

    // ── Dispatch-only reads/writes ────────────────────────────────────────────

    /// Fetch `identifier`'s manifest from `source`, write it to `o/` only when it is an image index,
    /// and return it verbatim; `Ok(None)` when the source has none (`adr_index_indirection.md#a3`).
    pub async fn persist_dispatch(
        &self,
        source: &super::Index,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        let Some((bytes, digest, manifest)) = source.fetch_manifest_raw_bytes(identifier).await? else {
            return Ok(None);
        };
        if let ocx_oci::Manifest::ImageIndex(_) = &manifest {
            self.stage_dispatch_bytes(identifier, &digest, &bytes).await?;
        }
        Ok(Some((bytes, digest, manifest)))
    }

    /// [`Self::persist_dispatch`] without the write, for a `ReadOnly` resolve.
    pub async fn fetch_dispatch_only(
        &self,
        source: &super::Index,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        source.fetch_manifest_raw_bytes(identifier).await
    }

    /// Commit `identifier`'s tag → `content` into its derived root (`adr_index_indirection.md#a2`).
    ///
    /// # Panics
    ///
    /// When `identifier` carries no tag.
    pub(super) async fn commit_root_tag(
        &self,
        identifier: &ocx_oci::PackageRef,
        content: &ocx_oci::Digest,
    ) -> Result<()> {
        let tag = identifier
            .tag()
            .expect("commit_root_tag invariant: identifier must carry a tag");
        self.commit_root_tags(identifier, &[(tag.to_owned(), content.clone())])
            .await
            .map(|_| ())
    }

    /// Upsert `entries` into `identifier`'s derived root in one locked read-modify-write
    /// (`adr_index_indirection.md#a2`); `identifier`'s own tag is ignored.
    async fn commit_root_tags(
        &self,
        identifier: &ocx_oci::PackageRef,
        entries: &[(String, ocx_oci::Digest)],
    ) -> Result<RootPins> {
        if entries.is_empty() {
            return Ok(RootPins::default());
        }
        let source = identifier.registry();
        let repository = identifier.repository();
        let expected_repository = format!("oci://{source}/{repository}");

        // Locked under `$OCX_HOME/locks`, never a sidecar in the index home, which may be read-only.
        let _guard = self
            .index_store
            .lock_source("index-root", source, repository, SOURCE_LOCK_TIMEOUT)
            .await?;

        let mut doc = match self.index_store.read_root_document_bytes(source, repository).await? {
            Some(bytes) => match serde_json::from_slice::<DerivedRoot>(&bytes) {
                Ok(doc) => {
                    if doc.repository != expected_repository {
                        return Err(super::error::Error::RootRepositoryMismatch {
                            repository: repository.to_string(),
                            expected: expected_repository,
                            found: doc.repository,
                        });
                    }
                    doc
                }
                // A derived root is only ever OCX's own write, so an unparseable one is a
                // crashed write to rewrite, not untrusted input to refuse.
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

        // Read under the lock; an unparseable root arrives empty, so it never authorises a removal.
        let previous: Vec<ocx_oci::Digest> = doc.tags.values().map(|tag| tag.content.clone()).collect();

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

        let current: Vec<ocx_oci::Digest> = doc.tags.values().map(|tag| tag.content.clone()).collect();
        let bytes = serde_json::to_vec_pretty(&doc)?;
        self.index_store.write_root_document(source, repository, &bytes).await?;
        Ok(RootPins { previous, current })
    }

    /// Resolve `identifier` against `o/` (`adr_index_indirection.md#a3`); `Ok(None)` only when the
    /// root or tag is unknown locally, and a missing object is [`DispatchResolution::AbsentDispatch`].
    pub(super) async fn resolve_dispatch(
        &self,
        identifier: &ocx_oci::PackageRef,
        kind: SourceKind,
    ) -> Result<Option<DispatchResolution>> {
        let source = identifier.registry();
        let repository = identifier.repository();

        let content = match identifier.digest() {
            Some(digest) => digest,
            None => {
                let Some(result) = self.read_root_by_kind(source, repository, kind).await? else {
                    return Ok(None);
                };
                let tag = identifier.tag_or_latest();
                let Some(tag_entry) = result.root.tags.get(tag) else {
                    return Ok(None);
                };
                // Tag lane only: a yank is never checked against an immutable digest pin.
                super::ocx_index::surface_root_status(identifier, &result.root, tag_entry, self.allow_yanked)?;
                tag_entry.content.clone()
            }
        };

        match self
            .index_store
            .read_dispatch_object(source, repository, &content)
            .await?
        {
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

    /// Read a root document, cross-checked against `c/index.json` only for a published source
    /// (`adr_index_indirection.md#decision-h`); `Ok(None)` when unknown locally.
    async fn read_root_by_kind(
        &self,
        source: &str,
        repository: &str,
        kind: SourceKind,
    ) -> Result<Option<crate::RootReadResult>> {
        self.check_format_version(source).await?;
        let repository_check =
            |root: &super::wire::IndexRoot| super::parse_repository_pointer(&root.repository).map(|_| ());
        match kind {
            SourceKind::Published => self.index_store.read_root(source, repository, repository_check).await,
            SourceKind::Derived => {
                self.index_store
                    .read_root_uncatalogued(source, repository, repository_check)
                    .await
            }
        }
    }

    /// Gate this source's local subtree on its `config.json` version, once per source; an absent file
    /// is [`IndexFormatConfig::assumed_v1`] (`adr_servable_index_snapshot.md`).
    ///
    /// `ChainedIndex`'s `is_local_read_refusal` must propagate a refusal, or the walk grows the refused tree.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat) on an unknown
    /// version; an unreadable or unparseable `config.json` propagates, never read as absent.
    async fn check_format_version(&self, source: &str) -> Result<()> {
        // `recover_base_with_real_registry` reads roots ungated, safe only because it soft-fails; a new
        // reader on that route needs this gate.
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

    /// Tags in `identifier`'s local root (`adr_index_indirection.md#a2`); `Ok(None)` when unknown locally.
    pub(super) async fn list_local_tags(
        &self,
        identifier: &ocx_oci::PackageRef,
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

    /// Repositories known locally under `source` (`adr_index_indirection.md#a2`).
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

    /// The physical location the local root's `repository` points at, with the logical tag and
    /// digest applied (`adr_index_indirection.md#c2-structural`); `Ok(None)` when there is no local root.
    ///
    /// # Errors
    ///
    /// [`Error::MalformedPhysicalRef`](super::error::Error::MalformedPhysicalRef) when `repository`
    /// is not a well-formed `oci://` pointer.
    pub(super) async fn physical_reference(
        &self,
        identifier: &ocx_oci::PackageRef,
        kind: SourceKind,
    ) -> Result<Option<ocx_oci::OciIdentifier>> {
        let Some(result) = self
            .read_root_by_kind(identifier.registry(), identifier.repository(), kind)
            .await?
        else {
            return Ok(None);
        };
        let physical = super::parse_repository_pointer(&result.root.repository)?;
        Ok(Some(physical.at_version_of(identifier)))
    }

    /// Merge a fetched published root into the local copy within `scope`, never deleting a tag
    /// (`adr_index_indirection.md#a2`), then write the source's `config.json` if absent
    /// (`adr_servable_index_snapshot.md`).
    pub(super) async fn commit_published_root(
        &self,
        identifier: &ocx_oci::PackageRef,
        fetched_bytes: &[u8],
        scope: RootScope<'_>,
    ) -> Result<RootPins> {
        let source = identifier.registry();
        let repository = identifier.repository();
        let repository_check =
            |root: &super::wire::IndexRoot| super::parse_repository_pointer(&root.repository).map(|_| ());

        // Read under the catalog lock, or the merge clobbers a concurrent writer's root.
        let mut transaction = self.index_store.begin_catalog_transaction(source).await?;

        let committed = self.index_store.read_root_document_bytes(source, repository).await?;
        let previous = published_pins(committed.as_deref());
        let mut current = previous.clone();
        // `None` skips the write, so a no-op update leaves the committed tree's mtime alone.
        if let Some(bytes) = merge_root(committed.as_deref(), fetched_bytes, scope) {
            transaction.write_root(repository, &bytes, repository_check).await?;
            // From the committed bytes, never the fetched root the merge may have taken only part of.
            current = published_pins(Some(&bytes));
        }
        transaction.commit().await?;
        let pins = RootPins { previous, current };

        // After `commit`: before it, this blocks for `SOURCE_LOCK_TIMEOUT` on the lock `commit` still
        // holds, and a crash could leave config without content.
        match self.index_store.ensure_source_config(source).await {
            // Only the lock timeout is absorbed, since the catalog already committed; I/O errors propagate.
            Err(error) if is_lock_timeout(&error) => {
                log::warn!(
                    "Index source '{source}' was published without a 'config.json': its catalog lock \
                     stayed held for {SOURCE_LOCK_TIMEOUT:?} ({}). The next index update writes it.",
                    ocx_util::error::render_chain(&error)
                );
                Ok(pins)
            }
            Err(error) => Err(error),
            Ok(()) => Ok(pins),
        }
    }

    /// Write already-fetched dispatch-object bytes to `o/` under `digest`, verified and idempotent
    /// (`adr_index_indirection.md#a3`).
    pub async fn stage_dispatch_bytes(
        &self,
        identifier: &ocx_oci::PackageRef,
        digest: &ocx_oci::Digest,
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
    pub(super) async fn seed_root_document(&self, identifier: &ocx_oci::PackageRef, bytes: &[u8]) -> Result<()> {
        let mut transaction = self
            .index_store
            .begin_catalog_transaction(identifier.registry())
            .await?;
        transaction
            .write_root(identifier.repository(), bytes, |root| {
                super::parse_repository_pointer(&root.repository).map(|_| ())
            })
            .await?;
        transaction.commit().await
    }
}

/// Whether a source is a published copy or one OCX derives from a plain registry
/// (`adr_index_indirection.md` Decision A2/H); only a published one has a `c/index.json` catalog.
// Not seam-gated: `IndexImpl::source_kind` returns it and the trait is `pub` under `__testing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Published,
    Derived,
}

/// Outcome of [`LocalIndex::resolve_dispatch`] (`adr_index_indirection.md` A3).
#[derive(Debug)]
pub(super) enum DispatchResolution {
    /// The image index `content` names, present in `o/`.
    Dispatch {
        content: ocx_oci::Digest,
        index: Box<ocx_oci::ImageIndex>,
    },
    /// `content` is absent from `o/`; the caller fetches it by digest, blob store first, and an
    /// image index self-heals via [`LocalIndex::stage_dispatch_bytes`].
    AbsentDispatch { content: ocx_oci::Digest },
}

/// How much of a fetched published root a write may adopt; no scope ever deletes a tag.
#[derive(Debug, Clone, Copy)]
pub(super) enum RootScope<'a> {
    /// The named tags' entries only; sibling pins and package-level fields stay as committed.
    Tags(&'a [&'a str]),
    /// Every listed tag plus the package-level fields, including a `repository` migration.
    Package,
    /// The package-level fields and no tags, written only when no root is committed yet;
    /// migrating a committed `repository` is [`Self::Package`]'s alone.
    Routing,
}

/// Whether `error` is an expired lock wait rather than a genuine I/O failure.
///
/// The kind alone would also match an OS `ETIMEDOUT`; only ocx's synthesized wait timeout lacks a
/// `raw_os_error`, so dropping that test absorbs a real write failure as a lost lock race.
fn is_lock_timeout(error: &super::error::Error) -> bool {
    matches!(
        error,
        super::error::Error::File(file)
            if file.cause.kind() == std::io::ErrorKind::TimedOut && file.cause.raw_os_error().is_none()
    )
}

/// When a partial commit also fails, log the withheld tag failure and return the commit failure
/// (`adr_index_sync_performance.md`).
///
/// Returning `withheld` instead exits with a transport "retry" code (69/75) for a local, durable
/// failure that every later package in the run hits identically.
fn withheld_by_commit_failure(
    identifier: &ocx_oci::PackageRef,
    withheld: &super::error::Error,
    commit: super::error::Error,
) -> super::error::Error {
    // `warn!`, not `debug!`: nothing else records the withheld tag failure.
    log::warn!(
        "Index refresh for '{identifier}' could not commit the tags that did refresh ({}). The \
         tag failure it was holding back is reported here and nowhere else: {}",
        ocx_util::error::render_chain(&commit),
        ocx_util::error::render_chain(withheld)
    );
    commit
}

/// The content digests a published root's tags pin, or none when absent or unparseable.
///
/// Unparseable must yield none, not an error or a guess: as [`RootPins::previous`] an empty set
/// authorises no removal, so a copy OCX cannot read never deletes the objects it names.
fn published_pins(bytes: Option<&[u8]>) -> Vec<ocx_oci::Digest> {
    bytes
        .and_then(|bytes| serde_json::from_slice::<super::wire::IndexRoot>(bytes).ok())
        .map(|root| root.tags.into_values().map(|tag| tag.content).collect())
        .unwrap_or_default()
}

/// Merge a fetched published root into `committed` within `scope`; `None` when nothing in scope changed.
///
/// Walked as a [`serde_json::Value`], not the typed [`super::wire::IndexRoot`], which would drop
/// every field a newer writer added.
fn merge_root(committed: Option<&[u8]>, fetched: &[u8], scope: RootScope<'_>) -> Option<Vec<u8>> {
    let fetched_root: serde_json::Value = serde_json::from_slice(fetched).ok()?;
    let adopted: Vec<(String, serde_json::Value)> = match scope {
        RootScope::Tags(named) => {
            let entries: Vec<(String, serde_json::Value)> = named
                .iter()
                .filter_map(|tag| {
                    let entry = fetched_root.get("tags").and_then(|tags| tags.get(tag)).cloned();
                    if entry.is_none() {
                        log::debug!("fetched root does not carry '{tag}' — leaving the committed root alone");
                    }
                    entry.map(|entry| ((*tag).to_string(), entry))
                })
                .collect();
            if entries.is_empty() {
                // No write, or a first-sight package adopts `repository` on a tag its root never listed.
                return None;
            }
            entries
        }
        RootScope::Package => fetched_root
            .get("tags")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .map(|(tag, entry)| (tag.clone(), entry.clone()))
            .collect(),
        RootScope::Routing if committed.is_some() => return None,
        RootScope::Routing => Vec::new(),
    };

    // The typed parse, not just valid JSON, or a root missing `repository` re-merges and re-fails
    // `write_root` on every update instead of healing.
    let usable = committed.is_some_and(|bytes| serde_json::from_slice::<super::wire::IndexRoot>(bytes).is_ok());
    let mut root: serde_json::Value = match committed.filter(|_| usable).map(serde_json::from_slice) {
        Some(Ok(root)) => root,
        _ => {
            // The fetched document with its tags emptied, so the scope alone decides which tags land.
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
        // Overwrite-only: a field the remote dropped stays.
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
    (changed || !usable).then(|| super::serialize_root(&root))
}

/// Whether `tag` may be recorded as a version in a derived root (`adr_index_indirection.md` D2/D7).
///
/// Both write paths skip `list_tags` for a tagged identifier, so its filters cannot replace this
/// gate: a bad entry would be committed, then hidden by the listing.
pub(super) fn records_root_tag(tag: &str, manifest: &ocx_oci::Manifest) -> bool {
    // A bare manifest writes nothing to `o/`, so recording it leaves a tag with no object.
    if !matches!(manifest, ocx_oci::Manifest::ImageIndex(_)) {
        log::debug!("tag '{tag}' resolves to a bare manifest, not an image index — not recorded in the root");
        return false;
    }
    if is_reserved_tag(tag) {
        log::debug!("tag '{tag}' is reserved and is never a version — not recorded in the root");
        return false;
    }
    true
}

/// Decode a dispatch object into its image index (`adr_index_indirection.md` A2,
/// `adr_oci_index_only_dispatch.md` D1); `Ok(None)` when the bytes are not an image index, which
/// the caller heals as [`DispatchResolution::AbsentDispatch`].
///
/// # Errors
///
/// [`Error::InvalidImageIndex`](super::error::Error::InvalidImageIndex) when the bytes are an image
/// index that fails validation; refused, never healed.
fn decode_index_manifest(bytes: &[u8]) -> Result<Option<ocx_oci::ImageIndex>> {
    let Ok(index) = serde_json::from_slice::<ocx_oci::ImageIndex>(bytes) else {
        return Ok(None);
    };
    ocx_oci::manifest::validate_image_index(&index).map_err(super::error::Error::from)?;
    Ok(Some(index))
}

#[async_trait]
impl index_impl::IndexImpl for LocalIndex {
    // Production calls the kind-routed inherent methods through `ChainedIndex`; this surface
    // serves unit tests.
    fn insecure_hosts(&self) -> &[String] {
        &self.insecure_hosts
    }

    // `SourceKind::Derived` only skips the catalog cross-check (`adr_index_indirection.md` A2/H).
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        self.list_local_repositories(registry, SourceKind::Derived).await
    }

    async fn list_tags(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
        Ok(self
            .list_local_tags(identifier, SourceKind::Derived)
            .await?
            .map(|tags| tags.into_iter().filter(|t| !is_reserved_tag(t)).collect()))
    }

    async fn fetch_manifest(
        &self,
        identifier: &ocx_oci::PackageRef,
        _op: IndexOperation,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        log::trace!("Fetching manifest for identifier '{}'.", identifier);
        match self.resolve_dispatch(identifier, SourceKind::Derived).await? {
            Some(DispatchResolution::Dispatch { content, index }) => {
                Ok(Some((content, ocx_oci::Manifest::ImageIndex(*index))))
            }
            Some(DispatchResolution::AbsentDispatch { .. }) | None => Ok(None),
        }
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &ocx_oci::PackageRef,
        _op: IndexOperation,
    ) -> Result<Option<ocx_oci::Digest>> {
        match self.resolve_dispatch(identifier, SourceKind::Derived).await? {
            Some(DispatchResolution::Dispatch { content, .. })
            | Some(DispatchResolution::AbsentDispatch { content }) => Ok(Some(content)),
            None => Ok(None),
        }
    }

    async fn fetch_blob(&self, _blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
        // Blobs live in `$OCX_HOME/blobs`, which `ChainedIndex::fetch_blob` reads directly.
        Ok(None)
    }

    async fn physical_reference(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        // Never the trait default: its `Ok(None)` makes callers dial the logical identifier.
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

    use ocx_oci::{Algorithm, ImageManifest, Manifest};

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";

    /// Every `warn!` this test binary emits, in arrival order.
    ///
    /// The D-008 double fault is only observable here: both errors are real, one
    /// of them has to be the return value, and the other survives as a log line.
    /// A source-text guard would pin the line's existence but not that the
    /// withheld error actually reaches it, which is the whole bug.
    static CAPTURED_WARNINGS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    struct CapturingLogger;

    impl log::Log for CapturingLogger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= log::Level::Warn
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                CAPTURED_WARNINGS.lock().unwrap().push(record.args().to_string());
            }
        }
        fn flush(&self) {}
    }

    /// Install the capture once for the whole test binary — the `log` facade's
    /// logger is process-global and cannot be scoped to one test.
    ///
    /// The buffer is therefore shared, so it is never drained and never counted:
    /// each caller searches it for a needle unique to its own fixture, which
    /// keeps these assertions independent of test order and of what else ran.
    fn install_warning_capture() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            log::set_boxed_logger(Box::new(CapturingLogger))
                .expect("nothing else in ocx_lib's test binary installs a logger");
            // Below `Warn` nothing is captured, so the macros' level check drops
            // every `debug!`/`trace!` in the crate before it formats anything.
            log::set_max_level(log::LevelFilter::Warn);
        });
    }

    /// Whether any captured warning contains `needle`.
    fn warned_about(needle: &str) -> bool {
        CAPTURED_WARNINGS
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(needle))
    }

    /// This module's PRODUCTION half with comment lines stripped — the one
    /// window every source-text guard here counts over.
    ///
    /// Two details are load-bearing. The cut is at the test MODULE, not at the
    /// first `#[cfg(test)]`: a small `#[cfg(test)] impl LocalIndex` block of
    /// scaffolding sits well above it, and a window stopping there silently
    /// drops `merge_root`, `RootScope` and `records_root_tag` — production code
    /// the guards exist to watch. And comments go first, or a denylist matches
    /// the comment that quotes the forms it forbids.
    fn production_half() -> String {
        let (production, _) = include_str!("local_index.rs")
            .split_once("\n#[cfg(test)]\nmod tests {")
            .expect("the module has a production half followed by its test module");
        let window = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        // Non-vacuity: a truncation bug must not be able to fake a zero count.
        assert!(
            window.contains("async fn refresh_published") && window.contains("fn merge_root"),
            "the scanned window must reach the code the guards are about"
        );
        assert!(
            !window.contains("mod tests {"),
            "the window must stop before the test module, or every count is polluted by tests"
        );
        window
    }

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
        let source = production_half();
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

    /// C-030 — the dispatch gate is `read_dispatch_object`, and no existence
    /// check can creep in.
    ///
    /// D-006 makes the whole self-heal property rest on one line, and a rule
    /// that important with no enforcement is exactly the asymmetry the design
    /// record flags. Both halves are required: a negative-only guard keeps
    /// passing once its needles stop matching anything, which reads as coverage
    /// while providing none.
    #[test]
    fn the_dispatch_gate_is_content_addressed_and_never_an_existence_check() {
        let production = production_half();

        // NEGATIVE half. A bare existence check cannot tell a valid object from
        // a zero-byte crash artifact or a tampered one, so a caller reading
        // "exists" as "already have it" skips the fetch and strands the
        // corruption permanently — nothing else repairs it.
        for forbidden in [
            ".exists()",
            "try_exists",
            "path_exists_lossy",
            "symlink_metadata",
            "metadata().is_ok()",
        ] {
            assert_eq!(
                production.matches(forbidden).count(),
                0,
                "'{forbidden}' is an existence check: it cannot verify the bytes, and this \
                 module decides from it whether to skip a fetch"
            );
        }

        // POSITIVE half: the gate that must be there, located in the function
        // that must hold it.
        let gate = production
            .split_once("async fn refresh_published")
            .and_then(|(_, rest)| rest.split_once("async fn refresh_derived"))
            .map(|(body, _)| body)
            .expect("refresh_published precedes refresh_derived in this module");
        assert!(
            gate.contains("read_dispatch_object"),
            "the published refresh must gate its fetch on the content-addressed store, which \
             hash-verifies the bytes against the digest the root pins"
        );
        assert!(
            gate.contains("persist_dispatch"),
            "non-vacuity: the gate is only a gate if a real fetch sits behind it"
        );
    }

    /// C-026 — the self-heal and the partial commit are silent.
    ///
    /// Counted per site, not merely as a budget: a count alone is satisfied by
    /// deleting one line and adding another. The four lines below are the
    /// module's entire operator-facing output — per-tag detail stays at
    /// `debug!`, because an index update over a many-tagged package must not
    /// flood the log, and a repaired object is a self-heal, not a warning.
    ///
    /// The double-fault line is the one exception, and only because it is not
    /// per-tag detail: when the partial commit fails, the withheld tag failure
    /// is not the return value and nothing else records it, so `debug!` would
    /// mean losing it. One site, shared by both provenance kinds.
    #[test]
    fn no_operator_facing_log_line_grows_in_this_module() {
        let production = production_half();

        assert_eq!(
            production.matches("log::warn!").count(),
            3,
            "the three warnings below are the whole warn budget of this module"
        );
        assert!(
            production.contains("derived root for '{source}/{repository}' is unparseable"),
            "warn 1 of 3: the kill-9 recovery of an unparseable derived root"
        );
        assert!(
            production.contains("was published without a 'config.json'"),
            "warn 2 of 3: the config.json hook losing its second lock"
        );
        assert!(
            production.contains("could not commit the tags that did refresh"),
            "warn 3 of 3: the D-008 double fault, the only record of the withheld tag failure"
        );

        assert_eq!(
            production.matches("log::info!").count(),
            1,
            "one info line per identifier, and no more"
        );
        assert!(
            production.contains(r#"log::info!("Refreshing tags for identifier"#),
            "the single info line is the per-identifier refresh announcement"
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
    fn catalog_on_disk(path: &std::path::Path) -> crate::CatalogIndex {
        let document: crate::CatalogDocument = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        document.into_packages().unwrap()
    }

    fn repo_id() -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::new_registry(REPO, REGISTRY)
    }

    fn tagged_id(tag: &str) -> ocx_oci::PackageRef {
        repo_id().clone_with_tag(tag)
    }

    /// Serialise a flat image manifest and return `(bytes, digest)` so the
    /// bytes genuinely hash to the digest — the A3 write invariant.
    fn image_manifest_bytes() -> (Vec<u8>, ocx_oci::Digest) {
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
    /// `ocx_oci::ImageIndex` does not model. A fixture that served exactly
    /// `serde_json::to_vec(&parsed)` could not tell a byte-copying
    /// implementation from a re-serialising one, so every verbatim-bytes and
    /// digest-stability assertion built on it would be vacuous — the whole
    /// point of storing registry bytes verbatim (D1/A4) would be pinned by
    /// nothing.
    fn image_index_bytes(leaves: &[(&str, &ocx_oci::Digest)]) -> (Vec<u8>, ocx_oci::Digest) {
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
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![self.tag.clone()]))
        }
        async fn fetch_manifest(
            &self,
            identifier: &ocx_oci::PackageRef,
            _op: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            id: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, Manifest)>> {
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
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![self.tag.clone()]))
        }
        async fn fetch_manifest(
            &self,
            id: &ocx_oci::PackageRef,
            _op: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(id)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, Manifest)>> {
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
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(self.tags.clone()))
        }
        async fn fetch_manifest(
            &self,
            _: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, Manifest)>> {
            let (bytes, digest) = index_bytes();
            Ok(Some((digest, serde_json::from_slice(&bytes).unwrap())))
        }
        async fn fetch_manifest_digest(
            &self,
            _: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            let (_, digest) = index_bytes();
            Ok(Some(digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, Manifest)>> {
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
    fn index_for_leaf(leaf: &ocx_oci::Digest) -> (Vec<u8>, ocx_oci::Digest) {
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
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string(), "2.0".to_string()]))
        }
        async fn fetch_manifest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, Manifest)>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, d, m)| (d, m)))
        }
        async fn fetch_manifest_digest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, d, _)| d))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            id: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, Manifest)>> {
            // Each tag resolves to a DISTINCT single-platform image index.
            let leaf_char = match id.tag() {
                Some("1.0") => "a",
                Some("2.0") => "b",
                _ => return Ok(None),
            };
            let leaf = ocx_oci::Digest::Sha256(leaf_char.repeat(64));
            let (bytes, digest) = index_for_leaf(&leaf);
            let manifest = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, digest, manifest)))
        }
        async fn fetch_root_document(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, super::super::wire::IndexRoot)>> {
            let (_, obs1) = index_for_leaf(&ocx_oci::Digest::Sha256("a".repeat(64)));
            let (_, obs2) = index_for_leaf(&ocx_oci::Digest::Sha256("b".repeat(64)));
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
        let (_, obs1) = index_for_leaf(&ocx_oci::Digest::Sha256("a".repeat(64)));
        let (_, obs2) = index_for_leaf(&ocx_oci::Digest::Sha256("b".repeat(64)));
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

    // ── D-006 / D-008: the CAS gate, and partial success ─────────────────────

    /// The dispatch object a scripted tag resolves to: a single-platform image
    /// index over the leaf `<char> x 64`. Distinct chars ⇒ distinct dispatch
    /// digests; the SAME char on two tags ⇒ two tags ALIASED onto one object,
    /// which is the shape C-013 is about.
    fn object_for(leaf: char) -> (Vec<u8>, ocx_oci::Digest) {
        index_for_leaf(&ocx_oci::Digest::Sha256(leaf.to_string().repeat(64)))
    }

    /// Just the digest half of [`object_for`] — what a scripted root pins.
    fn content_for(leaf: char) -> ocx_oci::Digest {
        object_for(leaf).1
    }

    /// A source driven by a per-tag script, counting every manifest fetch.
    ///
    /// The counter is the point: the D-006 gate is invisible in behaviour (the
    /// tag resolves and the object is on disk either way) and shows up only as a
    /// request that was not made, so every assertion here is on the fetch LIST,
    /// never on success.
    ///
    /// - `script` — tag → the leaf char its dispatch object names.
    /// - `leaves` — tags that resolve to a BARE platform manifest instead: no
    ///   `o/` object exists for one by design, so it must be fetched every run
    ///   (C-009 edge case (b)).
    /// - `fail` — tags whose fetch answers with an error.
    /// - `hold` — one tag that waits for another to finish before answering, so
    ///   a test can force completion order against input order (C-012(b)).
    /// - `published` — serve a verbatim root document (published provenance) or
    ///   `None` (derived, OCX authors the root field-wise).
    #[derive(Clone)]
    struct ScriptedSource {
        script: BTreeMap<String, char>,
        leaves: std::collections::BTreeSet<String>,
        fail: std::collections::BTreeSet<String>,
        repository: String,
        published: bool,
        fetched: Arc<std::sync::Mutex<Vec<String>>>,
        completed: Arc<std::sync::Mutex<Vec<String>>>,
        hold: Option<(String, Arc<tokio::sync::Notify>)>,
    }

    impl ScriptedSource {
        fn new(script: &[(&str, char)]) -> Self {
            Self {
                script: script.iter().map(|(tag, leaf)| ((*tag).to_string(), *leaf)).collect(),
                leaves: std::collections::BTreeSet::new(),
                fail: std::collections::BTreeSet::new(),
                repository: format!("oci://{REGISTRY}/{REPO}"),
                published: true,
                fetched: Arc::new(std::sync::Mutex::new(Vec::new())),
                completed: Arc::new(std::sync::Mutex::new(Vec::new())),
                hold: None,
            }
        }
        fn failing(mut self, tags: &[&str]) -> Self {
            self.fail = tags.iter().map(|tag| (*tag).to_string()).collect();
            self
        }
        fn with_leaves(mut self, tags: &[&str]) -> Self {
            self.leaves = tags.iter().map(|tag| (*tag).to_string()).collect();
            self
        }
        fn at_repository(mut self, repository: &str) -> Self {
            self.repository = repository.to_string();
            self
        }
        fn derived(mut self) -> Self {
            self.published = false;
            self
        }
        /// `tag` answers only after every other tag has answered.
        fn holding(mut self, tag: &str) -> Self {
            self.hold = Some((tag.to_string(), Arc::new(tokio::sync::Notify::new())));
            self
        }
        /// Drain the recorded fetches, so a second refresh is measured on its
        /// own. SORTED: `buffer_unordered` gives no ordering guarantee, and
        /// every assertion here is about WHICH objects were fetched, never in
        /// what order — the one test that is about order reads `completed`.
        fn take_fetched(&self) -> Vec<String> {
            let mut fetched = std::mem::take(&mut *self.fetched.lock().unwrap());
            fetched.sort();
            fetched
        }
        fn completed(&self) -> Vec<String> {
            self.completed.lock().unwrap().clone()
        }
        fn index(&self) -> super::super::Index {
            super::super::Index::from_impl(self.clone())
        }
    }

    #[async_trait]
    impl super::super::index_impl::IndexImpl for ScriptedSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(self.script.keys().cloned().collect()))
        }
        async fn fetch_manifest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, Manifest)>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, d, m)| (d, m)))
        }
        async fn fetch_manifest_digest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, d, _)| d))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            id: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, Manifest)>> {
            let tag = id
                .tag()
                .expect("the scripted source is only ever asked by tag")
                .to_string();
            self.fetched.lock().unwrap().push(tag.clone());
            if let Some((held, notify)) = &self.hold
                && *held == tag
            {
                // `notify_one` stores a permit when nobody is waiting yet, so
                // this cannot lose the wake-up to a scheduling race.
                notify.notified().await;
            }
            let outcome = if self.fail.contains(&tag) {
                // A TRANSPORT failure (ExitCode::Unavailable, 69), not a
                // not-found: `refresh_derived`'s `NoIndexableTag` fallback is
                // also 79, so a not-found fixture could not tell a surfaced
                // transport error from the fallback that replaced it. The tag
                // rides in the URL so the message still names which one failed.
                Err(super::super::error::Error::IndexHttpFailed {
                    url: format!("https://scripted.example/{tag}"),
                    // A connection-level failure carries no HTTP status — the
                    // shape a proxy reset produces, which is what this fixture
                    // stands in for.
                    status: None,
                    source: "scripted transport failure".into(),
                })
            } else if self.leaves.contains(&tag) {
                let (bytes, digest) = image_manifest_bytes();
                Ok(Some((bytes.clone(), digest, serde_json::from_slice(&bytes).unwrap())))
            } else {
                match self.script.get(&tag) {
                    Some(leaf) => {
                        let (bytes, digest) = object_for(*leaf);
                        Ok(Some((bytes.clone(), digest, serde_json::from_slice(&bytes).unwrap())))
                    }
                    None => Ok(None),
                }
            };
            self.completed.lock().unwrap().push(tag.clone());
            if let Some((held, notify)) = &self.hold
                && *held != tag
            {
                notify.notify_one();
            }
            outcome
        }
        async fn fetch_root_document(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, super::super::wire::IndexRoot)>> {
            if !self.published {
                return Ok(None);
            }
            let tags = self
                .script
                .iter()
                .map(|(tag, leaf)| {
                    let content = if self.leaves.contains(tag) {
                        image_manifest_bytes().1
                    } else {
                        content_for(*leaf)
                    };
                    format!(r#""{tag}":{{"content":"{content}"}}"#)
                })
                .collect::<Vec<_>>()
                .join(",");
            let bytes = format!(r#"{{"repository":"{}","tags":{{{tags}}}}}"#, self.repository).into_bytes();
            let root = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, root)))
        }
        fn source_kind(&self) -> SourceKind {
            if self.published {
                SourceKind::Published
            } else {
                SourceKind::Derived
            }
        }
        fn box_clone(&self) -> Box<dyn super::super::index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// C-009 — an unchanged published re-sync issues zero dispatch-object fetches.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unchanged_published_resync_fetches_no_dispatch_object() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = ScriptedSource::new(&[("1.0", 'a'), ("2.0", 'b')]);

        index.refresh_tags(&repo_id(), &source.index()).await.unwrap();
        assert_eq!(
            source.take_fetched().len(),
            2,
            "prerequisite: the cold run really does fetch both dispatch objects"
        );

        index.refresh_tags(&repo_id(), &source.index()).await.unwrap();

        assert_eq!(
            source.take_fetched(),
            Vec::<String>::new(),
            "every object the root names is on disk and hash-verifies, so the warm re-sync \
             must issue no dispatch fetch at all"
        );
        // The skip is a skip of the FETCH, never of the pin: both tags stay
        // pinned at exactly what the root names.
        let root = read_root_value(&dir);
        assert_eq!(
            root["tags"]["1.0"]["content"].as_str(),
            Some(content_for('a').to_string().as_str())
        );
        assert_eq!(
            root["tags"]["2.0"]["content"].as_str(),
            Some(content_for('b').to_string().as_str())
        );
    }

    /// C-009 edge (a) — one tag repointed at a new digest costs exactly one
    /// dispatch GET, for that digest.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_repointed_tag_is_the_only_one_refetched() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);

        index
            .refresh_tags(&repo_id(), &ScriptedSource::new(&[("1.0", 'a'), ("2.0", 'b')]).index())
            .await
            .unwrap();

        // The publisher moved `2.0` onto a new image index; `1.0` is untouched.
        let moved = ScriptedSource::new(&[("1.0", 'a'), ("2.0", 'c')]);
        index.refresh_tags(&repo_id(), &moved.index()).await.unwrap();

        assert_eq!(
            moved.take_fetched(),
            vec!["2.0".to_string()],
            "only the tag whose content digest changed may be fetched"
        );
        assert_eq!(
            read_root_value(&dir)["tags"]["2.0"]["content"].as_str(),
            Some(content_for('c').to_string().as_str()),
            "and the moved pin is adopted"
        );
    }

    /// C-009 edge (b) — a single-platform (leaf) tag has no `o/` object by
    /// design, so it is fetched every run. That is correct, not a gate miss.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_leaf_tag_is_refetched_every_run_because_it_has_no_object() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = ScriptedSource::new(&[("1.0", 'a')]).with_leaves(&["1.0"]);

        index.refresh_tags(&repo_id(), &source.index()).await.unwrap();
        assert_eq!(source.take_fetched(), vec!["1.0".to_string()]);

        index.refresh_tags(&repo_id(), &source.index()).await.unwrap();
        assert_eq!(
            source.take_fetched(),
            vec!["1.0".to_string()],
            "a leaf tag writes nothing to o/, so the gate must keep fetching it — reading \
             its absence as 'skip' would stop refreshing single-platform packages entirely"
        );
    }

    /// C-010 — a corrupt dispatch object is refetched and repaired, never
    /// skipped and never fatal.
    ///
    /// The gate's `Err(DigestMismatch) ⇒ fetch` arm is the ONLY thing that
    /// repairs a tampered object: `resolve_dispatch` propagates the mismatch
    /// with `?`, `ocx index regenerate` never touches `o/` bytes, and
    /// `ocx package verify` is Sigstore signature verification. Collapse this
    /// arm into "skip" and the corruption is stranded forever.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_tampered_dispatch_object_is_refetched_and_repaired() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = ScriptedSource::new(&[("1.0", 'a')]);
        let (verbatim, content) = object_for('a');

        // Seed the object's own path with bytes that do NOT hash to `content` —
        // a tampered file, indistinguishable from a valid one by `exists()`.
        let path = store(&dir).dispatch_object_path(REGISTRY, REPO, &content);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"schemaVersion":2,"manifests":[]}"#).unwrap();
        assert_ne!(
            Algorithm::Sha256.hash(std::fs::read(&path).unwrap()),
            content,
            "prerequisite: the seeded object really is corrupt"
        );

        index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect("a corrupt local object is a self-heal, never a failure the operator sees");

        assert_eq!(
            source.take_fetched(),
            vec!["1.0".to_string()],
            "the corrupt object must be refetched — a skip here strands it forever"
        );
        let repaired = std::fs::read(&path).unwrap();
        assert_eq!(
            repaired, verbatim,
            "the fetched bytes overwrite the tampered file verbatim"
        );
        assert_eq!(
            Algorithm::Sha256.hash(&repaired),
            content,
            "and the file on disk now hashes to the digest the root pins"
        );
    }

    /// C-027(a) — `persist_dispatch` still returns the fetched
    /// `(bytes, digest, manifest)` for an object the store already holds.
    ///
    /// This is the test the gate's PLACEMENT rests on. `persist_dispatch` is
    /// also the resolve path's fetch (`ChainedIndex::fetch_and_persist_chain`),
    /// it is handed a TAGGED identifier so it cannot know the digest without the
    /// fetch it would skip, and all three tuple members are consumed by that
    /// caller. Move the gate inside it and this goes red.
    #[tokio::test(flavor = "multi_thread")]
    async fn persist_dispatch_still_returns_the_manifest_for_an_object_already_held() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let fetches = Arc::new(AtomicUsize::new(0));
        let source = super::super::Index::from_impl(CountingDispatchSource {
            fetches: fetches.clone(),
        });
        let (verbatim, content) = two_platform_index();
        store(&dir)
            .write_dispatch_object(REGISTRY, REPO, &content, &verbatim)
            .await
            .unwrap();

        let (bytes, digest, manifest) = index
            .persist_dispatch(&source, &tagged_id("3.28"))
            .await
            .unwrap()
            .expect("persist_dispatch must still answer with the manifest, held object or not");

        assert_eq!(digest, content);
        assert_eq!(bytes, verbatim, "the resolve path consumes these bytes");
        assert!(
            matches!(manifest, Manifest::ImageIndex(_)),
            "and it consumes the decoded manifest"
        );
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "the resolve path's fetch is NOT gated: gating it would have to return None or \
             re-read and re-parse the object, on the hottest path in the binary"
        );
    }

    /// `RootScope::Routing` adopts the package-level fields on first sight and
    /// refuses to touch a package this machine has already committed.
    ///
    /// Asserted on `merge_root` directly, because the refusal arm is defence in
    /// depth for the scope's contract rather than a state the one production
    /// caller can reach: `ChainedIndex::physical_reference` answers from the
    /// committed root before it ever asks a source, so it calls this only when
    /// there is nothing committed. The contract is what a future caller would
    /// rely on, so it is the contract that is pinned here.
    #[test]
    fn routing_scope_adopts_on_first_sight_and_never_migrates() {
        let fetched = format!(
            r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"1.0":{{"content":"{}"}}}}}}"#,
            content_for('1')
        );

        let first_sight = merge_root(None, fetched.as_bytes(), RootScope::Routing)
            .expect("first sight must write: there is no committed pointer to preserve");
        let written: serde_json::Value = serde_json::from_slice(&first_sight).unwrap();
        assert_eq!(
            written["repository"].as_str(),
            Some(format!("oci://{REGISTRY}/{REPO}").as_str()),
            "the routing pointer is the whole point of the scope"
        );
        assert_eq!(
            written["tags"].as_object().map(serde_json::Map::len),
            Some(0),
            "a digest resolve names no tag, so it may adopt none — writing one \
             would move a pin the user never asked for"
        );

        let committed = format!(r#"{{"repository":"oci://elsewhere.example/{REPO}","tags":{{}}}}"#);
        assert!(
            merge_root(Some(committed.as_bytes()), fetched.as_bytes(), RootScope::Routing).is_none(),
            "a committed root is left exactly as committed: replacing its \
             repository is a routing migration, which is `ocx index update`'s"
        );
    }

    /// C-012 — a failing tag does not discard its package's succeeded tags.
    ///
    /// Covers edge (c) with them: a tag only the LOCAL copy holds must survive
    /// the partial commit too. `merge_root` only ever inserts, so it holds by
    /// construction — but D-008b authored the scope arm that decides what the
    /// partial commit writes, and "merge never deletes" is exactly the property
    /// a later rewrite of that arm would take away silently.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_tag_does_not_discard_its_packages_succeeded_tags() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // A tag this machine snapshotted and the source no longer lists.
        index
            .seed_root_document(
                &repo_id(),
                format!(
                    r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"retired":{{"content":"{}"}}}}}}"#,
                    content_for('9')
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let source = ScriptedSource::new(&[("a", '1'), ("b", '2'), ("c", '3')]).failing(&["b"]);

        let error = index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("the failing tag must still be reported");
        assert!(
            ocx_util::error::render_chain(&error).contains("scripted.example/b"),
            "the reported failure is the failing tag's, got: {}",
            ocx_util::error::render_chain(&error)
        );

        let root = read_root_value(&dir);
        assert_eq!(
            root["tags"]["a"]["content"].as_str(),
            Some(content_for('1').to_string().as_str()),
            "a's completed work survives its sibling's failure"
        );
        assert_eq!(
            root["tags"]["c"]["content"].as_str(),
            Some(content_for('3').to_string().as_str()),
            "and so does c's"
        );
        assert!(
            root["tags"].get("b").is_none(),
            "but b, whose dispatch object was never written, must NOT be pinned — that is \
             exactly the tag-without-an-object state the index design abolished"
        );
        assert_eq!(
            root["tags"]["retired"]["content"].as_str(),
            Some(content_for('9').to_string().as_str()),
            "C-012(c): a tag only the local copy holds survives the partial commit — the copy \
             records what this machine snapshotted, so a publisher retiring a version cannot \
             unpin a machine that took it"
        );
    }

    /// C-012 edge (a) — when every tag fails, nothing is committed.
    #[tokio::test(flavor = "multi_thread")]
    async fn when_every_tag_fails_nothing_is_committed() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = ScriptedSource::new(&[("a", '1'), ("b", '2')]).failing(&["a", "b"]);

        index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("every tag failed, so the refresh failed");

        assert!(
            !store(&dir).root_document_path(REGISTRY, REPO).exists(),
            "no tag survived, so there is nothing to commit — not even an empty root carrying \
             the fetched package-level fields"
        );
    }

    /// C-012 edge (b) — the returned failure is the LOWEST-INPUT-INDEX one, not
    /// whichever completed first. `buffer_unordered` finishes out of order, so
    /// without the input index the reported error would depend on scheduling.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_returned_failure_is_deterministic_not_the_first_to_complete() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // `a` sorts first (BTreeMap ⇒ input index 0) but is held until `b` has
        // already failed, so completion order is the reverse of input order.
        let source = ScriptedSource::new(&[("a", '1'), ("b", '2')])
            .failing(&["a", "b"])
            .holding("a");

        let error = index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("both tags failed");

        assert_eq!(
            source.completed(),
            vec!["b".to_string(), "a".to_string()],
            "non-vacuity: this test is only about determinism if b really did finish first"
        );
        assert!(
            ocx_util::error::render_chain(&error).contains("scripted.example/a"),
            "the lowest-index failure is returned, not the first to complete; got: {}",
            ocx_util::error::render_chain(&error)
        );
    }

    /// C-013 — tags aliasing a failed digest are excluded with it.
    ///
    /// `refresh_published` dedups by content digest, so only one of the aliased
    /// tags is ever fetched. The other must not free-ride on a fetch that never
    /// happened.
    #[tokio::test(flavor = "multi_thread")]
    async fn tags_aliasing_a_failed_digest_are_excluded_with_it() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // `x` and `y` both point at leaf '1'; `x` sorts first, so it is the
        // representative the dedup keeps — and the one that fails.
        let source = ScriptedSource::new(&[("x", '1'), ("y", '1'), ("z", '2')]).failing(&["x"]);

        index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("the representative tag failed");

        assert_eq!(
            source.take_fetched(),
            vec!["x".to_string(), "z".to_string()],
            "prerequisite: y is deduped away, so nothing ever fetched its object"
        );
        let root = read_root_value(&dir);
        assert!(
            root["tags"].get("x").is_none(),
            "the failing representative is not pinned"
        );
        assert!(
            root["tags"].get("y").is_none(),
            "and neither is the tag aliased onto its digest — the failure unit is the digest"
        );
        assert_eq!(
            root["tags"]["z"]["content"].as_str(),
            Some(content_for('2').to_string().as_str()),
            "the unrelated tag is still adopted"
        );
    }

    /// C-014 — a partial commit does not adopt package-level fields.
    ///
    /// `repository` is the OTHER half of what the local copy pins (logical →
    /// physical routing). Taking a routing migration while some of the package's
    /// tags were not refreshed would leave the unrefreshed pins routing through
    /// a location they were never observed at.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_partial_commit_does_not_adopt_package_level_fields() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        index
            .seed_root_document(&repo_id(), br#"{"repository":"oci://old.example/ns/pkg","tags":{}}"#)
            .await
            .unwrap();

        let source = ScriptedSource::new(&[("a", '1'), ("b", '2')])
            .at_repository("oci://new.example/ns/pkg")
            .failing(&["b"]);
        index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("one tag failed");

        let root = read_root_value(&dir);
        assert_eq!(
            root["repository"].as_str(),
            Some("oci://old.example/ns/pkg"),
            "a partial refresh must not take the routing migration"
        );
        assert_eq!(
            root["tags"]["a"]["content"].as_str(),
            Some(content_for('1').to_string().as_str()),
            "prerequisite: the partial commit really did happen"
        );
    }

    /// C-014 complement — with no failing tag the fetched `repository` IS
    /// adopted. Without this half the test cannot tell "conservative" from
    /// "broken".
    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_commit_still_adopts_package_level_fields() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        index
            .seed_root_document(&repo_id(), br#"{"repository":"oci://old.example/ns/pkg","tags":{}}"#)
            .await
            .unwrap();

        let source = ScriptedSource::new(&[("a", '1'), ("b", '2')]).at_repository("oci://new.example/ns/pkg");
        index.refresh_tags(&repo_id(), &source.index()).await.unwrap();

        assert_eq!(
            read_root_value(&dir)["repository"].as_str(),
            Some("oci://new.example/ns/pkg"),
            "a bare identifier that fully succeeded is the sanctioned point to take a routing \
             migration — RootScope::Package, unchanged"
        );
    }

    /// C-012, derived provenance — the other `try_collect` site. A derived root
    /// is authored per tag, so there is no package-level field to withhold; the
    /// contract is just that a sibling's failure keeps its own scope.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_derived_tag_does_not_discard_its_packages_succeeded_tags() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = ScriptedSource::new(&[("a", '1'), ("b", '2'), ("c", '3')])
            .derived()
            .failing(&["b"]);

        let _error = index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("the failing tag must still be reported");

        let root = read_root_value(&dir);
        assert_eq!(
            root["tags"]["a"]["content"].as_str(),
            Some(content_for('1').to_string().as_str())
        );
        assert_eq!(
            root["tags"]["c"]["content"].as_str(),
            Some(content_for('3').to_string().as_str())
        );
        assert!(root["tags"].get("b").is_none(), "the failing tag is not authored");
    }

    /// Seed a committed root pinning `tag` at `leaf`'s dispatch digest, and put
    /// that dispatch object on disk — the "this machine already snapshotted
    /// this version" starting state the sweep fixtures below move a pin away
    /// from. Returns the digest it pinned, so the test can assert on the file.
    async fn seed_pinned_tag(index: &LocalIndex, tag: &str, leaf: char) -> ocx_oci::Digest {
        let (bytes, digest) = object_for(leaf);
        index
            .seed_root_document(
                &repo_id(),
                format!(r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"{tag}":{{"content":"{digest}"}}}}}}"#)
                    .as_bytes(),
            )
            .await
            .unwrap();
        index.stage_dispatch_bytes(&repo_id(), &digest, &bytes).await.unwrap();
        digest
    }

    /// D-7 under partial success, published — the object a SIBLING's moved pin
    /// abandoned is still swept.
    ///
    /// The failing tag is a red herring by construction: its own dispatch
    /// object was never written, so it is in neither side of the
    /// `previous \ current` diff. What the partial commit abandons belongs to
    /// `x`, whose pin it moved D1 → D2 — and `RootPins.previous` is re-read
    /// from the committed root by every later run, which now says D2. Skipping
    /// the sweep on this branch therefore strands D1 forever, not until the
    /// next update.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_partial_published_commit_sweeps_the_object_a_siblings_moved_pin_abandoned() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let abandoned = seed_pinned_tag(&index, "x", '1').await;
        assert!(
            store(&dir).dispatch_object_path(REGISTRY, REPO, &abandoned).exists(),
            "prerequisite: the pin this refresh moves off really does have an object on disk"
        );

        // `x` moves to leaf '2'; `y` never persists, so the commit is partial.
        let source = ScriptedSource::new(&[("x", '2'), ("y", '3')]).failing(&["y"]);
        let error = index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("the failing tag must still be reported");
        assert!(
            ocx_util::error::render_chain(&error).contains("scripted.example/y"),
            "the sweep must not swallow the withheld tag failure; got: {}",
            ocx_util::error::render_chain(&error)
        );

        let adopted = content_for('2');
        assert_eq!(
            read_root_value(&dir)["tags"]["x"]["content"].as_str(),
            Some(adopted.to_string().as_str()),
            "prerequisite: the partial commit really did move x's pin"
        );
        assert!(
            store(&dir).dispatch_object_path(REGISTRY, REPO, &adopted).exists(),
            "the object the surviving pin names is kept — the sweep is a diff, not a walk"
        );
        assert!(
            !store(&dir).dispatch_object_path(REGISTRY, REPO, &abandoned).exists(),
            "no tag of this package pins the old object any more, so it must be gone"
        );
    }

    /// D-7 under partial success, derived — the same contract on the other
    /// commit site. Separate fixture, not a parametrisation: reverting one
    /// branch's sweep leaves the other green, so one test cannot pin both.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_partial_derived_commit_sweeps_the_object_a_siblings_moved_pin_abandoned() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let abandoned = seed_pinned_tag(&index, "x", '1').await;

        let source = ScriptedSource::new(&[("x", '2'), ("y", '3')]).derived().failing(&["y"]);
        let error = index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("the failing tag must still be reported");
        assert!(
            ocx_util::error::render_chain(&error).contains("scripted.example/y"),
            "the sweep must not swallow the withheld tag failure; got: {}",
            ocx_util::error::render_chain(&error)
        );

        let adopted = content_for('2');
        assert_eq!(
            read_root_value(&dir)["tags"]["x"]["content"].as_str(),
            Some(adopted.to_string().as_str()),
            "prerequisite: the partial commit really did move x's pin"
        );
        assert!(
            store(&dir).dispatch_object_path(REGISTRY, REPO, &adopted).exists(),
            "the object the surviving pin names is kept"
        );
        assert!(
            !store(&dir).dispatch_object_path(REGISTRY, REPO, &abandoned).exists(),
            "no tag of this package pins the old object any more, so it must be gone"
        );
    }

    /// The derived half of C-012, and the one that is about the EXIT CODE.
    ///
    /// `refresh_derived` falls back to `NoIndexableTag` (79, "package absent")
    /// when nothing was fetched. `try_collect` used to propagate a transport
    /// failure before that gate could run; `collect` does not, so the failure
    /// has to be surfaced explicitly ahead of it. Get the order wrong and a
    /// package whose tag manifests all 503 transiently reports 79 instead of
    /// 69, and the transport error never reaches the operator at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_derived_refresh_whose_every_tag_fails_keeps_its_transport_exit_code() {
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        let source = ScriptedSource::new(&[("a", '1'), ("b", '2')])
            .derived()
            .failing(&["a", "b"]);

        let error = index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("every tag failed, so the refresh failed");

        let rendered = ocx_util::error::render_chain(&error);
        assert!(
            !rendered.contains("no indexable tag"),
            "and the fallback must not have replaced the real cause; got: {rendered}"
        );
        assert!(
            !store(&dir).root_document_path(REGISTRY, REPO).exists(),
            "no tag survived, so nothing is authored"
        );
    }

    /// D-008 double fault, PUBLISHED — the partial commit fails too.
    ///
    /// The commit is what propagates (it is the local, durable fault, and it
    /// means nothing was written at all), so the withheld tag failure survives
    /// only as a log line. Written with `?` the commit's error propagated and
    /// the withheld one was dropped entirely: an operator saw a data error and
    /// was never told which tag failed, or that the refresh had been partial.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_partial_commit_that_fails_still_reports_the_withheld_tag() {
        install_warning_capture();
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // `gone` fails in transport, so `keep` alone goes to a partial commit —
        // and that commit fails, because a first-sight package merges onto the
        // fetched document and this one's routing pointer has no `oci://`.
        let source = ScriptedSource::new(&[("gone", '2'), ("keep", '1')])
            .at_repository("host.example/ns/pkg")
            .failing(&["gone"]);

        let error = index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("the tag fetch failed AND the commit of the survivor failed");

        assert_eq!(
            source.take_fetched(),
            vec!["gone".to_string(), "keep".to_string()],
            "non-vacuity: both tags were attempted, so there really was a survivor to commit"
        );
        assert!(
            !store(&dir).root_document_path(REGISTRY, REPO).exists(),
            "non-vacuity: the partial commit really did fail — nothing was written"
        );

        let rendered = ocx_util::error::render_chain(&error);
        assert!(
            rendered.contains("host.example/ns/pkg"),
            "the commit failure propagates: it is local and durable, and it means the partial \
             success the other error frames itself against did not happen; got: {rendered}"
        );
        assert!(
            warned_about("scripted.example/gone"),
            "and the withheld tag failure must still reach the operator — naming which tag did \
             not refresh is the entire point of the partial-commit branch"
        );
    }

    /// D-008 double fault, DERIVED — the same, at the other commit site.
    ///
    /// A derived root is authored, so its commit fails on the F1 cross-check
    /// instead of the physical-ref parse; the contract is identical.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_derived_partial_commit_that_fails_still_reports_the_withheld_tag() {
        install_warning_capture();
        let dir = TempDir::new().unwrap();
        let index = make_index(&dir);
        // An authored root naming a different physical location: committing over
        // it is corruption, so `commit_root_tags` refuses.
        index
            .seed_root_document(&repo_id(), br#"{"repository":"oci://other.example/ns/pkg","tags":{}}"#)
            .await
            .unwrap();

        let source = ScriptedSource::new(&[("kept", '3'), ("lost", '4')])
            .derived()
            .failing(&["lost"]);

        let error = index
            .refresh_tags(&repo_id(), &source.index())
            .await
            .expect_err("the tag fetch failed AND the commit of the survivor failed");

        assert_eq!(
            source.take_fetched(),
            vec!["kept".to_string(), "lost".to_string()],
            "non-vacuity: both tags were attempted, so there really was a survivor to commit"
        );

        let rendered = ocx_util::error::render_chain(&error);
        assert!(
            rendered.contains("oci://other.example/ns/pkg"),
            "the commit failure propagates, same as the published half; got: {rendered}"
        );
        assert_eq!(
            read_root_value(&dir)["tags"].as_object().map(|tags| tags.len()),
            Some(0),
            "non-vacuity: the commit really did refuse — the survivor was not authored"
        );
        assert!(
            warned_about("scripted.example/lost"),
            "and the withheld tag failure must still reach the operator"
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
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(self.tags.clone()))
        }
        async fn fetch_manifest(
            &self,
            _: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, Manifest)>> {
            let (bytes, digest) = index_bytes();
            Ok(Some((digest, serde_json::from_slice(&bytes).unwrap())))
        }
        async fn fetch_manifest_digest(
            &self,
            _: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            let (_, digest) = index_bytes();
            Ok(Some(digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, Manifest)>> {
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
    async fn seed_then_tamper_object(dir: &TempDir) -> ocx_oci::Digest {
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
                Err(crate::error::Error::Store(
                    ocx_store::file_structure::error::Error::DigestMismatch { .. }
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
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            _: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(
            &self,
            _: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            _: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, Manifest)>> {
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
            matches!(bare, Err(super::super::error::Error::RemoteManifestNotFound(_))),
            "bare nonexistent package must report not-found, got {bare:?}"
        );

        // Tagged identifier: the tag exists in the request but the source serves
        // no manifest for it — nothing persists, so it must not silently succeed.
        let tagged = index.refresh_tags(&tagged_id("9.9"), &empty).await;
        assert!(
            matches!(tagged, Err(super::super::error::Error::NoIndexableTag(_))),
            "tagged nonexistent package must report not-found, got {tagged:?}"
        );
    }

    // ── dispatch-object decode: one OCI parse, fail-closed (D1) ──────────────

    /// The verbatim image index a derived source serves for its tag: one
    /// descriptor naming the flat image manifest, plus the index's own digest.
    fn index_bytes() -> (Vec<u8>, ocx_oci::Digest) {
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
    /// serialisation of `ocx_oci::ImageIndex` can emit `schemaVersion: 1`.
    #[test]
    fn decode_index_manifest_refuses_an_index_with_the_wrong_schema_version() {
        let _error = decode_index_manifest(br#"{"schemaVersion":1,"manifests":[]}"#)
            .expect_err("an invalid image index must be refused, never reported as absent");
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
        // pretty-printed and carries a field `ocx_oci::ImageIndex` does not model,
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
    fn two_platform_index() -> (Vec<u8>, ocx_oci::Digest) {
        let leaf_a = ocx_oci::Digest::Sha256("a".repeat(64));
        let leaf_b = ocx_oci::Digest::Sha256("b".repeat(64));
        image_index_bytes(&[("amd64", &leaf_a), ("arm64", &leaf_b)])
    }

    /// Root-document bytes (wire grammar) that point tag `3.28` at `content` and
    /// carry an `oci://<REGISTRY>/<REPO>` physical pointer (passes the C3
    /// `parse_repository_pointer` cross-check).
    fn root_bytes_for(content: &ocx_oci::Digest) -> Vec<u8> {
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
        async fn list_tags(&self, _: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["3.28".to_string()]))
        }
        async fn fetch_manifest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<(ocx_oci::Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(id)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(
            &self,
            id: &ocx_oci::PackageRef,
            _: IndexOperation,
        ) -> Result<Option<ocx_oci::Digest>> {
            Ok(self.fetch_manifest_raw_bytes(id).await?.map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            id: &ocx_oci::PackageRef,
        ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, Manifest)>> {
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
        let content = ocx_oci::Digest::Sha256("a".repeat(64));
        for tag in [
            "3.28",
            "latest",
            "__ocx.desc",
            "__OCX.future",
            &format!("__ocx.keep.sha256-{}", "a".repeat(64)),
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
            matches!(result, Err(super::super::error::Error::NoIndexableTag(_))),
            "a bare-manifest-only refresh records nothing, got {result:?}"
        );
        assert!(
            root_tag_names(&dir).is_empty(),
            "no root tag entry may be committed for a bare-manifest tag"
        );
    }

    /// N-16 — the UPDATE path, reserved-tag half. The tag resolves to a genuine
    /// IMAGE INDEX, so the kind gate above passes and only the reserved verdict
    /// can exclude it. An `__ocx*` name — the keep tag included — is not a
    /// version, and neither is the frozen legacy `sha256.<hex>` form.
    #[tokio::test(flavor = "multi_thread")]
    async fn derived_refresh_skips_a_reserved_tag() {
        for reserved in [
            "__ocxfoo",
            "__OCX.future",
            &format!("__ocx.keep.sha256-{}", "a".repeat(64)),
            &format!("sha256.{}", "a".repeat(64)),
        ] {
            let dir = TempDir::new().unwrap();
            let index = make_index(&dir);
            let source = source_for_tag(reserved);

            let result = index.refresh_tags(&tagged_id(reserved), &source).await;
            assert!(
                matches!(result, Err(super::super::error::Error::NoIndexableTag(_))),
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
                .dispatch_object_path(REGISTRY, REPO, &ocx_oci::Digest::Sha256("0".repeat(64)))
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
        for reserved in [
            "__ocxfoo",
            "__OCX.future",
            &format!("__ocx.keep.sha256-{}", "a".repeat(64)),
            &format!("sha256.{}", "a".repeat(64)),
        ] {
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
        let content = ocx_oci::Digest::Sha256("a".repeat(64));

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
            .commit_root_tag(&tagged_id("3.28"), &ocx_oci::Digest::Sha256("a".repeat(64)))
            .await
            .unwrap();
        index
            .commit_root_tag(&tagged_id("3.27"), &ocx_oci::Digest::Sha256("b".repeat(64)))
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
            .commit_root_tag(&tagged_id("3.28"), &ocx_oci::Digest::Sha256("a".repeat(64)))
            .await;
        let _err = result.expect_err("a repository-mismatched existing root must be rejected, never overwritten");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_root_tag_recovery_starts_fresh_and_drops_every_prior_tag_on_malformed_content_digest() {
        // Locks the accepted data-loss-on-corruption behavior
        // (`adr_index_indirection.md` amendment 2026-07-19): `DerivedTag::content`
        // is an `ocx_oci::Digest`, whose exact-wire deserialize fails the whole
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
            .commit_root_tag(&tagged_id("3.28"), &ocx_oci::Digest::Sha256("c".repeat(64)))
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
    async fn seed_root_and_dispatch(dir: &TempDir) -> ocx_oci::Digest {
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
    fn yanked_root_bytes(content: &ocx_oci::Digest) -> Vec<u8> {
        format!(
            r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"3.28":{{"content":"{content}","observed":"2026-07-18T09:00:00Z","yanked":{{"reason":"critical security issue","at":"2026-02-01T00:00:00Z"}}}}}}}}"#
        )
        .into_bytes()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn resolve_dispatch_refuses_yanked_tag_offline_unless_allowed() {
        let dir = TempDir::new().unwrap();
        let content = ocx_oci::Digest::Sha256("a".repeat(64));

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
            matches!(err, super::super::error::Error::YankedRefused { .. }),
            "expected YankedRefused, got {err:?}"
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
        let wrong = ocx_oci::Digest::Sha256("a".repeat(64));
        let bytes = b"these bytes do not hash to the claimed digest";

        let result = index.stage_dispatch_bytes(&repo_id(), &wrong, bytes).await;
        assert!(
            matches!(
                result,
                Err(crate::error::Error::Store(
                    ocx_store::file_structure::error::Error::DigestMismatch { .. }
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

        // Tag AND digest on the input: the physical value carries both — the
        // digest content-addresses the read, the tag is what a read by tag
        // needs — in the exact shape `OcxIndex::physical_identifier` mints, so
        // a local answer and a source answer can never disagree.
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
            Some("3.28"),
            "the physical reference is addressed at the logical version, tag included"
        );
        assert_ne!(
            physical.registry(),
            REGISTRY,
            "reporting the LOGICAL registry as its own transport is the defect"
        );
    }

    #[tokio::test]
    async fn physical_reference_carries_only_the_tag_when_the_logical_reference_has_no_digest() {
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
        assert_eq!(physical.tag(), Some("3.28"));
        assert_eq!(physical.to_string(), "ghcr.io/ocx-contrib/cmake:3.28");
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
        assert_eq!(
            physical,
            ocx_oci::OciIdentifier::passthrough(&logical),
            "a derived rewrite must be the identity"
        );
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
            matches!(error, super::super::error::Error::UnsupportedIndexFormat { version: 2 }),
            "expected UnsupportedIndexFormat{{2}}, got {error:?}"
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
            .commit_published_root(
                &tagged_id("3.28"),
                &root_bytes_for(&content),
                RootScope::Tags(&["3.28"]),
            )
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
            .commit_published_root(&tagged_id("3.28"), &bytes, RootScope::Tags(&["3.28"]))
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
            .commit_published_root(&tagged_id("3.28"), &bytes, RootScope::Tags(&["3.28"]))
            .await
            .unwrap();
        let config_path = store(&dir).source_config_path(REGISTRY);
        let first = std::fs::read(&config_path).unwrap();
        let stamped = std::fs::metadata(&config_path).unwrap().modified().unwrap();

        index
            .commit_published_root(&tagged_id("3.28"), &bytes, RootScope::Tags(&["3.28"]))
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
            .commit_published_root(
                &tagged_id("3.28"),
                &root_bytes_for(&content),
                RootScope::Tags(&["3.28"]),
            )
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
    fn nfs_kind(error: &crate::error::Error) -> std::io::ErrorKind {
        match error {
            crate::error::Error::File(file) => file.cause.kind(),
            other => panic!("expected File, got {other:?}"),
        }
    }
}
