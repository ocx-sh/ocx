// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! A fake [`IndexImpl`] source serving fixed manifest and blob bytes.
//!
//! Shared rather than per-module: three test modules now want it —
//! `tasks/inspect.rs` (closure walk), `tasks/resolve.rs` (chain recovery) and
//! `package_manager/composer.rs` (the lazy compose roots) — which is past
//! `quality-core.md`'s extraction bar, not short of it. The third copy is what
//! made it shared; the first two were the same fixture drifting apart already
//! (one carried a blob map and a concurrency probe, the other did not).

use std::collections::HashMap;

use async_trait::async_trait;

use crate::oci::index::{IndexImpl, IndexOperation};
use crate::oci::{self, Algorithm, Digest, Identifier};

/// Peak-concurrency probe for [`FakeManifestSource::fetch_manifest_raw_bytes`]:
/// every clone shares the same counters via `Arc`, so it survives `box_clone`
/// (one per spawned gather task). `enter()` records one fetch entering flight,
/// bumps the peak, and returns a guard that records the fetch leaving flight on
/// drop.
#[derive(Clone, Default)]
pub struct ConcurrencyProbe {
    in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    peak: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ConcurrencyProbe {
    pub fn peak(&self) -> usize {
        self.peak.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn enter(&self) -> ConcurrencyProbeGuard {
        let now = self.in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
        ConcurrencyProbeGuard { probe: self.clone() }
    }
}

struct ConcurrencyProbeGuard {
    probe: ConcurrencyProbe,
}

impl Drop for ConcurrencyProbeGuard {
    fn drop(&mut self) {
        self.probe.in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A minimal fake source serving a fixed set of `(tag-or-digest) -> bytes`
/// manifest entries, keyed by tag for a tag-addressed lookup or by digest
/// string for a digest-addressed one (a platform-selected child), plus a
/// separate `digest -> bytes` map for opaque config blobs (`fetch_blob`, a
/// distinct seam from manifest resolution).
///
/// Used with `ChainMode::Default` so a resolve that needs leaf content recovers
/// it through the same absent-dispatch recovery path a live registry would — a
/// leaf platform manifest is never locally cached (`adr_index_indirection.md`
/// A3), so an offline-only pre-seeded fixture cannot answer a lookup for one.
#[derive(Clone, Default)]
pub struct FakeManifestSource {
    entries: HashMap<String, (Vec<u8>, Digest, oci::Manifest)>,
    blobs: HashMap<String, Vec<u8>>,
    /// Set only by the wide-frontier concurrency test — `None` elsewhere means
    /// zero behavior change (no counting, no artificial delay) for every other
    /// test using this fixture.
    probe: Option<ConcurrencyProbe>,
}

impl FakeManifestSource {
    /// Register a manifest under `key` (a tag or a digest string), digested by
    /// its own bytes.
    #[must_use]
    pub fn with(mut self, key: &str, bytes: &[u8]) -> Self {
        let digest = Algorithm::Sha256.hash(bytes);
        let manifest = serde_json::from_slice(bytes).unwrap();
        self.entries.insert(key.to_string(), (bytes.to_vec(), digest, manifest));
        self
    }

    /// Attaches a peak-concurrency probe — `fetch_manifest_raw_bytes` briefly
    /// holds the fetch open so concurrent per-node fetches actually overlap and
    /// the peak is observable.
    #[must_use]
    pub fn with_concurrency_probe(mut self, probe: ConcurrencyProbe) -> Self {
        self.probe = Some(probe);
        self
    }

    /// Register an opaque blob (e.g. a package-metadata config blob, which does
    /// not parse as an OCI [`oci::Manifest`]) served by `fetch_blob`, keyed by
    /// its own digest string.
    #[must_use]
    pub fn with_blob(mut self, digest: &str, bytes: &[u8]) -> Self {
        self.blobs.insert(digest.to_string(), bytes.to_vec());
        self
    }

    /// Like [`Self::with`], but forces the entry's returned digest to `digest`
    /// instead of the real hash of `bytes`.
    ///
    /// `ChainedIndex`'s manifest-fetch path never verifies a source's claimed
    /// digest against the identifier actually requested — only `fetch_blob`
    /// digest-verifies at that layer. Task-layer callers that persist raw
    /// manifest bytes under a caller-chosen digest re-verify before writing
    /// (CWE-345), so a `digest` that genuinely disagrees with `bytes`' real hash
    /// is rejected there rather than silently round-tripped. Used by the
    /// digest-mismatch regression test; every other call site passes the real
    /// hash because its node must survive that re-verify.
    #[must_use]
    pub fn with_digest(mut self, key: &str, bytes: &[u8], digest: Digest) -> Self {
        let manifest = serde_json::from_slice(bytes).unwrap();
        self.entries.insert(key.to_string(), (bytes.to_vec(), digest, manifest));
        self
    }

    fn lookup(&self, identifier: &Identifier) -> Option<(Vec<u8>, Digest, oci::Manifest)> {
        let key = match identifier.digest() {
            Some(digest) => digest.to_string(),
            None => identifier.tag_or_latest().to_string(),
        };
        self.entries.get(&key).cloned()
    }
}

#[async_trait]
impl IndexImpl for FakeManifestSource {
    async fn list_repositories(&self, _: &str) -> crate::Result<Vec<String>> {
        Ok(Vec::new())
    }
    async fn list_tags(&self, _: &Identifier) -> crate::Result<Option<Vec<String>>> {
        Ok(None)
    }
    async fn fetch_manifest(
        &self,
        identifier: &Identifier,
        _op: IndexOperation,
    ) -> crate::Result<Option<(Digest, oci::Manifest)>> {
        Ok(self.lookup(identifier).map(|(_, digest, manifest)| (digest, manifest)))
    }
    async fn fetch_manifest_digest(
        &self,
        identifier: &Identifier,
        _op: IndexOperation,
    ) -> crate::Result<Option<Digest>> {
        Ok(self.lookup(identifier).map(|(_, digest, _)| digest))
    }
    async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> crate::Result<Option<Vec<u8>>> {
        // Config blobs are fetched by digest via `Index::fetch_blob`
        // (`load_config_metadata`), a separate seam from
        // `fetch_manifest_raw_bytes`.
        Ok(self.blobs.get(&blob_ref.digest().to_string()).cloned())
    }
    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &Identifier,
    ) -> crate::Result<Option<(Vec<u8>, Digest, oci::Manifest)>> {
        // The actual network-touching seam for a genuine (uncached) digest
        // lookup under `ChainMode::Default` — `ChainedIndex::fetch_manifest`
        // routes a fresh miss through `LocalIndex::persist_dispatch`, which
        // calls THIS method, not `fetch_manifest` above (which only answers an
        // already-local dispatch hit or a `--remote` query). The wide-frontier
        // probe hooks here so it observes real per-node fetch concurrency.
        let guard = self.probe.as_ref().map(ConcurrencyProbe::enter);
        if guard.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        Ok(self.lookup(identifier))
    }
    fn box_clone(&self) -> Box<dyn IndexImpl> {
        Box::new(self.clone())
    }
}
