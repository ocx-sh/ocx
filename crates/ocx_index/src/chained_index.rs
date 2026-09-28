// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::error::Result;
use super::index_impl::IndexImpl;
use super::local_index::{DispatchResolution, RootScope, SourceKind};
use super::{ChainMode, Index, IndexOperation, Jurisdiction, LocalIndex, index_impl};
use ocx_store::file_structure::BlobStore;

use ocx_util::singleflight;

/// Whether `err` reports a present-but-corrupt dispatch object: bytes that fail their digest
/// (`adr_index_indirection.md § The object store holds dispatch objects only`) or are not an OCI
/// image index. An absent object reads as `Ok(None)` instead.
fn is_corrupt_index_object(err: &super::error::Error) -> bool {
    matches!(
        err,
        super::error::Error::Store(ocx_store::file_structure::error::Error::DigestMismatch { .. })
            | super::error::Error::InvalidImageIndex(_)
    )
}

/// Whether `err` is a local-read refusal (yank, unsupported format, unreadable `config.json`) the walk
/// must propagate: read as a miss, a source would serve the same name and bypass it
/// (`adr_servable_index_snapshot.md § The unrecognized-version path, both readers`).
///
/// Only `config.json` file errors match: a blanket file match would also catch a root read or lock
/// timeout, which the walk recovers from by re-fetching.
fn is_local_read_refusal(err: &super::error::Error) -> bool {
    match err {
        super::error::Error::YankedRefused { .. }
        | super::error::Error::UnsupportedIndexFormat { .. }
        | super::error::Error::MalformedIndexDocument { .. } => true,
        super::error::Error::File(file) => file.path.file_name() == Some(std::ffi::OsStr::new("config.json")),
        _ => false,
    }
}

/// Whether `err` is a source transport outage, which [`ChainedIndex::physical_reference`] holds while it
/// tries the local root; every other error propagates.
///
/// One variant, not a "not these" list, so a new error class fails closed.
/// Peeled via [`error::coalesced_cause`](super::error::coalesced_cause), or a coalesced
/// `SourceFetchFailed` hides the outage and a warm machine with the index site unreachable exits 69.
fn is_source_outage(err: &super::error::Error) -> bool {
    matches!(
        super::error::coalesced_cause(err),
        super::error::Error::IndexHttpFailed { .. }
    )
}

/// Whether a failed host lookup may be tolerated by [`ChainedIndex::guard_local_physical`]: only for a
/// genuine DNS name.
///
/// An address-shaped host stays fail-closed: `getaddrinfo` refuses a bracketed `[::1]` that a URL parser
/// accepts, so tolerating it would hand the pull a loopback target the guard never judged.
fn is_plain_dns_name(host: &str) -> bool {
    !host.starts_with('[') && host.parse::<std::net::IpAddr>().is_err()
}

/// Whether `bytes` hash to `digest`; [`BlobStore`] never self-verifies, so every read out of it must
/// check (CWE-345).
fn digest_matches(bytes: &[u8], digest: &ocx_oci::Digest) -> bool {
    digest.algorithm().hash(bytes) == *digest
}

/// A digest-addressed walk must come back with the digest it asked for, or a source could move a pin:
/// a source's bytes are only proven to match the digest that source computed.
fn verify_walked_digest(
    identifier: &ocx_oci::PackageRef,
    head: Option<(ocx_oci::Digest, ocx_oci::Manifest)>,
) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
    let (Some(requested), Some((answered, _))) = (identifier.digest(), head.as_ref()) else {
        return Ok(head);
    };
    if *answered != requested {
        return Err(super::error::Error::WalkedDigestMismatch {
            requested,
            answered: answered.clone(),
        });
    }
    Ok(head)
}

/// How much a resolve may write into the local index; blob-store writes are never gated.
///
/// `Full` persists the dispatch object and grows the tag pointer; `NoTag` skips the tag pointer, since
/// the caller's `ocx.lock` is canonical (`adr_toolchain_update_family.md`); `ReadOnly` writes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalWritePolicy {
    Full,
    NoTag,
    ReadOnly,
}

/// A local index plus ordered sources that `Resolve` callers walk, and persist from, on a miss;
/// `Query` callers never walk the chain.
///
/// `ChainMode::Remote` reads mutable lookups from the sources and propagates their failure rather than
/// falling back to the local index; `Offline` reads the local index only.
pub struct ChainedIndex {
    local_index: LocalIndex,
    sources: Vec<Index>,
    mode: ChainMode,
    /// Carries the manifest, not just the digest: a leaf manifest never lands in the local index, so a
    /// waiter cannot read it back after the walk.
    singleflight: singleflight::Group<String, Option<(ocx_oci::Digest, ocx_oci::Manifest)>>,
    write_policy: LocalWritePolicy,
    /// Machine-global blob store holding leaf manifests, consulted on `AbsentDispatch` before any source
    /// so an installed tool resolves offline; `None` only in test constructions.
    content_store: Option<BlobStore>,
    rules: Arc<ocx_oci::ssrf::ProxyRules>,
}

const SINGLEFLIGHT_MAX_KEYS: usize = 1024;

/// Matches the blob-store write timeout, so a stuck leader surfaces instead of stalling the CLI.
const SINGLEFLIGHT_TIMEOUT: Duration = Duration::from_secs(120);

impl ChainedIndex {
    pub fn new(local_index: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            local_index,
            sources,
            mode,
            singleflight: singleflight::Group::new(SINGLEFLIGHT_MAX_KEYS, SINGLEFLIGHT_TIMEOUT),
            write_policy: LocalWritePolicy::Full,
            content_store: None,
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Like [`Self::new`], but a tag resolve never commits a tag pointer: the caller's lock is the record.
    pub fn new_lock_scoped(local_index: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            write_policy: LocalWritePolicy::NoTag,
            ..Self::new(local_index, sources, mode)
        }
    }

    /// A clone that writes nothing into the local index ([`LocalWritePolicy::ReadOnly`]).
    ///
    /// Gets a fresh singleflight group: the key omits the write policy, so a shared group could coalesce
    /// a read-only resolve onto a persisting leader and apply its writes.
    fn read_only(&self) -> Self {
        Self {
            local_index: self.local_index.clone(),
            sources: self.sources.clone(),
            mode: self.mode,
            singleflight: singleflight::Group::new(SINGLEFLIGHT_MAX_KEYS, SINGLEFLIGHT_TIMEOUT),
            write_policy: LocalWritePolicy::ReadOnly,
            content_store: self.content_store.clone(),
            rules: self.rules.clone(),
        }
    }

    /// Attach the machine-global blob store an [`DispatchResolution::AbsentDispatch`] recovers from.
    pub fn with_content_store(mut self, content_store: BlobStore) -> Self {
        self.content_store = Some(content_store);
        self
    }

    /// Provenance of the configured source owning `registry`, or [`SourceKind::Derived`] when none does
    /// (e.g. `--offline`).
    ///
    /// Keyed on the registry, not the name: per-name jurisdiction would flip a name the index grammar
    /// cannot express to `Derived` and silently drop the published root's catalog cross-check.
    fn kind_for_registry(&self, registry: &str) -> SourceKind {
        self.sources
            .iter()
            .find(|source| source.serves_registry(registry))
            .map(Index::source_kind)
            .unwrap_or(SourceKind::Derived)
    }

    fn kind_for(&self, identifier: &ocx_oci::PackageRef) -> SourceKind {
        self.kind_for_registry(identifier.registry())
    }

    /// Sources not declaring `identifier` `Outside`, each paired with whether its miss or refusal stops
    /// the chain; a declining source is filtered out before any request is made to it.
    async fn candidate_sources(&self, identifier: &ocx_oci::PackageRef) -> Vec<(&Index, bool)> {
        let mut candidates = Vec::with_capacity(self.sources.len());
        for source in &self.sources {
            match source.jurisdiction(identifier) {
                Jurisdiction::Authoritative => candidates.push((source, true)),
                Jurisdiction::FallThrough => candidates.push((source, false)),
                Jurisdiction::Outside => {}
            }
        }
        candidates
    }

    /// SSRF floor for a physical target the local copy minted: the layer pull runs on the unguarded
    /// shared client, so without it a copied index tree is an unvalidated transport target.
    ///
    /// Call it after the local read: an early return ahead of it yields `Ok(None)`, which the caller
    /// turns into the logical identifier.
    ///
    /// # Errors
    ///
    /// [`Error::Ssrf`](super::error::Error::Ssrf) when the host resolves into a forbidden range without a
    /// `trusted_hosts` entry, or, on a direct dial, when its lookup failure is not tolerable.
    async fn guard_local_physical(
        &self,
        logical: &ocx_oci::PackageRef,
        physical: &ocx_oci::OciIdentifier,
    ) -> Result<()> {
        // Not a rewrite, so nothing to judge; guarding it would refuse every private registry whose
        // namespace has no source to carry a `trusted_hosts` exemption.
        if physical.registry() == logical.registry() {
            return Ok(());
        }
        // No `Offline` carve-out: the registry client is built in every mode, so an offline dial needs the floor too.
        let (host, port) = ocx_oci::ssrf::split_host_port(physical.registry());
        let scheme = ocx_oci::ssrf::DialScheme::for_registry(self.insecure_hosts(), physical.registry());
        match ocx_oci::ssrf::guard_destination(
            scheme,
            host,
            port,
            self.trusted_hosts_for(logical.registry()),
            &self.rules,
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(source @ ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. }) => Err(super::error::Error::Ssrf {
                source: ocx_oci::ssrf::PhysicalDialRefused {
                    namespace: logical.registry().to_string(),
                    source,
                },
            }),
            Err(source) if !is_plain_dns_name(host) => Err(super::error::Error::Ssrf {
                source: ocx_oci::ssrf::PhysicalDialRefused {
                    namespace: logical.registry().to_string(),
                    source,
                },
            }),
            // Tolerable only because `Index::guard_physical_dial` re-judges the host fail-closed before the
            // first request; this pre-flight runs on every resolve, so it must not fail on a missing resolver.
            Err(error) => {
                log::debug!(
                    "Physical host '{host}' for '{logical}' did not resolve, so the SSRF pre-flight could not \
                     judge it; proceeding — a connection cannot succeed where the lookup failed: {error}"
                );
                Ok(())
            }
        }
    }

    /// The committed local root's physical pointer for `identifier`, SSRF-guarded.
    ///
    /// A local read failure is a miss, so a broken index home cannot fail a resolve that would succeed;
    /// a guard refusal propagates, since asking a source next would make the guard a no-op.
    ///
    /// # Errors
    ///
    /// [`Error::Ssrf`](super::error::Error::Ssrf) from [`Self::guard_local_physical`].
    async fn local_physical_answer(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        match self
            .local_index
            .physical_reference(identifier, self.kind_for(identifier))
            .await
        {
            Ok(Some(physical)) => {
                self.guard_local_physical(identifier, &physical).await?;
                Ok(Some(physical))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                log::warn!("Local index physical reference read failed for '{identifier}': {e}");
                Ok(None)
            }
        }
    }

    /// Raises `PolicyResolutionBlocked` when the local index does not know the tag; local I/O and parse
    /// errors propagate unmasked. An [`DispatchResolution::AbsentDispatch`] still names a known digest,
    /// so it does not block.
    async fn ensure_locally_resolvable(&self, identifier: &ocx_oci::PackageRef) -> Result<()> {
        let kind = self.kind_for(identifier);
        let locally_resolvable = self.local_index.resolve_dispatch(identifier, kind).await?.is_some();
        if !locally_resolvable {
            return Err(super::error::Error::PolicyResolutionBlocked {
                identifier: identifier.to_string(),
                policy: self.mode.policy_label(),
                block: super::error::PolicyBlock::UnpinnedTag,
            });
        }
        Ok(())
    }

    /// Recover an [`DispatchResolution::AbsentDispatch`]'s `content` from the machine-global blob store,
    /// self-healing a recovered image index back into the local index.
    ///
    /// `Ok(None)` when no store is attached or the blob is absent, corrupt, or not a manifest.
    async fn recover_absent_dispatch(
        &self,
        identifier: &ocx_oci::PackageRef,
        content: &ocx_oci::Digest,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        let Some(content_store) = &self.content_store else {
            return Ok(None);
        };
        let Some(bytes) = content_store.read_blob(identifier.registry(), content).await? else {
            return Ok(None);
        };
        if !digest_matches(&bytes, content) {
            log::warn!(
                "blob-store manifest for '{content}' failed digest verification (recomputed {}); \
                 removing the corrupt object and falling through to the source walk",
                content.algorithm().hash(&bytes)
            );
            // Removed, or `write_blob`'s check-first fast path re-accepts the corrupt file on the next resolve.
            if let Err(error) = content_store.remove_blob(identifier.registry(), content).await {
                log::warn!("failed to remove corrupt blob-store object '{content}': {error}");
            }
            return Ok(None);
        }
        let manifest: ocx_oci::Manifest = match serde_json::from_slice(&bytes) {
            Ok(manifest) => manifest,
            Err(error) => {
                log::debug!("blob-store object for '{content}' is not an OCI manifest ({error}); not a recovery");
                return Ok(None);
            }
        };
        if self.write_policy != LocalWritePolicy::ReadOnly
            && matches!(manifest, ocx_oci::Manifest::ImageIndex(_))
            && let Err(error) = self.local_index.stage_dispatch_bytes(identifier, content, &bytes).await
        {
            log::warn!("failed to self-heal dispatch object '{content}' into the local index: {error}");
        }
        Ok(Some((content.clone(), manifest)))
    }

    /// Walk the source chain for `identifier`; concurrent waiters share the singleflight leader's result.
    ///
    /// `grow_root` is false for a known root whose dispatch object is merely absent, so a published root
    /// is never re-copied. `Err` means every source errored, kept distinct from a clean `Ok(None)` miss.
    async fn walk_chain(
        &self,
        identifier: &ocx_oci::PackageRef,
        grow_root: bool,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        match self.mode {
            // Never contacts a source: an unpinned miss is a policy block, and the `None` a pinned
            // identifier gets is refused at the content boundary.
            ChainMode::Offline => {
                if identifier.digest().is_none() {
                    self.ensure_locally_resolvable(identifier).await?;
                }
                return Ok(None);
            }
            // Frozen blocks only unknown-tag resolution; a known tag falls through to fetch its content.
            ChainMode::Frozen if identifier.digest().is_none() => {
                self.ensure_locally_resolvable(identifier).await?;
            }
            ChainMode::Default | ChainMode::Remote | ChainMode::Frozen => {}
        }

        // Bare names normalise to `:latest` so they share a singleflight key with explicit `:latest`.
        let walked = if identifier.digest().is_some() {
            identifier.clone()
        } else {
            identifier.clone_with_tag(identifier.tag_or_latest())
        };
        let key = format!("{}|{}|{}", self.mode as u8, walked.registry(), walked);

        use singleflight::Acquisition;
        let acquisition = self
            .singleflight
            .try_acquire(key)
            .await
            .map_err(super::error::Error::SingleflightFailed)?;
        let handle = match acquisition {
            Acquisition::Leader(h) => h,
            // Same check as the leader: a shared answer is still untrusted for this caller.
            Acquisition::Resolved(head) => return verify_walked_digest(&walked, head),
        };

        match self.fetch_and_persist_chain(&walked, grow_root).await {
            Ok(head) => {
                handle.complete(head.clone());
                verify_walked_digest(&walked, head)
            }
            Err(e) => {
                let arc = super::error::ArcError::from(e);
                let broadcast = super::error::Error::SourceWalkFailed(arc.clone());
                let _ = handle.fail(broadcast);
                Err(super::error::Error::SourceWalkFailed(arc))
            }
        }
    }

    /// Leader-side walk: persist the dispatch object from the first source that resolves `identifier`,
    /// then grow the local root when `grow_root` allows.
    ///
    /// `Err` when any source errored and none succeeded: a later clean miss does not disprove an earlier
    /// failure.
    async fn fetch_and_persist_chain(
        &self,
        identifier: &ocx_oci::PackageRef,
        grow_root: bool,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        let mut last_error: Option<super::error::Error> = None;
        for (source, authoritative) in self.candidate_sources(identifier).await {
            let fetched = if self.write_policy == LocalWritePolicy::ReadOnly {
                self.local_index.fetch_dispatch_only(source, identifier).await
            } else {
                self.local_index.persist_dispatch(source, identifier).await
            };
            match fetched {
                Ok(Some((bytes, digest, manifest))) => {
                    // Cached so the next resolve of this pin recovers a leaf manifest with zero network.
                    if let (Some(store), ocx_oci::Manifest::Image(_)) = (&self.content_store, &manifest)
                        && let Err(error) = store.write_blob(identifier.registry(), &digest, &bytes).await
                    {
                        log::debug!("could not cache the leaf manifest for '{identifier}' ({digest}): {error}");
                    }
                    // A pinned tag+digest pull grows no root, or the write would shadow the canonical `ocx.lock`.
                    if grow_root
                        && self.write_policy == LocalWritePolicy::Full
                        && identifier.tag().is_some()
                        && identifier.digest().is_none()
                    {
                        match source.source_kind() {
                            // Exactly this tag: sibling pins and `repository` are not this resolve's to move.
                            SourceKind::Published => {
                                let tag = identifier
                                    .tag()
                                    .expect("grow branch is gated on identifier.tag().is_some()");
                                if let Some((root_bytes, _root)) = source.fetch_root_document(identifier).await? {
                                    self.local_index
                                        .commit_published_root(identifier, &root_bytes, RootScope::Tags(&[tag]))
                                        .await?;
                                }
                            }
                            // A bare-manifest or reserved tag must not become a root entry: no write path
                            // consults `list_tags`, so its filters cannot catch it downstream.
                            SourceKind::Derived => {
                                let tag = identifier
                                    .tag()
                                    .expect("grow branch is gated on identifier.tag().is_some()");
                                if super::local_index::records_root_tag(tag, &manifest) {
                                    self.local_index.commit_root_tag(identifier, &digest).await?;
                                }
                            }
                        }
                    }
                    log::debug!("Fetched '{}' from chained source, persisted to cache.", identifier);
                    return Ok(Some((digest, manifest)));
                }
                Ok(None) => {
                    // An authoritative miss is terminal, or the `OciIndex` catch-all answers the same name.
                    // Kept apart from the `Err` arm, or an index outage becomes a confident "not in index".
                    if authoritative {
                        if let Some(base_url) = source.index_base_url() {
                            return Err(super::error::Error::NotInIndex {
                                identifier: identifier.to_string(),
                                namespace: identifier.registry().to_string(),
                                base_url: base_url.to_string(),
                            });
                        }
                        log::debug!("Authoritative source has no '{}' — stopping.", identifier);
                        return Ok(None);
                    }
                    log::debug!("Source has no '{}' — trying next source.", identifier);
                }
                Err(e) => {
                    // An authoritative refusal stops the walk, or a lower source answers the same name and bypasses it.
                    if authoritative {
                        log::warn!("Authoritative source refused '{}': {e}", identifier);
                        return Err(e);
                    }
                    log::warn!("Could not fetch '{}' from chained source: {e}", identifier);
                    last_error = Some(e);
                }
            }
        }

        if let Some(e) = last_error {
            return Err(e);
        }
        Ok(None)
    }

    /// Remote-mode `Query` read-through: the first source hit, never persisted.
    ///
    /// Errors propagate rather than masking as a miss; an authoritative miss is terminal
    /// (`adr_index_routing_semantics.md`).
    async fn query_sources_manifest(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        let mut last_error: Option<super::error::Error> = None;
        for (source, authoritative) in self.candidate_sources(identifier).await {
            match source.fetch_manifest(identifier, IndexOperation::Query).await {
                Ok(Some(result)) => return Ok(Some(result)),
                Ok(None) if authoritative => return Ok(None),
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Remote-mode fetch_manifest failed for '{}': {e}", identifier);
                    last_error = Some(e);
                }
            }
        }
        last_error.map_or(Ok(None), Err)
    }

    /// Digest counterpart to [`Self::query_sources_manifest`].
    async fn query_sources_manifest_digest(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::Digest>> {
        let mut last_error: Option<super::error::Error> = None;
        for (source, authoritative) in self.candidate_sources(identifier).await {
            match source.fetch_manifest_digest(identifier, IndexOperation::Query).await {
                Ok(Some(digest)) => return Ok(Some(digest)),
                Ok(None) if authoritative => return Ok(None),
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
        // Remote never falls back to the local index: stale repos on a registry outage would hide it.
        if self.mode == ChainMode::Remote {
            let mut last_error: Option<super::error::Error> = None;
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

    async fn list_tags(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
        // Remote never falls back to the local index: stale local tags on an outage would hide it and
        // break retry policy.
        if self.mode == ChainMode::Remote {
            let mut last_error: Option<super::error::Error> = None;
            for (source, authoritative) in self.candidate_sources(identifier).await {
                match source.list_tags(identifier).await {
                    Ok(Some(tags)) => return Ok(Some(tags)),
                    Ok(None) if authoritative => {
                        log::debug!("Authoritative source lists no tags for '{}' — stopping.", identifier);
                        return Ok(None);
                    }
                    Ok(None) => {}
                    // An authoritative refusal stops here, or the registry catch-all lists the literal name's
                    // tags: for `ocx.sh/ocx/cli` a stale, capped list that reads as "no newer version".
                    Err(e) if authoritative => {
                        log::warn!("Authoritative source refused a tag listing for '{}': {e}", identifier);
                        return Err(e);
                    }
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
        identifier: &ocx_oci::PackageRef,
        op: IndexOperation,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        // Digest-addressed reads are local-first in every mode: immutable content cannot be stale.
        let is_digest_addressed = identifier.digest().is_some();
        let kind = self.kind_for(identifier);
        // A corrupt object under a known root: the walk heals that object only, never re-grows the root.
        let mut corrupt_known = false;
        let local = if is_digest_addressed || self.mode != ChainMode::Remote {
            match self.local_index.resolve_dispatch(identifier, kind).await {
                Ok(resolution) => resolution,
                Err(e) => {
                    if is_local_read_refusal(&e) {
                        return Err(e);
                    }
                    // Only an online Resolve can re-fetch and heal a corrupt object; elsewhere it is a hard
                    // error, never a silent miss.
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
        if let Some(DispatchResolution::Dispatch { content, index }) = local {
            return Ok(Some((content, ocx_oci::Manifest::ImageIndex(*index))));
        }
        if let Some(DispatchResolution::AbsentDispatch { content }) = &local
            && let Some(recovered) = self.recover_absent_dispatch(identifier, content).await?
        {
            return Ok(Some(recovered));
        }
        if self.mode == ChainMode::Remote && op == IndexOperation::Query && !is_digest_addressed {
            return self.query_sources_manifest(identifier).await;
        }
        // A query never walks the chain, or a read-only command would mutate the local index.
        match op {
            IndexOperation::Query => Ok(None),
            IndexOperation::Resolve => {
                let grow_root = !corrupt_known && !matches!(local, Some(DispatchResolution::AbsentDispatch { .. }));
                // Walk the pinned digest, not the tag: the tag answers whatever it points at now, moving a
                // pin the user never named.
                let pinned = match &local {
                    Some(DispatchResolution::AbsentDispatch { content }) if !is_digest_addressed => {
                        Some(identifier.clone_with_digest(content.clone()))
                    }
                    _ => None,
                };
                self.walk_chain(pinned.as_ref().unwrap_or(identifier), grow_root).await
            }
        }
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &ocx_oci::PackageRef,
        op: IndexOperation,
    ) -> Result<Option<ocx_oci::Digest>> {
        let is_digest_addressed = identifier.digest().is_some();
        let kind = self.kind_for(identifier);
        let mut corrupt_known = false;
        if is_digest_addressed || self.mode != ChainMode::Remote {
            match self.local_index.resolve_dispatch(identifier, kind).await {
                Ok(Some(DispatchResolution::Dispatch { content, .. })) => return Ok(Some(content)),
                // Tag-addressed only: the root confirmed the tag names this content. A digest-addressed one
                // is the caller's input echoed back and must be confirmed below.
                Ok(Some(DispatchResolution::AbsentDispatch { content })) if !is_digest_addressed => {
                    return Ok(Some(content));
                }
                Ok(Some(DispatchResolution::AbsentDispatch { content })) => {
                    if let Some((digest, _)) = self.recover_absent_dispatch(identifier, &content).await? {
                        return Ok(Some(digest));
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    if is_local_read_refusal(&e) {
                        return Err(e);
                    }
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
            IndexOperation::Resolve => Ok(self
                .walk_chain(identifier, !corrupt_known)
                .await?
                .map(|(digest, _)| digest)),
        }
    }

    /// Config-blob bytes via the machine-global blob store, digest-verified on every path (CWE-345).
    ///
    /// A corrupt cached copy is a hard `DigestMismatch` under `--offline` (no source to heal it), else it is
    /// re-fetched; an offline miss is `Ok(None)`.
    async fn fetch_blob(&self, blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
        let digest = blob_ref.digest();
        // A corrupt entry needs `replace_blob`: `write_blob`'s existence check would re-accept it untouched.
        let mut heal_corrupt = false;
        if let Some(content_store) = &self.content_store
            && let Some(bytes) = content_store.read_blob(blob_ref.registry(), &digest).await?
        {
            if digest_matches(&bytes, &digest) {
                return Ok(Some(bytes));
            }
            if self.mode == ChainMode::Offline {
                return Err(ocx_store::file_structure::error::Error::DigestMismatch {
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
        let mut last_error: Option<super::error::Error> = None;
        for (source, authoritative) in self.candidate_sources(blob_ref.as_identifier()).await {
            match source.fetch_blob(blob_ref).await {
                Ok(Some(bytes)) => {
                    if !digest_matches(&bytes, &digest) {
                        return Err(ocx_store::file_structure::error::Error::DigestMismatch {
                            claimed: digest.clone(),
                            computed: digest.algorithm().hash(&bytes),
                        }
                        .into());
                    }
                    if let Some(content_store) = &self.content_store {
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
                Ok(None) if authoritative => return Ok(None),
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Source fetch_blob failed for '{blob_ref}': {e}");
                    last_error = Some(e);
                }
            }
        }
        last_error.map_or(Ok(None), Err)
    }

    /// Verbatim manifest bytes straight from the sources, bypassing the local dispatch cache.
    ///
    /// Overrides the trait default, whose re-serialised body would not hash back to the source-claimed
    /// digest the staging caller persists it under.
    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        if self.mode == ChainMode::Offline {
            return Ok(None);
        }
        let mut last_error: Option<super::error::Error> = None;
        for (source, authoritative) in self.candidate_sources(identifier).await {
            match source.fetch_manifest_raw_bytes(identifier).await {
                Ok(Some(result)) => return Ok(Some(result)),
                Ok(None) if authoritative => return Ok(None),
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Source fetch_manifest_raw_bytes failed for '{}': {e}", identifier);
                    last_error = Some(e);
                }
            }
        }
        last_error.map_or(Ok(None), Err)
    }

    /// The physical transport identifier (`adr_index_indirection.md § Structural enforcement`): committed
    /// local root first, sources on a miss; source-first under [`ChainMode::Remote`], which wants the live
    /// pointer. `Ok(None)` means no rewrite (physical == logical).
    async fn physical_reference(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        let source_first = self.mode == ChainMode::Remote;
        // Local-first is sound only because the local answer carries its own SSRF floor and
        // `Index::guard_physical_dial` re-judges it fail-closed before the first request.
        if !source_first && let Some(physical) = self.local_physical_answer(identifier).await? {
            return Ok(Some(physical));
        }
        // No `Offline` early return: ahead of the local read, `Ok(None)` would report an indirected
        // package's logical identifier as its transport.
        let mut last_error: Option<super::error::Error> = None;
        for (source, _) in self.candidate_sources(identifier).await {
            match source.physical_reference(identifier).await {
                Ok(Some(physical)) => return Ok(Some(physical)),
                Ok(None) => {}
                // A refusal propagates, or the local root would discard the source's SSRF verdict; an outage
                // is held so `Remote` can fall back to the committed root instead of exiting 69.
                Err(e) if !is_source_outage(&e) => return Err(e),
                Err(e) => {
                    log::warn!("Source physical_reference failed for '{identifier}': {e}");
                    last_error = Some(e);
                }
            }
        }
        if source_first && let Some(physical) = self.local_physical_answer(identifier).await? {
            return Ok(Some(physical));
        }
        last_error.map_or(Ok(None), Err)
    }

    /// The committed local root's answer only, with [`Self::physical_reference`]'s guard and refusal semantics.
    async fn physical_reference_local(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<ocx_oci::OciIdentifier>> {
        self.local_physical_answer(identifier).await
    }

    /// Record the routing pointer for `identifier` so the next invocation reads it off disk.
    ///
    /// Only the resolve path may call this: `cascade check`, `verify`, `sign` and `attest` must not snapshot
    /// `p/<ns>/<pkg>.json` from a pure read. Best-effort: a failed write is logged, never fails the resolve.
    async fn record_routing_pointer(&self, identifier: &ocx_oci::PackageRef) {
        // Not under `--frozen`, which promises the invocation changes nothing.
        if self.write_policy != LocalWritePolicy::Full || self.mode == ChainMode::Frozen {
            return;
        }
        // A committed root already answers: any write here would be a routing migration.
        if matches!(self.local_physical_answer(identifier).await, Ok(Some(_))) {
            return;
        }
        for (source, _) in self.candidate_sources(identifier).await {
            // A derived source's root is OCX-authored and written elsewhere.
            if !matches!(source.source_kind(), SourceKind::Published) {
                continue;
            }
            match source.fetch_root_document(identifier).await {
                Ok(Some((bytes, _))) => {
                    if let Err(error) = self
                        .local_index
                        .commit_published_root(identifier, &bytes, RootScope::Routing)
                        .await
                    {
                        log::debug!("could not record the routing pointer for '{identifier}': {error}");
                    }
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    log::debug!("could not read the root document for '{identifier}': {error}");
                    return;
                }
            }
        }
    }

    fn jurisdiction(&self, identifier: &ocx_oci::PackageRef) -> Jurisdiction {
        let mut jurisdiction = Jurisdiction::Outside;
        for source in &self.sources {
            match source.jurisdiction(identifier) {
                Jurisdiction::Authoritative => return Jurisdiction::Authoritative,
                Jurisdiction::FallThrough => jurisdiction = Jurisdiction::FallThrough,
                Jurisdiction::Outside => {}
            }
        }
        jurisdiction
    }

    fn serves_registry(&self, registry: &str) -> bool {
        self.sources.iter().any(|source| source.serves_registry(registry))
    }

    /// Refuse a name whose registry an index owns by config but no chained source serves (`--offline`),
    /// rather than dialling the host the name spells.
    fn refuse_unrouted(&self, identifier: &ocx_oci::PackageRef) -> Option<super::error::Error> {
        let registry = identifier.registry();
        (self.local_index.is_index_namespace(registry) && !self.serves_registry(registry)).then(|| {
            super::error::Error::PolicyResolutionBlocked {
                identifier: identifier.without_digest().without_tag().to_string(),
                policy: self.mode.policy_label(),
                block: super::error::PolicyBlock::UnrecordedLocation,
            }
        })
    }

    fn authoritative_index_base_url(&self, identifier: &ocx_oci::PackageRef) -> Option<&str> {
        self.sources
            .iter()
            .find_map(|source| source.authoritative_index_base_url(identifier))
    }

    /// The `[registries."<ns>"].trusted_hosts` set for `registry`: the local index's copy first, so it
    /// answers with no source to ask (`--offline`), then the owning source's.
    ///
    /// Keyed on the registry, not per-name jurisdiction, so a name the index grammar cannot express keeps
    /// its operator's exemption.
    fn trusted_hosts_for(&self, registry: &str) -> &[String] {
        let configured = self.local_index.trusted_hosts_for(registry);
        if !configured.is_empty() {
            return configured;
        }
        self.sources
            .iter()
            .find(|source| source.serves_registry(registry))
            .map_or(&[], |source| source.trusted_hosts())
    }

    /// The plain-HTTP authorities (`OCX_INSECURE_REGISTRIES`), a flat list, not registry-keyed.
    fn insecure_hosts(&self) -> &[String] {
        self.local_index.insecure_hosts()
    }

    fn set_proxy_rules(&mut self, rules: Arc<ocx_oci::ssrf::ProxyRules>) {
        self.rules = rules;
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(Self {
            local_index: self.local_index.clone(),
            sources: self.sources.clone(),
            mode: self.mode,
            // Singleflight group is shared across clones so waiters coalesce.
            singleflight: self.singleflight.clone(),
            write_policy: self.write_policy,
            content_store: self.content_store.clone(),
            rules: self.rules.clone(),
        })
    }

    fn read_only_view(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(self.read_only())
    }

    fn remote_view(&self) -> Box<dyn index_impl::IndexImpl> {
        // Keeps `read_only`'s fresh singleflight group.
        Box::new(Self {
            mode: ChainMode::Remote,
            ..self.read_only()
        })
    }
}

// ── Specification tests ───────────────────────────────────────────────────
//
// ChainMode routing, singleflight dedup, disk-persistence properties.
#[cfg(test)]
mod chain_refs_tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::error::Result;
    use crate::{
        ChainMode, Index, IndexFetch, IndexOperation, IndexStore, IndexTransport, Jurisdiction, LocalConfig,
        LocalIndex, OcxIndex, OcxIndexConfig, index_impl,
    };
    use ocx_oci::client::test_transport::{StubTransport, StubTransportData};
    use ocx_oci::{Algorithm, Digest, ImageManifest, Manifest, PackageRef};
    use ocx_store::file_structure::BlobStore;

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";

    fn tagged_id() -> PackageRef {
        PackageRef::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }
    fn digest_only_id() -> PackageRef {
        PackageRef::new_registry(REPO, REGISTRY).clone_with_digest(digest_a())
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
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            *self.call_count.lock().unwrap() += 1;
            Ok(Some(self.known_tags.keys().cloned().collect()))
        }
        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            _op: super::super::IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            let tag = identifier.tag_or_latest();
            *self.call_count.lock().unwrap() += 1;
            Ok(self.known_tags.get(tag).map(|d| (d.clone(), manifest_for(d))))
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &PackageRef,
            _op: super::super::IndexOperation,
        ) -> Result<Option<Digest>> {
            let tag = identifier.tag_or_latest();
            *self.call_count.lock().unwrap() += 1;
            Ok(self.known_tags.get(tag).cloned())
        }
        async fn fetch_blob(&self, _blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            *self.call_count.lock().unwrap() += 1;
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &PackageRef,
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
    async fn seed_full(cache: &LocalIndex, identifier: &PackageRef, _d: Digest, source: &Index) {
        let (_bytes, digest, _manifest) = cache
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
            "the no-tag write policy must be carried through box_clone; got a manifest for the tag"
        );
    }

    // ── read-only view: inspect resolves without growing the local index ─────

    /// A `ReadOnly` view (`ocx package inspect`) resolves through the source
    /// but writes NOTHING into the local index — no dispatch object, no tag
    /// pointer. A read-only look never grows the permanent index; content warms
    /// only the GC-able blob cache (unexercised here — this fake source has no
    /// blob store attached).
    #[tokio::test(flavor = "multi_thread")]
    async fn read_only_view_resolves_but_writes_no_dispatch_object_or_tag_pointer() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let store = index_store(&cache_dir);

        let (spy, src_idx) = make_source(TAG, digest_a());
        let read_only = Index::from_chained(cache, vec![src_idx], ChainMode::Remote).read_only_view();
        let result = read_only
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "read-only resolve must still reach the source");
        assert!(spy.calls() > 0, "read-only resolve must consult the source");

        // No dispatch object staged — a read-only view never writes into `o/`,
        // unlike the lock-scoped (`NoTag`) resolve which still persists it.
        let dispatch = store.dispatch_object_path(REGISTRY, REPO, &digest_a());
        assert!(
            !dispatch.exists(),
            "read-only resolve must not persist a dispatch object into the local index"
        );

        // No tag pointer committed — an offline probe cannot resolve the tag.
        let probe = Index::from_chained(make_local_index(&cache_dir), Vec::new(), ChainMode::Offline);
        let tag_pointer = probe
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Query)
            .await
            .unwrap();
        assert!(
            tag_pointer.is_none(),
            "read-only resolve must never commit a tag pointer; got a manifest for the tag"
        );
    }

    // ── remote view: update-check lists live without moving a pin ────────────

    /// A `remote_view` lists tags AND resolves from the SOURCE even though the
    /// ambient mode is `Default` (which reads local-index-first), and writes
    /// nothing into the local index while doing so.
    ///
    /// Backs the update-check probe: the freshest upstream release must be
    /// found through the configured chain — which understands logical
    /// `ocx.sh/<ns>/<pkg>` names — regardless of the ambient ChainMode, and
    /// looking must never move a pin (the local index is the package-tier lock).
    ///
    /// The resolve is what makes the two write assertions falsifiable: a
    /// listing never writes under any policy, so asserting on it alone would
    /// hold even with the view's [`LocalWritePolicy`] flipped to `Full`.
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_view_lists_from_source_under_default_mode_and_writes_nothing() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let store = index_store(&cache_dir);

        let (spy, src_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![src_idx], ChainMode::Default);
        let remote = chained.remote_view();

        // Ambient Default lists the (empty) local index — the source is untouched.
        let local_only = chained.list_tags(&tagged_id()).await.unwrap();
        assert!(
            local_only.is_none_or(|tags| tags.is_empty()),
            "precondition: Default mode lists the empty local index"
        );
        assert_eq!(spy.calls(), 0, "precondition: Default mode must not consult the source");

        let tags = remote
            .list_tags(&tagged_id())
            .await
            .unwrap()
            .expect("the remote view must reach the source");
        assert_eq!(
            tags,
            vec![TAG.to_string()],
            "the remote view must list the source's tags"
        );
        assert!(spy.calls() > 0, "the remote view must consult the source");

        // A resolve through the SAME view — the path that persists under every
        // other write policy — must still reach the source and still write
        // nothing.
        let resolved = remote
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(resolved.is_some(), "the remote view must resolve from the source");

        // No dispatch object staged, no tag pointer committed.
        assert!(
            !store.dispatch_object_path(REGISTRY, REPO, &digest_a()).exists(),
            "a remote-view resolve must not persist a dispatch object"
        );
        let probe = Index::from_chained(make_local_index(&cache_dir), Vec::new(), ChainMode::Offline);
        assert!(
            probe
                .fetch_manifest(&tagged_id(), super::super::IndexOperation::Query)
                .await
                .unwrap()
                .is_none(),
            "a remote-view resolve must not commit a tag pointer"
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
    fn assert_policy_blocked(err: &crate::error::Error, expected_policy: &str) {
        match err {
            super::super::error::Error::PolicyResolutionBlocked { policy, identifier, .. } => {
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
            // dispatch object is never written (the AbsentDispatch shape).
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
                !matches!(result, Err(super::super::error::Error::PolicyResolutionBlocked { .. })),
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
                !matches!(result, Err(super::super::error::Error::PolicyResolutionBlocked { .. })),
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
            async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
                Ok(None)
            }
            async fn fetch_manifest(
                &self,
                _: &PackageRef,
                _op: super::super::IndexOperation,
            ) -> Result<Option<(Digest, Manifest)>> {
                Err(super::super::error::Error::RemoteManifestNotFound(
                    "test error".to_string(),
                ))
            }
            async fn fetch_manifest_digest(
                &self,
                _: &PackageRef,
                _op: super::super::IndexOperation,
            ) -> Result<Option<Digest>> {
                Err(super::super::error::Error::RemoteManifestNotFound(
                    "test error".to_string(),
                ))
            }
            async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
                Err(super::super::error::Error::RemoteManifestNotFound(
                    "test error".to_string(),
                ))
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
    /// (`index list`, `index catalog`, `package description pull`). The invariant this
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
        // `PinnedPackageRef` via `clone_with_digest` after `lock` resolved
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
                Err(super::super::error::Error::RemoteManifestNotFound("boom".to_string()))
            }
            async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
                Ok(None)
            }
            async fn fetch_manifest(
                &self,
                _: &PackageRef,
                _op: super::super::IndexOperation,
            ) -> Result<Option<(Digest, Manifest)>> {
                Ok(None)
            }
            async fn fetch_manifest_digest(
                &self,
                _: &PackageRef,
                _op: super::super::IndexOperation,
            ) -> Result<Option<Digest>> {
                Ok(None)
            }
            async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
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
            async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
                Err(super::super::error::Error::RemoteManifestNotFound("boom".to_string()))
            }
            async fn fetch_manifest(
                &self,
                _: &PackageRef,
                _op: super::super::IndexOperation,
            ) -> Result<Option<(Digest, Manifest)>> {
                Ok(None)
            }
            async fn fetch_manifest_digest(
                &self,
                _: &PackageRef,
                _op: super::super::IndexOperation,
            ) -> Result<Option<Digest>> {
                Err(super::super::error::Error::RemoteManifestNotFound("boom".to_string()))
            }
            async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
                Err(super::super::error::Error::RemoteManifestNotFound("boom".to_string()))
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
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
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

    fn pinned_for_test() -> ocx_oci::PinnedPackageRef {
        ocx_oci::PinnedPackageRef::try_from(digest_only_id()).unwrap()
    }

    /// Cache hit: blob already in the machine-global blob store (`fs.blobs`)
    /// — returns the bytes without consulting any source. Proves the
    /// offline-rehydration path works when the local CAS already holds the
    /// blob (the index-home flat blob CAS has been retired, `adr_index_indirection.md` B2).
    #[tokio::test(flavor = "multi_thread")]
    async fn chained_fetch_blob_cache_hit_no_source_call() {
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
                crate::error::Error::Store(ocx_store::file_structure::error::Error::DigestMismatch { .. })
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

    /// A fake PUBLISHED-kind source (authoritative over `REGISTRY`,
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
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![TAG.to_string()]))
        }
        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            _op: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(&self, identifier: &PackageRef, _op: IndexOperation) -> Result<Option<Digest>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &PackageRef,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            // Digest-honest, as a registry is: answering one fixed document for
            // every digest lets a test green on a resolve that asked for
            // something else entirely.
            if identifier.digest().is_some_and(|requested| requested != digest_a()) {
                return Ok(None);
            }
            Ok(Some((
                manifest_a_bytes().to_vec(),
                digest_a(),
                manifest_for(&digest_a()),
            )))
        }
        async fn fetch_root_document(
            &self,
            _identifier: &PackageRef,
        ) -> Result<Option<(Vec<u8>, super::super::IndexRoot)>> {
            *self.fetch_root_document_calls.lock().unwrap() += 1;
            Ok(None)
        }
        fn jurisdiction(&self, identifier: &PackageRef) -> Jurisdiction {
            if identifier.registry() == REGISTRY {
                Jurisdiction::Authoritative
            } else {
                Jurisdiction::Outside
            }
        }
        fn serves_registry(&self, registry: &str) -> bool {
            registry == REGISTRY
        }
        fn source_kind(&self) -> super::super::local_index::SourceKind {
            super::super::local_index::SourceKind::Published
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    // ── provenance is per REGISTRY, never per name (R3) ──────────────────────

    /// A published source's provenance decides the LOCAL subtree layout
    /// (`c/index.json` catalog vs `p/` enumeration), which is per-source and
    /// never per-name. Routing it through per-name jurisdiction would flip a
    /// name the index's grammar cannot express to `Derived`, silently dropping
    /// the published root's catalog cross-check for it.
    #[tokio::test]
    async fn kind_for_reports_published_for_every_name_under_a_published_registry() {
        let dir = TempDir::new().unwrap();
        let chained = super::ChainedIndex::new(
            make_local_index(&dir),
            vec![Index::from_impl(PublishedSource {
                fetch_root_document_calls: Arc::new(Mutex::new(0)),
            })],
            ChainMode::Default,
        );

        for repository in ["cmake", "kitware/cmake", "deeply/nested/name"] {
            assert_eq!(
                chained.kind_for(&PackageRef::new_registry(repository, REGISTRY)),
                super::SourceKind::Published,
                "'{repository}' lives under a published registry whatever its shape"
            );
        }
        assert_eq!(
            chained.kind_for(&PackageRef::new_registry("cmake", "other.io")),
            super::SourceKind::Derived,
            "a registry nobody configured falls back to the uncatalogued read"
        );
    }

    /// The provenance primitive takes a bare registry — there is no placeholder
    /// identifier anywhere in the path to encode an assumption about a
    /// predicate's shape.
    #[tokio::test]
    async fn kind_for_registry_takes_a_registry_and_no_placeholder_identifier() {
        let dir = TempDir::new().unwrap();
        let chained = super::ChainedIndex::new(
            make_local_index(&dir),
            vec![Index::from_impl(PublishedSource {
                fetch_root_document_calls: Arc::new(Mutex::new(0)),
            })],
            ChainMode::Default,
        );

        assert_eq!(chained.kind_for_registry(REGISTRY), super::SourceKind::Published);
        assert_eq!(chained.kind_for_registry("other.io"), super::SourceKind::Derived);
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

    /// A dispatch object whose bytes hash correctly but declare
    /// `schemaVersion: 1` is corruption of the same class as a digest
    /// mismatch — under `--offline` there is no source to heal it from, so it
    /// escalates to a hard `DataError` instead of being reported as a cache
    /// miss (which would surface as "nothing to install", hiding malformed
    /// index data behind a plausible verdict).
    ///
    /// The object is written at the digest its own bytes hash to, so the A4
    /// digest anchor passes and only the semantic check can refuse it. The
    /// fixture is a byte literal: serialising an `ocx_oci::ImageIndex` cannot emit
    /// `schemaVersion: 1`.
    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_dispatch_object_is_a_data_error_offline() {
        let cache_dir = TempDir::new().unwrap();
        let cache = make_local_index(&cache_dir);
        let store = index_store(&cache_dir);

        let invalid_bytes: &[u8] =
            br#"{"schemaVersion":1,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        let invalid_digest = Algorithm::Sha256.hash(invalid_bytes);
        let root_bytes = format!(
            r#"{{"repository":"oci://{REGISTRY}/{REPO}","tags":{{"{TAG}":{{"content":"{invalid_digest}","observed":"2026-07-18T00:00:00Z"}}}}}}"#,
        )
        .into_bytes();
        store
            .write_dispatch_object(REGISTRY, REPO, &invalid_digest, invalid_bytes)
            .await
            .unwrap();
        store.write_root_document(REGISTRY, REPO, &root_bytes).await.unwrap();

        let source = PublishedSource {
            fetch_root_document_calls: Arc::new(Mutex::new(0)),
        };
        let chained = Index::from_chained(cache, vec![super::super::Index::from_impl(source)], ChainMode::Offline);

        let error = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect_err("an invalid dispatch object must not read back as a miss");
        assert!(
            matches!(error, super::super::error::Error::InvalidImageIndex(_)),
            "expected InvalidImageIndex, got {error:?}"
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
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![TAG.to_string()]))
        }
        async fn fetch_manifest(
            &self,
            _identifier: &PackageRef,
            _op: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some((self.digest(), serde_json::from_slice(self.bytes).unwrap())))
        }
        async fn fetch_manifest_digest(&self, _identifier: &PackageRef, _op: IndexOperation) -> Result<Option<Digest>> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some(self.digest()))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &PackageRef,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            // Digest-honest, as a registry is — see `PublishedSource`'s note.
            if identifier.digest().is_some_and(|requested| requested != self.digest()) {
                return Ok(None);
            }
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

    /// A flat (single-platform) tag never gains a dispatch object (A3/B2) —
    /// and therefore never gains a root tag entry either (D2). The resolve
    /// still succeeds and hands the manifest to the caller; only the local
    /// write is excluded. Offline mode policy-blocks an unknown flat-manifest
    /// tag before ever consulting the source.
    ///
    /// This test previously asserted the opposite for the root — that the tag
    /// entry grew with `content` set to the leaf digest — which is exactly the
    /// tag-without-an-object absence D2 abolishes.
    #[tokio::test(flavor = "multi_thread")]
    async fn flat_manifest_tag_routing() {
        let flat_digest = Algorithm::Sha256.hash(FLAT_MANIFEST_JSON);

        // (a) + (b): Default-mode Resolve writes nothing to `o/` and, because
        // nothing landed there, nothing to the root either.
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

            assert!(
                !store.root_document_path(REGISTRY, REPO).exists(),
                "a tag that wrote nothing to `o/` must not be recorded in the root (D2)"
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
    // the regression: an `AbsentDispatch` (content absent from `o/`) is recovered
    // from the machine-global blob store BEFORE any source walk, so an
    // installed tool resolves offline with zero network — the regression that
    // left `test_offline.py` / `test_pinned_offline.py` red.

    /// A flat single-platform (leaf) image manifest and the digest its bytes
    /// hash to. A leaf is never written to the dispatch-object CAS (A3), so a
    /// tag or digest pointing at it reports `DispatchResolution::AbsentDispatch`;
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

    /// Regression (#215-family, `test_offline.py`): a tag-addressed `AbsentDispatch`
    /// resolves offline from the blob store with zero sources. The tag pointer
    /// is locally known (root committed), the leaf is absent from `o/`, and its
    /// bytes sit in `$OCX_HOME/blobs` — exactly the post-install offline-exec
    /// state.
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_tag_absent_dispatch_recovers_from_blob_store() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let (leaf_bytes, leaf_digest) = leaf_manifest_bytes();
        // Root tag → leaf content; a single-platform tag writes nothing to `o/`
        // (AbsentDispatch).
        cache.commit_root_tag(&tagged_id(), &leaf_digest).await.unwrap();
        let blobs = seeded_blob_store(&dir, &leaf_digest, &leaf_bytes).await;

        // Offline, zero sources: recovery must come from the blob store alone.
        let chained = Index::from_chained_with_content_store(cache, vec![], ChainMode::Offline, blobs);
        let (digest, manifest) = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("offline AbsentDispatch must recover the leaf manifest from the blob store");
        assert_eq!(digest, leaf_digest);
        assert!(matches!(manifest, Manifest::Image(_)), "recovered a flat leaf manifest");
    }

    /// A digest-addressed `AbsentDispatch` (a pinned pull's leaf) recovers offline
    /// from the blob store through both `fetch_manifest` and
    /// `fetch_manifest_digest` (`test_pinned_offline.py`).
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_digest_absent_dispatch_recovers_from_blob_store() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let (leaf_bytes, leaf_digest) = leaf_manifest_bytes();
        let blobs = seeded_blob_store(&dir, &leaf_digest, &leaf_bytes).await;
        let id = PackageRef::new_registry(REPO, REGISTRY).clone_with_digest(leaf_digest.clone());
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
    /// (the `from_chained` seam every unit test uses), an offline `AbsentDispatch`
    /// stays a clean `None` — proving the fix changes nothing for the
    /// no-content-store construction and cannot mask a genuine offline miss.
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_absent_dispatch_without_content_store_returns_none() {
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
            "no content store → offline AbsentDispatch stays a clean None"
        );
    }

    /// The blob-store recovery must NOT mask the no-resolve policy block: an
    /// unindexed tag (no root → a genuine `None` miss, not `AbsentDispatch`) still
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

    // ── the committed pin decides, never the live tag (the index IS the lock) ─
    //
    // A bare-leaf tag (single-platform) never gains an `o/` object by design, so
    // every resolve of one reports `AbsentDispatch` and, once the blob store no
    // longer holds the leaf, reaches the source walk. What it must ask the
    // source for is the digest the committed root PINS — asking for the tag
    // would return whatever the tag points at now, silently moving a pin the
    // user never named.

    /// The leaf a committed root pins. Single-platform image manifests, so
    /// nothing is ever written to `o/` (A3) — the shape that makes the walk the
    /// deciding step.
    const PINNED_LEAF_JSON: &[u8] = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:0000000000000000000000000000000000000000000000000000000000000000","size":2},"layers":[]}"#;
    /// The leaf the same tag points at after a re-push — different bytes, so a
    /// different digest, still fetchable by digest like the pinned one.
    const CURRENT_LEAF_JSON: &[u8] = br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:1111111111111111111111111111111111111111111111111111111111111111","size":3},"layers":[]}"#;

    fn pinned_leaf_digest() -> Digest {
        Algorithm::Sha256.hash(PINNED_LEAF_JSON)
    }
    fn current_leaf_digest() -> Digest {
        Algorithm::Sha256.hash(CURRENT_LEAF_JSON)
    }

    /// A registry whose tag was re-pushed: the tag now serves
    /// [`CURRENT_LEAF_JSON`], while [`PINNED_LEAF_JSON`] stays addressable by
    /// digest. Counts tag-addressed fetches so a test can prove a committed pin
    /// is never re-resolved through the floating tag.
    #[derive(Clone)]
    struct RepushedLeafSource {
        tag_fetches: Arc<Mutex<usize>>,
    }

    impl RepushedLeafSource {
        fn new() -> Self {
            Self {
                tag_fetches: Arc::new(Mutex::new(0)),
            }
        }
        fn tag_fetches(&self) -> usize {
            *self.tag_fetches.lock().unwrap()
        }
        fn served(&self, identifier: &PackageRef) -> Option<&'static [u8]> {
            match identifier.digest() {
                Some(digest) if digest == pinned_leaf_digest() => Some(PINNED_LEAF_JSON),
                Some(digest) if digest == current_leaf_digest() => Some(CURRENT_LEAF_JSON),
                Some(_) => None,
                None => {
                    *self.tag_fetches.lock().unwrap() += 1;
                    Some(CURRENT_LEAF_JSON)
                }
            }
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for RepushedLeafSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![TAG.to_string()]))
        }
        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            _op: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(&self, identifier: &PackageRef, _op: IndexOperation) -> Result<Option<Digest>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &PackageRef,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            Ok(self.served(identifier).map(|bytes| {
                (
                    bytes.to_vec(),
                    Algorithm::Sha256.hash(bytes),
                    serde_json::from_slice(bytes).unwrap(),
                )
            }))
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// The invariant: a tag the local root already pins resolves to the PINNED
    /// digest even when the registry has moved the tag on. The blob store holds
    /// nothing, so the answer comes from the source walk — which must address it
    /// by the pinned digest, never by the tag.
    #[tokio::test(flavor = "multi_thread")]
    async fn absent_dispatch_resolve_returns_the_pinned_leaf_not_the_moved_tag() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        assert_ne!(pinned_leaf_digest(), current_leaf_digest());

        cache
            .commit_root_tag(&tagged_id(), &pinned_leaf_digest())
            .await
            .unwrap();
        let root_path = index_store(&dir).root_document_path(REGISTRY, REPO);
        let root_before = std::fs::read(&root_path).unwrap();

        // No content store: the blob-store recovery misses, so the source walk
        // is what answers — exactly the state `ocx clean` leaves behind.
        let source = RepushedLeafSource::new();
        let chained = Index::from_chained(cache, vec![Index::from_impl(source.clone())], ChainMode::Default);

        let (digest, manifest) = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("the pinned leaf is still fetchable by digest");
        assert_eq!(
            digest,
            pinned_leaf_digest(),
            "the committed pin decides the resolve, never the source's current tag"
        );
        assert!(matches!(manifest, Manifest::Image(_)));
        assert_eq!(
            source.tag_fetches(),
            0,
            "a committed pin must be fetched by digest — asking the tag is what moves it"
        );
        assert_eq!(
            std::fs::read(&root_path).unwrap(),
            root_before,
            "recovering a pinned leaf must not rewrite the root"
        );

        // `fetch_manifest_digest` already short-circuits from the pin with zero
        // network; the two must agree or one command's answer contradicts the other.
        let confirmed = chained
            .fetch_manifest_digest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("the pin answers the digest read locally");
        assert_eq!(confirmed, pinned_leaf_digest());
    }

    /// A source that answers a digest-addressed walk with different content moves
    /// the pin unless the answer is checked. Addressing the walk by digest is
    /// only half the guarantee — the other half is refusing an answer that does
    /// not carry the digest that was asked for.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_answering_a_pinned_walk_with_other_content_is_refused() {
        /// Answers every request with `CURRENT_LEAF_JSON`, whatever was asked
        /// for — a registry that re-pushed under a digest it kept serving, or a
        /// hostile mirror.
        #[derive(Clone)]
        struct WrongAnswerSource;

        #[async_trait]
        impl index_impl::IndexImpl for WrongAnswerSource {
            async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
                Ok(Vec::new())
            }
            async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
                Ok(Some(vec![TAG.to_string()]))
            }
            async fn fetch_manifest(
                &self,
                identifier: &PackageRef,
                _op: IndexOperation,
            ) -> Result<Option<(Digest, Manifest)>> {
                Ok(self
                    .fetch_manifest_raw_bytes(identifier)
                    .await?
                    .map(|(_, digest, manifest)| (digest, manifest)))
            }
            async fn fetch_manifest_digest(&self, _: &PackageRef, _op: IndexOperation) -> Result<Option<Digest>> {
                Ok(Some(current_leaf_digest()))
            }
            async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
                Ok(None)
            }
            async fn fetch_manifest_raw_bytes(&self, _: &PackageRef) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
                Ok(Some((
                    CURRENT_LEAF_JSON.to_vec(),
                    current_leaf_digest(),
                    serde_json::from_slice(CURRENT_LEAF_JSON).unwrap(),
                )))
            }
            fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
                Box::new(self.clone())
            }
        }

        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        cache
            .commit_root_tag(&tagged_id(), &pinned_leaf_digest())
            .await
            .unwrap();
        let root_path = index_store(&dir).root_document_path(REGISTRY, REPO);
        let root_before = std::fs::read(&root_path).unwrap();

        let chained = Index::from_chained(cache, vec![Index::from_impl(WrongAnswerSource)], ChainMode::Default);
        let _error = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("an answer that is not the digest we asked for must not be accepted");
        assert_eq!(
            std::fs::read(&root_path).unwrap(),
            root_before,
            "the pin must be exactly where it was"
        );
    }

    /// The leaf bytes a resolve fetched land in the machine-global blob store, so
    /// the next resolve of the same pin needs no source at all (A3 step 2 / B2).
    #[tokio::test(flavor = "multi_thread")]
    async fn leaf_resolve_warms_the_blob_store_for_the_next_offline_one() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let blobs = BlobStore::new(dir.path().join("blobs"));
        cache
            .commit_root_tag(&tagged_id(), &pinned_leaf_digest())
            .await
            .unwrap();

        let chained = Index::from_chained_with_content_store(
            cache.clone(),
            vec![Index::from_impl(RepushedLeafSource::new())],
            ChainMode::Default,
            blobs.clone(),
        );
        let (digest, _) = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("the pinned leaf resolves through the source");
        assert_eq!(digest, pinned_leaf_digest());

        assert_eq!(
            blobs
                .read_blob(REGISTRY, &pinned_leaf_digest())
                .await
                .unwrap()
                .as_deref(),
            Some(PINNED_LEAF_JSON),
            "a fetched leaf manifest must be cached verbatim so the next recovery reads it locally"
        );

        // Same pin, no sources, offline: only the warmed blob store can answer.
        let offline = Index::from_chained_with_content_store(cache, vec![], ChainMode::Offline, blobs);
        let (offline_digest, manifest) = offline
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("the warmed blob store answers offline with zero network");
        assert_eq!(offline_digest, pinned_leaf_digest());
        assert!(matches!(manifest, Manifest::Image(_)));
    }

    // ── grow-on-resolve adopts one tag, never a whole root ────────────────
    //
    // Resolving a tag the local copy has never seen must add exactly that tag.
    // Copying the fetched root over the committed one would move every sibling
    // pin and the `repository` routing pointer at the same time — an update the
    // user never asked for, performed by a command that only reads.

    /// A published source whose root has moved on since the local copy was
    /// taken: `TAG` now points at a different object, `SIBLING_TAG` is new, and
    /// the physical `repository` was re-pointed at another host.
    #[derive(Clone)]
    struct MovedPublishedSource;

    const SIBLING_TAG: &str = "3.29";
    const LOCAL_REPOSITORY: &str = "oci://old.example/cmake";
    const FETCHED_REPOSITORY: &str = "oci://new.example/cmake";

    /// The root the SOURCE serves now — `TAG` re-pointed at `digest_b`,
    /// `SIBLING_TAG` added, `repository` moved.
    fn fetched_root_bytes() -> Vec<u8> {
        format!(
            "{{\n  \"repository\": \"{FETCHED_REPOSITORY}\",\n  \"tags\": {{\n    \
             \"{TAG}\": {{ \"content\": \"{}\", \"observed\": \"2026-08-01T00:00:00Z\" }},\n    \
             \"{SIBLING_TAG}\": {{ \"content\": \"{}\", \"observed\": \"2026-08-02T00:00:00Z\" }}\n  }}\n}}\n",
            digest_b(),
            digest_b()
        )
        .into_bytes()
    }

    /// The root the local copy COMMITTED earlier — `TAG` pinned to `digest_a`,
    /// no sibling, the original `repository`.
    fn committed_root_bytes() -> Vec<u8> {
        format!(
            "{{\n  \"repository\": \"{LOCAL_REPOSITORY}\",\n  \"tags\": {{\n    \
             \"{TAG}\": {{ \"content\": \"{}\", \"observed\": \"2026-07-01T00:00:00Z\" }}\n  }}\n}}\n",
            digest_a()
        )
        .into_bytes()
    }

    #[async_trait]
    impl index_impl::IndexImpl for MovedPublishedSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![TAG.to_string(), SIBLING_TAG.to_string()]))
        }
        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            _op: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, manifest)| (digest, manifest)))
        }
        async fn fetch_manifest_digest(&self, identifier: &PackageRef, _op: IndexOperation) -> Result<Option<Digest>> {
            Ok(self
                .fetch_manifest_raw_bytes(identifier)
                .await?
                .map(|(_, digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &PackageRef,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            // Whatever is asked for, the source now serves `digest_b` — the
            // moved-on state. Addressed by digest, it answers with the bytes
            // hashing to it, as a registry does.
            let digest = match identifier.digest() {
                Some(requested) if requested == digest_a() => digest_a(),
                Some(requested) if requested == digest_b() => digest_b(),
                Some(_) => return Ok(None),
                None => digest_b(),
            };
            Ok(Some((bytes_for(&digest), digest.clone(), manifest_for(&digest))))
        }
        async fn fetch_root_document(&self, _: &PackageRef) -> Result<Option<(Vec<u8>, super::super::IndexRoot)>> {
            let bytes = fetched_root_bytes();
            let root = serde_json::from_slice(&bytes).unwrap();
            Ok(Some((bytes, root)))
        }
        fn jurisdiction(&self, identifier: &PackageRef) -> Jurisdiction {
            if identifier.registry() == REGISTRY {
                Jurisdiction::Authoritative
            } else {
                Jurisdiction::Outside
            }
        }
        fn serves_registry(&self, registry: &str) -> bool {
            registry == REGISTRY
        }
        fn source_kind(&self) -> super::SourceKind {
            super::SourceKind::Published
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Resolving a NEW sibling tag of an already-committed published package
    /// adopts that one tag and nothing else: the tag the copy already pins keeps
    /// its digest, and the `repository` routing pointer is untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn grow_on_resolve_adopts_only_the_resolved_tag_of_a_committed_published_root() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let store = index_store(&dir);
        store
            .write_root_document(REGISTRY, REPO, &committed_root_bytes())
            .await
            .unwrap();

        let chained = Index::from_chained(cache, vec![Index::from_impl(MovedPublishedSource)], ChainMode::Default);
        let sibling = PackageRef::new_registry(REPO, REGISTRY).clone_with_tag(SIBLING_TAG);
        let (digest, _) = chained
            .fetch_manifest(&sibling, IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("a first resolve of an unknown tag grows the local copy");
        assert_eq!(digest, digest_b());

        let after: super::super::IndexRoot =
            serde_json::from_slice(&std::fs::read(store.root_document_path(REGISTRY, REPO)).unwrap()).unwrap();
        assert_eq!(
            after.tags.get(TAG).map(|entry| entry.content.clone()),
            Some(digest_a()),
            "a tag the copy already pins must keep its digest — nobody asked to update it"
        );
        assert_eq!(
            after.tags.get(SIBLING_TAG).map(|entry| entry.content.clone()),
            Some(digest_b()),
            "the newly-resolved tag is adopted from the fetched root"
        );
        assert_eq!(
            after.repository, LOCAL_REPOSITORY,
            "the committed routing pointer must survive a resolve of an unrelated tag"
        );
    }

    /// A package the local copy has never seen lands the tag that was resolved
    /// and its routing pointer — not the site's whole tag list. First sight is
    /// no exception to the scope: the resolve still only adds what it resolved,
    /// it just has an empty document to add it to.
    #[tokio::test(flavor = "multi_thread")]
    async fn grow_on_resolve_lands_only_the_resolved_tag_of_a_new_published_package() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let store = index_store(&dir);

        let chained = Index::from_chained(cache, vec![Index::from_impl(MovedPublishedSource)], ChainMode::Default);
        chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("first resolve of an unknown package");

        let root: super::super::IndexRoot =
            serde_json::from_slice(&std::fs::read(store.root_document_path(REGISTRY, REPO)).unwrap()).unwrap();
        assert_eq!(
            root.tags.keys().collect::<Vec<_>>(),
            vec![TAG],
            "only the resolved tag lands, though the site lists a sibling too"
        );
        assert_eq!(
            root.repository, FETCHED_REPOSITORY,
            "routing comes from the fetched root — there was no committed pointer to protect"
        );
    }

    // ── authoritative-stop on a clean miss (no silent fallthrough) ─────────

    /// A fake source claiming authoritative ownership of `REGISTRY`'s
    /// namespace (mirrors `OcxIndex::jurisdiction`) but reporting a
    /// clean miss for every identifier — the case where the one configured
    /// ocx-index for a namespace genuinely has no such package.
    #[derive(Clone)]
    struct AuthoritativeMissSource;

    #[async_trait]
    impl index_impl::IndexImpl for AuthoritativeMissSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _: &PackageRef, _op: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _op: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn jurisdiction(&self, identifier: &PackageRef) -> Jurisdiction {
            if identifier.registry() == REGISTRY {
                Jurisdiction::Authoritative
            } else {
                Jurisdiction::Outside
            }
        }
        fn serves_registry(&self, registry: &str) -> bool {
            registry == REGISTRY
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

    /// A fake source authoritative over `REGISTRY` whose **tag listing fails**
    /// the way a published index does when its site is unreachable or serving a
    /// 5xx — the outage case, not the clean miss above.
    #[derive(Clone)]
    struct AuthoritativeListErrorSource;

    #[async_trait]
    impl index_impl::IndexImpl for AuthoritativeListErrorSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Err(super::super::error::Error::RemoteManifestNotFound(
                "index outage".to_string(),
            ))
        }
        async fn fetch_manifest(&self, _: &PackageRef, _op: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _op: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn jurisdiction(&self, identifier: &PackageRef) -> Jurisdiction {
            if identifier.registry() == REGISTRY {
                Jurisdiction::Authoritative
            } else {
                Jurisdiction::Outside
            }
        }
        fn serves_registry(&self, registry: &str) -> bool {
            registry == REGISTRY
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// Regression: an authoritative source's tag-listing **failure** stops the
    /// Remote-mode `list_tags` walk — it must never fall through to the
    /// registry catch-all, which answers for the LITERAL name.
    ///
    /// The self-update blind spot in its outage shape: `ocx.sh/ocx/cli` is a
    /// logical name the published index routes elsewhere, so a catch-all answer
    /// is the stale pre-indirection repository's capped tag list. The propagated
    /// error becomes `Skipped(RegistryProbeFailed)` (exit 75, "could not
    /// determine") at the update-check caller; a fall-through would instead
    /// report a confident "up to date".
    #[tokio::test(flavor = "multi_thread")]
    async fn authoritative_list_tags_error_does_not_fall_through_to_registry() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let authoritative_idx = Index::from_impl(AuthoritativeListErrorSource);
        let (registry_spy, registry_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![authoritative_idx, registry_idx], ChainMode::Remote);

        let result = chained.list_tags(&tagged_id()).await;
        assert!(
            result.is_err(),
            "an authoritative source's listing failure must propagate, not be masked by a later source"
        );
        assert_eq!(
            registry_spy.calls(),
            0,
            "the registry catch-all must never be listed once the authoritative source failed"
        );
    }

    /// The clean-miss half of the same stop on the `list_tags` loop: an
    /// authoritative source that lists nothing is a terminal `Ok(None)`.
    #[tokio::test(flavor = "multi_thread")]
    async fn authoritative_list_tags_clean_miss_does_not_fall_through_to_registry() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let authoritative_idx = Index::from_impl(AuthoritativeMissSource);
        let (registry_spy, registry_idx) = make_source(TAG, digest_a());
        let chained = Index::from_chained(cache, vec![authoritative_idx, registry_idx], ChainMode::Remote);

        let tags = chained.list_tags(&tagged_id()).await.unwrap();
        assert!(
            tags.is_none(),
            "an authoritative source's empty listing must be a terminal None; got {tags:?}"
        );
        assert_eq!(
            registry_spy.calls(),
            0,
            "the registry catch-all must never be listed once the authoritative source reported a clean miss"
        );
    }

    /// The registry catch-all in its OUTAGE shape: non-authoritative
    /// (`jurisdiction` defaults to `FallThrough`) and unreachable, counting
    /// every call it receives. A transport error, never a clean miss — so a
    /// walk that reaches it turns a resolvable read into a hard failure.
    #[derive(Clone)]
    struct FallThroughErrorSource {
        calls: Arc<Mutex<usize>>,
    }

    #[async_trait]
    impl index_impl::IndexImpl for FallThroughErrorSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _: &PackageRef, _op: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _op: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            *self.calls.lock().unwrap() += 1;
            Err(super::super::error::Error::RemoteManifestNotFound(
                "registry unreachable".to_string(),
            ))
        }
        async fn fetch_manifest_raw_bytes(&self, _: &PackageRef) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            *self.calls.lock().unwrap() += 1;
            Err(super::super::error::Error::RemoteManifestNotFound(
                "registry unreachable".to_string(),
            ))
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn fall_through_error_source() -> (Arc<Mutex<usize>>, Index) {
        let calls = Arc::new(Mutex::new(0));
        (calls.clone(), Index::from_impl(FallThroughErrorSource { calls }))
    }

    /// Regression (ocx#407 fallout): the verbatim-bytes walk must stop on the
    /// authoritative source's clean miss, exactly like
    /// [`authoritative_clean_miss_does_not_fall_through_to_registry_resolve`]
    /// and its `--remote` siblings — a clean miss from the namespace's one
    /// authoritative source is terminal, never a fall-through to the
    /// `OciIndex` catch-all.
    ///
    /// The second half is the paired positive: the SAME two behaviours with
    /// the first source non-authoritative still walk on, so the assertion
    /// discriminates on jurisdiction alone and not on the walk being dead.
    ///
    /// Without the stop, an index-answered install dies on the catch-all's
    /// transport error — the shape acceptance case A hits behind a proxy that
    /// cannot reach the logical registry, and the shape the twin
    /// `test_index_ocx_sh.py` install was quietly surviving only by reaching
    /// the public `ocx.sh` for a 404.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_authoritative_miss_on_the_manifest_walk_is_terminal_and_never_reaches_the_fall_through_source() {
        let dir = TempDir::new().unwrap();
        let (spy_calls, error_idx) = fall_through_error_source();
        let chained = Index::from_chained(
            make_local_index(&dir),
            vec![Index::from_impl(AuthoritativeMissSource), error_idx],
            ChainMode::Default,
        );

        let result = chained.fetch_manifest_raw_bytes(&digest_only_id()).await;
        assert!(
            matches!(result, Ok(None)),
            "an authoritative clean miss must end the walk as Ok(None), got: {result:?}"
        );
        assert_eq!(
            *spy_calls.lock().unwrap(),
            0,
            "the catch-all must never be asked once the authoritative source reported a clean miss"
        );

        // Paired positive: non-authoritative first source ⇒ the walk continues.
        let dir = TempDir::new().unwrap();
        let (spy_calls, error_idx) = fall_through_error_source();
        let (_, fall_through_miss) = make_source(TAG, digest_a());
        let chained = Index::from_chained(
            make_local_index(&dir),
            vec![fall_through_miss, error_idx],
            ChainMode::Default,
        );

        assert!(
            chained.fetch_manifest_raw_bytes(&digest_only_id()).await.is_err(),
            "a fall-through source's miss must still walk on, and the next source's outage propagates"
        );
        assert_eq!(
            *spy_calls.lock().unwrap(),
            1,
            "the walk must have reached the second source"
        );
    }

    /// The blob walk carries the same omission and the same fix — see the
    /// manifest sibling above for why, including its paired positive.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_authoritative_miss_on_the_blob_walk_is_terminal_and_never_reaches_the_fall_through_source() {
        let dir = TempDir::new().unwrap();
        let (spy_calls, error_idx) = fall_through_error_source();
        let chained = Index::from_chained(
            make_local_index(&dir),
            vec![Index::from_impl(AuthoritativeMissSource), error_idx],
            ChainMode::Default,
        );

        let result = chained.fetch_blob(&pinned_for_test()).await;
        assert!(
            matches!(result, Ok(None)),
            "an authoritative clean miss must end the blob walk as Ok(None), got: {result:?}"
        );
        assert_eq!(
            *spy_calls.lock().unwrap(),
            0,
            "the catch-all must never be asked once the authoritative source reported a clean miss"
        );

        // Paired positive: non-authoritative first source ⇒ the walk continues.
        let dir = TempDir::new().unwrap();
        let (spy_calls, error_idx) = fall_through_error_source();
        let (_, fall_through_miss) = make_source(TAG, digest_a());
        let chained = Index::from_chained(
            make_local_index(&dir),
            vec![fall_through_miss, error_idx],
            ChainMode::Default,
        );

        assert!(
            chained.fetch_blob(&pinned_for_test()).await.is_err(),
            "a fall-through source's miss must still walk on, and the next source's outage propagates"
        );
        assert_eq!(
            *spy_calls.lock().unwrap(),
            1,
            "the walk must have reached the second source"
        );
    }

    // ── physical_reference: local root answers when no source can ─────────

    const PHYSICAL_REGISTRY: &str = "ghcr.io";
    const PHYSICAL_REPO: &str = "ocx-contrib/cmake";

    /// A published root whose `repository` points somewhere OTHER than the
    /// logical identifier — the `index.ocx.sh` indirection (C2). Seeded through
    /// the same verbatim-copy write path a resolve or `ocx index update` uses.
    async fn seed_indirected_root(cache: &LocalIndex) {
        let bytes = format!(r#"{{"repository":"oci://{PHYSICAL_REGISTRY}/{PHYSICAL_REPO}","tags":{{}}}}"#);
        cache
            .seed_root_document(&PackageRef::new_registry(REPO, REGISTRY), bytes.as_bytes())
            .await
            .unwrap();
    }

    /// Assert `physical` is the indirected address, not the logical one. The
    /// negative half is the load-bearing one: `Ok(None)` from
    /// `physical_reference` makes `resolve_transport_pinned` report the LOGICAL
    /// identifier as its own transport, which succeeds just as loudly as a
    /// correct answer.
    fn assert_is_physical(physical: &ocx_oci::OciIdentifier) {
        assert_eq!(physical.registry(), PHYSICAL_REGISTRY);
        assert_eq!(physical.repository(), PHYSICAL_REPO);
        assert_eq!(
            physical.digest(),
            Some(digest_a()),
            "the logical digest must be carried onto the physical location"
        );
        assert_ne!(
            physical.registry(),
            REGISTRY,
            "the logical identifier reported as its own transport is the defect"
        );
    }

    /// A source that is asked but cannot answer: every physical dereference
    /// fails the way `OcxIndex` does with no network — its `GET
    /// <base>/config.json` never completes.
    #[derive(Clone)]
    struct UnreachableSource;

    #[async_trait]
    impl index_impl::IndexImpl for UnreachableSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn physical_reference(&self, _: &PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
            Err(super::super::error::Error::IndexHttpFailed {
                url: "https://index.example.com/config.json".to_string(),
                status: None,
                source: "connection refused".into(),
            })
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// A source that REFUSES a target rather than failing to reach one — the
    /// shape `OcxIndex` takes when its own SSRF pre-flight rejects a root's
    /// `repository` host. Structurally distinct from [`UnreachableSource`]:
    /// without it the whole error-holding design is exercised only on the one
    /// error class where holding happens to be correct.
    #[derive(Clone)]
    struct RefusingSource;

    #[async_trait]
    impl index_impl::IndexImpl for RefusingSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn physical_reference(&self, _: &PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
            Err(super::super::error::Error::Ssrf {
                source: ocx_oci::ssrf::PhysicalDialRefused {
                    namespace: "ocx.sh".to_string(),
                    source: ocx_oci::ssrf::SsrfError::ForbiddenTarget {
                        host: "127.0.0.1".to_string(),
                        ip: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                    },
                },
            })
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// A source that answers with its own distinct physical address — stands in
    /// for the `OcxIndex` whose answer has passed the SSRF pre-flight.
    ///
    /// Counts **every** question the chain asks it, `jurisdiction` as well as
    /// `physical_reference`: in production both are HTTP (a `GET
    /// <base>/config.json` for the jurisdiction declaration, a `GET
    /// <base>/p/<ns>/<pkg>.json` for the root), so a non-zero count here is a
    /// request on the wire on a resolve that already had every byte locally.
    #[derive(Clone, Default)]
    struct PhysicalSource {
        calls: Arc<Mutex<usize>>,
    }

    impl PhysicalSource {
        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for PhysicalSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn physical_reference(&self, identifier: &PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
            *self.calls.lock().unwrap() += 1;
            let mut physical =
                ocx_oci::OciIdentifier::from_parts("mirror.example.com/from-source", "mirror.example.com");
            if let Some(digest) = identifier.digest() {
                physical = physical.clone_with_digest(digest);
            }
            Ok(Some(physical))
        }
        fn jurisdiction(&self, _: &PackageRef) -> Jurisdiction {
            *self.calls.lock().unwrap() += 1;
            Jurisdiction::FallThrough
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// `--offline` builds the chain with NO sources, so the source loop cannot
    /// answer and the committed root is the only thing that knows the physical
    /// address. Without a local answer this returns `Ok(None)` and the caller
    /// silently reports the logical identifier as its own transport.
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_physical_reference_reports_the_physical_address_from_the_local_root() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_indirected_root(&cache).await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Offline);
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .unwrap()
            .expect("the committed root's `repository` pointer is the physical address");
        assert_is_physical(&physical);
    }

    /// No policy flag, no network: the committed root answers and the source's
    /// transport failure never escapes as exit 69. Under the local-first order
    /// the source is not even reached — the regression this guards is closed by
    /// construction rather than by the outage hold, which now only backs
    /// `--remote`.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreachable_index_site_does_not_stop_a_warm_resolve() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_indirected_root(&cache).await;

        let chained = Index::from_chained(cache, vec![Index::from_impl(UnreachableSource)], ChainMode::Default);
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .unwrap()
            .expect("a warm local copy must answer when the index site is unreachable");
        assert_is_physical(&physical);
    }

    /// Ordering lock, `Default`: the committed root answers and **no source is
    /// asked at all**.
    ///
    /// The count is the assertion, not the answer: a source-first order returns
    /// the same physical address whenever the source happens to agree, so only
    /// the absence of the two requests (`config.json` for jurisdiction, the root
    /// document for the pointer) distinguishes the orders. Every warm resolve —
    /// `install`, `exec`, `which`, `run`, `lock` — pays those two per
    /// invocation when the local read comes second, on data it already has.
    #[tokio::test(flavor = "multi_thread")]
    async fn default_mode_answers_from_the_local_root_without_asking_any_source() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_indirected_root(&cache).await;

        let spy = PhysicalSource::default();
        let chained = Index::from_chained(cache, vec![Index::from_impl(spy.clone())], ChainMode::Default);
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .unwrap()
            .expect("the committed root's `repository` pointer is the physical address");
        assert_is_physical(&physical);
        assert_eq!(
            spy.calls(),
            0,
            "a warm resolve must not touch the index site: the committed root already carries the pointer"
        );
    }

    /// `--frozen` freezes resolution to the local index, so the same local-first
    /// order applies — asserted separately because the mode split is a `match`
    /// on `ChainMode` and a wrong arm here is invisible to the `Default` test.
    #[tokio::test(flavor = "multi_thread")]
    async fn frozen_mode_answers_from_the_local_root_without_asking_any_source() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_indirected_root(&cache).await;

        let spy = PhysicalSource::default();
        let chained = Index::from_chained(cache, vec![Index::from_impl(spy.clone())], ChainMode::Frozen);
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .unwrap()
            .expect("the committed root answers under --frozen too");
        assert_is_physical(&physical);
        assert_eq!(
            spy.calls(),
            0,
            "--frozen resolves from the local index; it asks no source"
        );
    }

    /// `--remote` keeps the source-first order: the update family has just
    /// resolved the tag against the source (so the root really is memoized
    /// there) and deliberately wants the *live* pointer, not the snapshotted
    /// one. The local root can answer here, so only the order decides.
    #[tokio::test(flavor = "multi_thread")]
    async fn remote_mode_asks_the_source_ahead_of_the_local_root() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_indirected_root(&cache).await;

        let spy = PhysicalSource::default();
        let chained = Index::from_chained(cache, vec![Index::from_impl(spy.clone())], ChainMode::Remote);
        let physical = chained.physical_reference(&digest_only_id()).await.unwrap().unwrap();
        assert_eq!(
            physical.registry(),
            "mirror.example.com",
            "under --remote the source's live answer must win over the committed root"
        );
        assert!(spy.calls() > 0, "the source must have been asked");
    }

    /// Local-first is not local-only: a name the local copy knows nothing about
    /// still walks the sources under `Default`, which is what makes a first-time
    /// resolve work at all.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_miss_still_walks_the_sources_under_default() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);

        let spy = PhysicalSource::default();
        let chained = Index::from_chained(cache, vec![Index::from_impl(spy.clone())], ChainMode::Default);
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .unwrap()
            .expect("with no local root the source must be asked");
        assert_eq!(physical.registry(), "mirror.example.com");
        assert!(spy.calls() > 0, "the source must have been asked on a local miss");
    }

    /// The source walk's refusal semantics are unchanged by the mode split: on
    /// the local miss that reaches the walk under `Default`, a refusal still
    /// propagates instead of degrading to `Ok(None)` (which the caller reads as
    /// "no rewrite" and turns into the logical identifier).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_refusal_on_a_local_miss_propagates_under_default() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);

        let chained = Index::from_chained(cache, vec![Index::from_impl(RefusingSource)], ChainMode::Default);
        let error = chained
            .physical_reference(&digest_only_id())
            .await
            .expect_err("an SSRF refusal must not degrade to a clean no-rewrite");
        assert!(
            matches!(error, super::super::error::Error::Ssrf { .. }),
            "expected the source's own SSRF refusal, got: {error:?}"
        );
    }

    /// A registry-backed package has no local root and no source rewrite:
    /// `Ok(None)` = physical == logical, the answer `resolve_transport_pinned`
    /// turns into the logical identifier on purpose.
    #[tokio::test(flavor = "multi_thread")]
    async fn no_local_root_and_no_source_is_a_clean_no_rewrite() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let chained = Index::from_chained(cache, vec![], ChainMode::Offline);
        assert!(
            chained.physical_reference(&digest_only_id()).await.unwrap().is_none(),
            "no root anywhere means no known rewrite"
        );
    }

    /// A damaged index home is a miss, never fatal. `OCX_INDEX` can point at a
    /// half-written directory, a failing mount, or (as here) a plain file — the
    /// physical lookup must not be the one place that turns a resolve which was
    /// going to succeed into a hard error.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreadable_index_home_is_a_miss_not_an_error() {
        let dir = TempDir::new().unwrap();
        // A file where the index home directory belongs: every read under it
        // fails with ENOTDIR rather than reporting a clean absence.
        std::fs::write(dir.path().join("index"), b"not a directory").unwrap();

        let chained = Index::from_chained(make_local_index(&dir), vec![], ChainMode::Offline);
        assert!(
            chained
                .physical_reference(&digest_only_id())
                .await
                .expect("an unreadable index home must not fail the lookup")
                .is_none(),
            "an unreadable index home knows no rewrite"
        );
    }

    // ── SSRF floor on the local answer (F1) + refusal vs outage (F2) ──────

    /// Seed a published root whose `repository` names an arbitrary physical
    /// location, under an arbitrary logical source.
    async fn seed_root_pointing_at(cache: &LocalIndex, logical_registry: &str, physical: &str) {
        let bytes = format!(r#"{{"repository":"oci://{physical}","tags":{{}}}}"#);
        cache
            .seed_root_document(&PackageRef::new_registry(REPO, logical_registry), bytes.as_bytes())
            .await
            .unwrap();
    }

    /// **F1.** A committed root's `repository` is remote-controlled data — a
    /// copied index tree is a supported distribution mechanism — and the layer
    /// pull that consumes this answer runs on the shared, UNGUARDED client. A
    /// local root naming a loopback target must therefore be refused here, with
    /// no source able to answer for the name.
    ///
    /// The forbidden target is a **real listening socket**, so the un-guarded
    /// edge is a live repro rather than a type assertion: if the guard is
    /// removed, the returned physical address is dialled and the connection is
    /// accepted, and the failure message says so.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_root_pointing_at_loopback_is_refused_before_the_pull_can_dial_it() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let forbidden = listener.local_addr().unwrap();

        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_root_pointing_at(&cache, REGISTRY, &format!("{forbidden}/evil/pkg")).await;

        // Default mode, online shape: the local root is the answer — by the
        // local-first order, and by the source being unable to answer anyway.
        // Exactly the position every warm resolve is in.
        let chained = Index::from_chained(cache, vec![Index::from_impl(UnreachableSource)], ChainMode::Default)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        match chained.physical_reference(&digest_only_id()).await {
            Err(super::super::error::Error::Ssrf {
                source:
                    ocx_oci::ssrf::PhysicalDialRefused {
                        source: ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. },
                        ..
                    },
            }) => {}
            Err(other) => panic!("expected an SSRF refusal of the loopback target, got: {other:?}"),
            Ok(None) => panic!("the local root answers; `Ok(None)` would mean the read itself regressed"),
            Ok(Some(physical)) => {
                // The guard is gone. Prove the answer is a live transport target
                // by dialling it the way the unguarded pull client would.
                let dialled = tokio::net::TcpStream::connect(forbidden).await;
                panic!(
                    "physical_reference handed the pull the forbidden target '{physical}'; \
                     a connection to {forbidden} {}",
                    if dialled.is_ok() {
                        "WAS ACCEPTED"
                    } else {
                        "failed (the socket died first)"
                    }
                );
            }
        }
    }

    /// The same refusal against a source that **can** answer: a local guard
    /// refusal propagates without consulting a source, so a healthy source cannot
    /// launder a forbidden local pointer into a clean answer.
    ///
    /// Its twin above pairs the forbidden root with [`UnreachableSource`], which
    /// cannot answer under either ordering — a guard error that regressed into a
    /// swallowed miss would surface there as the source's own `IndexHttpFailed`,
    /// which is still an `Err` and still not the forbidden target. Here the source
    /// answers, so the swallowed-refusal shape is `Ok(Some(mirror.example.com))`:
    /// the pull proceeds, and the only trace the guard ever ran is a log line.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_guard_refusal_is_not_laundered_by_a_source_that_can_answer() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_root_pointing_at(&cache, REGISTRY, "127.0.0.1:5999/evil/pkg").await;

        let spy = PhysicalSource::default();
        let chained = Index::from_chained(cache, vec![Index::from_impl(spy.clone())], ChainMode::Default)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        let error = chained
            .physical_reference(&digest_only_id())
            .await
            .expect_err("a forbidden local pointer must be refused, never answered around");
        assert!(
            matches!(
                error,
                super::super::error::Error::Ssrf {
                    source: ocx_oci::ssrf::PhysicalDialRefused {
                        source: ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. },
                        ..
                    },
                }
            ),
            "expected the local root's own SSRF refusal, got: {error:?}"
        );
        assert_eq!(
            spy.calls(),
            0,
            "the refusal must propagate before any source is asked — consulting one is what makes laundering possible"
        );
    }

    /// The guard judges a **rewrite**, never a root that names the identifier's
    /// own registry. Every OCX-authored derived root is that shape, so guarding
    /// it would refuse every private registry that worked before indices existed
    /// — including namespaces with no `index` declared and therefore no source
    /// to carry a `trusted_hosts` exemption.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_root_naming_its_own_registry_is_not_a_rewrite_and_is_not_guarded() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let private = listener.local_addr().unwrap().to_string();

        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_root_pointing_at(&cache, &private, &format!("{private}/{REPO}")).await;

        let logical = PackageRef::new_registry(REPO, &private).clone_with_digest(digest_a());
        let chained = Index::from_chained(cache, vec![], ChainMode::Default)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        let physical = chained
            .physical_reference(&logical)
            .await
            .expect("a private registry naming itself must not be refused")
            .expect("the committed root answers");
        assert_eq!(physical.registry(), private);
    }

    /// A local index carrying the operator's configured `trusted_hosts` for
    /// `namespace` — what `context.rs` threads in from
    /// `[registries."<ns>"].trusted_hosts`.
    fn local_index_trusting(dir: &TempDir, namespace: &str, hosts: &[&str]) -> LocalIndex {
        make_local_index(dir).with_trusted_hosts(HashMap::from([(
            namespace.to_string(),
            hosts.iter().map(|host| (*host).to_string()).collect::<Vec<_>>(),
        )]))
    }

    /// **`--offline` is guarded too.** It used to be a carve-out on the premise
    /// that offline builds no OCI client (nothing could follow the answer) and
    /// no sources (no `trusted_hosts` to read). Both halves are false:
    /// `context.rs` builds the registry client in every mode so `ocx package
    /// verify` can read a signature referrer offline, and the exemption now
    /// rides the local index. Offline is in fact the shape with the weakest
    /// other protection — no source answer to prefer, so the local root always
    /// decides.
    ///
    /// The forbidden target is a **real listening socket**, so removing the
    /// guard is a live repro rather than a type assertion: the returned address
    /// is dialled and the connection accepted, and the failure says so.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_offline_local_root_pointing_at_loopback_is_refused_without_a_trusted_hosts_entry() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let forbidden = listener.local_addr().unwrap();

        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_root_pointing_at(&cache, REGISTRY, &format!("{forbidden}/evil/pkg")).await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Offline)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        match chained.physical_reference(&digest_only_id()).await {
            Err(super::super::error::Error::Ssrf {
                source:
                    ocx_oci::ssrf::PhysicalDialRefused {
                        source: ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. },
                        ..
                    },
            }) => {}
            Err(other) => panic!("expected an SSRF refusal of the loopback target, got: {other:?}"),
            Ok(None) => panic!("the local root answers; `Ok(None)` would mean the read itself regressed"),
            Ok(Some(physical)) => {
                let dialled = tokio::net::TcpStream::connect(forbidden).await;
                panic!(
                    "offline physical_reference handed the pull the forbidden target '{physical}'; \
                     a connection to {forbidden} {}",
                    if dialled.is_ok() {
                        "WAS ACCEPTED"
                    } else {
                        "failed (the socket died first)"
                    }
                );
            }
        }
    }

    /// The other direction, and the reason the floor cannot simply refuse every
    /// private target offline: an air-gapped deployment legitimately points at a
    /// private mirror. Declaring `[registries."<ns>"].trusted_hosts` is what
    /// makes it work — the same exemption, and the same CIDR grammar, that
    /// `OcxIndex` applies to its own answers online.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_offline_private_mirror_is_allowed_when_the_namespace_declares_it_trusted() {
        let dir = TempDir::new().unwrap();
        let cache = local_index_trusting(&dir, REGISTRY, &["10.0.0.0/8"]);
        seed_root_pointing_at(&cache, REGISTRY, "10.0.0.7/private/cmake").await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Offline)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .expect("a declared trusted_hosts entry must admit the air-gapped private mirror")
            .expect("the committed root answers");
        assert_eq!(physical.registry(), "10.0.0.7");
    }

    /// The exemption is keyed per namespace, never a union: one namespace's
    /// declaration must not admit a forbidden target under another's. This is
    /// also the negative half of the air-gapped case — the identical root is
    /// refused when the operator has NOT declared it for this namespace.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_trusted_hosts_entry_for_another_namespace_never_exempts_this_one() {
        let dir = TempDir::new().unwrap();
        let cache = local_index_trusting(&dir, "other.example.com", &["10.0.0.0/8"]);
        seed_root_pointing_at(&cache, REGISTRY, "10.0.0.7/private/cmake").await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Offline)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        let error = chained
            .physical_reference(&digest_only_id())
            .await
            .expect_err("another namespace's exemption must not widen this one");
        assert!(
            matches!(
                error,
                super::super::error::Error::Ssrf {
                    source: ocx_oci::ssrf::PhysicalDialRefused {
                        source: ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. },
                        ..
                    },
                }
            ),
            "expected a ForbiddenTarget refusal, got: {error:?}"
        );
    }

    /// Guarding offline must not break the steady state it exists in: a warm
    /// store, no working resolver, and a root naming a genuine DNS host. The
    /// lookup fails, [`is_plain_dns_name`] tolerates it, and the resolve
    /// proceeds — refusing here would re-raise the exit-69 bug the local
    /// fallback was added to fix. On a machine that CAN resolve, the same host
    /// passes the floor as an ordinary public address; either way the answer is
    /// the physical address, never a refusal.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_offline_root_naming_an_unresolvable_dns_host_still_answers() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_root_pointing_at(&cache, REGISTRY, "registry.invalid/mirrored/cmake").await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Offline)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .expect("a DNS lookup failure on a plain DNS name is tolerated, not a refusal")
            .expect("the committed root answers");
        assert_eq!(physical.registry(), "registry.invalid");
    }

    /// The guard's **tolerated lookup failure** arm ([`is_plain_dns_name`]): a
    /// physical host that is a genuine DNS name and does not resolve is admitted,
    /// because that is the steady state of the warm-store-no-network case
    /// local-first serves — and a connect can no more succeed than the lookup did.
    /// Refusing here re-raises the exit-69 bug local-first closes, and local-first
    /// makes this arm the ordinary warm path rather than an edge.
    ///
    /// The pre-condition assertion is what proves the arm: `.invalid` is reserved
    /// by RFC 6761 and never resolves, so the answer below can only have come from
    /// a tolerated failure, not from a host that quietly resolved fine.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_root_naming_an_unresolvable_dns_host_is_tolerated_by_the_guard() {
        const UNRESOLVABLE: &str = "physical.invalid";
        let (host, port) = ocx_oci::ssrf::split_host_port(UNRESOLVABLE);
        assert!(
            matches!(
                ocx_oci::ssrf::resolve_and_validate(host, port, &[]).await,
                Err(ocx_oci::ssrf::SsrfError::Resolution { .. })
            ),
            "the fixture must genuinely fail to resolve, or this test says nothing about the tolerated arm"
        );

        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_root_pointing_at(&cache, REGISTRY, &format!("{UNRESOLVABLE}/evil/pkg")).await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Default)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .expect("a DNS name that does not resolve must not fail the warm read")
            .expect("the committed root answers");
        assert_eq!(physical.registry(), UNRESOLVABLE);
    }

    /// The fail-closed sibling of the arm above: an **address-shaped** authority
    /// whose lookup fails is refused. [`ocx_oci::ssrf::split_host_port`](ocx_oci::ssrf::split_host_port)
    /// deliberately leaves a bracketed IPv6 authority bracketed, so `getaddrinfo`
    /// rejects it while a URL parser accepts those brackets natively — tolerating
    /// this lookup failure would hand the pull a loopback target the guard never
    /// judged.
    ///
    /// The `.invalid` suffix is what keeps the test deterministic *and* fast: a
    /// bare `[::1]` carries no dot, so the resolver expands it against the search
    /// list and the query is dropped rather than answered — measured at 120 s on a
    /// glibc host, which is a network-latency time bomb, not a unit test. With the
    /// suffix the name is absolute and RFC 6761 guarantees the NXDOMAIN.
    ///
    /// The pre-condition separates this from the [`SsrfError::ForbiddenTarget`](ocx_oci::ssrf::SsrfError::ForbiddenTarget)
    /// arm the loopback tests cover: the refusal below must come from an
    /// unjudgeable host, not from one judged and found forbidden.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_root_naming_an_unresolvable_address_shaped_host_fails_closed() {
        const BRACKETED_LOOPBACK: &str = "[::1].invalid:5000";
        let (host, port) = ocx_oci::ssrf::split_host_port(BRACKETED_LOOPBACK);
        assert!(
            !super::is_plain_dns_name(host),
            "the fixture must be classified address-shaped, or the tolerated arm runs instead"
        );
        assert!(
            matches!(
                ocx_oci::ssrf::resolve_and_validate(host, port, &[]).await,
                Err(ocx_oci::ssrf::SsrfError::Resolution { .. })
            ),
            "the fixture must fail to RESOLVE, not resolve-and-be-forbidden, or this test duplicates the loopback one"
        );

        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_root_pointing_at(&cache, REGISTRY, &format!("{BRACKETED_LOOPBACK}/evil/pkg")).await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Default)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());
        let error = chained
            .physical_reference(&digest_only_id())
            .await
            .expect_err("an unjudgeable address-shaped host must not be admitted");
        assert!(
            matches!(
                error,
                super::super::error::Error::Ssrf {
                    source: ocx_oci::ssrf::PhysicalDialRefused {
                        source: ocx_oci::ssrf::SsrfError::Resolution { .. },
                        ..
                    },
                }
            ),
            "expected the guard's fail-closed resolution refusal, got: {error:?}"
        );
    }

    /// The proxied half of the sibling above: configuring a proxy must not
    /// launder an address-shaped authority past the floor.
    ///
    /// `[::1].invalid:5000` is not spellable as a URL host, so it never
    /// normalises into a destination the matcher can intercept — the route
    /// degrades to `Direct` (`ProxyRules::dial_route`'s fail-closed
    /// direction), the resolving floor runs, and the lookup failure is refused
    /// because the authority is address-shaped. Without that degradation a
    /// proxied route would skip the lookup and admit it on the text alone.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_local_root_naming_an_address_shaped_host_fails_closed_under_a_proxy_too() {
        const BRACKETED_LOOPBACK: &str = "[::1].invalid:5000";
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_root_pointing_at(&cache, REGISTRY, &format!("{BRACKETED_LOOPBACK}/evil/pkg")).await;

        let chained = Index::from_chained(cache, vec![], ChainMode::Default)
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::proxied_everywhere("http://proxy.corp:3128"));
        let error = chained
            .physical_reference(&digest_only_id())
            .await
            .expect_err("a proxy must not turn an unjudgeable address-shaped host into an admission");
        assert!(
            matches!(
                error,
                super::super::error::Error::Ssrf {
                    source: ocx_oci::ssrf::PhysicalDialRefused {
                        source: ocx_oci::ssrf::SsrfError::Resolution { .. },
                        ..
                    },
                }
            ),
            "expected the direct route's fail-closed resolution refusal, got: {error:?}"
        );
    }

    /// **F2.** A source REFUSAL is not an outage. Holding it and then answering
    /// from the local root discards the verdict the source was asked for — the
    /// guard fires and is then overridden. The local root here can answer, so
    /// only propagation distinguishes the two.
    ///
    /// `Remote` is the mode where the source walk runs with a local root
    /// present, so it is the only one where "answered around locally" is even
    /// expressible; the `Default` half of this invariant (a refusal on the local
    /// miss that reaches the walk) is
    /// [`a_source_refusal_on_a_local_miss_propagates_under_default`].
    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_refusal_propagates_instead_of_being_answered_around_locally() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_indirected_root(&cache).await;

        let chained = Index::from_chained(cache, vec![Index::from_impl(RefusingSource)], ChainMode::Remote);
        let error = chained
            .physical_reference(&digest_only_id())
            .await
            .expect_err("an SSRF refusal must not be overridden by the local root");
        assert!(
            matches!(error, super::super::error::Error::Ssrf { .. }),
            "expected the source's own SSRF refusal, got: {error:?}"
        );
    }

    /// The refusal/outage split cuts both ways: a transport outage from the SAME
    /// chain shape is still held, so [`is_source_outage`] is not simply
    /// "propagate everything". Asserted under `Remote` for the same reason the
    /// refusal twin above is: it is the mode whose source walk runs with a local
    /// root present, so held-then-answered-locally is observable there.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_source_outage_is_still_held_while_a_refusal_is_not() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_indirected_root(&cache).await;

        let chained = Index::from_chained(cache, vec![Index::from_impl(UnreachableSource)], ChainMode::Remote);
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .expect("a transport outage is held, not propagated")
            .expect("the local root answers around the outage");
        assert_is_physical(&physical);
    }

    // ── the same two invariants, driven by a REAL `OcxIndex` ─────────────
    //
    // Both stub-driven guards above emit a **bare** `IndexHttpFailed`, which no
    // real source has returned since its `config.json` / root fetches were
    // coalesced: a singleflight leader hands its error to the broadcast and
    // returns `SourceFetchFailed(ArcError)` instead. So neither stub can
    // express the shape `is_source_outage` actually meets in production, and
    // both stayed green while the offline fallback was broken. These two drive
    // `OcxIndex` itself over an unreachable transport, which is the only way to
    // produce the wrapper.

    /// An [`IndexTransport`] whose every request fails at the transport layer.
    /// The index site is unreachable and has no other symptom — the warm
    /// machine, network down.
    #[derive(Clone)]
    struct UnreachableIndexTransport;

    #[async_trait]
    impl IndexTransport for UnreachableIndexTransport {
        async fn get(&self, url: &str) -> Result<IndexFetch> {
            Err(super::super::error::Error::IndexHttpFailed {
                url: url.to_string(),
                status: None,
                source: "connection refused".into(),
            })
        }
        fn box_clone(&self) -> Box<dyn IndexTransport> {
            Box::new(self.clone())
        }
    }

    /// **The owner's red line, under concurrency.** A coalesced fetch has two
    /// losing shapes for one failure: the **leader** returns
    /// `SourceFetchFailed`, and every **waiter** that lost the race receives the
    /// same failure as `SingleflightFailed(Failed(..))`. Peeling only the leader
    /// would make "outage held" vs "outage propagated" depend on which caller
    /// happened to win — and the group exists *because* there is a concurrent
    /// fan-out, so the waiter shape is the common one under load.
    ///
    /// Drives the real primitive rather than fabricating the shape: a genuine
    /// leader handle is failed through `broadcast_failure`, and the waiter's
    /// error is the one `try_acquire` actually hands back, mapped exactly as
    /// production maps it.
    ///
    /// *Red-reachability:* with only the leader form peeled, the waiter
    /// assertion below fails. Demonstrated red before the peel was added.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_waiters_outage_is_held_just_like_the_leaders() {
        use ocx_util::singleflight::{Acquisition, Group};

        let group: Group<(), Option<u8>> = Group::new(1, std::time::Duration::from_secs(5));
        let handle = match group.try_acquire(()).await.expect("the first caller acquires") {
            Acquisition::Leader(handle) => handle,
            Acquisition::Resolved(_) => panic!("the first caller must lead an empty group"),
        };

        let waiting = group.clone();
        let waiter = tokio::spawn(async move { waiting.try_acquire(()).await });
        // The waiter must be parked on the leader's channel *before* the failure
        // lands: eviction-on-read means a caller arriving afterwards finds the
        // entry gone and is handed fresh leadership instead — the sequential
        // case, which is already safe and is not what this test is about. The
        // `Ok(_)` arm below fails loudly if that is what happened.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let outage = super::super::error::Error::IndexHttpFailed {
            url: "https://index.example.com/config.json".to_string(),
            status: None,
            source: "connection refused".into(),
        };
        let leader_error = super::super::error::broadcast_failure(handle, outage);

        let cause = match waiter.await.expect("the waiter task joins") {
            Err(cause) => cause,
            Ok(_) => panic!(
                "non-vacuity: the waiter must observe the leader's failure, not be handed fresh \
                 leadership — it did not park before the failure landed"
            ),
        };
        let waiter_error = super::super::error::Error::SingleflightFailed(cause);

        assert!(
            super::is_source_outage(&leader_error),
            "the leader's outage must be held, got: {leader_error:?}"
        );
        assert!(
            super::is_source_outage(&waiter_error),
            "the WAITER's outage must be held on the same terms as the leader's, got: {waiter_error:?}"
        );
    }

    /// A real published source for [`REGISTRY`] that cannot be reached.
    ///
    /// The OCI client underneath is never dialled: every path here fails at the
    /// index transport, before a physical reference exists to fetch with.
    fn unreachable_ocx_source() -> Index {
        Index::from_impl(OcxIndex::new(OcxIndexConfig {
            transport: Box::new(UnreachableIndexTransport),
            base_url: "https://index.example.com".to_string(),
            namespace: REGISTRY.to_string(),
            client: ocx_oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new()))),
            allow_yanked: false,
            trusted_hosts: Vec::new(),
            insecure_hosts: Vec::new(),
            proxy_rules: ocx_oci::ssrf::ProxyRules::direct(),
        }))
    }

    /// **The owner's red line, end to end.** A warm machine whose index site is
    /// unreachable resolves from the committed local root — it does not exit 69.
    ///
    /// `Remote` because that is the mode whose source walk runs *before* the
    /// local read, so "the outage was held rather than propagated" is
    /// observable; under the local-first modes the committed root answers before
    /// a source is ever asked.
    ///
    /// *Red-reachability:* this is the guard the stub-driven
    /// [`a_source_outage_is_still_held_while_a_refusal_is_not`] could not be.
    /// Matching `is_source_outage` against the error **as returned** rather than
    /// against its `coalesced_cause` fails here — the leader's wrapper reads as
    /// a refusal, propagates, and the local root is never consulted.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_real_sources_outage_is_held_and_the_committed_root_answers() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        seed_indirected_root(&cache).await;

        let chained = Index::from_chained(cache, vec![unreachable_ocx_source()], ChainMode::Remote);
        let physical = chained
            .physical_reference(&digest_only_id())
            .await
            .expect("a real source's transport outage is held, not propagated")
            .expect("the committed local root answers around the outage");
        assert_is_physical(&physical);
    }

    /// The re-raise half against the same real source: with no local root the
    /// outage still fails loudly, and it still exits 69.
    ///
    /// The exit code is the assertion that carries, not the variant: what
    /// re-raises is the leader's transparent `SourceFetchFailed` wrapper, so a
    /// structural `matches!` on `IndexHttpFailed` — what the stub-driven twin
    /// below asserts — would fail here despite the behaviour being identical.
    /// `Unavailable` (69) is what a script actually branches on, and it reads
    /// through the wrapper.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_real_sources_outage_still_exits_unavailable_when_no_local_root_answers() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let chained = Index::from_chained(cache, vec![unreachable_ocx_source()], ChainMode::Default);
        let error = chained
            .physical_reference(&digest_only_id())
            .await
            .expect_err("with no local root the source failure must surface");
        assert!(
            matches!(
                super::super::error::coalesced_cause(&error),
                super::super::error::Error::IndexHttpFailed { .. }
            ),
            "expected the source's own transport error under the leader's wrapper, got: {error:?}"
        );
    }

    /// The held source error is re-raised when the local copy cannot answer
    /// either — a genuine outage still fails loudly instead of silently
    /// degrading to the logical address.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreachable_source_still_errors_when_the_local_copy_has_no_root() {
        let dir = TempDir::new().unwrap();
        let cache = make_local_index(&dir);
        let chained = Index::from_chained(cache, vec![Index::from_impl(UnreachableSource)], ChainMode::Default);
        let error = chained
            .physical_reference(&digest_only_id())
            .await
            .expect_err("with no local root the source failure must surface");
        assert!(
            matches!(error, super::super::error::Error::IndexHttpFailed { .. }),
            "expected the source's own transport error, got: {error:?}"
        );
    }
}

// ── Specification tests ───────────────────────────────────────────────────
//
// Expected ChainedIndex behaviour: a cached tag returns without walking a source; a miss persists what
// a source has; a source miss or network failure warns and reports not found; a digest-only ref never
// falls back; `box_clone` shares caches; listings read the local index only; empty sources act like `LocalIndex`.
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use tempfile::TempDir;

    use crate::error::Result;
    use crate::{Index, IndexStore, LocalConfig, LocalIndex, index_impl};
    use ocx_oci::{Algorithm, Digest, Manifest, PackageRef};

    // ── Test helpers ──────────────────────────────────────────────────────

    const REGISTRY: &str = "example.com";
    const REPO: &str = "cmake";
    const TAG: &str = "3.28";

    fn tagged_id() -> PackageRef {
        PackageRef::new_registry(REPO, REGISTRY).clone_with_tag(TAG)
    }

    fn digest_only_id() -> PackageRef {
        PackageRef::new_registry(REPO, REGISTRY).clone_with_digest(digest_a())
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

        async fn list_tags(&self, _identifier: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(self.known_tags.keys().cloned().collect()))
        }

        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            _op: super::super::IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            if let Some(msg) = &self.force_error {
                // Use a RemoteManifestNotFound error to simulate registry errors.
                // The exact variant is unimportant for these tests — only that an
                // error is returned so ChainedIndex's degradation logic is exercised.
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()));
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
            identifier: &PackageRef,
            _op: super::super::IndexOperation,
        ) -> Result<Option<Digest>> {
            if let Some(msg) = &self.force_error {
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()));
            }
            let tag = identifier.tag_or_latest();
            self.calls.lock().unwrap().push(tag.to_string());
            Ok(self.known_tags.get(tag).cloned())
        }

        async fn fetch_blob(&self, _blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            if let Some(msg) = &self.force_error {
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()));
            }
            Ok(None)
        }

        async fn fetch_manifest_raw_bytes(
            &self,
            identifier: &PackageRef,
        ) -> Result<Option<(Vec<u8>, Digest, Manifest)>> {
            if let Some(msg) = &self.force_error {
                return Err(super::super::error::Error::RemoteManifestNotFound(msg.clone()));
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
    async fn seed_full(cache: &LocalIndex, identifier: &PackageRef, _d: Digest, source: &Index) {
        let (_bytes, digest, _manifest) = cache
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

        let bare = PackageRef::new_registry(REPO, REGISTRY);
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

        let bare = PackageRef::new_registry(REPO, REGISTRY);
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

    // ── C-005 through the production chain mode ───────────────────────────
    //
    // The version gate is only a gate if the layer above propagates it. These
    // drive `ChainMode::Default` — the mode ocx ships — because a refusal that
    // holds in `LocalIndex::resolve_dispatch` and dissolves one frame up is
    // advisory, and both `fetch_manifest` and `fetch_manifest_digest` classify
    // local errors on their own.

    /// Seed a healthy local subtree — root tag pointer plus dispatch object —
    /// and return a fresh spy source for the chained index under test.
    ///
    /// Each caller plants its own `config.json`: C-022's structural test bounds
    /// who may name that path outside a test body, and a shared helper counts
    /// as a production namer.
    async fn seed_healthy_subtree(dir: &TempDir) -> TestIndex {
        let cache = make_local_index(dir);
        seed_full(
            &cache,
            &tagged_id(),
            digest_a(),
            &make_source(TestIndex::with_tag(TAG, digest_a())),
        )
        .await;
        TestIndex::with_tag(TAG, digest_a())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_format_version_propagates_through_the_default_chain() {
        let dir = TempDir::new().unwrap();
        let spy = seed_healthy_subtree(&dir).await;
        let config = index_store(&dir).source_config_path(REGISTRY);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, br#"{"format_version":2}"#).unwrap();
        let chained = Index::from_chained(
            make_local_index(&dir),
            vec![make_source(spy.clone())],
            super::super::ChainMode::Default,
        );

        for error in [
            chained
                .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
                .await
                .expect_err("a v2 tree must refuse the read, not degrade to a source walk"),
            chained
                .fetch_manifest_digest(&tagged_id(), super::super::IndexOperation::Resolve)
                .await
                .expect_err("the digest reader classifies local errors separately — same rule"),
        ] {
            assert!(
                matches!(error, super::super::error::Error::UnsupportedIndexFormat { version: 2 }),
                "expected UnsupportedIndexFormat{{2}}, got {error:?}"
            );
        }
        // The second half of the finding: a swallowed refusal lets the walk
        // GROW the refused tree, writing v1-shaped documents into a tree
        // declaring v2 — which `ensure_source_config` then never corrects,
        // being write-if-absent.
        assert!(
            spy.calls.lock().unwrap().is_empty(),
            "a refused tree must never be walked, and so never grown"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unparseable_config_refuses_rather_than_hiding_the_subtree() {
        // A gate that cannot be evaluated fails closed. Degrading to a miss
        // would make every package under the source silently invisible — a
        // cache bypass, and a "not found" under `--frozen`/`Offline`.
        let dir = TempDir::new().unwrap();
        let spy = seed_healthy_subtree(&dir).await;
        let config = index_store(&dir).source_config_path(REGISTRY);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, b"{not json").unwrap();
        let chained = Index::from_chained(
            make_local_index(&dir),
            vec![make_source(spy.clone())],
            super::super::ChainMode::Default,
        );

        let error = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect_err("an unreadable version gate is a refusal, not a local miss");
        assert!(
            matches!(error, super::super::error::Error::MalformedIndexDocument { .. }),
            "expected MalformedIndexDocument, got {error:?}"
        );
        assert!(spy.calls.lock().unwrap().is_empty(), "and the source is not consulted");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreadable_config_refuses_rather_than_hiding_the_subtree() {
        use std::os::unix::fs::PermissionsExt;

        // C-003's fourth row, one layer up: a permission failure read as
        // absence promotes an unreadable tree to a valid v1 index; read as a
        // MISS it hides an otherwise healthy subtree. Neither is acceptable.
        let dir = TempDir::new().unwrap();
        let spy = seed_healthy_subtree(&dir).await;
        let path = index_store(&dir).source_config_path(REGISTRY);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"format_version":1}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root (CAP_DAC_OVERRIDE) reads it anyway — the contract holds, it is
        // just not observable here.
        if std::fs::read(&path).is_ok() {
            return;
        }

        let chained = Index::from_chained(
            make_local_index(&dir),
            vec![make_source(spy.clone())],
            super::super::ChainMode::Default,
        );
        let error = chained
            .fetch_manifest(&tagged_id(), super::super::IndexOperation::Resolve)
            .await
            .expect_err("an unreadable config.json must not flatten to a local miss");
        assert!(
            matches!(error, crate::error::Error::File(_)),
            "expected the file_error I/O wrapper, got {error:?}"
        );
        assert!(spy.calls.lock().unwrap().is_empty(), "and the source is not consulted");
    }
}
