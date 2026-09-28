// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The OCX resolution-index protocol and its local collection.

use ocx_oci::tag::is_reserved_tag;
use ocx_util::prelude::*;

use error::Result;

pub mod error;

pub use self::ocx_index::{IndexBase, IndexFetch, IndexTransport, OcxIndex, OcxIndexConfig, ReqwestIndexTransport};
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

/// The current instant in the index bot's `%Y-%m-%dT%H:%M:%SZ` form;
/// `__OCX_TESTING_ANNOUNCE_CLOCK` pins it in test and `__testing` builds.
pub fn current_timestamp() -> String {
    #[cfg(any(test, feature = "__testing"))]
    {
        if let Ok(fixed) = std::env::var("__OCX_TESTING_ANNOUNCE_CLOCK") {
            return fixed;
        }
    }
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The current instant as a bare `%Y-%m-%d` date: a prefix of
/// [`current_timestamp`], never a second clock read, or the two can straddle midnight and disagree.
pub fn current_date() -> String {
    let timestamp = current_timestamp();
    // A test pin shorter than ten bytes panics by design, not a truncated date written into a root.
    timestamp[..10].to_string()
}

/// Test seam: lets tests outside this crate build an `Index` from a mock `IndexImpl`.
#[cfg(any(test, feature = "__testing"))]
pub use index_impl::IndexImpl;

/// Whether a chain source answers for a name, and what its silence means
/// (`adr_index_indirection.md#decision-h`); asked before any fetch, so it is never confused with a fetch's `Ok(None)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jurisdiction {
    /// Ask it; its miss or refusal is terminal, never shadowed by a registry serving the same name.
    Authoritative,
    /// Ask it; its miss falls through to the next source (the OCI catch-all).
    FallThrough,
    /// The name is in another registry: never ask it, and its silence decides nothing.
    Outside,
}

/// Cache/source routing policy for a [`ChainedIndex`](chained_index::ChainedIndex).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainMode {
    /// Local index first; a tag `Resolve` miss walks the chain and persists, a `Query` miss is `None`.
    Default,
    /// `--remote`: mutable lookups (tags, catalog, tag-addressed manifests) go
    /// straight to the source; digest lookups stay local-first.
    Remote,
    /// `--offline`: local index only; a digest miss is `None`, an unpinned tag `Resolve` miss errors.
    Offline,
    /// `--frozen`: an unpinned tag `Resolve` miss errors as under [`Self::Offline`],
    /// but digest-addressed content is still fetched from the source.
    Frozen,
}

impl ChainMode {
    /// Lowercase flag name embedded in [`error::Error::PolicyResolutionBlocked`].
    pub fn policy_label(self) -> &'static str {
        match self {
            ChainMode::Default => "default",
            ChainMode::Remote => "remote",
            ChainMode::Offline => "offline",
            ChainMode::Frozen => "frozen",
        }
    }
}

/// Caller intent for a manifest lookup (`adr_index_routing_semantics.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOperation {
    /// Pure read: never walks the source chain or writes the local index on a miss.
    Query,
    /// Read with write-through: a miss walks the chain and persists the manifest
    /// (and, for a tag-only identifier, the tag pointer).
    Resolve,
}

/// The result of a platform-aware package selection.
pub enum SelectResult {
    /// Exactly one candidate matched.
    Found(ocx_oci::PackageRef),
    /// Multiple candidates matched; the caller decides.
    Ambiguous(Vec<ocx_oci::PackageRef>),
    /// No candidate matched the platform, or the package is absent.
    NotFound,
    /// Candidates share the host's os+arch but none satisfies its `os.features` (e.g. a
    /// different libc); `available` lists the platforms a `--platform` override could target.
    FeatureMismatch {
        host_features: Vec<String>,
        available: Vec<ocx_oci::Platform>,
    },
}

/// Who says which content a package's tag names — see [`Index::resolve_version`].
#[derive(Debug)]
pub enum ResolvedVersion {
    /// No index owns the name, or the identifier carries its own digest: read the registry.
    Registry,
    /// The index's committed dispatch manifest, never whatever the physical tag names now.
    Indexed {
        digest: ocx_oci::Digest,
        manifest: Box<ocx_oci::Manifest>,
    },
    /// The index owns the name and holds no such tag.
    Absent,
}

/// A package index; clones share one in-memory cache, which is never cleared.
pub struct Index {
    inner: Box<dyn index_impl::IndexImpl>,
    /// Proxy-route rules for the dial-site SSRF guard ([`Self::guard_physical_dial`]).
    rules: std::sync::Arc<ocx_oci::ssrf::ProxyRules>,
}

impl Index {
    pub fn from_remote(oci_index: OciIndex) -> Self {
        Self {
            inner: Box::new(oci_index),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Wrap an [`OcxIndex`] (an `index.ocx.sh`-style static-file source) as a chain source
    /// (`adr_index_indirection.md#decision-f`).
    pub fn from_source(source: OcxIndex) -> Self {
        Self {
            inner: Box::new(source),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Test seam: wrap a mock `IndexImpl` without going through `from_chained`.
    #[cfg(any(test, feature = "__testing"))]
    pub fn from_impl(inner: impl index_impl::IndexImpl + 'static) -> Self {
        Self {
            inner: Box::new(inner),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// An index reading `cache` first, then `sources` in order, routed by `mode`.
    pub fn from_chained(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            inner: Box::new(chained_index::ChainedIndex::new(cache, sources, mode)),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Like [`Self::from_chained`], but an absent dispatch object recovers from the
    /// machine-global blob store before any source walk (`adr_index_indirection.md#a3`).
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

    /// Like [`Self::from_chained`], but never commits a tag pointer: the caller's lock
    /// file records tag -> digest (`adr_toolchain_update_family.md`).
    pub fn from_chained_lock_scoped(cache: LocalIndex, sources: Vec<Index>, mode: ChainMode) -> Self {
        Self {
            inner: Box::new(chained_index::ChainedIndex::new_lock_scoped(cache, sources, mode)),
            rules: ocx_oci::ssrf::proxy_rules(),
        }
    }

    /// Test seam: override the proxy rules used by [`Self::guard_physical_dial`].
    #[must_use]
    pub fn with_proxy_rules(mut self, rules: std::sync::Arc<ocx_oci::ssrf::ProxyRules>) -> Self {
        // Pin the chain's copy too, or its resolve-time guard stays on the ambient environment.
        self.inner.set_proxy_rules(rules.clone());
        self.rules = rules;
        self
    }

    /// A view that resolves identically but writes nothing into the local index;
    /// content-addressed blob writes still happen.
    pub fn read_only_view(&self) -> Self {
        Self {
            inner: self.inner.read_only_view(),
            rules: self.rules.clone(),
        }
    }

    /// A view that lists and reads live from the sources whatever the [`ChainMode`],
    /// and writes nothing into the local index.
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

    /// Tags for `identifier`, minus reserved tags ([`is_reserved_tag`]); `None` when the
    /// package is unknown to this index.
    pub async fn list_tags(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<Vec<String>>> {
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

    /// Fetch the manifest for `identifier`; `None` when unavailable under `op` and the mode's routing.
    pub async fn fetch_manifest(
        &self,
        identifier: &ocx_oci::PackageRef,
        op: IndexOperation,
    ) -> Result<Option<(ocx_oci::Digest, ocx_oci::Manifest)>> {
        log::trace!("Fetching candidates for identifier '{}'.", identifier);
        self.inner.fetch_manifest(identifier, op).await
    }

    /// The manifest digest for `identifier`, `None` when unresolvable; `op` as on [`Self::fetch_manifest`].
    pub async fn fetch_manifest_digest(
        &self,
        identifier: &ocx_oci::PackageRef,
        op: IndexOperation,
    ) -> Result<Option<ocx_oci::Digest>> {
        self.inner.fetch_manifest_digest(identifier, op).await
    }

    /// Fetch a content blob's bytes; `Ok(None)` is a miss the routing policy cannot recover.
    pub async fn fetch_blob(&self, blob_ref: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
        log::trace!("Fetching blob '{blob_ref}'.");
        self.inner.fetch_blob(blob_ref).await
    }

    /// Where a read of `identifier` goes: the physical location its index routes it to, or `identifier`
    /// itself when nothing rewrites it, carrying `identifier`'s tag and digest.
    ///
    /// Transport-only (`adr_index_indirection.md#c2-structural`), never a storage key. No dial guard runs,
    /// so a warm offline resolve never looks up the physical host; a caller about to dial wants
    /// [`Self::route_for_dial`].
    ///
    /// # Errors
    ///
    /// Index lookup failures, the local SSRF floor included; [`error::Error::NotInIndex`] when
    /// the authoritative index lacks the name.
    pub async fn route(&self, identifier: &ocx_oci::PackageRef) -> Result<ocx_oci::OciIdentifier> {
        match self.physical_reference(identifier).await? {
            Some(physical) => Ok(physical),
            None => self.unrouted(identifier),
        }
    }

    /// [`Self::route`] plus the dial-site SSRF floor, for a caller that dials the registry
    /// directly through a `Client`.
    ///
    /// # Errors
    ///
    /// [`Self::route`]'s errors; [`error::Error::Ssrf`] when the floor refuses the rewritten target.
    pub async fn route_for_dial(&self, identifier: &ocx_oci::PackageRef) -> Result<ocx_oci::OciIdentifier> {
        let Some(routed) = self.physical_reference(identifier).await? else {
            return self.unrouted(identifier);
        };
        self.guard_physical_dial(identifier, &routed).await?;
        Ok(routed)
    }

    /// Which content `identifier`'s tag names, asked of whoever owns the answer; `routed` is
    /// [`Self::route_for_dial`]'s answer for it.
    ///
    /// For an index-served name the index's resolution is the version, not the physical tag, which may
    /// have moved since publication. Asked through [`Self::read_only_view`], so nothing is written locally.
    ///
    /// # Errors
    ///
    /// Index resolution failures, a yanked tag and an `--offline`/`--frozen` refusal included.
    pub async fn resolve_version(
        &self,
        identifier: &ocx_oci::PackageRef,
        routed: &ocx_oci::OciIdentifier,
    ) -> Result<ResolvedVersion> {
        let index_served = !identifier.is_located_at(routed) || self.authoritative_index_base_url(identifier).is_some();
        if identifier.digest().is_some() || !index_served {
            return Ok(ResolvedVersion::Registry);
        }
        Ok(
            match self
                .read_only_view()
                .fetch_manifest(identifier, IndexOperation::Resolve)
                .await?
            {
                Some((digest, manifest)) => ResolvedVersion::Indexed {
                    digest,
                    manifest: Box::new(manifest),
                },
                None => ResolvedVersion::Absent,
            },
        )
    }

    /// Where the locally committed root routes `identifier`, never dialling a source;
    /// `None` when no root is committed for the name.
    ///
    /// # Errors
    ///
    /// The local SSRF floor refusing the committed pointer.
    pub async fn route_local(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        Ok(self
            .inner
            .physical_reference_local(identifier)
            .await?
            .map(|physical| physical.at_version_of(identifier)))
    }

    /// [`Self::route`] that also records a source-answered routing pointer locally; a separate
    /// entry point so only a materializing resolve writes, never a reader.
    ///
    /// # Errors
    ///
    /// [`Self::route`]'s errors; the record itself is best-effort.
    pub async fn route_to_materialize(&self, identifier: &ocx_oci::PackageRef) -> Result<ocx_oci::OciIdentifier> {
        match self.physical_reference(identifier).await? {
            Some(physical) => {
                self.inner.record_routing_pointer(identifier).await;
                Ok(physical)
            }
            None => self.unrouted(identifier),
        }
    }

    /// `identifier` itself, unless an index is authoritative for its registry: then the miss is
    /// [`error::Error::NotInIndex`], never a read of the host the name happens to spell.
    fn unrouted(&self, identifier: &ocx_oci::PackageRef) -> Result<ocx_oci::OciIdentifier> {
        if let Some(base_url) = self.authoritative_index_base_url(identifier) {
            return Err(error::Error::NotInIndex {
                identifier: identifier.to_string(),
                namespace: identifier.registry().to_string(),
                base_url: base_url.to_string(),
            });
        }
        match self.inner.refuse_unrouted(identifier) {
            Some(refusal) => Err(refusal),
            None => Ok(ocx_oci::OciIdentifier::passthrough(identifier)),
        }
    }

    /// The `trusted_hosts` SSRF exemption configured for `registry`.
    pub fn trusted_hosts_for(&self, registry: &str) -> &[String] {
        self.inner.trusted_hosts_for(registry)
    }

    /// The plain-HTTP allowance set (`[registries."<ns>"].insecure`).
    #[must_use]
    pub fn insecure_hosts(&self) -> &[String] {
        self.inner.insecure_hosts()
    }

    /// The dial-site SSRF floor as a value a pipeline can hold without the index; `registry` is
    /// the logical one, which `trusted_hosts` is keyed on, never the physical host.
    #[must_use]
    pub fn dial_policy(&self, registry: &str) -> ocx_oci::ssrf::DialPolicy<'_> {
        ocx_oci::ssrf::DialPolicy {
            insecure_hosts: self.inner.insecure_hosts(),
            trusted_hosts: self.inner.trusted_hosts_for(registry),
            rules: &self.rules,
        }
    }

    /// SSRF floor for a rewritten physical target, run at the dial site just before the first request.
    ///
    /// Fails closed on a lookup failure too, unlike [`Self::route`]'s tolerant guard: the pull re-resolves
    /// on an unguarded client, so a tree answering NXDOMAIN here and loopback there would pass.
    ///
    /// # Errors
    ///
    /// [`error::Error::Ssrf`] when the host resolves into a forbidden range without a
    /// `trusted_hosts` entry, or, on a direct dial only, cannot be resolved.
    pub async fn guard_physical_dial(
        &self,
        logical: &ocx_oci::PackageRef,
        physical: &ocx_oci::OciIdentifier,
    ) -> Result<()> {
        // Not a rewrite: guarding it would refuse every private registry that predates indices.
        if physical.registry() == logical.registry() {
            return Ok(());
        }
        // Unresolvable fails closed on a direct dial only: behind a proxy the name never reaches a local
        // resolver, so refusing would break proxy-only-DNS networks. Residual: validate → connect stays open.
        ocx_oci::ssrf::guard_physical_dial(&self.dial_policy(logical.registry()), logical, physical)
            .await
            .map_err(|refused| error::Error::Ssrf { source: refused })?;
        Ok(())
    }

    /// A published root document verbatim (bytes + parsed) so a local copy grows byte-for-byte
    /// (`adr_index_indirection.md#a2`); `None` for a derived source.
    pub async fn fetch_root_document(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, wire::IndexRoot)>> {
        self.inner.fetch_root_document(identifier).await
    }

    /// The physical location a source rewrites `identifier` to, or `None`; re-addressed at
    /// `identifier`'s version so a source that dropped the tag or digest cannot hand back the wrong one.
    async fn physical_reference(&self, identifier: &ocx_oci::PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
        Ok(self
            .inner
            .physical_reference(identifier)
            .await?
            .map(|physical| physical.at_version_of(identifier)))
    }

    /// Whether this index answers for `identifier`, and what its silence means.
    fn jurisdiction(&self, identifier: &ocx_oci::PackageRef) -> Jurisdiction {
        self.inner.jurisdiction(identifier)
    }

    /// Whether a source in this index is the configured owner of `registry` (no I/O).
    fn serves_registry(&self, registry: &str) -> bool {
        self.inner.serves_registry(registry)
    }

    /// This source's `trusted_hosts` SSRF exemption set.
    fn trusted_hosts(&self) -> &[String] {
        self.inner.trusted_hosts()
    }

    /// The static-file base URL this source resolves against, `None` when it is not a configured ocx-index.
    fn index_base_url(&self) -> Option<&str> {
        self.inner.index_base_url()
    }

    /// The base URL of the index authoritative for `identifier`, if any.
    fn authoritative_index_base_url(&self, identifier: &ocx_oci::PackageRef) -> Option<&str> {
        self.inner.authoritative_index_base_url(identifier)
    }

    /// This source's provenance (`adr_index_indirection.md#a2`): `Published` for an
    /// `index.ocx.sh`-style source, `Derived` otherwise (no I/O).
    fn source_kind(&self) -> local_index::SourceKind {
        self.inner.source_kind()
    }

    /// The verbatim manifest bytes with digest and parsed manifest, so
    /// [`LocalIndex::persist_dispatch`] writes a verifiable dispatch object; `Ok(None)` when absent.
    pub async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &ocx_oci::PackageRef,
    ) -> Result<Option<(Vec<u8>, ocx_oci::Digest, ocx_oci::Manifest)>> {
        log::trace!("Fetching raw manifest bytes for identifier '{}'.", identifier);
        self.inner.fetch_manifest_raw_bytes(identifier).await
    }

    pub async fn fetch_candidates(
        &self,
        identifier: &ocx_oci::PackageRef,
        op: IndexOperation,
    ) -> Result<Option<Vec<(ocx_oci::PackageRef, ocx_oci::Platform)>>> {
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
                    // A descriptor with no representable platform is an attestation/referrer entry, not a fault.
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
        identifier: &ocx_oci::PackageRef,
        platform: &ocx_oci::Platform,
        op: IndexOperation,
    ) -> Result<SelectResult> {
        log::debug!("Selecting package '{}' for platform {}.", identifier, platform);

        let Some(candidates) = self.fetch_candidates(identifier, op).await? else {
            log::debug!("No candidates found for '{}'.", identifier);
            return Ok(SelectResult::NotFound);
        };

        // Lock-read and authoring pinning use `select_best` too; a local matcher here would make them disagree.
        let result = match ocx_oci::select_best(platform, &candidates) {
            ocx_oci::Selection::Found(id) => SelectResult::Found(id),
            ocx_oci::Selection::Ambiguous(ids) => SelectResult::Ambiguous(ids),
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

/// The requested platform's `os_features` when `Specific` and non-empty: the signal that a
/// no-match is a feature mismatch rather than a plain not-found.
fn host_os_features(platform: &ocx_oci::Platform) -> Option<Vec<String>> {
    match platform {
        ocx_oci::Platform::Specific { os_features, .. } if !os_features.is_empty() => Some(os_features.clone()),
        _ => None,
    }
}

/// Parses a root's `repository` pointer (`oci://host/path`), refusing with
/// [`Error::MalformedPhysicalRef`](error::Error::MalformedPhysicalRef).
///
/// Every root read and rewrite parses through this one function, so any pointer a read accepts a rewrite can dereference.
fn parse_repository_pointer(value: &str) -> Result<ocx_oci::OciIdentifier> {
    ocx_oci::OciIdentifier::parse_repository_pointer(value).map_err(|_| error::Error::MalformedPhysicalRef {
        value: value.to_string(),
    })
}

/// Candidate platforms sharing os+arch with the `Specific` request, sorted by display string
/// for deterministic error output.
fn candidates_sharing_host_os_arch(
    platform: &ocx_oci::Platform,
    candidates: &[(ocx_oci::PackageRef, ocx_oci::Platform)],
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
    use ocx_oci::{self, Digest, Manifest, PackageRef};

    /// A source that owns a namespace and carries its `trusted_hosts` exemption, counting each
    /// time that set is asked for: one ask per [`Index::guard_physical_dial`] evaluation.
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

    /// A source answering only where a name lives ([`Index::route_for_dial`]) and the tags it is told about.
    ///
    /// Answers digest-only, so a tag on the dialled reference is the router's doing.
    #[derive(Clone)]
    pub struct RoutingSource {
        physical: Option<(String, String)>,
        authoritative_base_url: Option<String>,
        tags: Vec<(String, Digest, Manifest)>,
        yanked: Vec<String>,
    }

    impl RoutingSource {
        pub fn rewriting(registry: &str, repository: &str) -> Self {
            Self {
                physical: Some((registry.to_string(), repository.to_string())),
                authoritative_base_url: None,
                tags: Vec::new(),
                yanked: Vec::new(),
            }
        }

        pub fn passthrough() -> Self {
            Self {
                physical: None,
                authoritative_base_url: None,
                tags: Vec::new(),
                yanked: Vec::new(),
            }
        }

        pub fn authoritative_miss(base_url: &str) -> Self {
            Self {
                physical: None,
                authoritative_base_url: Some(base_url.to_string()),
                tags: Vec::new(),
                yanked: Vec::new(),
            }
        }

        /// Claims authority for every name, as a configured index does: its
        /// silence is a miss, never a hand-off to the registry.
        pub fn authoritative(mut self, base_url: &str) -> Self {
            self.authoritative_base_url = Some(base_url.to_string());
            self
        }

        /// Commits `tag` to `manifest` under `digest`.
        pub fn with_tag(mut self, tag: &str, digest: Digest, manifest: Manifest) -> Self {
            self.tags.push((tag.to_string(), digest, manifest));
            self
        }

        /// Yanks `tag`: resolving it is refused.
        pub fn with_yanked_tag(mut self, tag: &str) -> Self {
            self.yanked.push(tag.to_string());
            self
        }

        /// This source as an [`Index`] on direct proxy rules, so the dial-site floor never reads the
        /// ambient environment; a `registry` equal to the logical one skips its DNS lookup.
        pub fn into_index(self) -> Index {
            Index::from_impl(self).with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct())
        }
    }

    #[async_trait]
    impl index_impl::IndexImpl for RoutingSource {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(None)
        }
        async fn fetch_manifest(
            &self,
            identifier: &PackageRef,
            _: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            if identifier.digest().is_some() {
                return Ok(None);
            }
            let tag = identifier.tag_or_latest();
            if self.yanked.iter().any(|yanked| yanked == tag) {
                return Err(super::error::Error::YankedRefused {
                    identifier: identifier.to_string(),
                });
            }
            Ok(self
                .tags
                .iter()
                .find(|(committed, _, _)| committed == tag)
                .map(|(_, digest, manifest)| (digest.clone(), manifest.clone())))
        }
        async fn fetch_manifest_digest(
            &self,
            identifier: &PackageRef,
            operation: IndexOperation,
        ) -> Result<Option<Digest>> {
            Ok(self
                .fetch_manifest(identifier, operation)
                .await?
                .map(|(digest, _)| digest))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn physical_reference(&self, identifier: &PackageRef) -> Result<Option<ocx_oci::OciIdentifier>> {
            Ok(self.physical.as_ref().map(|(registry, repository)| {
                let physical = ocx_oci::OciIdentifier::from_parts(repository.clone(), registry.clone());
                match identifier.digest() {
                    Some(digest) => physical.clone_with_digest(digest),
                    None => physical,
                }
            }))
        }
        fn jurisdiction(&self, _: &PackageRef) -> super::Jurisdiction {
            match self.authoritative_base_url {
                Some(_) => super::Jurisdiction::Authoritative,
                None => super::Jurisdiction::FallThrough,
            }
        }
        fn index_base_url(&self) -> Option<&str> {
            self.authoritative_base_url.as_deref()
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }
}

// ── Index::select integration tests with multi-libc ImageIndex ──

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ocx_oci::{self, Digest, Manifest, PackageRef, Platform};

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

        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }

        async fn fetch_manifest(
            &self,
            _identifier: &PackageRef,
            _op: IndexOperation,
        ) -> Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }

        async fn fetch_manifest_digest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }

        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }

        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(MultiLibcIndex {
                manifest: self.manifest.clone(),
            })
        }
    }

    fn test_id() -> PackageRef {
        PackageRef::new_registry("test/tool", "example.com").clone_with_tag("1.0")
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
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
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
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
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
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &PackageRef, _: IndexOperation) -> Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &ocx_oci::PinnedPackageRef) -> Result<Option<Vec<u8>>> {
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
        async fn list_tags(&self, _: &PackageRef) -> Result<Option<Vec<String>>> {
            Ok(Some(vec![
                "3.28".to_string(),
                "latest".to_string(),
                "__ocx.desc".to_string(),
                "__OCX.future".to_string(),
                format!("__ocx.keep.sha256-{}", "a".repeat(64)),
            ]))
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

    fn logical_id() -> PackageRef {
        PackageRef::new_registry("kitware/cmake", "example.com")
    }

    fn physical_id(registry: &str) -> ocx_oci::OciIdentifier {
        ocx_oci::OciIdentifier::from_parts("evil/pkg", registry)
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
        let logical = PackageRef::new_registry("kitware/cmake", LOOPBACK);
        chained_with(&directory, vec![])
            .guard_physical_dial(&logical, &physical_id(LOOPBACK))
            .await
            .expect("a root naming the identifier's own registry is not a rewrite");
    }

    // ── `route_for_dial`: routing, version carry, and the floor in one call ──

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
        fn jurisdiction(&self, _: &PackageRef) -> Jurisdiction {
            self.jurisdiction
        }
        fn index_base_url(&self) -> Option<&str> {
            Some(EMPTY_INDEX_BASE_URL)
        }
        fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn versioned_logical_id() -> PackageRef {
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
        assert_eq!(routed, ocx_oci::OciIdentifier::passthrough(&logical));
    }

    /// `--offline` builds no sources, so nothing in the chain can claim a
    /// registry config names an index for. Ownership comes from config
    /// instead: a digest-pinned name there with no committed root is refused
    /// under the offline policy — never passed through to the host it spells
    /// (`https://ocx.sh/v2`). A registry config names no index for passes
    /// through exactly as before.
    #[tokio::test(flavor = "multi_thread")]
    async fn offline_routing_refuses_an_index_owned_name_it_holds_no_root_for() {
        let directory = tempfile::tempdir().unwrap();
        let offline = Index::from_chained(
            LocalIndex::new(LocalConfig {
                index_store: crate::IndexStore::new(directory.path().join("index")),
            })
            .with_index_namespaces(std::collections::HashSet::from(["example.com".to_string()])),
            vec![],
            ChainMode::Offline,
        )
        .with_proxy_rules(ocx_oci::ssrf::ProxyRules::direct());

        let error = offline
            .route_for_dial(&versioned_logical_id())
            .await
            .expect_err("an index-owned name must not route to its logical host offline");
        assert!(
            matches!(
                &error,
                error::Error::PolicyResolutionBlocked {
                    identifier,
                    policy: "offline",
                    block: error::PolicyBlock::UnrecordedLocation,
                } if identifier == "example.com/kitware/cmake"
            ),
            "got: {error:?}"
        );
        assert_eq!(
            error.to_string(),
            "'example.com/kitware/cmake' is served by an index and has no locally recorded location, \
             which offline mode cannot look up; run `ocx index update example.com/kitware/cmake` once online",
            "the refusal says what happened, not that a reference was unpinned"
        );

        let registry_backed = PackageRef::new_registry("kitware/cmake", "registry.example.com")
            .clone_with_digest(Digest::Sha256("a".repeat(64)));
        let routed = offline
            .route_for_dial(&registry_backed)
            .await
            .expect("a registry no index owns routes to itself");
        assert_eq!(routed, ocx_oci::OciIdentifier::passthrough(&registry_backed));
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

    /// `route` refuses the same authoritative miss `route_for_dial` does: no
    /// routing call falls back to the logical host of a namespace an index
    /// owns (ocx#504), with or without a dial guard.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_refuses_a_name_its_authoritative_index_does_not_hold() {
        let directory = tempfile::tempdir().unwrap();
        let source = Index::from_impl(EmptyIndexSource {
            jurisdiction: Jurisdiction::Authoritative,
        });
        let error = chained_with(&directory, vec![source])
            .route(&versioned_logical_id())
            .await
            .expect_err("an authoritative index's miss must not fall back to the logical host");
        assert!(
            matches!(error, error::Error::NotInIndex { .. }),
            "expected NotInIndex, got: {error:?}"
        );
    }

    /// And so does `route_to_materialize`: a pull must not read an index-owned
    /// name from the host it spells.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_to_materialize_refuses_a_name_its_authoritative_index_does_not_hold() {
        let directory = tempfile::tempdir().unwrap();
        let source = Index::from_impl(EmptyIndexSource {
            jurisdiction: Jurisdiction::Authoritative,
        });
        let error = chained_with(&directory, vec![source])
            .route_to_materialize(&versioned_logical_id())
            .await
            .expect_err("an authoritative index's miss must not fall back to the logical host");
        assert!(
            matches!(error, error::Error::NotInIndex { .. }),
            "expected NotInIndex, got: {error:?}"
        );
    }

    /// `route_local` answers from committed state alone and dials nothing, so
    /// a name with no committed root is `None` — an answer, not a refusal —
    /// even where an index is authoritative for it.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_local_answers_none_for_a_name_with_no_committed_root() {
        let directory = tempfile::tempdir().unwrap();
        let source = Index::from_impl(EmptyIndexSource {
            jurisdiction: Jurisdiction::Authoritative,
        });
        let routed = chained_with(&directory, vec![source])
            .route_local(&versioned_logical_id())
            .await
            .expect("no committed root is not an error");
        assert_eq!(routed, None);
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
        assert_eq!(routed, ocx_oci::OciIdentifier::passthrough(&logical));
    }

    /// The routed identifier names the physical location at the logical
    /// VERSION: the digest content-addresses the read, and the tag is what a
    /// read by tag (the `any`-provenance fetch) needs. A source that drops the
    /// tag must not cost the caller it.
    #[tokio::test(flavor = "multi_thread")]
    async fn route_for_dial_carries_the_logical_tag_and_digest_onto_the_physical_location() {
        let logical = versioned_logical_id();
        // `test_source::RoutingSource` mints digest-only, the way a source
        // minted before C-2, so the tag can only be `route_for_dial`'s doing.
        let routed = super::test_source::RoutingSource::rewriting("example.com", "contrib/kitware/cmake")
            .into_index()
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
        let error = super::test_source::RoutingSource::rewriting(LOOPBACK, "contrib/kitware/cmake")
            .into_index()
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
        let physical = ocx_oci::OciIdentifier::from_parts("ocx-contrib/cmake", "ghcr.io").at_version_of(&logical);
        assert_eq!(
            physical.to_string(),
            format!("ghcr.io/ocx-contrib/cmake:3.28@sha256:{}", "a".repeat(64))
        );

        let digest_only = logical.without_tag();
        assert_eq!(
            ocx_oci::OciIdentifier::from_parts("ocx-contrib/cmake", "ghcr.io")
                .at_version_of(&digest_only)
                .tag(),
            None
        );
        let tag_only = logical.without_digest();
        assert_eq!(
            ocx_oci::OciIdentifier::from_parts("ocx-contrib/cmake", "ghcr.io")
                .at_version_of(&tag_only)
                .digest(),
            None
        );
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
