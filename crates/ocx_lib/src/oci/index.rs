// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::log;
use crate::package::tag::Tag;
use crate::{oci, prelude::*};

pub mod error;

pub use index_sync::IndexSync;
pub use local_index::Config as LocalConfig;
pub use local_index::LocalIndex;
pub use oci_index::OciIndex;
pub use oci_index::OciIndexConfig;
pub use ocx_index::{
    CatalogSyncOutcome, DEFAULT_INDEX_BASE_URL, IndexFetch, IndexTransport, OcxIndex, OcxIndexConfig,
    ReqwestIndexTransport, SUPPORTED_FORMAT_VERSION, parse_physical_repository,
};
pub use wire::{CatalogIndex, IndexRoot, Observation, ObservationPlatform, RootTag};

mod chained_index;
mod index_impl;
mod index_sync;
mod local_index;
mod oci_index;
mod ocx_index;
mod wire;

/// Re-export the private `IndexImpl` trait for sibling-module tests.
///
/// Sibling modules (e.g. `project::resolve` unit tests) that need to
/// construct an `Index` from a hand-rolled mock must implement
/// `IndexImpl`. Production code reaches `Index` only via
/// [`Index::from_chained`] / [`Index::from_remote`].
#[cfg(test)]
pub(crate) use index_impl::IndexImpl;

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
#[non_exhaustive]
pub enum IndexOperation {
    /// Pure read. `ChainedIndex` returns the local-index result and never
    /// walks the source chain on miss. Used by `index list`,
    /// `index catalog`, `package info`, and any other path that reports
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
#[non_exhaustive]
pub enum SelectResult {
    /// Exactly one candidate matched.
    Found(oci::Identifier),
    /// Multiple candidates matched — the caller must decide how to handle the
    /// ambiguity (e.g. ask the user or report an error).
    Ambiguous(Vec<oci::Identifier>),
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
        available: Vec<oci::Platform>,
    },
}

/// Note, some operations are cached and the cache is shared between clones of the index.
/// This means that if you clone the index, they will share the same cache and benefit from each other's cached data.
/// On the other hand, if you have a long-running index instance, you may want to periodically clear the cache to avoid memory bloat and ensure that you always have the latest data.
/// The cache is currently never cleared, but expiration or manual clearing may be added in the future if needed.
pub struct Index {
    inner: Box<dyn index_impl::IndexImpl>,
}

impl Index {
    pub fn from_remote(oci_index: OciIndex) -> Self {
        Self {
            inner: Box::new(oci_index),
        }
    }

    /// Wrap an [`OcxIndex`] (an `index.ocx.sh`-style static-file source) as a
    /// chain source. Registered alongside [`OciIndex`] in the default chain
    /// so a logical `ocx.sh/<ns>/<pkg>` reference the registry does not serve
    /// resolves through the two-hop index path (`adr_index_indirection.md` F).
    pub fn from_source(source: OcxIndex) -> Self {
        Self {
            inner: Box::new(source),
        }
    }

    /// Inject an arbitrary `IndexImpl` implementation.
    ///
    /// Used exclusively in unit tests to wrap `TestIndex` fakes without
    /// exposing `IndexImpl` as a public trait.  Not available in production
    /// builds.
    ///
    /// Visibility is `pub(crate)` under `#[cfg(test)]` so sibling modules
    /// (e.g. `project::resolve`) can inject their own mock index
    /// implementations without going through the heavier `from_chained`
    /// construction path.
    #[cfg(test)]
    pub(crate) fn from_impl(inner: impl index_impl::IndexImpl + 'static) -> Self {
        Self { inner: Box::new(inner) }
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
        }
    }

    /// Like [`Self::from_chained`], but attaches the machine-global blob store
    /// (`$OCX_HOME/blobs`) so an `AbsentLeaf` recovers its leaf platform
    /// manifest from installed content before any source walk
    /// (`adr_index_indirection.md` A3 step 2 / B2). This is the production
    /// construction (`context.rs`); the blob store is opt-in here so the
    /// signature-stable [`Self::from_chained`] keeps every unit-test caller
    /// unchanged (no blob store → recovery is a no-op).
    pub fn from_chained_with_content_store(
        cache: LocalIndex,
        sources: Vec<Index>,
        mode: ChainMode,
        content_store: crate::file_structure::BlobStore,
    ) -> Self {
        Self {
            inner: Box::new(chained_index::ChainedIndex::new(cache, sources, mode).with_content_store(content_store)),
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
        }
    }

    /// List all repositories available in the given registry.
    pub async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        log::debug!("Listing repositories for registry '{}'.", registry);
        self.inner.list_repositories(registry).await
    }

    /// List all tags available for the given identifier.
    ///
    /// Internal tags (prefixed with `__ocx.`) are automatically filtered out.
    /// Returns `None` when the package is not known to this index.
    pub async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        log::debug!("Listing tags for '{}'.", identifier);
        self.inner.list_tags(identifier).await.map(|opt| {
            opt.map(|tags| {
                tags.into_iter()
                    .filter(|t| !Tag::is_internal_str(t))
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
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        log::trace!("Fetching candidates for identifier '{}'.", identifier);
        self.inner.fetch_manifest(identifier, op).await
    }

    /// Find the manifest digest for the given identifier and tag.
    ///
    /// `op` carries the same contract as on [`Self::fetch_manifest`].
    /// Returns `None` when the identifier cannot be resolved.
    pub async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<oci::Digest>> {
        self.inner.fetch_manifest_digest(identifier, op).await
    }

    /// Fetch the raw bytes of a content blob.
    ///
    /// `blob_ref` carries `(registry, repo)` for the OCI blob endpoint and
    /// the blob's own digest for content addressing. `Ok(None)` = unrecoverable
    /// miss under the active routing policy (e.g. `ChainMode::Offline` + local
    /// cache miss).
    pub async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
        log::trace!("Fetching blob '{blob_ref}'.");
        self.inner.fetch_blob(blob_ref).await
    }

    /// The physical transport identifier for `identifier`, or `None` when no
    /// source rewrites it (registry-backed: physical == logical). Transport-only
    /// (`adr_index_indirection.md` C2) — the pull pipeline fetches layer content
    /// from this location; storage paths stay keyed on the logical identifier.
    pub async fn physical_reference(&self, identifier: &oci::Identifier) -> Result<Option<oci::Identifier>> {
        self.inner.physical_reference(identifier).await
    }

    /// Fetch a published index root document verbatim (bytes + parsed
    /// [`IndexRoot`](wire::IndexRoot)) so a published source's local copy can be
    /// grown byte-for-byte (copy-a-mirror, `adr_index_indirection.md` A2). A
    /// derived source returns `None` — its root is OCX-authored, not copied. See
    /// [`index_impl::IndexImpl::fetch_root_document`].
    pub async fn fetch_root_document(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, wire::IndexRoot)>> {
        self.inner.fetch_root_document(identifier).await
    }

    /// Whether a source in this index is the authoritative resolver for
    /// `identifier`'s namespace (its refusal must not be bypassed).
    pub fn is_authoritative_for(&self, identifier: &oci::Identifier) -> bool {
        self.inner.is_authoritative_for(identifier)
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
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
        log::trace!("Fetching raw manifest bytes for identifier '{}'.", identifier);
        self.inner.fetch_manifest_raw_bytes(identifier).await
    }

    pub async fn fetch_candidates(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<Vec<(oci::Identifier, oci::Platform)>>> {
        let Some((digest, manifest)) = self.fetch_manifest(identifier, op).await? else {
            return Ok(None);
        };
        log::trace!(
            "Fetched manifest for identifier '{}'. Determining candidates based on manifest type.",
            identifier
        );

        match manifest {
            oci::Manifest::Image(_) => Ok(Some(vec![(
                identifier.clone_with_digest(digest),
                oci::Platform::default(),
            )])),
            oci::Manifest::ImageIndex(index) => {
                let mut candidates = Vec::with_capacity(index.manifests.len());
                for manifest in index.manifests {
                    let digest = manifest.digest.try_into()?;
                    let candidate = identifier.clone_with_digest(digest);
                    let platform = match manifest.platform {
                        Some(platform) => match oci::Platform::try_from(platform) {
                            Ok(platform) => platform,
                            Err(error) => {
                                // A foreign or corrupted image index may carry an
                                // entry OCX cannot represent (unsupported os/arch,
                                // malformed fields). Skip it rather than failing
                                // the whole candidate list — the remaining
                                // entries are still selectable.
                                log::warn!(
                                    "skipping image-index entry for '{}' with unparseable platform: {error}",
                                    candidate
                                );
                                continue;
                            }
                        },
                        None => oci::Platform::any(),
                    };
                    candidates.push((candidate, platform));
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
        identifier: &oci::Identifier,
        platform: &oci::Platform,
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
        let result = match oci::select_best(platform, &candidates) {
            oci::Selection::Found(id) => SelectResult::Found(id),
            oci::Selection::Ambiguous(ids) => SelectResult::Ambiguous(ids),
            // Distinguish a feature mismatch from a plain not-found. When the
            // host declared non-empty `os.features` and there exist candidates
            // sharing the host's os+arch but none satisfied subset matching,
            // the package ships for this os/arch under different `os.features`
            // only — surface the dedicated variant so the caller can report a
            // feature mismatch rather than a generic not-found.
            oci::Selection::None => match host_os_features(platform) {
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
        }
    }
}

/// Extract the requested platform's declared `os_features`, when it is
/// `Specific` and carries a non-empty set.
///
/// A non-empty `os_features` set (e.g. a detected libc, or an explicit
/// `--platform linux/amd64+libc.musl`) is the signal that a no-match is a
/// feature mismatch rather than a plain not-found.
fn host_os_features(platform: &oci::Platform) -> Option<Vec<String>> {
    match platform {
        oci::Platform::Specific { os_features, .. } if !os_features.is_empty() => Some(os_features.clone()),
        _ => None,
    }
}

/// Collect the candidate platforms that share os+arch with the requested
/// `Specific` platform.
///
/// These are the entries the user could target with `--platform` — the package
/// ships for this os/arch, just under a different libc. Returns them sorted by
/// display string for deterministic error output.
fn candidates_sharing_host_os_arch(
    platform: &oci::Platform,
    candidates: &[(oci::Identifier, oci::Platform)],
) -> Vec<oci::Platform> {
    let oci::Platform::Specific {
        os: host_os,
        arch: host_arch,
        ..
    } = platform
    else {
        return Vec::new();
    };

    let mut matched: Vec<oci::Platform> = candidates
        .iter()
        .filter_map(|(_, candidate)| match candidate {
            oci::Platform::Specific { os, arch, .. } if os == host_os && arch == host_arch => Some(candidate.clone()),
            _ => None,
        })
        .collect();
    matched.sort_by_key(|platform| platform.to_string());
    matched.dedup();
    matched
}

// ── Index::select integration tests with multi-libc ImageIndex (Step 3.4) ──

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oci::{self, Digest, Identifier, Manifest, Platform};
    use async_trait::async_trait;

    // ── Minimal mock IndexImpl returning a fixed ImageIndex ──────────

    /// A mock `IndexImpl` that always returns the same pre-built `ImageIndex`
    /// manifest so `Index::select` can be exercised without a real registry.
    struct MultiLibcIndex {
        manifest: oci::ImageIndex,
    }

    impl MultiLibcIndex {
        /// Build a mock image index with three linux/amd64 entries:
        ///   entry 0 — libc.glibc (digest sha256:glibc_entry…)
        ///   entry 1 — libc.musl  (digest sha256:musl_entry…)
        ///   entry 2 — no os.features (digest sha256:untagged_entry…)
        fn new() -> Self {
            fn platform_with_features(features: Option<Vec<String>>) -> oci::native::Platform {
                oci::native::Platform {
                    os: oci::native::Os::Linux,
                    architecture: oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: features,
                }
            }

            let glibc_entry = oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "a".repeat(64)),
                size: 100,
                platform: Some(platform_with_features(Some(vec!["libc.glibc".to_string()]))),
                artifact_type: None,
                annotations: None,
            };
            let musl_entry = oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "b".repeat(64)),
                size: 100,
                platform: Some(platform_with_features(Some(vec!["libc.musl".to_string()]))),
                artifact_type: None,
                annotations: None,
            };
            let untagged_entry = oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "c".repeat(64)),
                size: 100,
                platform: Some(platform_with_features(None)),
                artifact_type: None,
                annotations: None,
            };

            Self {
                manifest: oci::ImageIndex {
                    schema_version: oci::INDEX_SCHEMA_VERSION,
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
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(vec![])
        }

        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }

        async fn fetch_manifest(
            &self,
            _identifier: &Identifier,
            _op: IndexOperation,
        ) -> crate::Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }

        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }

        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
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
            os: oci::OperatingSystem::Linux,
            arch: oci::Architecture::Amd64,
            variant: None,
            os_features: vec!["libc.glibc".to_string()],
        }
    }

    fn musl_host_platform() -> Platform {
        Platform::Specific {
            os: oci::OperatingSystem::Linux,
            arch: oci::Architecture::Amd64,
            variant: None,
            os_features: vec!["libc.musl".to_string()],
        }
    }

    fn no_libc_host_platform() -> Platform {
        // Represents a host where libc was undetected (empty os_features).
        Platform::Specific {
            os: oci::OperatingSystem::Linux,
            arch: oci::Architecture::Amd64,
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
            os: oci::OperatingSystem::Linux,
            arch: oci::Architecture::Amd64,
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
        manifest: oci::ImageIndex,
    }

    impl UntaggedOnlyIndex {
        fn new() -> Self {
            let untagged_entry = oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "d".repeat(64)),
                size: 100,
                platform: Some(oci::native::Platform {
                    os: oci::native::Os::Linux,
                    architecture: oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: None, // empty — declares no libc requirement
                }),
                artifact_type: None,
                annotations: None,
            };
            Self {
                manifest: oci::ImageIndex {
                    schema_version: oci::INDEX_SCHEMA_VERSION,
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
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
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
        manifest: oci::ImageIndex,
    }

    impl MuslOnlyIndex {
        fn new() -> Self {
            let musl_entry = oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "e".repeat(64)),
                size: 100,
                platform: Some(oci::native::Platform {
                    os: oci::native::Os::Linux,
                    architecture: oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: Some(vec!["libc.musl".to_string()]),
                }),
                artifact_type: None,
                annotations: None,
            };
            Self {
                manifest: oci::ImageIndex {
                    schema_version: oci::INDEX_SCHEMA_VERSION,
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
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
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
        manifest: oci::ImageIndex,
    }

    impl WindowsOnlyIndex {
        fn new() -> Self {
            let windows_entry = oci::ImageIndexEntry {
                media_type: "application/vnd.oci.image.manifest.v1+json".to_string(),
                digest: format!("sha256:{}", "f".repeat(64)),
                size: 100,
                platform: Some(oci::native::Platform {
                    os: oci::native::Os::Windows,
                    architecture: oci::native::Arch::Amd64,
                    variant: None,
                    features: None,
                    os_version: None,
                    os_features: None,
                }),
                artifact_type: None,
                annotations: None,
            };
            Self {
                manifest: oci::ImageIndex {
                    schema_version: oci::INDEX_SCHEMA_VERSION,
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
        async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
            Ok(vec![])
        }
        async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<(Digest, Manifest)>> {
            let digest = Digest::Sha256("0".repeat(64));
            Ok(Some((digest, Manifest::ImageIndex(self.manifest.clone()))))
        }
        async fn fetch_manifest_digest(&self, _: &Identifier, _: IndexOperation) -> crate::Result<Option<Digest>> {
            Ok(Some(Digest::Sha256("0".repeat(64))))
        }
        async fn fetch_blob(&self, _: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
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
}
