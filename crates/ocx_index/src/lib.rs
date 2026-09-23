// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The OCX resolution-index protocol and its local collection.

use ocx_oci::tag::is_reserved_tag;
use ocx_util::prelude::*;

use error::Result;

pub mod error;

pub use self::ocx_index::{
    IndexBase, IndexFetch, IndexTransport, OcxIndex, OcxIndexConfig, ReqwestIndexTransport, parse_physical_repository,
};
pub use file_transport::FileIndexTransport;
pub use local_index::Config as LocalConfig;
pub use local_index::LocalIndex;
pub use local_index::TAG_REFRESH_CONCURRENCY;
pub use oci_index::OciIndex;
pub use oci_index::OciIndexConfig;
pub use regenerate::{RegenerateOutcome, regenerate_catalog};
pub use store::{CatalogEntryStatus, CatalogTransaction, IndexStore, RootReadResult, SOURCE_LOCK_TIMEOUT};
pub use wire::{
    CatalogDocument, CatalogIndex, IndexFormatConfig, IndexRoot, RootTag, SUPPORTED_FORMAT_VERSION, YankMarker,
};
pub use wire_writer::{serialize_catalog, serialize_config, serialize_root};

mod chained_index;
mod file_transport;
mod index_impl;
mod local_index;
mod oci_index;
mod ocx_index;
mod regenerate;
mod store;
mod wire;
pub mod wire_writer;

// ── Shared clock ──
//
// One instant, two renderings (announce + claim design record, C-005/C-006).
// `current_date` is defined in terms of `current_timestamp` rather than taking
// its own reading, so the two never straddle midnight and disagree.

/// The current instant, rendered in the index bot's `%Y-%m-%dT%H:%M:%SZ`
/// seconds-Z form.
///
/// Used for the `observed` timestamp on new/changed tags (announce) and the
/// analogous timestamp fields a claim writes. The `__OCX_TESTING_ANNOUNCE_CLOCK`
/// env seam (test / `__testing` builds only) pins this so acceptance tests get
/// byte-deterministic output; production reads the wall clock.
pub fn current_timestamp() -> String {
    #[cfg(any(test, feature = "__testing"))]
    {
        if let Ok(fixed) = std::env::var("__OCX_TESTING_ANNOUNCE_CLOCK") {
            return fixed;
        }
    }
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The current instant, rendered as a bare `%Y-%m-%d` date.
///
/// This is the first ten characters of [`current_timestamp`]'s own render of
/// the *same* instant, never an independent `Utc::now()` read — two
/// independent reads can straddle midnight and disagree, which is the defect
/// this contract exists to prevent. Used for the claim root's `created` field.
pub fn current_date() -> String {
    let timestamp = current_timestamp();
    // A pin shorter than ten ASCII bytes panics here by design: a malformed
    // test pin must fail loudly at the seam, not silently yield a truncated
    // "date" that then propagates into a written root. Unreachable outside a
    // `__testing` build, where `__OCX_TESTING_ANNOUNCE_CLOCK` is the only source.
    timestamp[..10].to_string()
}

/// Re-export the private `IndexImpl` trait for sibling-module tests.
///
/// Tests that need to construct an `Index` from a hand-rolled mock must
/// implement `IndexImpl`. Production code reaches `Index` only via
/// [`Index::from_chained`] / [`Index::from_remote`], which is why this is
/// behind the seam rather than plainly `pub`: the callers are unit tests in
/// `ocx_lib` (`project::resolve`, `package_manager::tasks::*`), which reach it
/// through a `__testing` dev edge, and no production path may.
#[cfg(any(test, feature = "__testing"))]
pub use index_impl::IndexImpl;

/// Whether a chain source will answer for a given name, and what its silence
/// means (`adr_index_indirection.md` F3/H).
///
/// A distinct type returned by a distinct method, consulted **before** a source
/// is asked — so "outside jurisdiction" can never be confused with the
/// `Ok(None)` a fetch returns. An out-of-jurisdiction source is never fetched
/// from, so there is no fetch outcome to misread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jurisdiction {
    /// Ask it; its miss or refusal is terminal for the chain. A registry that
    /// happens to serve the same name must never shadow this source's answer.
    Authoritative,
    /// Ask it; its miss falls through to the next source (the OCI catch-all).
    FallThrough,
    /// The name is in another registry altogether — never ask it, and its
    /// silence decides nothing.
    ///
    /// This is the verdict's **only** remaining meaning (ocx#251). A configured
    /// index used to be able to decline an individual name it declared its
    /// grammar could not express, handing it to the plain registry; it no longer
    /// can, so no source ever declines a name inside a registry it serves.
    Outside,
}

/// Routing policy for a [`ChainedIndex`](chained_index::ChainedIndex).
///
/// Threaded through `Index::from_chained` and on into the chained index so
/// that callers can pick the right cache/source policy without changing the
/// `IndexImpl` trait. The parameter is threaded end-to-end; each variant
/// below documents its own cache/source routing behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainMode {
    /// Local-index first for all lookups. Tag-addressed `fetch_manifest`
    /// with `IndexOperation::Resolve` walks the chain and persists on
    /// miss; pure `Query` calls return `None` without contacting the
    /// chain. Default online operation.
    Default,
    /// Mutable lookups (tag list, catalog, tag-addressed `fetch_manifest`)
    /// bypass the local index and go straight to the source. Digest-
    /// addressed (immutable) lookups still consult the local index first.
    /// Used for `--remote`.
    Remote,
    /// Local index only. Source list is empty or never consulted; misses
    /// return `None` for digest-addressed content and **error** for an
    /// unpinned (tag-only) `Resolve` miss (no source was allowed to be
    /// consulted, so "policy blocked" is the honest answer). Used for
    /// `--offline`.
    Offline,
    /// Freeze tag resolution to the local index: a tag-only `Resolve` miss
    /// **errors** (never walks the chain to fetch + commit an unknown
    /// reference), but digest-addressed content is still fetched from the
    /// source exactly like [`Self::Default`] (a digest is an already-known
    /// version). Used for `--frozen`. Distinct from [`Self::Offline`] on the
    /// digest axis: offline blocks all source contact; frozen still pulls
    /// locked digests.
    Frozen,
}

impl ChainMode {
    /// Lowercase label for the no-resolve policies, embedded in the
    /// [`error::Error::PolicyResolutionBlocked`] message so a user sees which
    /// flag refused the resolution. `Default` / `Remote` are not no-resolve
    /// policies and never reach the policy-block path, but return their own
    /// label for completeness.
    pub fn policy_label(self) -> &'static str {
        match self {
            ChainMode::Default => "default",
            ChainMode::Remote => "remote",
            ChainMode::Offline => "offline",
            ChainMode::Frozen => "frozen",
        }
    }
}

/// Caller intent for a manifest lookup on `IndexImpl`.
///
/// The trait conflated query and update before this enum existed: pure
/// queries (e.g. `index list --platforms`) and install/pull resolution
/// shared the same surface, and a cache miss in `ChainedIndex::fetch_manifest`
/// would silently walk the source chain and persist the result to the local
/// index even from query callers. Making intent explicit at every call site
/// prevents that leak. See `adr_index_routing_semantics.md`.
///
/// Naming: `Resolve` (not `Persist`) describes caller intent — "resolve
/// this identifier for use" — rather than the side effect (`Persist`),
/// because not every `Resolve` actually persists (digest-only identifiers
/// skip the tag-pointer commit; Remote-mode hits the source without
/// touching the local index for tag listings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOperation {
    /// Pure read. `ChainedIndex` returns the local-index result and never
    /// walks the source chain on miss. Used by `index list`,
    /// `index catalog`, `package description pull`, and any other path that reports
    /// existing data without producing it.
    Query,
    /// Read with write-through on miss. Install/pull paths walk the source
    /// chain on cache miss and persist the manifest blobs (and, for tag-
    /// addressed identifiers without a digest, the tag pointer). The only
    /// callers are `package_manager::tasks::resolve` (install/pull) and
    /// project lock resolution.
    Resolve,
}

/// The result of a platform-aware package selection.
pub enum SelectResult {
    /// Exactly one candidate matched.
    Found(ocx_oci::Identifier),
    /// Multiple candidates matched — the caller must decide how to handle the
    /// ambiguity (e.g. ask the user or report an error).
    Ambiguous(Vec<ocx_oci::Identifier>),
    /// No candidates matched the requested platforms (or the package was not
    /// found in the index at all).
    NotFound,
    /// The host declared non-empty `os.features` but no candidate sharing the
    /// host's os+arch satisfied subset matching on `os_features`. Distinct
    /// from [`NotFound`](Self::NotFound) (no os/arch candidates at all): here
    /// the package ships for this os/arch but only under different
    /// `os.features` (e.g. a different libc). The caller (package-manager
    /// layer) maps this to a feature-mismatch error so the user can
    /// `--platform`-override. `available` lists the candidate platforms the
    /// user could target.
    FeatureMismatch {
        host_features: Vec<String>,
        available: Vec<ocx_oci::Platform>,
    },
}

/// Note, some operations are cached and the cache is shared between clones of the index.
/// This means that if you clone the index, they will share the same cache and benefit from each other's cached data.
/// On the other hand, if you have a long-running index instance, you may want to periodically clear the cache to avoid memory bloat and ensure that you always have the latest data.
/// The cache is currently never cleared, but expiration or manual clearing may be added in the future if needed.
pub struct Index {
    inner: Box<dyn index_impl::IndexImpl>,
    /// Proxy-route rules for the dial-site SSRF guard ([`Self::guard_physical_dial`]).
    /// Defaults to the process-wide [`ocx_oci::ssrf::proxy_rules`]; override
    /// with [`Self::with_proxy_rules`] (test seam).
    rules: std::sync::Arc<ocx_oci::ssrf::ProxyRules>,
}

impl Index {
    pub fn from_remote(oci_index: OciIndex) -> Self {
        Self {
            inner: Box::new(oci_index),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Wrap an [`OcxIndex`] (an `index.ocx.sh`-style static-file source) as a
    /// chain source. Registered alongside [`OciIndex`] in the default chain
    /// so a logical `ocx.sh/<ns>/<pkg>` reference the registry does not serve
    /// resolves through the two-hop index path (`adr_index_indirection.md` F).
    pub fn from_source(source: OcxIndex) -> Self {
        Self {
            inner: Box::new(source),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Inject an arbitrary `IndexImpl` implementation.
    ///
    /// Used exclusively in unit tests to wrap `TestIndex` fakes without
    /// exposing `IndexImpl` as a public trait.  Not available in production
    /// builds.
    ///
    /// Behind the `__testing` seam so a test can inject its own mock index
    /// implementation without going through the heavier `from_chained`
    /// construction path. Those callers used to be sibling modules; the crate
    /// split moved them to `ocx_lib`, so `pub(crate)` no longer reaches them
    /// and the seam is what keeps the widening out of production builds.
    #[cfg(any(test, feature = "__testing"))]
    pub fn from_impl(inner: impl index_impl::IndexImpl + 'static) -> Self {
        Self {
            inner: Box::new(inner),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Construct an index that reads from `cache` first, falling through to
    /// `sources` in order on miss. Successful source fetches are persisted
    /// into `cache` via `update_tag`.
    ///
    /// `mode` controls cache/source routing — see [`ChainMode`] for each
    /// variant's behaviour.
    pub fn from_chained(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            inner: Box::new(chained_index::ChainedIndex::new(cache, sources, mode)),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Like [`Self::from_chained`], but attaches the machine-global blob store
    /// (`$OCX_HOME/blobs`) so an absent dispatch object recovers its content
    /// from installed blobs before any source walk
    /// (`adr_index_indirection.md` A3 step 2 / B2). This is the production
    /// construction (`context.rs`); the blob store is opt-in here so the
    /// signature-stable [`Self::from_chained`] keeps every unit-test caller
    /// unchanged (no blob store → recovery is a no-op).
    pub fn from_chained_with_content_store(
        cache: LocalIndex,
        sources: Vec<Index>,
        mode: ChainMode,
        content_store: ocx_store::file_structure::BlobStore,
    ) -> Self {
        Self {
            inner: Box::new(chained_index::ChainedIndex::new(cache, sources, mode).with_content_store(content_store)),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Like [`Self::from_chained`], but tag resolution never commits a tag
    /// pointer into `cache` — the caller's lock file is the canonical record
    /// of tag -> digest. Content-addressed blob writes still happen. Built
    /// for the update-verb family (`ocx update`); see
    /// `adr_toolchain_update_family.md`.
    pub fn from_chained_lock_scoped(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            inner: Box::new(chained_index::ChainedIndex::new_lock_scoped(cache, sources, mode)),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Overrides the proxy-route rules used by [`Self::guard_physical_dial`].
    /// Test seam — production always builds from the process-wide
    /// [`ocx_oci::ssrf::proxy_rules`].
    #[must_use]
    pub fn with_proxy_rules(mut self, rules: std::sync::Arc<ocx_oci::ssrf::ProxyRules>) -> Self {
        // The chain's own resolve-time guard reads its own copy, so pinning
        // one half would leave the other on the ambient environment.
        self.inner.set_proxy_rules(rules.clone());
        self.rules = rules;
        self
    }

    /// A view of this index that resolves identically but writes nothing into
    /// the local index (no dispatch object, no tag pointer) — see
    /// [`index_impl::IndexImpl::read_only_view`]. Content-addressed blob writes
    /// still happen. Used by `ocx package inspect` so a read-only look never
    /// grows the permanent index.
    pub fn read_only_view(&self) -> Self {
        Self {
            inner: self.inner.read_only_view(),
            rules: self.rules.clone(),
        }
    }

    /// A view of this index that lists and reads live from the sources
    /// regardless of the ambient [`ChainMode`], and writes nothing into the
    /// local index — see [`index_impl::IndexImpl::remote_view`]. Used by the
    /// update-check probe, which must see the freshest published release and
    /// must never move a pin.
    pub fn remote_view(&self) -> Self {
        Self {
            inner: self.inner.remote_view(),
            rules: self.rules.clone(),
        }
    }

    /// List all repositories available in the given registry.
    pub async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        log::debug!("Listing repositories for registry '{}'.", registry);
        self.inner.list_repositories(registry).await
    }

    /// List all tags available for the given identifier.
    ///
    /// Reserved tags — the `__ocx` namespace (keep tags included) and the
    /// frozen legacy `sha256.<hex>` keep tags
    /// ([`is_reserved_tag`]) — are automatically filtered out. Returns `None`
    /// when the package is not known to this index.
    pub async fn list_tags(&self, identifier: &ocx_oci::Identifier) -> Result<Option<Vec<String>>> {
        log::debug!("Listing tags for '{}'.", identifier);
        self.inner.list_tags(identifier).await.map(|opt| {
            opt.map(|tags| {
                tags.into_iter()
                    .filter(|t| !is_reserved_tag(t))
                    .collect::<Vec<_>>()
                    .sorted()
            })
        })
    }

    /// Fetch the manifest for the given identifier.
    ///
    /// `op` declares whether the call is a pure query (no chain walk on
    /// miss, no local-index writes) or a resolve (walk + persist on miss
    /// for install/pull paths). Returns `None` when the manifest is not
    /// available under the routing implied by `op` and the impl's mode.
    pub async fn fetch_manifest(
        &self,
        identifier: &ocx_oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        log::trace!("Fetching candidates for identifier '{}'.", identifier);
        self.inner.fetch_manifest(identifier, op).await
    }

    /// Find the manifest digest for the given identifier and tag.
    ///
    /// `op` carries the same contract as on [`Self::fetch_manifest`].
    /// Returns `None` when the identifier cannot be resolved.
    pub async fn fetch_manifest_digest(
        &self,
        identifier: &ocx_oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<ocx_oci::Digest>> {
        self.inner.fetch_manifest_digest(identifier, op).await
    }

    /// Fetch the raw bytes of a content blob.
    ///
    /// `blob_ref` carries `(registry, repo)` for the OCI blob endpoint and
    /// the blob's own digest for content addressing. `Ok(None)` = unrecoverable
    /// miss under the active routing policy (e.g. `ChainMode::Offline` + local
    /// cache miss).
    pub async fn fetch_blob(&self, blob_ref: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
        log::trace!("Fetching blob '{blob_ref}'.");
        self.inner.fetch_blob(blob_ref).await
    }

    /// The physical transport identifier for `identifier`, or `None` when no
    /// source rewrites it (registry-backed: physical == logical). Transport-only
    /// (`adr_index_indirection.md` C2) — the pull pipeline fetches layer content
    /// from this location; storage paths stay keyed on the logical identifier.
    pub async fn physical_reference(&self, identifier: &ocx_oci::Identifier) -> Result<Option<ocx_oci::Identifier>> {
        self.inner.physical_reference(identifier).await
    }

    /// The identifier a read of `identifier` must dial: the physical location
    /// the index routes it to, or `identifier` itself when no source rewrites
    /// it (registry-backed). Either way it carries `identifier`'s tag and
    /// digest.
    ///
    /// For a caller that reads the registry **directly** through a `Client`
    /// rather than through this index — `ocx package push`'s dependency-pin
    /// gate. A logical name dialled as-is reaches whatever host shares its
    /// spelling, not the registry the index points at (ocx#504). The dial-site
    /// SSRF floor ([`Self::guard_physical_dial`]) runs inside, so routing
    /// cannot be had without it. Transport-only (`adr_index_indirection.md`
    /// C2): the answer is never a storage key, and anything reported to the
    /// user still names `identifier`.
    ///
    /// No rewrite is passthrough only where no index is authoritative for the
    /// name. Where one is, its miss is terminal — OCX never falls back from
    /// the index protocol to the logical host — exactly as the resolve path's
    /// authoritative miss is.
    ///
    /// # Errors
    ///
    /// Whatever [`Self::physical_reference`] raises;
    /// [`error::Error::NotInIndex`] when the index authoritative for the name
    /// does not hold it; [`error::Error::Ssrf`] when the floor refuses the
    /// rewritten target.
    pub async fn route_for_dial(&self, identifier: &ocx_oci::Identifier) -> Result<ocx_oci::Identifier> {
        let Some(physical) = self.physical_reference(identifier).await? else {
            if let Some(base_url) = self.authoritative_index_base_url(identifier) {
                return Err(error::Error::NotInIndex {
                    identifier: identifier.to_string(),
                    namespace: identifier.registry().to_string(),
                    base_url: base_url.to_string(),
                });
            }
            return Ok(identifier.clone());
        };
        let routed = at_version_of(physical.registry(), physical.repository(), identifier);
        self.guard_physical_dial(identifier, &routed).await?;
        Ok(routed)
    }

    /// The physical transport identifier known **locally**, never dialling a
    /// source — see [`index_impl::IndexImpl::physical_reference_local`]. Used by
    /// the store-hit path of `PackageManager::find` (`ocx_lib`),
    /// which is downloading nothing and so must not pay for a pointer it will
    /// not use.
    pub async fn physical_reference_local(
        &self,
        identifier: &ocx_oci::Identifier,
    ) -> Result<Option<ocx_oci::Identifier>> {
        self.inner.physical_reference_local(identifier).await
    }

    /// Record the routing pointer for `identifier` locally — see
    /// [`index_impl::IndexImpl::record_routing_pointer`]. Called by
    /// `PackageManager::resolve_transport_pinned` (`ocx_lib`),
    /// the one path that resolves in order to materialize; readers ask for a
    /// physical address without calling this, and so leave no snapshot behind.
    pub async fn record_routing_pointer(&self, identifier: &ocx_oci::Identifier) {
        self.inner.record_routing_pointer(identifier).await;
    }

    /// The `trusted_hosts` escape hatch configured for `registry`.
    ///
    /// Exposed so the Sigstore trust-service dial guard
    /// ([`ocx_oci::endpoint::resolve_sigstore_url`](ocx_oci::endpoint::resolve_sigstore_url))
    /// reads the same operator-configured allowlist the registry guard reads,
    /// rather than minting a second config surface for the same question.
    pub fn trusted_hosts_for(&self, registry: &str) -> &[String] {
        self.inner.trusted_hosts_for(registry)
    }

    /// The plain-HTTP allowance set (`[registries."<ns>"].insecure`).
    ///
    /// Exposed for the same reason [`Self::trusted_hosts_for`] is: a caller
    /// that builds a [`DialPolicy`](ocx_oci::ssrf::DialPolicy) for a signing
    /// pipeline reads the operator's configured set through the index that
    /// already resolved it, rather than minting a second config surface.
    #[must_use]
    pub fn insecure_hosts(&self) -> &[String] {
        self.inner.insecure_hosts()
    }

    /// The dial-site SSRF floor this index enforces, as a value a pipeline can
    /// hold without holding the index (ADR 1.9).
    ///
    /// `registry` is the **logical** one — the `trusted_hosts` entry is keyed
    /// on it, never on the physical host it rewrites to (ocx#455).
    #[must_use]
    pub fn dial_policy(&self, registry: &str) -> ocx_oci::ssrf::DialPolicy<'_> {
        ocx_oci::ssrf::DialPolicy {
            insecure_hosts: self.inner.insecure_hosts(),
            trusted_hosts: self.inner.trusted_hosts_for(registry),
            rules: &self.rules,
        }
    }

    /// SSRF floor for a **rewritten** physical target, applied at the dial site —
    /// immediately before the first request that would reach it, and only when
    /// one is imminent.
    ///
    /// [`Self::physical_reference`] resolves the pointer on *every* resolve, warm
    /// or cold, so its own guard
    /// ([`guard_local_physical`](chained_index::ChainedIndex::guard_local_physical))
    /// must tolerate a lookup failure — a machine with no resolver has to keep
    /// resolving from its committed index. That tolerance admits an answer the
    /// guard could not judge, and the pull that later consumes it dials on the
    /// shared `PackageManager` client, which carries no
    /// [`GuardedResolver`](ocx_oci::ssrf::GuardedResolver) and performs its own,
    /// independent lookup. A hostile local tree naming an attacker-controlled
    /// domain therefore only has to answer NXDOMAIN while the pre-flight asks and
    /// a loopback address when the pull dials.
    ///
    /// So this half **fails closed on everything**, a lookup failure included:
    /// here a dial is about to happen, and a connection can no more succeed on a
    /// name that does not resolve than the lookup did — while an answer that
    /// appears only between the two questions is precisely the attack.
    ///
    /// That rationale is about a lookup **this process performs**, so it scopes
    /// to a direct dial. When a configured HTTP proxy intercepts the dial
    /// ([`ocx_oci::ssrf::guard_destination`]) the destination name never reaches a
    /// local resolver at all — it is text in the proxy's `CONNECT` line — so
    /// there is no answer for a second one to contradict, and refusing an
    /// unresolvable name would abort a pull that was going to work on a
    /// proxy-only-DNS network (ocx#407). The floor itself does not move: a
    /// forbidden IP literal is refused on either route. The two
    /// halves share the carve-out for a **non-rewrite** (`physical.registry() ==
    /// logical.registry()`) and the one `trusted_hosts` set
    /// ([`index_impl::IndexImpl::trusted_hosts_for`], keyed on the LOGICAL
    /// registry): a pull that was always going to that host is not something the
    /// index added, and guarding it would refuse every private registry that
    /// predates indices.
    ///
    /// Residual: the validate → connect window stays open, because the shared
    /// client resolves the name again for itself. Closing it needs a
    /// per-namespace `GuardedResolver` on that client.
    ///
    /// # Errors
    ///
    /// [`error::Error::Ssrf`] when the physical host resolves into a forbidden
    /// range without a `trusted_hosts` entry, or — on a direct dial only —
    /// cannot be resolved at all.
    pub async fn guard_physical_dial(
        &self,
        logical: &ocx_oci::Identifier,
        physical: &ocx_oci::Identifier,
    ) -> Result<()> {
        if physical.registry() == logical.registry() {
            return Ok(());
        }
        ocx_oci::ssrf::guard_physical_dial(&self.dial_policy(logical.registry()), logical, physical)
            .await
            .map_err(|refused| error::Error::Ssrf { source: refused })?;
        Ok(())
    }

    /// Fetch a published index root document verbatim (bytes + parsed
    /// [`IndexRoot`](wire::IndexRoot)) so a published source's local copy can be
    /// grown byte-for-byte (copy-a-mirror, `adr_index_indirection.md` A2). A
    /// derived source returns `None` — its root is OCX-authored, not copied. See
    /// [`index_impl::IndexImpl::fetch_root_document`].
    pub async fn fetch_root_document(
        &self,
        identifier: &ocx_oci::Identifier,
    ) -> Result<Option<(Vec<u8>, wire::IndexRoot)>> {
        self.inner.fetch_root_document(identifier).await
    }

    /// Whether this index will answer for `identifier`, and what its silence
    /// means. See [`index_impl::IndexImpl::jurisdiction`].
    ///
    /// `oci::index`-internal (no `pub`), like [`Self::source_kind`] — the chain
    /// is the only consumer, and `OcxIndex`'s inherent `pub` method serves the
    /// one caller outside this module.
    fn jurisdiction(&self, identifier: &ocx_oci::Identifier) -> Jurisdiction {
        self.inner.jurisdiction(identifier)
    }

    /// Whether a source in this index is the configured owner of `registry` —
    /// cheap, synchronous, no I/O. See [`index_impl::IndexImpl::serves_registry`].
    fn serves_registry(&self, registry: &str) -> bool {
        self.inner.serves_registry(registry)
    }

    /// This source's `trusted_hosts` SSRF exemption set. See
    /// [`index_impl::IndexImpl::trusted_hosts`]. `oci::index`-internal, like
    /// [`Self::serves_registry`] — the chain is the only consumer.
    fn trusted_hosts(&self) -> &[String] {
        self.inner.trusted_hosts()
    }

    /// The static-file base URL this source resolves against, or `None` when it
    /// is not a configured ocx-index. See
    /// [`index_impl::IndexImpl::index_base_url`].
    fn index_base_url(&self) -> Option<&str> {
        self.inner.index_base_url()
    }

    /// The base URL of the index authoritative for `identifier`, if any. See
    /// [`index_impl::IndexImpl::authoritative_index_base_url`].
    fn authoritative_index_base_url(&self, identifier: &ocx_oci::Identifier) -> Option<&str> {
        self.inner.authoritative_index_base_url(identifier)
    }

    /// This source's provenance (`adr_index_indirection.md` A2/H) — `Published`
    /// for an `index.ocx.sh`-style source, `Derived` for everything else. Cheap,
    /// synchronous, no I/O. [`ChainedIndex`](chained_index::ChainedIndex) uses
    /// this to pick the local dispatch-object read/recovery routing.
    ///
    /// `oci::index`-internal (no `pub`, default visibility reaches every
    /// descendant module including `chained_index`) — `SourceKind` itself is
    /// `pub(super)` inside `local_index`, so this stays unexported at the
    /// crate boundary.
    fn source_kind(&self) -> local_index::SourceKind {
        self.inner.source_kind()
    }

    /// Fetch the verbatim manifest bytes alongside the parsed manifest and its
    /// digest — the seam [`LocalIndex::persist_dispatch`] uses to write a
    /// self-contained, verifiable dispatch object (`adr_index_indirection.md` A3).
    ///
    /// Returns `Ok(None)` when the tag/manifest is absent.
    pub async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &ocx_oci::Identifier,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        log::trace!("Fetching raw manifest bytes for identifier '{}'.", identifier);
        self.inner.fetch_manifest_raw_bytes(identifier).await
    }

    pub async fn fetch_candidates(
        &self,
        identifier: &ocx_oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<Vec<(ocx_oci::Identifier, ocx_oci::Platform)>>> {
        let Some((digest, manifest)) = self.fetch_manifest(identifier, op).await? else {
            return Ok(None);
        };
        log::trace!(
            "Fetched manifest for identifier '{}'. Determining candidates based on manifest type.",
            identifier
        );

        match manifest {
            ocx_oci::Manifest::Image(_) => Ok(Some(vec![(
                identifier.clone_with_digest(digest),
                ocx_oci::Platform::default(),
            )])),
            ocx_oci::Manifest::ImageIndex(index) => {
                let mut candidates = Vec::with_capacity(index.manifests.len());
                for entry in index.manifests {
                    // One shared eligibility rule (see
                    // `ocx_oci::Platform::candidate_from_descriptor`): a descriptor
                    // that names no platform, or one OCX cannot represent, is
                    // an attestation/referrer entry — not a fault, and not
                    // something a `--platform` request can ever mean.
                    let Some(platform) = ocx_oci::Platform::candidate_from_descriptor(&entry) else {
                        log::debug!(
                            "skipping non-candidate image-index descriptor {} for '{}'",
                            entry.digest,
                            identifier
                        );
                        continue;
                    };
                    let digest = entry.digest.try_into()?;
                    candidates.push((identifier.clone_with_digest(digest), platform));
                }
                log::debug!(
                    "Found {} candidate(s) for identifier '{}'.",
                    candidates.len(),
                    identifier
                );
                Ok(Some(candidates))
            }
        }
    }

    pub async fn select(
        &self,
        identifier: &ocx_oci::Identifier,
        platform: &ocx_oci::Platform,
        op: IndexOperation,
    ) -> Result<SelectResult> {
        log::debug!("Selecting package '{}' for platform {}.", identifier, platform);

        let Some(candidates) = self.fetch_candidates(identifier, op).await? else {
            log::debug!("No candidates found for '{}'.", identifier);
            return Ok(SelectResult::NotFound);
        };

        // Route through the shared D1 selection helper (`is_compatible` +
        // `compatibility_score`): the same relation `lookup_host_leaf` and
        // authoring `resolve_for_specific` use, so fresh-resolve gives an
        // identical answer for the same requested platform and candidate set.
        // An `Any`-offered candidate satisfies every requirement by
        // construction (D1 rule), so no separate `Any` fallback tier is
        // needed here.
        let result = match ocx_oci::select_best(platform, &candidates) {
            ocx_oci::Selection::Found(id) => SelectResult::Found(id),
            ocx_oci::Selection::Ambiguous(ids) => SelectResult::Ambiguous(ids),
            // Distinguish a feature mismatch from a plain not-found. When the
            // host declared non-empty `os.features` and there exist candidates
            // sharing the host's os+arch but none satisfied subset matching,
            // the package ships for this os/arch under different `os.features`
            // only — surface the dedicated variant so the caller can report a
            // feature mismatch rather than a generic not-found.
            ocx_oci::Selection::None => match host_os_features(platform) {
                Some(host_features) => {
                    let available = candidates_sharing_host_os_arch(platform, &candidates);
                    if available.is_empty() {
                        SelectResult::NotFound
                    } else {
                        SelectResult::FeatureMismatch {
                            host_features,
                            available,
                        }
                    }
                }
                None => SelectResult::NotFound,
            },
        };

        match &result {
            SelectResult::Found(id) => log::debug!("Selected '{}'.", id),
            SelectResult::Ambiguous(ids) => {
                log::debug!("Selection ambiguous for '{}': {} candidates.", identifier, ids.len())
            }
            SelectResult::NotFound => log::debug!(
                "No matching platform for '{}' among {} candidate(s).",
                identifier,
                candidates.len()
            ),
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => log::debug!(
                "feature mismatch for '{}': host provides {:?}; {} candidate(s) share os+arch but differ on os.features.",
                identifier,
                host_features,
                available.len()
            ),
        }

        Ok(result)
    }
}

impl Clone for Index {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.box_clone(),
            rules: self.rules.clone(),
        }
    }
}

/// Extract the requested platform's declared `os_features`, when it is
/// `Specific` and carries a non-empty set.
///
/// A non-empty `os_features` set (e.g. a detected libc, or an explicit
/// `--platform linux/amd64+libc.musl`) is the signal that a no-match is a
/// feature mismatch rather than a plain not-found.
fn host_os_features(platform: &ocx_oci::Platform) -> Option<Vec<String>> {
    match platform {
        ocx_oci::Platform::Specific { os_features, .. } if !os_features.is_empty() => Some(os_features.clone()),
        _ => None,
    }
}

/// The physical location `registry/repository` addressed at `logical`'s
/// version — its tag and its digest, whichever it carries.
///
/// The one place a rewrite stamps the logical version onto a physical
/// location, so every source answers in one shape. The digest content-addresses
/// a pinned read; the tag is what a read by tag needs (the `any`-provenance
/// fetch of `ocx package push`'s gate). Tag first: `clone_with_tag` drops any
/// digest.
fn at_version_of(
    registry: impl Into<String>,
    repository: impl Into<String>,
    logical: &ocx_oci::Identifier,
) -> ocx_oci::Identifier {
    let mut physical = ocx_oci::Identifier::new_registry(repository, registry);
    if let Some(tag) = logical.tag() {
        physical = physical.clone_with_tag(tag);
    }
    if let Some(digest) = logical.digest() {
        physical = physical.clone_with_digest(digest);
    }
    physical
}

/// Collect the candidate platforms that share os+arch with the requested
/// `Specific` platform.
///
/// These are the entries the user could target with `--platform` — the package
/// ships for this os/arch, just under a different libc. Returns them sorted by
/// display string for deterministic error output.
fn candidates_sharing_host_os_arch(
    platform: &ocx_oci::Platform,
    candidates: &[(ocx_oci::Identifier, ocx_oci::Platform)],
) -> Vec<ocx_oci::Platform> {
    let ocx_oci::Platform::Specific {
        os: host_os,
        arch: host_arch,
        ..
    } = platform
    else {
        return Vec::new();
    };

    let mut matched: Vec<ocx_oci::Platform> = candidates
        .iter()
        .filter_map(|(_, candidate)| match candidate {
            ocx_oci::Platform::Specific { os, arch, .. } if os == host_os && arch == host_arch => {
                Some(candidate.clone())
            }
            _ => None,
        })
        .collect();
    matched.sort_by_key(|platform| platform.to_string());
    matched.dedup();
    matched
}

/// Test-only sources for tests **outside** this crate: [`index_impl::IndexImpl`]
/// is private here, so a consumer cannot write its own.
#[cfg(any(test, feature = "__testing"))]
pub mod test_source {
    use super::error::Result;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::{Index, IndexOperation, index_impl};
    use ocx_oci::{self, Digest, Identifier, Manifest};

    /// A source that owns a namespace and carries its `trusted_hosts` exemption —
    /// the `OcxIndex` shape the chain keys on, with nothing else wired up.
    ///
    /// It also **counts** every time it is asked for that set, which is the one
    /// question [`Index::guard_physical_dial`] asks per evaluation. That makes
    /// the counter an evaluation counter, so a caller can prove a memoized
    /// verdict is asked for once rather than once per layer — without asserting
    /// on `tokio::sync::OnceCell`'s internals.
    #[derive(Clone)]
    pub struct TrustingSource {
        namespace: String,
        trusted: Vec<String>,
        asked: Arc<AtomicUsize>,
    }

    impl TrustingSource {
        /// `asked` is bumped once per `trusted_hosts` question; pass a clone of a
        /// counter the test keeps, since building an [`Index`] consumes the source.
        pub fn new(namespace: &str, trusted: Vec<String>, asked: Arc<AtomicUsize>) -> Self {
            Self {
                namespace: namespace.to_string(),
                trusted,
                asked,
            }
        }

        /// This source wrapped as an [`Index`], ready to hand to `from_chained`.
        pub fn into_index(self) -> Index {
            Index::from_impl(self)
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for TrustingSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
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
        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn serves_registry(&self, registry: &str) -> bool {
            registry == self.namespace
        }
        fn trusted_hosts(&self) -> &[String] {
            self.asked.fetch_add(1, Ordering::SeqCst);
            &self.trusted
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }
}

// ── Index::select integration tests with multi-libc ImageIndex (Step 3.4) ──

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ocx_oci::{self, Digest, Identifier, Manifest, Platform};

    // ── Minimal mock IndexImpl returning a fixed ImageIndex ──────────

    /// A mock `IndexImpl` that always returns the same pre-built `ImageIndex`
    /// manifest so `Index::select` can be exercised without a real registry.
    struct MultiLibcIndex {
        manifest: ocx_oci::ImageIndex,
    }

    impl MultiLibcIndex {
        /// Serve an arbitrary pre-built image index through the same mock.
        fn from_manifest(manifest: ocx_oci::ImageIndex) -> Self {
            Self { manifest }
        }

        /// Build a mock image index with three linux/amd64 entries:
        ///   entry 0 — libc.glibc (digest sha256:glibc_entry…)
        ///   entry 1 — libc.musl  (digest sha256:musl_entry…)
        ///   entry 2 — no os.features (digest sha256:untagged_entry…)
        fn new() -> Self {
            fn platform_with_features(features: Option<Vec<String>>) -> ocx_oci::native::Platform {
                ocx_oci::native::Platform {
                    os: ocx_oci::native::Os::Linux,
                    architecture: ocx_oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: features,
                }
            }

            let glibc_entry = ocx_oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
                size: 100,
                platform: Some(platform_with_features(Some(vec!["libc.glibc".to_string()]))),
                artifact_type: None,
                annotations: None,
            };
            let musl_entry = ocx_oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "b".repeat(64)),
                size: 100,
                platform: Some(platform_with_features(Some(vec!["libc.musl".to_string()]))),
                artifact_type: None,
                annotations: None,
            };
            let untagged_entry = ocx_oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "c".repeat(64)),
                size: 100,
                platform: Some(platform_with_features(None)),
                artifact_type: None,
                annotations: None,
            };

            Self {
                manifest: ocx_oci::ImageIndex {
                    schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
                    media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
                    artifact_type: None,
                    manifests: vec![glibc_entry, musl_entry, untagged_entry],
                    annotations: None,
                },
            }
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for MultiLibcIndex {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }

        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }

        async fn fetch_manifest(
            &self,
            _identifier: &Identifier,
            _op: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }

        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(MultiLibcIndex {
                manifest: self.manifest.clone(),
            })
        }
    }

    fn test_id() -> Identifier {
        Identifier::new_registry("test/tool", "example.com").clone_with_tag("1.0")
    }

    fn glibc_host_platform() -> Platform {
        Platform::Specific {
            os: ocx_oci::OperatingSystem::Linux,
            arch: ocx_oci::Architecture::Amd64,
            variant: None,
            os_features: vec!["libc.glibc".to_string()],
        }
    }

    fn musl_host_platform() -> Platform {
        Platform::Specific {
            os: ocx_oci::OperatingSystem::Linux,
            arch: ocx_oci::Architecture::Amd64,
            variant: None,
            os_features: vec!["libc.musl".to_string()],
        }
    }

    fn no_libc_host_platform() -> Platform {
        // Represents a host where libc was undetected (empty os_features).
        Platform::Specific {
            os: ocx_oci::OperatingSystem::Linux,
            arch: ocx_oci::Architecture::Amd64,
            variant: None,
            os_features: Vec::new(),
        }
    }

    // 3.4 — glibc host selects the libc.glibc entry

    #[tokio::test]
    async fn select_glibc_host_picks_glibc_entry() {
        let index = Index::from_impl(MultiLibcIndex::new());
        let glibc_digest = format!("sha256:{}", "a".repeat(64));

        let result = index
            .select(&test_id(), &glibc_host_platform(), IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::Found(id) => {
                assert_eq!(
                    id.digest().map(|d| d.to_string()),
                    Some(glibc_digest),
                    "glibc host must select the libc.glibc index entry"
                );
            }
            SelectResult::NotFound => panic!("glibc host should find a matching entry"),
            SelectResult::Ambiguous(candidates) => panic!("expected single match, got ambiguous: {:?}", candidates),
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => panic!(
                "expected single match, got feature mismatch: host {:?}, available {:?}",
                host_features, available
            ),
        }
    }

    // 3.4 — musl host selects the libc.musl entry

    #[tokio::test]
    async fn select_musl_host_picks_musl_entry() {
        let index = Index::from_impl(MultiLibcIndex::new());
        let musl_digest = format!("sha256:{}", "b".repeat(64));

        let result = index
            .select(&test_id(), &musl_host_platform(), IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::Found(id) => {
                assert_eq!(
                    id.digest().map(|d| d.to_string()),
                    Some(musl_digest),
                    "musl host must select the libc.musl index entry"
                );
            }
            SelectResult::NotFound => panic!("musl host should find a matching entry"),
            SelectResult::Ambiguous(candidates) => panic!("expected single match, got ambiguous: {:?}", candidates),
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => panic!(
                "expected single match, got feature mismatch: host {:?}, available {:?}",
                host_features, available
            ),
        }
    }

    // 3.4 — host with no detected libc selects the untagged entry

    #[tokio::test]
    async fn select_no_libc_host_picks_untagged_entry() {
        let index = Index::from_impl(MultiLibcIndex::new());
        let untagged_digest = format!("sha256:{}", "c".repeat(64));

        let result = index
            .select(&test_id(), &no_libc_host_platform(), IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::Found(id) => {
                assert_eq!(
                    id.digest().map(|d| d.to_string()),
                    Some(untagged_digest),
                    "host with no libc must select the un-tagged (empty os.features) entry"
                );
            }
            SelectResult::NotFound => panic!("no-libc host should find the untagged entry"),
            SelectResult::Ambiguous(candidates) => panic!("expected single match, got ambiguous: {:?}", candidates),
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => panic!(
                "expected single match, got feature mismatch: host {:?}, available {:?}",
                host_features, available
            ),
        }
    }

    // ── Dual-libc host can select either tagged entry (deterministic tiebreak) ──
    //
    // A host advertising {libc.glibc, libc.musl} (Ubuntu + musl-tools, or a
    // multi-target CI runner) satisfies BOTH the libc.glibc and libc.musl index
    // entries. Each declares exactly one os.feature, so both are equally
    // specific (specificity 1) and the untagged entry (specificity 0) loses.
    // With two equally specific matches the resolver surfaces `Ambiguous`,
    // ordered by manifest order — glibc entry first, musl entry second — so the
    // caller (or an explicit `--platform`) can deterministically pick either.

    fn dual_libc_host_platform() -> Platform {
        Platform::Specific {
            os: ocx_oci::OperatingSystem::Linux,
            arch: ocx_oci::Architecture::Amd64,
            variant: None,
            os_features: vec!["libc.glibc".to_string(), "libc.musl".to_string()],
        }
    }

    #[tokio::test]
    async fn select_dual_libc_host_can_select_either_tagged_entry() {
        let index = Index::from_impl(MultiLibcIndex::new());
        let glibc_digest = format!("sha256:{}", "a".repeat(64));
        let musl_digest = format!("sha256:{}", "b".repeat(64));

        let result = index
            .select(&test_id(), &dual_libc_host_platform(), IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::Ambiguous(candidates) => {
                let digests: Vec<String> = candidates
                    .iter()
                    .filter_map(|id| id.digest().map(|d| d.to_string()))
                    .collect();
                // Both libc-tagged entries match; the untagged entry is less
                // specific and excluded. Order follows manifest order
                // (deterministic): glibc first, musl second.
                assert_eq!(
                    digests,
                    vec![glibc_digest, musl_digest],
                    "dual-libc host must match both tagged entries in deterministic manifest order"
                );
            }
            SelectResult::Found(id) => panic!("expected ambiguity between glibc+musl entries, got single: {id}"),
            SelectResult::NotFound => panic!("dual-libc host must match the tagged entries"),
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => panic!(
                "expected ambiguity, got feature mismatch: host {:?}, available {:?}",
                host_features, available
            ),
        }
    }

    // ── Discriminating test: proves subset semantics, NOT just equality (3.4) ────
    //
    // This test FAILS under a strict-equality matcher and only passes under
    // `is_compatible()`'s subset semantics.
    //
    // Setup: index contains ONLY an untagged entry (empty os_features).
    //        Host is a glibc host (os_features = ["libc.glibc"]).
    //
    // Under strict equality:
    //   host {os_features: ["libc.glibc"]} != candidate {os_features: []}
    //   → SelectResult::NotFound  (test assertion fails here — proving it drives impl)
    //
    // Under subset semantics (is_compatible):
    //   candidate.os_features = []  (empty set)
    //   {} ⊆ {libc.glibc}  → true
    //   → SelectResult::Found(untagged entry)  (test passes)
    //
    // This is the "static-musl / legacy untagged package on a libc-aware host"
    // scenario: a glibc host should still be able to install a package that
    // declares no libc requirement (empty os_features = "runs everywhere").

    /// Mock index with only a single untagged linux/amd64 entry.
    struct UntaggedOnlyIndex {
        manifest: ocx_oci::ImageIndex,
    }

    impl UntaggedOnlyIndex {
        fn new() -> Self {
            let untagged_entry = ocx_oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "d".repeat(64)),
                size: 100,
                platform: Some(ocx_oci::native::Platform {
                    os: ocx_oci::native::Os::Linux,
                    architecture: ocx_oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: None, // empty — declares no libc requirement
                }),
                artifact_type: None,
                annotations: None,
            };
            Self {
                manifest: ocx_oci::ImageIndex {
                    schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
                    media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
                    artifact_type: None,
                    manifests: vec![untagged_entry],
                    annotations: None,
                },
            }
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for UntaggedOnlyIndex {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(UntaggedOnlyIndex {
                manifest: self.manifest.clone(),
            })
        }
    }

    // ── B2: FeatureMismatch path coverage ────────────────────────────────────
    //
    // This is the keystone error-path test: when the host detected a libc but
    // the index has entries for this os+arch under a DIFFERENT libc only, the
    // result must be `SelectResult::FeatureMismatch` (not `NotFound`).
    //
    // Setup: index contains ONLY a `linux/amd64 + os_features:[libc.musl]` entry.
    //        Host is a glibc host (`os_features: ["libc.glibc"]`).
    //
    // Expected: `FeatureMismatch { host_features: ["libc.glibc"], available: [linux/amd64+musl] }`.

    /// Mock index with only a single musl-tagged linux/amd64 entry.
    struct MuslOnlyIndex {
        manifest: ocx_oci::ImageIndex,
    }

    impl MuslOnlyIndex {
        fn new() -> Self {
            let musl_entry = ocx_oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "e".repeat(64)),
                size: 100,
                platform: Some(ocx_oci::native::Platform {
                    os: ocx_oci::native::Os::Linux,
                    architecture: ocx_oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: Some(vec!["libc.musl".to_string()]),
                }),
                artifact_type: None,
                annotations: None,
            };
            Self {
                manifest: ocx_oci::ImageIndex {
                    schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
                    media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
                    artifact_type: None,
                    manifests: vec![musl_entry],
                    annotations: None,
                },
            }
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for MuslOnlyIndex {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(MuslOnlyIndex {
                manifest: self.manifest.clone(),
            })
        }
    }

    /// B2 keystone: glibc host + musl-only index → FeatureMismatch with correct
    /// `host_features` and `available` fields.
    #[tokio::test]
    async fn select_glibc_host_musl_only_index_returns_feature_mismatch() {
        let index = Index::from_impl(MuslOnlyIndex::new());

        let result = index
            .select(&test_id(), &glibc_host_platform(), IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => {
                assert_eq!(
                    host_features,
                    vec!["libc.glibc".to_string()],
                    "host_features must report the glibc host tag"
                );
                assert_eq!(available.len(), 1, "exactly one available platform expected");
                let avail = &available[0];
                match avail {
                    Platform::Specific {
                        os, arch, os_features, ..
                    } => {
                        assert_eq!(os.to_string(), "linux");
                        assert_eq!(arch.to_string(), "amd64");
                        assert_eq!(
                            os_features.as_slice(),
                            &["libc.musl".to_string()],
                            "available entry must carry libc.musl"
                        );
                    }
                    _ => panic!("available entry must be Specific, got {:?}", avail),
                }
            }
            SelectResult::NotFound => {
                panic!("expected FeatureMismatch (index has linux/amd64+musl, host is glibc), got NotFound")
            }
            SelectResult::Found(id) => panic!("expected FeatureMismatch, got Found({id})"),
            SelectResult::Ambiguous(ids) => panic!("expected FeatureMismatch, got Ambiguous({ids:?})"),
        }
    }

    // ── Explicit --platform override selects feature-tagged manifest (D4) ────
    //
    // An explicit `--platform linux/amd64+libc.musl` must select the musl
    // manifest even when host *detection* would baseline to glibc. The parsed
    // platform carries `os_features: ["libc.musl"]`, so `is_compatible` admits
    // only the musl-tagged candidate. This proves the `+features` syntax flows
    // through `Index::select` end to end.

    #[tokio::test]
    async fn select_explicit_musl_platform_picks_musl_entry_over_glibc_baseline() {
        let index = Index::from_impl(MultiLibcIndex::new());
        let musl_digest = format!("sha256:{}", "b".repeat(64));

        // Explicit override parsed from the CLI `--platform` syntax. No `Any`
        // fallback and no glibc tier — only the musl-tagged platform.
        let explicit: Platform = "linux/amd64+libc.musl".parse().unwrap();

        let result = index
            .select(&test_id(), &explicit, IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::Found(id) => {
                assert_eq!(
                    id.digest().map(|d| d.to_string()),
                    Some(musl_digest),
                    "explicit --platform linux/amd64+libc.musl must select the libc.musl entry"
                );
            }
            SelectResult::NotFound => panic!("expected Found(musl entry), got NotFound"),
            SelectResult::Ambiguous(ids) => panic!("expected Found(musl entry), got Ambiguous({ids:?})"),
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => panic!(
                "expected Found(musl entry), got FeatureMismatch: host {host_features:?}, available {available:?}"
            ),
        }
    }

    // ── Discriminating test: proves subset semantics, NOT just equality (3.4) ────
    //
    // This test FAILS under a strict-equality matcher and only passes under
    // `is_compatible()`'s subset semantics.
    //
    // Setup: index contains ONLY an untagged entry (empty os_features).
    //        Host is a glibc host (os_features = ["libc.glibc"]).
    //
    // Under strict equality:
    //   host {os_features: ["libc.glibc"]} != candidate {os_features: []}
    //   → SelectResult::NotFound  (test assertion fails here — proving it drives impl)
    //
    // Under subset semantics (is_compatible):
    //   candidate.os_features = []  (empty set)
    //   {} ⊆ {libc.glibc}  → true
    //   → SelectResult::Found(untagged entry)  (test passes)
    //
    // This is the "static-musl / legacy untagged package on a libc-aware host"
    // scenario: a glibc host should still be able to install a package that
    // declares no libc requirement (empty os_features = "runs everywhere").

    /// Discriminating test: glibc host, index has ONLY untagged entry.
    ///
    /// - Fails under strict equality  (NotFound, not Found)
    /// - Passes under `is_compatible()` subset semantics  (empty ⊆ glibc-host)
    ///
    /// This is the load-bearing case that proves subset semantics are actually
    /// exercised, not just equality-by-coincidence.
    #[tokio::test]
    async fn select_glibc_host_picks_untagged_entry_when_only_untagged_present() {
        let index = Index::from_impl(UntaggedOnlyIndex::new());
        let untagged_digest = format!("sha256:{}", "d".repeat(64));

        // Glibc host (os_features = ["libc.glibc"]).
        // The only index entry has empty os_features.
        // Subset: {} ⊆ {libc.glibc} → must match.
        let result = index
            .select(&test_id(), &glibc_host_platform(), IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::Found(id) => {
                assert_eq!(
                    id.digest().map(|d| d.to_string()),
                    Some(untagged_digest),
                    "glibc host must match untagged entry via subset semantics (empty ⊆ glibc-host)"
                );
            }
            SelectResult::NotFound => panic!(
                "glibc host failed to select untagged entry — strict equality rejected \
                 empty-set ⊆ {{libc.glibc}}; this test drives implementation of \
                 is_compatible() subset semantics"
            ),
            SelectResult::Ambiguous(candidates) => panic!("expected single match, got ambiguous: {:?}", candidates),
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => panic!(
                "expected single match, got feature mismatch: host {:?}, available {:?}",
                host_features, available
            ),
        }
    }

    // ── NotFound vs FeatureMismatch boundary (review finding #9) ────────────
    //
    // `SelectResult::FeatureMismatch` is only correct when the host reported a
    // non-empty `os_features` AND at least one candidate shares its os+arch.
    // Both surrounding conditions collapse to plain `NotFound` instead. The two
    // tests below each pin one of those conditions independently of the other.

    /// A no-libc host (empty `os_features`, detection found nothing) must
    /// surface a plain `NotFound` against a musl-only index — never
    /// `FeatureMismatch`. `host_os_features()` returns `None` for an empty
    /// `os_features` platform, so the `0`-match arm takes the `None` branch
    /// straight to `NotFound` without ever computing `available`.
    #[tokio::test]
    async fn select_no_libc_host_musl_only_index_returns_not_found() {
        let index = Index::from_impl(MuslOnlyIndex::new());

        let result = index
            .select(&test_id(), &no_libc_host_platform(), IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::NotFound => {}
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => panic!(
                "no-libc host must yield NotFound (host_os_features is None), \
                 got FeatureMismatch: host {host_features:?}, available {available:?}"
            ),
            SelectResult::Found(id) => panic!("expected NotFound, got Found({id})"),
            SelectResult::Ambiguous(ids) => panic!("expected NotFound, got Ambiguous({ids:?})"),
        }
    }

    /// Mock index with only a single `windows/amd64` entry — no os+arch
    /// overlap with a linux glibc host.
    struct WindowsOnlyIndex {
        manifest: ocx_oci::ImageIndex,
    }

    impl WindowsOnlyIndex {
        fn new() -> Self {
            let windows_entry = ocx_oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "f".repeat(64)),
                size: 100,
                platform: Some(ocx_oci::native::Platform {
                    os: ocx_oci::native::Os::Windows,
                    architecture: ocx_oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: None,
                }),
                artifact_type: None,
                annotations: None,
            };
            Self {
                manifest: ocx_oci::ImageIndex {
                    schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
                    media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
                    artifact_type: None,
                    manifests: vec![windows_entry],
                    annotations: None,
                },
            }
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for WindowsOnlyIndex {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(WindowsOnlyIndex {
                manifest: self.manifest.clone(),
            })
        }
    }

    /// A glibc host has non-empty `os_features` (`host_os_features()` returns
    /// `Some`), but a windows/amd64-only index shares no os+arch with the
    /// linux host — `candidates_sharing_host_os_arch` returns empty, so the
    /// `available.is_empty()` sub-branch must still resolve to `NotFound`,
    /// never `FeatureMismatch`.
    #[tokio::test]
    async fn select_glibc_host_windows_only_index_returns_not_found() {
        let index = Index::from_impl(WindowsOnlyIndex::new());

        let result = index
            .select(&test_id(), &glibc_host_platform(), IndexOperation::Query)
            .await
            .unwrap();

        match result {
            SelectResult::NotFound => {}
            SelectResult::FeatureMismatch {
                host_features,
                available,
            } => panic!(
                "no os+arch overlap must yield NotFound (available.is_empty()), \
                 got FeatureMismatch: host {host_features:?}, available {available:?}"
            ),
            SelectResult::Found(id) => panic!("expected NotFound, got Found({id})"),
            SelectResult::Ambiguous(ids) => panic!("expected NotFound, got Ambiguous({ids:?})"),
        }
    }

    // ── N-2: descriptor eligibility at the SELECTION boundary ────────────────

    #[tokio::test]
    async fn attestation_descriptor_is_not_a_selection_candidate() {
        // Under D1 the index stores the registry's own image index, so
        // published sources now carry attestation and referrer descriptors for
        // the first time. Such a descriptor omits `platform` entirely; mapping
        // that to `Platform::any()` made it a UNIVERSAL candidate (an `Any`
        // OFFER satisfies every requirement), so one of them matched anything
        // and two made every selection `Ambiguous`.
        fn descriptor(fill: char, platform: Option<ocx_oci::native::Platform>) -> ocx_oci::ImageIndexEntry {
            ocx_oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", fill.to_string().repeat(64)),
                size: 100,
                platform,
                artifact_type: None,
                annotations: None,
            }
        }
        let real = ocx_oci::native::Platform {
            os: ocx_oci::native::Os::Linux,
            architecture: ocx_oci::native::Arch::Amd64,
            variant: None,
            features: None,
            os_version: None,
            os_features: Some(vec!["libc.glibc".to_string()]),
        };
        let index = Index::from_impl(MultiLibcIndex::from_manifest(ocx_oci::ImageIndex {
            schema_version: ocx_oci::INDEX_SCHEMA_VERSION,
            media_type: Some("application/vnd.oci.image.index.v1+json".to_string()),
            artifact_type: None,
            manifests: vec![
                descriptor('a', Some(real)),
                descriptor('b', None),
                descriptor('c', None),
            ],
            annotations: None,
        }));

        let candidates = index
            .fetch_candidates(&test_id(), IndexOperation::Query)
            .await
            .unwrap()
            .expect("the mock always answers");
        assert_eq!(
            candidates.len(),
            1,
            "only the descriptor that names a platform is a candidate, got {:?}",
            candidates
                .iter()
                .map(|(id, p)| (id.to_string(), p.to_string()))
                .collect::<Vec<_>>()
        );
        assert_eq!(candidates[0].1.to_string(), "linux/amd64+libc.glibc");

        // Two platform-less descriptors used to make this Ambiguous.
        match index
            .select(&test_id(), &glibc_host_platform(), IndexOperation::Query)
            .await
            .unwrap()
        {
            SelectResult::Found(_) => {}
            other => panic!(
                "two attestation descriptors must not make selection ambiguous, got {}",
                match other {
                    SelectResult::Ambiguous(ids) => format!("Ambiguous({ids:?})"),
                    SelectResult::NotFound => "NotFound".to_string(),
                    _ => "FeatureMismatch".to_string(),
                }
            ),
        }
    }

    // ── D7 at the WRAPPER listing boundary ───────────────────────────────────

    /// A source that reports every tag it knows, reserved names included — the
    /// shape a foreign or older `IndexImpl` can legitimately have. The wrapper
    /// filter is what guarantees `ocx index list` never shows one.
    struct UnfilteredTagSource;

    #[async_trait]
    impl index_impl::IndexImpl for UnfilteredTagSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_tags(&self, _: &Identifier) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![
                "3.28".to_string(),
                "latest".to_string(),
                "__ocx.desc".to_string(),
                "__OCX.future".to_string(),
                format!("__ocx.keep.sha256-{}", "a".repeat(64)),
            ]))
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            Ok(None)
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(None)
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(UnfilteredTagSource)
        }
    }

    #[tokio::test]
    async fn list_tags_filters_reserved_tags() {
        let tags = Index::from_impl(UnfilteredTagSource)
            .list_tags(&test_id())
            .await
            .unwrap()
            .expect("the mock answers");
        assert_eq!(tags, vec!["3.28".to_string(), "latest".to_string()]);
    }

    // ── The dial-site SSRF floor (`guard_physical_dial`) ─────────────────────

    /// A loopback authority nothing needs to be listening on: the guard resolves
    /// the host and never connects.
    const LOOPBACK: &str = "localhost:5999";
    /// RFC 6761 reserves `.invalid`, so this never resolves — the input the
    /// resolve-time guard tolerates and this one must refuse.
    const UNRESOLVABLE: &str = "physical.invalid";

    /// A chained index over `sources` — the production shape, so the
    /// `trusted_hosts` lookup goes through the same registry-ownership keying
    /// `ChainedIndex` uses rather than a single source's own set.
    /// Every chain a guard test drives is pinned to explicit no-proxy rules:
    /// `cargo test` runs on machines that may have a real `HTTPS_PROXY`, and
    /// the route decides whether the floor resolves at all. Tests that want a
    /// proxy override with [`Index::with_proxy_rules`] afterwards.
    fn chained_with(directory: &tempfile::TempDir, sources: Vec<Index>) -> Index {
        Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: crate::IndexStore::new(directory.path().join("index")),
            }),
            sources,
            ChainMode::Default,
        )
        .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct())
    }

    fn logical_id() -> Identifier {
        Identifier::new_registry("kitware/cmake", "example.com")
    }

    fn physical_id(registry: &str) -> Identifier {
        Identifier::new_registry("evil/pkg", registry)
    }

    fn is_forbidden_refusal(error: &crate::error::Error) -> bool {
        matches!(
            error,
            error::Error::Ssrf {
                source: ocx_oci::ssrf::PhysicalDialRefused {
                    source: ocx_oci::ssrf::SsrfError::ForbiddenTarget { .. },
                    ..
                },
            }
        )
    }

    /// The refusal the resolve-time guard also makes: a rewritten target that
    /// resolves into a forbidden range never reaches the pull's client.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_refuses_a_rewritten_forbidden_target() {
        let directory = tempfile::tempdir().unwrap();
        let error = chained_with(&directory, vec![])
            .guard_physical_dial(&logical_id(), &physical_id(LOOPBACK))
            .await
            .expect_err("a rewritten loopback target must be refused at the dial site");
        assert!(is_forbidden_refusal(&error), "expected an SSRF refusal, got: {error:?}");
    }

    /// The same rewritten loopback NAME on a proxied route. A proxy resolves
    /// the destination itself, so the floor has no address to judge — but
    /// `localhost` names one by definition, and admitting it would let an
    /// index root reach back into the caller's own machine through the proxy.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_refuses_a_rewritten_loopback_name_on_a_proxied_route() {
        let directory = tempfile::tempdir().unwrap();
        let error = chained_with(&directory, vec![])
            .with_proxy_rules(proxied_everywhere())
            .guard_physical_dial(&logical_id(), &physical_id(LOOPBACK))
            .await
            .expect_err("a loopback name must be refused whoever dials it");
        assert!(is_forbidden_refusal(&error), "expected an SSRF refusal, got: {error:?}");
    }

    /// The one place the two halves of the floor differ, and the reason this one
    /// exists: `ChainedIndex::guard_local_physical` **tolerates** a failed lookup
    /// on a plain DNS name (proven by its own
    /// `a_local_root_naming_an_unresolvable_dns_host_is_tolerated_by_the_guard`),
    /// because it runs on every resolve including ones that never fetch. An
    /// attacker-controlled domain can therefore answer NXDOMAIN while that guard
    /// asks and loopback when the pull's own lookup dials. Here a request is
    /// imminent, so the same input fails closed.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_fails_closed_where_the_resolve_time_guard_tolerates() {
        let (host, port) = ocx_oci::ssrf::split_host_port(UNRESOLVABLE);
        assert!(
            matches!(
                ocx_oci::ssrf::resolve_and_validate(host, port, &[]).await,
                Err(ocx_oci::ssrf::SsrfError::Resolution { .. })
            ),
            "the fixture must genuinely fail to resolve, or this says nothing about the tolerated arm"
        );

        let directory = tempfile::tempdir().unwrap();
        let error = chained_with(&directory, vec![])
            .guard_physical_dial(&logical_id(), &physical_id(UNRESOLVABLE))
            .await
            .expect_err("an unjudgeable host must not be dialled");
        assert!(
            matches!(
                error,
                error::Error::Ssrf {
                    source: ocx_oci::ssrf::PhysicalDialRefused {
                        source: ocx_oci::ssrf::SsrfError::Resolution { .. },
                        ..
                    },
                }
            ),
            "expected the lookup failure to refuse, got: {error:?}"
        );
    }

    /// The escape hatch reaches this guard through the chain's registry-ownership
    /// keying: the exemption is configured on the LOGICAL namespace's source, not
    /// on the physical host's.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_admits_a_target_the_logical_namespace_trusts() {
        let directory = tempfile::tempdir().unwrap();
        let (host, _) = ocx_oci::ssrf::split_host_port(LOOPBACK);
        let source = super::test_source::TrustingSource::new(
            "example.com",
            vec![host.to_string()],
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        )
        .into_index();
        chained_with(&directory, vec![source])
            .guard_physical_dial(&logical_id(), &physical_id(LOOPBACK))
            .await
            .expect("a host the namespace's trusted_hosts names must be admitted");
    }

    /// Same forbidden host, no exemption — but the "rewrite" carve-out applies
    /// because the physical target IS the identifier's own registry. Guarding it
    /// would refuse every private registry that predates indices, including the
    /// loopback registry the acceptance suite pulls from.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_does_not_judge_a_target_that_is_not_a_rewrite() {
        let directory = tempfile::tempdir().unwrap();
        let logical = Identifier::new_registry("kitware/cmake", LOOPBACK);
        chained_with(&directory, vec![])
            .guard_physical_dial(&logical, &physical_id(LOOPBACK))
            .await
            .expect("a root naming the identifier's own registry is not a rewrite");
    }

    // ── `route_for_dial`: routing, version carry, and the floor in one call ──

    /// A source that serves every identifier it is asked about from
    /// `registry/contrib/<repository>`, minted the way a source minted it
    /// before C-2 — digest carried, tag dropped — so a tag on the routed
    /// identifier can only be [`Index::route_for_dial`]'s doing.
    #[derive(Clone)]
    struct RewritingSource {
        registry: &'static str,
    }

    #[async_trait]
    impl index_impl::IndexImpl for RewritingSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
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
        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn physical_reference(&self, identifier: &Identifier) -> Result<Option<Identifier>> {
            let physical = Identifier::new_registry(format!("contrib/{}", identifier.repository()), self.registry);
            Ok(Some(match identifier.digest() {
                Some(digest) => physical.clone_with_digest(digest),
                None => physical,
            }))
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    /// A configured index that holds no root for anything: its jurisdiction
    /// verdict is the one under test, and every question it is asked misses.
    #[derive(Clone)]
    struct EmptyIndexSource {
        jurisdiction: Jurisdiction,
    }

    const EMPTY_INDEX_BASE_URL: &str = "https://index.example.invalid";

    #[async_trait]
    impl index_impl::IndexImpl for EmptyIndexSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
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
        async fn fetch_blob(&self, _: &ocx_oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        fn jurisdiction(&self, _: &Identifier) -> Jurisdiction {
            self.jurisdiction
        }
        fn index_base_url(&self) -> Option<&str> {
            Some(EMPTY_INDEX_BASE_URL)
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn versioned_logical_id() -> Identifier {
        logical_id()
            .clone_with_tag("3.28")
            .clone_with_digest(Digest::Sha256("a".repeat(64)))
    }

    /// No source rewrites a registry-backed name, so the dial goes where the
    /// name says — tag and digest untouched.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_for_dial_passes_an_unrewritten_identifier_through() {
        let directory = tempfile::tempdir().unwrap();
        let logical = versioned_logical_id();
        let routed = chained_with(&directory, vec![])
            .route_for_dial(&logical)
            .await
            .expect("an unrewritten identifier routes to itself");
        assert_eq!(routed, logical);
    }

    /// A name in a registry an index serves authoritatively, which that index
    /// does not hold, is not in the index — never a dial to the logical host,
    /// which is not a registry (`https://ocx.sh/v2`). Same verdict, same error
    /// as the resolve path's authoritative miss, so push and pull agree.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_for_dial_refuses_a_name_its_authoritative_index_does_not_hold() {
        let directory = tempfile::tempdir().unwrap();
        let source = Index::from_impl(EmptyIndexSource {
            jurisdiction: Jurisdiction::Authoritative,
        });
        let logical = versioned_logical_id();
        let error = chained_with(&directory, vec![source])
            .route_for_dial(&logical)
            .await
            .expect_err("an authoritative index's miss must not fall back to the logical host");
        match error {
            error::Error::NotInIndex {
                identifier,
                namespace,
                base_url,
            } => {
                assert_eq!(identifier, logical.to_string());
                assert_eq!(namespace, "example.com");
                assert_eq!(
                    base_url, EMPTY_INDEX_BASE_URL,
                    "the error names the index that answered"
                );
            }
            other => panic!("expected NotInIndex, got: {other:?}"),
        }
    }

    /// The same miss from a source that only falls through — a plain registry
    /// claims nothing — is no verdict at all: the name is registry-backed and
    /// routes to itself (S-5).
    #[tokio::test(flavor = "multi_thread")]
    async fn route_for_dial_passes_through_a_miss_no_index_is_authoritative_for() {
        let directory = tempfile::tempdir().unwrap();
        let source = Index::from_impl(EmptyIndexSource {
            jurisdiction: Jurisdiction::FallThrough,
        });
        let logical = versioned_logical_id();
        let routed = chained_with(&directory, vec![source])
            .route_for_dial(&logical)
            .await
            .expect("a fall-through miss is a registry-backed name");
        assert_eq!(routed, logical);
    }

    /// The routed identifier names the physical location at the logical
    /// VERSION: the digest content-addresses the read, and the tag is what a
    /// read by tag (the `any`-provenance fetch) needs. A source that drops the
    /// tag must not cost the caller it.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_for_dial_carries_the_logical_tag_and_digest_onto_the_physical_location() {
        let logical = versioned_logical_id();
        let routed = Index::from_impl(RewritingSource {
            registry: "example.com",
        })
        .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct())
        .route_for_dial(&logical)
        .await
        .expect("a same-registry rewrite is not judged by the floor");

        assert_eq!(routed.registry(), "example.com");
        assert_eq!(routed.repository(), "contrib/kitware/cmake");
        assert_eq!(routed.tag(), Some("3.28"), "the logical tag must survive routing");
        assert_eq!(routed.digest(), logical.digest(), "and so must the logical digest");
    }

    /// Routing and the dial-site floor are one call, so no caller can route
    /// and forget to guard: a rewrite into a forbidden range is refused here.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_for_dial_refuses_a_rewrite_into_a_forbidden_target() {
        let error = Index::from_impl(RewritingSource { registry: LOOPBACK })
            .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct())
            .route_for_dial(&versioned_logical_id())
            .await
            .expect_err("a rewritten loopback target must be refused before anything dials it");
        assert!(is_forbidden_refusal(&error), "expected an SSRF refusal, got: {error:?}");
    }

    /// C-2: one helper stamps the logical version onto a physical location,
    /// and `clone_with_tag` drops a digest — so the order inside it matters.
    #[test]
    fn at_version_of_keeps_both_the_tag_and_the_digest() {
        let logical = versioned_logical_id();
        let physical = at_version_of("ghcr.io", "ocx-contrib/cmake", &logical);
        assert_eq!(
            physical.to_string(),
            format!("ghcr.io/ocx-contrib/cmake:3.28@sha256:{}", "a".repeat(64))
        );

        let digest_only = logical.without_tag();
        assert_eq!(at_version_of("ghcr.io", "ocx-contrib/cmake", &digest_only).tag(), None);
        let tag_only = logical.without_digest();
        assert_eq!(at_version_of("ghcr.io", "ocx-contrib/cmake", &tag_only).digest(), None);
    }

    // ── The dial-site floor under an HTTP proxy (ocx#407) ────────────────────

    /// A registry authority with a `.invalid` name (RFC 6761: never resolves)
    /// and an explicit port, so it is also a legal `OCX_INSECURE_REGISTRIES`
    /// spelling. On a proxy-only-DNS network this is every external registry:
    /// the process cannot resolve it and does not have to.
    const PHANTOM_REGISTRY: &str = "no-such-registry.invalid:5000";
    /// The forbidden target the proxy route must not launder.
    const FORBIDDEN_LITERAL: &str = "127.0.0.1:5000";
    /// Hostname form on purpose: a proxy is operator config, and its own name
    /// is what the process resolves. Nothing listens on it — the guard decides
    /// the route without dialling either host.
    const PROXY: &str = "http://proxy.corp:3128";

    /// `ALL_PROXY`-equivalent: every scheme goes through the proxy.
    fn proxied_everywhere() -> std::sync::Arc<ocx_oci::ssrf::ProxyRules> {
        ocx_oci::ssrf::ProxyRules::proxied_everywhere(PROXY)
    }

    /// `HTTP_PROXY` only — the proxy intercepts plain-HTTP dials and nothing
    /// else, so the route now depends on the scheme the guard picks. Built
    /// from an injected matcher, never the environment: mutating env vars is
    /// unsafe in edition 2024 and racy across a test binary's threads.
    fn proxied_for_plain_http_only() -> std::sync::Arc<ocx_oci::ssrf::ProxyRules> {
        std::sync::Arc::new(ocx_oci::ssrf::ProxyRules::new(
            hyper_util::client::proxy::matcher::Matcher::builder()
                .http(PROXY)
                .build(),
        ))
    }

    /// [`chained_with`], plus the `OCX_INSECURE_REGISTRIES` authorities the
    /// operator declared — the set that decides whether a dial is `http` or
    /// `https`, and so which proxy setting applies to it.
    fn chained_with_insecure_hosts(directory: &tempfile::TempDir, insecure_hosts: Vec<String>) -> Index {
        Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: crate::IndexStore::new(directory.path().join("index")),
            })
            .with_insecure_hosts(insecure_hosts),
            vec![],
            ChainMode::Default,
        )
        .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct())
    }

    fn is_resolution_refusal(error: &crate::error::Error) -> bool {
        matches!(
            error,
            error::Error::Ssrf {
                source: ocx_oci::ssrf::PhysicalDialRefused {
                    source: ocx_oci::ssrf::SsrfError::Resolution { .. },
                    ..
                },
            }
        )
    }

    /// ocx#407. Under a proxy the process never resolves the destination: the
    /// name is literal text in the `CONNECT host:port` line, and the proxy
    /// resolves it. On a corporate network where only the proxy has external
    /// DNS the pre-flight lookup therefore cannot succeed, and refusing on it
    /// aborts a pull that would have worked — the guard is answering a question
    /// nobody asked.
    ///
    /// The pair of
    /// [`guard_physical_dial_fails_closed_where_the_resolve_time_guard_tolerates`]:
    /// same unresolvable input, opposite verdict, and the ONLY thing that
    /// differs is the route. The rebinding rationale that makes the direct case
    /// fail closed has no purchase here, because there is no local lookup whose
    /// answer a later one could contradict.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_admits_an_unresolvable_target_on_a_proxied_route() {
        let (host, port) = ocx_oci::ssrf::split_host_port(PHANTOM_REGISTRY);
        assert!(
            matches!(
                ocx_oci::ssrf::resolve_and_validate(host, port, &[]).await,
                Err(ocx_oci::ssrf::SsrfError::Resolution { .. })
            ),
            "the fixture must genuinely fail to resolve, or admitting it proves nothing"
        );

        let directory = tempfile::tempdir().unwrap();
        chained_with(&directory, vec![])
            .with_proxy_rules(proxied_everywhere())
            .guard_physical_dial(&logical_id(), &physical_id(PHANTOM_REGISTRY))
            .await
            .expect("a destination only the proxy has to resolve must not be refused for not resolving here");
    }

    /// The guard half of the pair above: the proxy route skips the LOOKUP, not
    /// the FLOOR. An index root naming a loopback literal is refused on the
    /// text alone — no DNS needed to judge an address that is already one, and
    /// a proxy would happily connect back into the caller's own network.
    ///
    /// Green before and after the fix, deliberately: this is what proves the
    /// #407 change did not turn `Proxied` into a bypass.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_refuses_a_forbidden_literal_even_on_a_proxied_route() {
        let directory = tempfile::tempdir().unwrap();
        let error = chained_with(&directory, vec![])
            .with_proxy_rules(proxied_everywhere())
            .guard_physical_dial(&logical_id(), &physical_id(FORBIDDEN_LITERAL))
            .await
            .expect_err("a forbidden literal must be refused whoever dials it");
        assert!(is_forbidden_refusal(&error), "expected an SSRF refusal, got: {error:?}");
    }

    /// The scheme decides which proxy setting applies, and only the declared
    /// `OCX_INSECURE_REGISTRIES` set can say a dial is plain HTTP. With an
    /// `HTTP_PROXY`-only configuration a declared-insecure destination is
    /// proxied, so the unresolvable name is admitted exactly as above.
    ///
    /// Paired with
    /// [`guard_physical_dial_keeps_the_https_route_for_a_registry_not_declared_insecure`]:
    /// same rules, same host, and the ONLY difference is whether the operator
    /// declared it insecure. Without the pair, an implementation that always
    /// probed `http` would pass this test while silently skipping the floor for
    /// every HTTPS registry on a machine that only sets `HTTP_PROXY`.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_takes_the_plain_http_route_for_a_registry_declared_insecure() {
        let directory = tempfile::tempdir().unwrap();
        chained_with_insecure_hosts(&directory, vec![PHANTOM_REGISTRY.to_string()])
            .with_proxy_rules(proxied_for_plain_http_only())
            .guard_physical_dial(&logical_id(), &physical_id(PHANTOM_REGISTRY))
            .await
            .expect("an insecure registry is dialled over http, which this proxy intercepts");
    }

    /// The negative half: the same `HTTP_PROXY`-only rules leave an ordinary
    /// (HTTPS) registry on the direct route, where the floor still resolves and
    /// an unresolvable host is still refused.
    #[tokio::test(flavor = "multi_thread")]
    async fn guard_physical_dial_keeps_the_https_route_for_a_registry_not_declared_insecure() {
        let directory = tempfile::tempdir().unwrap();
        let error = chained_with_insecure_hosts(&directory, vec![])
            .with_proxy_rules(proxied_for_plain_http_only())
            .guard_physical_dial(&logical_id(), &physical_id(PHANTOM_REGISTRY))
            .await
            .expect_err("an https destination is not covered by an http-only proxy");
        assert!(
            is_resolution_refusal(&error),
            "expected the direct route's lookup failure, got: {error:?}"
        );
    }
    // ── Shared clock (C-005 / C-006) ─────────────────────────────────

    /// The instant every clock test below pins the seam to.
    ///
    /// Deliberately **past-dated**: an implementation that ignores the pin
    /// renders today's wall clock, which can never equal `2001-02-03…`, so the
    /// assertions red. A "today at 23:59:59" pin would agree with an unpinned
    /// read on the one day of the year it names — green for the wrong reason.
    const PINNED_INSTANT: &str = "2001-02-03T04:05:06Z";

    /// [`PINNED_INSTANT`]'s `%Y-%m-%d` rendering, written out rather than
    /// sliced from it, so a `current_date` that slices the wrong window is
    /// compared against a literal instead of against its own arithmetic.
    const PINNED_DATE: &str = "2001-02-03";

    /// Owns `__OCX_TESTING_ANNOUNCE_CLOCK` for the lifetime of one test.
    ///
    /// The variable's name is a **literal here**, never a constant shared with
    /// production: renaming the name production reads leaves this pin inert and
    /// reds every test holding the guard (C-006). A test that borrowed
    /// production's own constant would follow the rename and prove nothing.
    ///
    /// The real process variable is written rather than
    /// [`ocx_util::env::overrides::EnvLock`]'s override map, because that map is only
    /// consulted by `ocx_util::env::var`, while a real variable is seen by both
    /// `ocx_util::env::var` (which falls through to it) and `std::env::var` — so
    /// the pin lands whichever way production reads the seam, and this test does
    /// not silently dictate that choice. `EnvLock` is still held, for the
    /// process-wide serialisation it exists to provide.
    struct ClockSeam {
        _lock: ocx_util::env::overrides::EnvLock,
    }

    impl ClockSeam {
        /// Pins the seam to `instant` until the guard drops.
        fn pinned(instant: &str) -> Self {
            let lock = ocx_util::env::overrides::lock();
            // SAFETY: nextest (`taskfiles/rust.taskfile.yml:146`) gives every
            // test its own process, and `EnvLock` serialises this write against
            // every test that goes through `ocx_util::env::overrides`. Two seams opt out
            // of that serialisation instead — `ocx_oci::host_capabilities` and
            // `file_structure::shim_bin_store` — each safe only because one test
            // function owns its variable.
            unsafe { std::env::set_var("__OCX_TESTING_ANNOUNCE_CLOCK", instant) };
            Self { _lock: lock }
        }

        /// Holds the seam **unset**, so production renders the wall clock.
        fn unset() -> Self {
            let lock = ocx_util::env::overrides::lock();
            // SAFETY: see `ClockSeam::pinned`.
            unsafe { std::env::remove_var("__OCX_TESTING_ANNOUNCE_CLOCK") };
            Self { _lock: lock }
        }
    }

    impl Drop for ClockSeam {
        fn drop(&mut self) {
            // SAFETY: see `ClockSeam::pinned`. A struct's own `Drop` runs before
            // its fields', so the variable is gone before `_lock` releases the
            // mutex. Unconditional, so a stub that panics mid-test cannot leak
            // the pin into a sibling — the Specify phase runs against
            // `unimplemented!()`, where every one of these tests panics.
            unsafe { std::env::remove_var("__OCX_TESTING_ANNOUNCE_CLOCK") };
        }
    }

    /// C-005 — `current_date()` is the first ten characters of
    /// `current_timestamp()`'s render of the **same** instant.
    ///
    /// The live-read form of this test is worthless and is deliberately not
    /// written: two functions that each call `Utc::now()` independently agree on
    /// their date component on every run but one that straddles midnight UTC, so
    /// a bare `assert_eq!(current_date(), &current_timestamp()[..10])` passes
    /// under the exact defect C-005 exists to prevent. The instant is therefore
    /// pinned, and both renderings are asserted against that known value rather
    /// than against each other alone.
    #[test]
    fn current_date_is_first_ten_chars_of_timestamp() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        let timestamp = current_timestamp();
        let date = current_date();

        assert_eq!(timestamp, PINNED_INSTANT, "the pinned instant is what gets rendered");
        assert_eq!(
            date, PINNED_DATE,
            "the date must derive from the timestamp's instant, not from an independent clock read"
        );
        assert_eq!(date, timestamp[..10], "C-005: one instant, two renderings");
    }

    /// C-006 — the seam is spelled exactly `__OCX_TESTING_ANNOUNCE_CLOCK`, and
    /// it overrides **both** renderings.
    ///
    /// Because [`PINNED_INSTANT`] is in the past, a production read of any other
    /// spelling leaves the pin inert and both calls render today — which reds
    /// every assertion below. That is what ties this test to the exact name
    /// rather than to "some override exists".
    #[test]
    fn testing_clock_overrides_both() {
        let _seam = ClockSeam::pinned(PINNED_INSTANT);

        assert_eq!(current_timestamp(), PINNED_INSTANT, "the timestamp is overridden");
        assert_eq!(current_date(), PINNED_DATE, "and so is the date");
        assert_ne!(
            current_date(),
            chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "the pin must displace the wall clock, not merely coexist with it"
        );
    }

    /// C-005's format half, which no pinned test can reach: the seam returns its
    /// value verbatim, so only an **unpinned** call observes the render that
    /// production actually writes into an index root. The ten-character prefix
    /// [`current_date_is_first_ten_chars_of_timestamp`] asserts is a date only
    /// if this holds. Parsing is delegated to chrono, not hand-matched.
    #[test]
    fn current_timestamp_renders_the_seconds_z_form() {
        let _seam = ClockSeam::unset();

        let rendered = current_timestamp();

        assert!(
            chrono::NaiveDateTime::parse_from_str(&rendered, "%Y-%m-%dT%H:%M:%SZ").is_ok(),
            "expected the index bot's %Y-%m-%dT%H:%M:%SZ seconds-Z form, got {rendered:?}"
        );
    }
}
