// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use async_trait::async_trait;

use crate::{Result, oci};

use super::IndexOperation;

#[async_trait]
pub trait IndexImpl: Send + Sync {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>>;

    /// List all tags for the given identifier.
    ///
    /// Implementations return the source's tags as recorded; reserved tags
    /// ([`Tag::is_reserved`](crate::package::tag::Tag::is_reserved) — the
    /// `__ocx` namespace and `sha256.<hex>` digest aliases) are filtered
    /// once, in [`Index::list_tags`](super::Index::list_tags).
    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>>;

    /// Fetch the manifest for the given identifier.
    ///
    /// Pure-read callers must pass [`IndexOperation::Query`]; install/pull
    /// callers pass [`IndexOperation::Resolve`]. The trait does not validate
    /// this — misuse silently leaks writes through query paths. The
    /// [`IndexOperation`] enum exists to make the choice unmissable at every
    /// call site.
    async fn fetch_manifest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>>;
    /// Fetch the manifest digest for the given identifier.
    ///
    /// `op` carries the same contract as on [`Self::fetch_manifest`].
    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        op: IndexOperation,
    ) -> Result<Option<oci::Digest>>;

    /// Fetch the raw bytes of a content blob.
    ///
    /// `blob_ref` carries `(registry, repo)` for the OCI blob endpoint and
    /// the blob's own digest for content addressing. `Ok(None)` = unrecoverable
    /// miss (e.g. local-only mode + absent).
    async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>>;

    /// Fetch the verbatim manifest bytes alongside the parsed manifest and its
    /// digest.
    ///
    /// A registry-backed source ([`super::OciIndex`]) returns the exact
    /// bytes the registry served — digest recompute-verified — so the index
    /// store can persist them without re-serialisation
    /// (`adr_index_indirection.md` A3). `Ok(None)` = tag/manifest absent.
    ///
    /// The default derives the bytes by re-serialising the parsed manifest.
    /// That fallback is correct only for sources that do not retain the wire
    /// bytes (test fakes that never persist); every source that a persisting
    /// caller can reach overrides it — registry sources, `OcxIndex`, and
    /// `ChainedIndex` itself (which walks its own source chain rather than
    /// falling back to a re-serialisation of its cache-first `fetch_manifest`
    /// read). The persist path ([`super::LocalIndex::persist_dispatch`]) and
    /// chain-blob staging ([`crate::package_manager::tasks::common::stage_and_link_chain_blobs`])
    /// are always driven with a registry-backed source in production, so the
    /// re-serialising default never reaches a verifying write.
    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
        match self.fetch_manifest(identifier, IndexOperation::Resolve).await? {
            Some((digest, manifest)) => {
                let bytes = serde_json::to_vec(&manifest)?;
                Ok(Some((bytes, digest, manifest)))
            }
            None => Ok(None),
        }
    }

    /// Fetch a published index root document verbatim: the exact
    /// `p/<ns>/<pkg>.json` bytes the index site served, alongside the parsed
    /// [`IndexRoot`](super::IndexRoot).
    ///
    /// A **published** ocx-index source ([`super::OcxIndex`]) serves the
    /// verbatim root so `LocalIndex::persist_published_root` can grow the local
    /// copy byte-for-byte (copy-a-mirror, `adr_index_indirection.md` A2). A
    /// **derived** (plain OCI-registry) source publishes no index of its own, so
    /// the default returns `None` — its root is OCX-authored field-wise instead
    /// (`LocalIndex::commit_root_tag`, A2/H). `Ok(None)` = this source serves no
    /// verbatim root for `identifier`.
    ///
    /// `Ok(None)` never means "outside jurisdiction" — that outcome has its own
    /// type ([`super::Jurisdiction`]) on its own method, consulted before a
    /// source is asked, precisely so
    /// [`LocalIndex::refresh_tags`](super::LocalIndex::refresh_tags)'s
    /// derived-source switch on this return value cannot misread it.
    async fn fetch_root_document(&self, identifier: &oci::Identifier) -> Result<Option<(Vec<u8>, super::IndexRoot)>> {
        let _ = identifier;
        Ok(None)
    }

    /// The physical transport identifier for `identifier`, when this source
    /// rewrites a logical reference to a distinct physical location
    /// (`index.ocx.sh`'s `repository` pointer). `Ok(None)` = no rewrite
    /// (registry sources: physical == logical).
    ///
    /// The returned reference is **transport-only** (Decision C2) — used to
    /// fetch layer/manifest content from the registry the index points at, and
    /// never round-tripped into a storage path or lock. The default returns
    /// `None`; only [`super::OcxIndex`] (and `ChainedIndex`, which delegates)
    /// override it.
    async fn physical_reference(&self, identifier: &oci::Identifier) -> Result<Option<oci::Identifier>> {
        let _ = identifier;
        Ok(None)
    }

    /// Whether this source will answer for `identifier`, and what its silence
    /// means — asked **before** the source is fetched from.
    ///
    /// An [`Authoritative`](super::Jurisdiction::Authoritative) source's
    /// **refusal** (a yanked tag without opt-in, a dispatch-object tamper, a
    /// fail-closed format mismatch) and its clean miss both stop the chain walk
    /// — neither may fall through to a lower source that could answer the same
    /// name and both bypass the refusal and leak the induced-error traffic to
    /// that source. An [`Outside`](super::Jurisdiction::Outside) source serves
    /// another registry entirely, so it is never asked and its silence decides
    /// nothing.
    ///
    /// Synchronous and I/O-free: every verdict is decided from the identifier's
    /// registry alone. It used to be `async` because a source could consult its
    /// own published `config.json` to decline an individual name; that
    /// declaration is gone (ocx#251 — a configured index is authoritative for
    /// its whole registry), and with it the only reason to await here.
    ///
    /// The default is [`FallThrough`](super::Jurisdiction::FallThrough) (a
    /// plain registry claims nothing); only [`super::OcxIndex`] and
    /// [`ChainedIndex`](super::chained_index::ChainedIndex) override it.
    fn jurisdiction(&self, identifier: &oci::Identifier) -> super::Jurisdiction {
        let _ = identifier;
        super::Jurisdiction::FallThrough
    }

    /// Whether this source is the configured owner of `registry` — a cheap,
    /// no-I/O ownership test, deliberately distinct from the per-name
    /// [`Self::jurisdiction`].
    ///
    /// Ownership decides local-subtree *layout* (a published source's
    /// `c/index.json` catalog vs a derived source's `p/` enumeration), which is
    /// per-source and never per-name — so every name under an owned registry
    /// reports that source's [`Self::source_kind`], grammar notwithstanding.
    /// The default returns `false`; only [`super::OcxIndex`] (its own
    /// namespace) and [`ChainedIndex`](super::chained_index::ChainedIndex) (any
    /// of its sources) override it.
    fn serves_registry(&self, registry: &str) -> bool {
        let _ = registry;
        false
    }

    /// The static-file base URL this source resolves against, when it is a
    /// configured ocx-index — the value a
    /// [`Jurisdiction::Authoritative`](super::Jurisdiction::Authoritative)
    /// terminal miss names so the user learns *which* index answered "no"
    /// (ocx#251).
    ///
    /// The effective base is not obvious from the outside: it is merged across
    /// the compiled-in default, the managed tier, `[registries."<ns>"] index`
    /// and the `[mirrors."<host>"] index` role override, so an error that only
    /// named the namespace would still leave the reader guessing which endpoint
    /// was consulted. The default is `None` — a plain OCI registry is not an
    /// index and has no base to name; only [`super::OcxIndex`] overrides it.
    fn index_base_url(&self) -> Option<&str> {
        None
    }

    /// This source's provenance (`adr_index_indirection.md` A2/H — the "two
    /// ifs" that distinguish a published copy from a derived one).
    ///
    /// A cheap, synchronous, no-I/O classification with exactly two jobs, both
    /// about who authored the local copy's root document: the root-read catalog
    /// cross-check (`c/index.json` for a published copy, none for a derived one)
    /// and root authorship on growth (verbatim copy vs OCX-authored field-wise).
    /// Recovery routing is **not** one of them — an absent dispatch object is
    /// fetched by digest regardless of provenance. `ChainedIndex` calls this to
    /// pick [`super::local_index::SourceKind`] without contacting the source.
    /// The default is [`super::local_index::SourceKind::Derived`] (an OCI
    /// registry publishes no index of its own); only [`super::OcxIndex`]
    /// overrides it (`Published`).
    fn source_kind(&self) -> super::local_index::SourceKind {
        super::local_index::SourceKind::Derived
    }

    fn box_clone(&self) -> Box<dyn IndexImpl>;

    /// A view that resolves identically but writes **nothing** into the local
    /// index — no dispatch object, no root-document tag pointer, no
    /// absent-dispatch self-heal. Content-addressed blob writes (leaf manifests,
    /// config blobs) still happen: the blob store is the GC-able content
    /// cache, distinct from the permanent local index. Used by read-only
    /// views (`ocx package inspect`) so merely looking at a package never
    /// grows the committed index (`adr_index_indirection.md` — the index is
    /// deployment-managed, outside GC; only `ocx index update` / pins may
    /// populate it).
    ///
    /// Default: [`Self::box_clone`] — a source with no local index of its own
    /// (a bare remote) has nothing to suppress. Only [`super::chained_index::ChainedIndex`]
    /// overrides it, returning a clone whose write policy is read-only.
    fn read_only_view(&self) -> Box<dyn IndexImpl> {
        self.box_clone()
    }
}
