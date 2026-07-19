// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `index.ocx.sh`-style static-file index source (`adr_index_indirection.md`
//! Decision F).
//!
//! A pointer index, **not** a registry: no `/v2`, no blobs. OCX resolves a
//! logical reference through two verified HTTP hops and then fetches the
//! physical manifest/layers from the registry the index points at — the OCI
//! image-index hop is bypassed entirely (Decision C1):
//!
//! ```text
//! logical id (ocx.sh/<ns>/<pkg>[:tag])
//!   → GET /p/<ns>/<pkg>.json              root   → tags[tag].content = obs digest
//!   → GET /p/<ns>/<pkg>/o/sha256/<hex>.json obs  → VERIFY sha256(bytes)==hex
//!                                                → platforms[] = [(Platform, manifest-digest)]
//!   → select_best(host, platforms)        → leaf platform-manifest digest
//!   → root.repository (oci://…)           → physical fetch through the mirror seam
//! ```
//!
//! Only the ● frozen wire shapes in the ADR's Data Model are contract; the obs
//! object's `platforms[].digest` leaves are already the doctrine-correct
//! platform-manifest digests, each OCI-CAS-verified when its manifest is
//! fetched from the physical registry (Decision D).
//!
//! ## Trust anchor
//!
//! The observation-object verify (`sha256(bytes) == <hex>`) is the one place
//! OCX re-derives a digest it did not mint, so it is the trust boundary of the
//! whole index path (F1). A mismatch is a hard [`DataError`](crate::cli::ExitCode::DataError),
//! never a silent load.
//!
//! ## Snapshot integration
//!
//! This source is a live [`IndexImpl`](super::index_impl::IndexImpl) chain source
//! (like [`OciIndex`](super::OciIndex)). Its
//! [`fetch_manifest_raw_bytes`](OcxIndex::fetch_manifest_raw_bytes) returns the
//! verbatim observation bytes (which hash to the observation digest, A3-valid)
//! paired with the synthetic image index, so [`LocalIndex::persist_dispatch`](super::LocalIndex)
//! writes the observation object under its own digest as the dispatch object
//! — the physical platform-manifest leaf it names is fetched on demand, never
//! copied into the local index (A3/B2). The read-back
//! (`decode_index_manifest` in `local_index`) dispatches on the per-source
//! payload codec (A2): an object that is not an OCI manifest is decoded as an
//! observation via [`observation_to_index`], so an offline resolve walks the
//! local index without re-fetching.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::RwLock;

use super::wire::{CatalogIndex, IndexRoot, Observation, RootTag};
use super::{IndexOperation, index_impl};
use crate::{Result, log, oci};

// ── Frozen wire shapes (● contract) ──────────────────────────────────────────
//
// `IndexRoot` / `RootTag` / `Observation` / `ObservationPlatform` /
// `CatalogIndex` live in `oci::index::wire` (`adr_index_indirection.md`
// §Data Model) — the frozen grammar shared verbatim by this remote client
// and the local store (`crate::file_structure::IndexStore`), imported
// above.

/// `config.json` — the version pin (● `{"format_version": 1}`).
///
/// Read once per source; an unknown `format_version` is a hard error
/// (fail-closed, F1). Forward-compatible: unknown sibling fields are ignored.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexFormatConfig {
    pub format_version: u64,
}

/// The `format_version` OCX understands. A served value other than this is
/// rejected (`adr_index_indirection.md` F1).
pub const SUPPORTED_FORMAT_VERSION: u64 = 1;

/// Hard cap on an index-document body (CWE-400). Root, observation, config, and
/// catalog documents are all small; a misbehaving or hostile endpoint must not
/// be able to stream an unbounded body into memory. The public catalog is the
/// largest legitimate shape (one `<ns>/<pkg> -> digest` line per package), and
/// 32 MiB covers hundreds of thousands of packages with headroom.
const MAX_INDEX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;

/// Connect-phase timeout for an index document fetch (CWE-400). A dead or
/// slow-to-accept endpoint must not stall a resolve indefinitely.
const INDEX_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Total request timeout (connect + response + capped body read) for an index
/// document fetch (CWE-400). Index documents are small (see
/// [`MAX_INDEX_DOCUMENT_BYTES`]), so a generous ceiling still bounds a
/// slowloris-style stall that a byte-cap alone cannot.
const INDEX_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Redacts any `user[:password]@` userinfo from `url`'s authority before it
/// lands in an error or log line (CWE-532). A `[registries."<ns>"] index` or
/// `[mirrors]` base may embed credentials, and the index HTTP errors below echo
/// the full request URL; without this a captured error or debug log would leak
/// them. Purely string-level (scheme `://`, then the authority up to the next
/// `/`), so a malformed URL is returned untouched rather than dropped.
fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    match authority.rsplit_once('@') {
        Some((_userinfo, host)) => format!("{scheme}://***@{host}{tail}"),
        None => url.to_string(),
    }
}

// ── HTTP transport seam ──────────────────────────────────────────────────────

/// A single static-file fetch outcome.
#[derive(Debug)]
pub enum IndexFetch {
    /// `200 OK` — the response body and its `ETag`, when the server supplied one.
    Found { bytes: Vec<u8>, etag: Option<String> },
    /// `304 Not Modified` — only returned for a conditional GET whose
    /// `If-None-Match` matched.
    NotModified,
    /// `404 Not Found` — the object is absent (a normal miss, not an error).
    NotFound,
}

/// Plain-HTTPS transport for the static-file index endpoints.
///
/// The seam that lets [`OcxIndex`] resolve without hitting the network in
/// tests (mock this the way [`StubTransport`](super::super::client::test_transport::StubTransport)
/// mocks the OCI transport). The production impl is
/// [`ReqwestIndexTransport`].
#[async_trait]
pub trait IndexTransport: Send + Sync {
    /// `GET url`. When `if_none_match` is set, the request is conditional
    /// (`If-None-Match`) so an unchanged catalog can answer `304`.
    async fn get(&self, url: &str, if_none_match: Option<&str>) -> Result<IndexFetch>;

    fn box_clone(&self) -> Box<dyn IndexTransport>;
}

impl Clone for Box<dyn IndexTransport> {
    fn clone(&self) -> Self {
        self.box_clone()
    }
}

/// `reqwest`-backed [`IndexTransport`] — the production static-file client.
///
/// Reuses the workspace `reqwest` (already in the tree via the `oci-client`
/// fork; F7). TLS is rustls, matching the fork so the build carries one
/// provider.
#[derive(Clone)]
pub struct ReqwestIndexTransport {
    client: reqwest::Client,
}

/// Builds the index HTTP client with the bundled Mozilla CA roots.
///
/// Without an explicit root set, reqwest 0.13's rustls path uses the OS trust
/// store via `rustls_platform_verifier::Verifier::new`, which **panics** on a
/// host with an empty store (minimal container / CI runner) — the reported
/// `No CA certificates were loaded` crash. Seeding any roots flips reqwest onto
/// the `Verifier::new_with_extra_roots` branch that never touches the system
/// store (reqwest client.rs:751). This mirrors the OCI half, which gets the
/// same set from the `oci-client` fork's `ClientConfig::default()`; the shared
/// single source is the `webpki_root_certs::TLS_SERVER_ROOT_CERTS` const (the
/// two client types — `oci_client` config vs bare `reqwest` — cannot share a
/// literal builder).
fn build_index_http_client() -> reqwest::Client {
    // Transport hardening applied to both the primary and the (unreachable)
    // fallback build so the degraded path never silently drops the gates:
    // bounded connect + total timeout (CWE-400), and no redirect following
    // (CWE-918 / CWE-319) — a static-file index needs no redirects, and a 3xx
    // must not relocate the fetch to http:// or an internal host AFTER the
    // plain-HTTP gate in `resolve_base_url` already ran.
    let harden = |builder: reqwest::ClientBuilder| {
        builder
            .connect_timeout(INDEX_CONNECT_TIMEOUT)
            .timeout(INDEX_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
    };
    let mut builder = harden(reqwest::Client::builder());
    for root in webpki_root_certs::TLS_SERVER_ROOT_CERTS {
        if let Ok(certificate) = reqwest::Certificate::from_der(root.as_ref()) {
            builder = builder.add_root_certificate(certificate);
        }
    }
    builder.build().unwrap_or_else(|error| {
        // The bundled-roots build cannot hit the empty-store panic (roots are
        // non-empty). A different init failure is not expected; fall back so
        // construction stays infallible, logging so it is not silent — the
        // fallback keeps the same timeout + no-redirect hardening.
        log::warn!("index HTTP client build with bundled roots failed ({error}); using hardened reqwest defaults");
        harden(reqwest::Client::builder())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

impl ReqwestIndexTransport {
    pub fn new() -> Self {
        Self {
            client: build_index_http_client(),
        }
    }
}

impl Default for ReqwestIndexTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IndexTransport for ReqwestIndexTransport {
    async fn get(&self, url: &str, if_none_match: Option<&str>) -> Result<IndexFetch> {
        let mut request = self.client.get(url);
        if let Some(etag) = if_none_match {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let mut response = request
            .send()
            .await
            .map_err(|source| super::error::Error::IndexHttpFailed {
                url: redact_url(url),
                source: Box::new(source),
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(IndexFetch::NotModified);
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(IndexFetch::NotFound);
        }
        if !status.is_success() {
            return Err(super::error::Error::IndexHttpFailed {
                url: redact_url(url),
                source: format!("unexpected status {status}").into(),
            }
            .into());
        }

        // Reject a declared oversize body before reading a single byte (CWE-400).
        if let Some(declared) = response.content_length()
            && declared > MAX_INDEX_DOCUMENT_BYTES as u64
        {
            return Err(super::error::Error::IndexHttpFailed {
                url: redact_url(url),
                source: format!(
                    "response body {declared} bytes exceeds the {MAX_INDEX_DOCUMENT_BYTES}-byte index-document cap"
                )
                .into(),
            }
            .into());
        }

        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);

        // Stream the body under a hard cap (CWE-400): a server that omits or lies
        // about Content-Length (chunked transfer, or a hostile endpoint) still
        // cannot stream more than the cap into memory — the running total is
        // checked before each chunk is appended.
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|source| super::error::Error::IndexHttpFailed {
                url: redact_url(url),
                source: Box::new(source),
            })?
        {
            if body.len() + chunk.len() > MAX_INDEX_DOCUMENT_BYTES {
                return Err(super::error::Error::IndexHttpFailed {
                    url: redact_url(url),
                    source: format!("response body exceeds the {MAX_INDEX_DOCUMENT_BYTES}-byte index-document cap")
                        .into(),
                }
                .into());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(IndexFetch::Found { bytes: body, etag })
    }

    fn box_clone(&self) -> Box<dyn IndexTransport> {
        Box::new(self.clone())
    }
}

// ── Physical reference parsing (C3, one-way door) ────────────────────────────

/// Parses a root's `repository` pointer (`oci://host/path`) into its physical
/// `(registry, repository)`.
///
/// Strict (C3): the `oci://` scheme is a wire contract — a missing or unknown
/// scheme, or an empty host/path, is a hard [`Error::MalformedPhysicalRef`].
/// The parsed value is transport-only routing input, never a storage key (C2).
pub fn parse_physical_repository(value: &str) -> Result<(String, String)> {
    let malformed = || super::error::Error::MalformedPhysicalRef {
        value: value.to_string(),
    };
    let rest = value.strip_prefix("oci://").ok_or_else(malformed)?;
    let (host, path) = rest.split_once('/').ok_or_else(malformed)?;
    if host.is_empty() || path.is_empty() {
        return Err(malformed().into());
    }
    // `repository` is a wire-contract pointer OCX did not mint (C3, one-way
    // door). Re-parse `host/path` through the Identifier registry grammar and
    // require an EXACT round-trip: the same lowercase / character-class /
    // traversal / length checks every logical reference passes now guard this
    // physical ref too, and demanding `registry()`/`repository()` equal the
    // split `host`/`path` — with no tag and no digest — rejects a smuggled tag
    // (`repo:x`), digest (`repo@sha256:…`), whitespace, control character,
    // uppercase segment, or stray colon that the bare prefix + first-slash split
    // above would otherwise wave through. Host allowlisting / private-IP
    // rejection is deliberately out of scope (a separate, deferred decision).
    let parsed = oci::Identifier::parse_with_default_registry(rest, host).map_err(|_| malformed())?;
    if parsed.registry() != host || parsed.repository() != path || parsed.tag().is_some() || parsed.digest().is_some() {
        return Err(malformed().into());
    }
    Ok((host.to_string(), path.to_string()))
}

// ── Catalog sync (F2) ────────────────────────────────────────────────────────

/// Outcome of [`OcxIndex::sync_catalog`].
#[derive(Debug, Clone)]
pub struct CatalogSyncOutcome {
    /// The catalog to persist as the next diff basis + offline listing source.
    pub catalog: CatalogIndex,
    /// The new `ETag`, when the CDN supplied one (persist alongside the catalog).
    pub etag: Option<String>,
    /// Packages whose root digest moved (or appeared) versus the previous
    /// catalog — the exact set to re-snapshot. Sorted, deduplicated.
    pub moved: Vec<String>,
    /// The conditional GET answered `304`; nothing changed, `moved` is empty.
    pub unchanged: bool,
}

/// Diffs a freshly fetched catalog against the previously persisted (local)
/// one, returning the packages that were ALREADY known locally and whose root
/// digest moved.
///
/// A package ABSENT from `previous` is deliberately excluded: it is new to this
/// local copy — a listing row recorded in `c/index.json` but materialized only
/// when first `update`d (`adr_index_indirection.md` F2), so it must NOT trigger
/// a re-snapshot fetch here. Counting every absent-from-previous entry as
/// "moved" turned the first catalog sync (empty previous) into an unbounded
/// fetch storm over the whole remote catalog. The consumer
/// ([`LocalIndex::sync_catalog`](super::LocalIndex)) narrows this set further to
/// the packages that actually have a local root document on disk, so a changed
/// listing row (present in the catalog but never materialized) updates its
/// catalog value without a fetch either.
fn diff_moved(previous: &CatalogIndex, fetched: &CatalogIndex) -> Vec<String> {
    let mut moved: Vec<String> = fetched
        .iter()
        .filter(|(pkg, digest)| match previous.get(*pkg) {
            // Known locally and the root digest changed → a genuine move.
            Some(previous_digest) => previous_digest != *digest,
            // New to the local catalog → a listing row, never a re-snapshot (F2).
            None => false,
        })
        .map(|(pkg, _)| pkg.clone())
        .collect();
    moved.sort();
    moved.dedup();
    moved
}

// ── Source ───────────────────────────────────────────────────────────────────

/// Default base URL when no `[registries."<ns>"] index` field is configured.
pub const DEFAULT_INDEX_BASE_URL: &str = "https://index.ocx.sh";

/// In-memory caches shared across [`OcxIndex`] clones (per-invocation).
///
/// Roots are volatile but cheap to re-read within one resolution (the tag →
/// obs → physical hops all need the same root); observations are immutable, so
/// caching them by their observed digest is cache-forever (F1). This is the
/// same "shared cache across clones" model [`OciIndex`](super::OciIndex)
/// uses — it is not the committed local index.
#[derive(Default)]
struct SourceCacheInner {
    /// repository → root document.
    roots: BTreeMap<String, Arc<IndexRoot>>,
    /// observation hex → observation object.
    observations: BTreeMap<String, Arc<Observation>>,
    /// Set once `config.json`'s `format_version` has been confirmed supported
    /// this invocation, so a repeat call skips the fetch (F1 "read once").
    /// Never set on a served-but-unsupported version (a re-checked hard error,
    /// not a remembered steady state) NOR on an absent `config.json` (a
    /// re-checked inert state — a fixed deploy that later serves `config.json`
    /// is picked up without restarting). Config-driven construction means there
    /// is no probe outcome to soften a transport failure into — that always
    /// propagates.
    format_version_confirmed: bool,
}

/// Outcome of the per-source `config.json` version check
/// ([`OcxIndex::check_format_version`]).
enum FormatVersionState {
    /// `config.json` present and its `format_version` is supported — roots may
    /// resolve.
    Confirmed,
    /// `config.json` absent (404) — this base URL is not (yet) a version-pinned
    /// OCX index, so no root resolves and nothing is cached (fail-closed, F1;
    /// re-checked every call). Serving a valid-looking root without the version
    /// pin must not let it resolve — `adr_index_indirection.md` F1, the
    /// "misconfigured index endpoint fails loud" contract in `subsystem-oci`.
    NotAnIndex,
}

/// A live `index.ocx.sh`-style source.
#[derive(Clone)]
pub struct OcxIndex {
    transport: Box<dyn IndexTransport>,
    /// The static-file base URL (`[registries."<ns>"] index` ▸ default, with
    /// the `[mirrors."<host>"] index` role override applied), trailing slash
    /// trimmed.
    base_url: String,
    /// The logical registry this source serves (e.g. `"ocx.sh"`). An
    /// identifier whose `registry()` differs is not this source's concern.
    namespace: String,
    /// OCI client for the physical manifest/layer fetches (applies `[mirrors]`).
    client: oci::Client,
    /// When false, a tag resolving to a yanked entry is refused (F3).
    allow_yanked: bool,
    cache: Arc<RwLock<SourceCacheInner>>,
}

/// Construction inputs for [`OcxIndex::new`].
pub struct OcxIndexConfig {
    pub transport: Box<dyn IndexTransport>,
    pub base_url: String,
    pub namespace: String,
    pub client: oci::Client,
    pub allow_yanked: bool,
}

impl OcxIndex {
    pub fn new(config: OcxIndexConfig) -> Self {
        Self {
            transport: config.transport,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            namespace: config.namespace,
            client: config.client,
            allow_yanked: config.allow_yanked,
            cache: Arc::new(RwLock::new(SourceCacheInner::default())),
        }
    }

    /// The logical registry this source serves (e.g. `"ocx.sh"`).
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Resolves the static-file base URL for `namespace`: the
    /// `[registries."<ns>"] index` base (already merged through the managed
    /// tier) if present, else [`DEFAULT_INDEX_BASE_URL`] — then applies the
    /// `[mirrors."<host>"] index` role override for the base's own traffic
    /// host, if one is declared (`mirrors_index`, replace semantics, no
    /// fallback). Minted **once** here, the single place base URLs come
    /// from (`adr_index_indirection.md` F5c).
    ///
    /// A plain-`http://` final target is refused unless its host is in
    /// `insecure_hosts` (`OCX_INSECURE_REGISTRIES`) — the root document is the
    /// index path's trust anchor, so a plaintext index is an on-path takeover
    /// (CWE-319), gated exactly like the registry role.
    ///
    /// # Errors
    ///
    /// [`Error::PlainHttpIndexNotAllowed`](super::error::Error::PlainHttpIndexNotAllowed)
    /// for an ungated `http://` target; [`Error::InvalidIndexUrl`](super::error::Error::InvalidIndexUrl)
    /// for an unparseable `[registries."<ns>"] index` base.
    pub fn resolve_base_url(
        config: &crate::config::Config,
        namespace: &str,
        mirrors_index: &BTreeMap<String, crate::config::mirror::ParsedMirror>,
        insecure_hosts: &[String],
    ) -> Result<String> {
        let base = config
            .registries
            .as_ref()
            .and_then(|table| table.get(namespace))
            .and_then(|entry| entry.index.as_deref())
            .filter(|url| !url.is_empty())
            .unwrap_or(DEFAULT_INDEX_BASE_URL);

        // Reuse the mirror URL parser (scheme/host split, https default) so the
        // plain-HTTP gate matches the registry role byte for byte.
        let parsed = crate::config::mirror::parse_url(base).map_err(|source| super::error::Error::InvalidIndexUrl {
            namespace: namespace.to_string(),
            source,
        })?;

        // Index-role mirror override, keyed by the base's own traffic host —
        // replace semantics, no fallback.
        let target = mirrors_index.get(&parsed.host).cloned().unwrap_or(parsed);

        if target.protocol == "http" && !insecure_hosts.iter().any(|host| host == &target.host) {
            return Err(super::error::Error::PlainHttpIndexNotAllowed {
                namespace: namespace.to_string(),
                host: target.host,
            }
            .into());
        }

        let path = if target.path_prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", target.path_prefix)
        };
        Ok(format!("{}://{}{}", target.protocol, target.host, path))
    }

    // ── config.json (F1) ─────────────────────────────────────────────────────

    /// Validates `config.json`'s `format_version`, fetching it once per source
    /// instance on success (F1 "read once") and skipping the fetch on every
    /// later call. Config-driven construction (`[registries."<ns>"].index`
    /// presence) already decided this host serves an ocx-index, so there is
    /// nothing left to *probe* for — this only guards the wire-format version.
    ///
    /// A served-but-unsupported `format_version` is a hard, fail-closed error
    /// (F1) and is never cached as a steady state — every call re-checks so a
    /// fixed deploy is picked up without restarting the process. An absent
    /// `config.json` (404) yields [`FormatVersionState::NotAnIndex`] — inert,
    /// never cached, and (critically) never a pass that lets roots resolve: a
    /// base URL serving a valid-looking root but no version pin is not an OCX
    /// index, and resolving it anyway would consume roots against an
    /// unversioned endpoint (F1 fail-closed). A transport failure reaching
    /// `config.json` propagates as a hard error on every call — there is no
    /// soft "maybe not an index yet" state left to absorb it.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat)
    /// on a served-but-unknown version; the transport error otherwise.
    async fn check_format_version(&self) -> Result<FormatVersionState> {
        if self.cache.read().await.format_version_confirmed {
            return Ok(FormatVersionState::Confirmed);
        }
        let url = format!("{}/config.json", self.base_url);
        match self.transport.get(&url, None).await? {
            IndexFetch::Found { bytes, .. } => {
                let config: IndexFormatConfig = parse_document(&bytes, &url)?;
                if config.format_version != SUPPORTED_FORMAT_VERSION {
                    return Err(super::error::Error::UnsupportedIndexFormat {
                        version: config.format_version,
                    }
                    .into());
                }
            }
            // Absent config: not a version-pinned OCX index at this base URL.
            // Do NOT cache and do NOT let roots resolve — fail-closed (F1),
            // re-checked every call so a later-deployed config.json is picked up.
            IndexFetch::NotFound | IndexFetch::NotModified => return Ok(FormatVersionState::NotAnIndex),
        }
        self.cache.write().await.format_version_confirmed = true;
        Ok(FormatVersionState::Confirmed)
    }

    // ── root (F1 volatile) ──────────────────────────────────────────────────

    /// Fetches (and caches) the root for `repository`. `Ok(None)` on a 404
    /// miss.
    async fn resolve_root(&self, repository: &str) -> Result<Option<Arc<IndexRoot>>> {
        // An absent config.json makes the base a non-index — no root resolves
        // (fail-closed, F1), never a pass that consumes a valid-looking root.
        if let FormatVersionState::NotAnIndex = self.check_format_version().await? {
            return Ok(None);
        }
        if let Some(root) = self.cache.read().await.roots.get(repository) {
            return Ok(Some(root.clone()));
        }
        let url = format!("{}/p/{}.json", self.base_url, repository);
        let root = match self.transport.get(&url, None).await? {
            IndexFetch::Found { bytes, .. } => {
                let parsed: IndexRoot = parse_document(&bytes, &url)?;
                Arc::new(parsed)
            }
            IndexFetch::NotFound | IndexFetch::NotModified => return Ok(None),
        };
        self.cache
            .write()
            .await
            .roots
            .insert(repository.to_string(), root.clone());
        Ok(Some(root))
    }

    // ── observation (F1 immutable, VERIFIED) ─────────────────────────────────

    /// Fetches, verifies, and caches the observation object for
    /// `(repository, obs_digest)`, returning its verbatim bytes and parsed form.
    ///
    /// Verifies `sha256(bytes) == obs_digest` before parsing — the index path's
    /// trust anchor (F1). `Ok(None)` on a 404 miss.
    async fn resolve_observation(
        &self,
        repository: &str,
        obs_digest: &oci::Digest,
    ) -> Result<Option<(Vec<u8>, Arc<Observation>)>> {
        let url = format!(
            "{}/p/{}/o/{}/{}.json",
            self.base_url,
            repository,
            obs_digest.algorithm().prefix(),
            obs_digest.hex()
        );
        let bytes = match self.transport.get(&url, None).await? {
            IndexFetch::Found { bytes, .. } => bytes,
            IndexFetch::NotFound | IndexFetch::NotModified => return Ok(None),
        };

        // Trust boundary: re-derive the digest OCX did not mint and compare.
        let computed = obs_digest.algorithm().hash(&bytes);
        if &computed != obs_digest {
            return Err(super::error::Error::ObservationDigestMismatch {
                claimed: obs_digest.clone(),
                computed,
            }
            .into());
        }

        let observation: Arc<Observation> = Arc::new(parse_document(&bytes, &url)?);
        self.cache
            .write()
            .await
            .observations
            .insert(obs_digest.hex().to_string(), observation.clone());
        Ok(Some((bytes, observation)))
    }

    /// Surfaces the human-governed status lane (F3) for a live tag resolve —
    /// delegates to the shared [`surface_root_status`] with this source's
    /// `allow_yanked` opt-in. Called on the tag path only; a digest-pinned
    /// resolve skips it (immutability).
    fn surface_status(&self, identifier: &oci::Identifier, root: &IndexRoot, tag: &RootTag) -> Result<()> {
        surface_root_status(identifier, root, tag, self.allow_yanked)
    }

    /// Resolves a tag-addressed identifier to its observation object: root →
    /// tag → status surfacing → obs digest → fetch + verify.
    ///
    /// `Ok(None)` when the package or tag is absent. Errors on a yanked refusal
    /// or an obs digest mismatch.
    async fn resolve_tag(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, Arc<Observation>)>> {
        let Some(root) = self.resolve_root(identifier.repository()).await? else {
            return Ok(None);
        };
        let tag = identifier.tag_or_latest();
        let Some(tag_entry) = root.tags.get(tag) else {
            return Ok(None);
        };
        self.surface_status(identifier, &root, tag_entry)?;

        let obs_digest = tag_entry.content.clone();
        let Some((_, observation)) = self.resolve_observation(identifier.repository(), &obs_digest).await? else {
            return Ok(None);
        };
        Ok(Some((obs_digest, observation)))
    }

    /// Builds the physical [`oci::Identifier`] for `identifier` by dereferencing
    /// the root's `repository` pointer. The logical tag/digest are copied onto
    /// the physical location; the physical value is transport-only routing (C2).
    async fn physical_identifier(&self, identifier: &oci::Identifier) -> Result<Option<oci::Identifier>> {
        let Some(root) = self.resolve_root(identifier.repository()).await? else {
            return Ok(None);
        };
        let (registry, repository) = parse_physical_repository(&root.repository)?;
        let mut physical = oci::Identifier::new_registry(repository, registry);
        if let Some(digest) = identifier.digest() {
            physical = physical.clone_with_digest(digest);
        }
        Ok(Some(physical))
    }

    // ── catalog sync (F2) ────────────────────────────────────────────────────

    /// Syncs `c/index.json` with a conditional GET and diffs per-package root
    /// digests against `previous`, returning only the packages whose root moved.
    ///
    /// A `304` short-circuits to `unchanged` with `previous` carried forward; a
    /// `404` yields an empty catalog. The returned catalog is what the caller
    /// persists at `{index-home}/c/index.json` — both the offline listing source
    /// and the next diff basis (F2).
    pub async fn sync_catalog(
        &self,
        previous: &CatalogIndex,
        previous_etag: Option<&str>,
    ) -> Result<CatalogSyncOutcome> {
        // No config.json ⇒ not a version-pinned index: nothing to list, an
        // empty catalog (fail-closed, F1) — never a walk of an unversioned base.
        if let FormatVersionState::NotAnIndex = self.check_format_version().await? {
            return Ok(CatalogSyncOutcome {
                catalog: CatalogIndex::new(),
                etag: None,
                moved: Vec::new(),
                unchanged: false,
            });
        }
        let url = format!("{}/c/index.json", self.base_url);
        match self.transport.get(&url, previous_etag).await? {
            IndexFetch::NotModified => Ok(CatalogSyncOutcome {
                catalog: previous.clone(),
                etag: previous_etag.map(str::to_owned),
                moved: Vec::new(),
                unchanged: true,
            }),
            IndexFetch::NotFound => Ok(CatalogSyncOutcome {
                catalog: CatalogIndex::new(),
                etag: None,
                moved: Vec::new(),
                unchanged: false,
            }),
            IndexFetch::Found { bytes, etag } => {
                let fetched: CatalogIndex = parse_document(&bytes, &url)?;
                let moved = diff_moved(previous, &fetched);
                Ok(CatalogSyncOutcome {
                    catalog: fetched,
                    etag,
                    moved,
                    unchanged: false,
                })
            }
        }
    }
}

/// Builds a synthetic [`oci::ImageIndex`] from an observation's `platforms[]`
/// so the existing `fetch_candidates` → `select_best` pipeline consumes it
/// unchanged. This is an **in-memory resolution view only** — it is never
/// persisted (that would invent a digest namespace, Rejected Alternative R4);
/// the leaf digests it carries are the real platform-manifest digests.
///
/// Shared with [`LocalIndex`](super::LocalIndex)'s dispatch-object read-back so
/// an offline resolve through a persisted observation presents the identical
/// candidate set as a live source (A2 native codec).
pub(super) fn observation_to_index(observation: &Observation) -> Result<oci::ImageIndex> {
    let mut manifests = Vec::with_capacity(observation.platforms.len());
    for entry in &observation.platforms {
        // `entry.digest` is already a validated `oci::Digest` — `Observation`
        // deserialize fails the whole document on a malformed leaf digest
        // (`adr_index_indirection.md` amendment 2026-07-19), so there is
        // nothing left to re-validate here.
        let leaf = &entry.digest;
        manifests.push(oci::ImageIndexEntry {
            media_type: oci::OCI_IMAGE_MEDIA_TYPE.to_string(),
            digest: leaf.to_string(),
            // Size is unknown from the obs object and unused by candidate
            // selection (only digest + platform matter).
            size: 0,
            platform: Some(entry.platform.clone()),
            artifact_type: None,
            annotations: None,
        });
    }
    Ok(oci::ImageIndex {
        schema_version: oci::INDEX_SCHEMA_VERSION,
        media_type: Some(oci::OCI_IMAGE_INDEX_MEDIA_TYPE.to_string()),
        artifact_type: None,
        manifests,
        annotations: None,
    })
}

/// Surfaces the human-governed status lane of a resolved tag (F3): warns on
/// yank / deprecation / supersession, and **refuses** a yanked tag resolve
/// unless `allow_yanked`. Shared verbatim by the live [`OcxIndex`] resolve
/// (`surface_status`) and the OFFLINE committed-root resolve
/// ([`LocalIndex::resolve_dispatch`](super::LocalIndex)) so a yank/deprecation
/// is honored identically whether the root was just fetched or read from a
/// shipped copy with zero network. Called on the TAG path only — a
/// digest-pinned resolve skips it (a yank is a tag-lane signal, never checked on
/// an immutable pin).
pub(super) fn surface_root_status(
    identifier: &oci::Identifier,
    root: &IndexRoot,
    tag: &RootTag,
    allow_yanked: bool,
) -> Result<()> {
    let yanked = tag.yanked.unwrap_or(false) || root.status.as_deref() == Some("yanked");
    if yanked {
        log::warn!("'{identifier}' resolves to a yanked entry — a yank is a publisher signal, not a delete");
        if !allow_yanked {
            return Err(super::error::Error::YankedRefused {
                identifier: identifier.to_string(),
            }
            .into());
        }
    }
    if root.status.as_deref() == Some("deprecated") {
        match &root.deprecated_message {
            Some(message) => log::warn!("'{identifier}' is deprecated: {message}"),
            None => log::warn!("'{identifier}' is deprecated"),
        }
    }
    // Advisory only — never auto-follows the successor (the C-46 identity
    // binding: never override the requested identity).
    if let Some(successor) = &root.superseded_by {
        log::warn!("'{identifier}' is superseded by '{successor}' (advisory; not followed automatically)");
    }
    Ok(())
}

/// Parses `bytes` as `T`, wrapping a serde failure with the source `url` so a
/// malformed index document reports where it came from.
fn parse_document<T: for<'de> Deserialize<'de>>(bytes: &[u8], url: &str) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|source| {
        super::error::Error::MalformedIndexDocument {
            url: redact_url(url),
            source,
        }
        .into()
    })
}

#[async_trait]
impl index_impl::IndexImpl for OcxIndex {
    async fn list_repositories(&self, registry: &str) -> Result<Vec<String>> {
        // Only this source's namespace is served; a different registry is not
        // this source's concern. The catalog is the offline listing source.
        if registry != self.namespace {
            return Ok(Vec::new());
        }
        let outcome = self.sync_catalog(&CatalogIndex::new(), None).await?;
        let mut repositories: Vec<String> = outcome.catalog.into_keys().collect();
        repositories.sort();
        repositories.dedup();
        Ok(repositories)
    }

    async fn list_tags(&self, identifier: &oci::Identifier) -> Result<Option<Vec<String>>> {
        if identifier.registry() != self.namespace {
            return Ok(None);
        }
        let Some(root) = self.resolve_root(identifier.repository()).await? else {
            return Ok(None);
        };
        Ok(Some(root.tags.keys().cloned().collect()))
    }

    async fn fetch_manifest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
        if identifier.registry() != self.namespace {
            return Ok(None);
        }

        // Digest-addressed (a resolved platform-manifest leaf): the physical
        // fetch. The OCI image-index hop is bypassed — the digest IS the
        // platform manifest digest (C1).
        if identifier.digest().is_some() {
            let Some(physical) = self.physical_identifier(identifier).await? else {
                return Ok(None);
            };
            return Ok(Some(self.client.fetch_manifest(&physical).await?));
        }

        // Tag-addressed: resolve the observation and present its platforms[] as
        // a synthetic image index for `fetch_candidates` / `select_best`.
        let Some((obs_digest, observation)) = self.resolve_tag(identifier).await? else {
            return Ok(None);
        };
        let index = observation_to_index(&observation)?;
        Ok(Some((obs_digest, oci::Manifest::ImageIndex(index))))
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<oci::Digest>> {
        if identifier.registry() != self.namespace {
            return Ok(None);
        }
        // A digest-addressed identifier already names its manifest digest.
        if let Some(digest) = identifier.digest() {
            return Ok(Some(digest));
        }
        // Tag-addressed: the observation digest is what the tag points at.
        Ok(self.resolve_tag(identifier).await?.map(|(digest, _)| digest))
    }

    async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
        if blob_ref.as_identifier().registry() != self.namespace {
            return Ok(None);
        }
        // Layers are physical — dereference the root's repository pointer and
        // pull through the mirror-aware client.
        let Some(physical) = self.physical_identifier(blob_ref.as_identifier()).await? else {
            return Ok(None);
        };
        let physical_pinned = oci::PinnedIdentifier::try_from(physical)?;
        Ok(Some(self.client.pull_blob(&physical_pinned).await?))
    }

    async fn fetch_manifest_raw_bytes(
        &self,
        identifier: &oci::Identifier,
    ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
        if identifier.registry() != self.namespace {
            return Ok(None);
        }

        // Leaf: the physical registry serves the verbatim platform manifest,
        // whose bytes hash to the leaf digest — A3-valid for an index write.
        if identifier.digest().is_some() {
            let Some(physical) = self.physical_identifier(identifier).await? else {
                return Ok(None);
            };
            return Ok(self.client.fetch_manifest_raw_bytes(&physical).await?);
        }

        // Tag: the verbatim observation bytes (which hash to the observation
        // digest — A3-valid) paired with the synthetic index the persist path
        // recurses over. Bytes and parsed manifest deliberately differ in
        // shape: the persist layer writes the bytes and only inspects the
        // manifest kind to decide recursion.
        let Some(root) = self.resolve_root(identifier.repository()).await? else {
            return Ok(None);
        };
        let tag = identifier.tag_or_latest();
        let Some(tag_entry) = root.tags.get(tag) else {
            return Ok(None);
        };
        self.surface_status(identifier, &root, tag_entry)?;
        let obs_digest = tag_entry.content.clone();
        let Some((bytes, observation)) = self.resolve_observation(identifier.repository(), &obs_digest).await? else {
            return Ok(None);
        };
        let index = observation_to_index(&observation)?;
        Ok(Some((bytes, obs_digest, oci::Manifest::ImageIndex(index))))
    }

    async fn fetch_root_document(&self, identifier: &oci::Identifier) -> Result<Option<(Vec<u8>, IndexRoot)>> {
        // A published source serves the verbatim `p/<ns>/<pkg>.json` bytes paired
        // with the parsed root, so `LocalIndex::persist_published_root` grows the
        // local copy byte-for-byte (copy-a-mirror, A2). The bytes are returned
        // verbatim (never re-serialized) so they hash to the catalog entry (F1).
        if identifier.registry() != self.namespace {
            return Ok(None);
        }
        // Fail-closed: an absent config.json means this base is not a
        // version-pinned index, so its root documents must not be consumed (F1).
        if let FormatVersionState::NotAnIndex = self.check_format_version().await? {
            return Ok(None);
        }
        let url = format!("{}/p/{}.json", self.base_url, identifier.repository());
        match self.transport.get(&url, None).await? {
            IndexFetch::Found { bytes, .. } => {
                let root: IndexRoot = parse_document(&bytes, &url)?;
                Ok(Some((bytes, root)))
            }
            // A 404 (or a 304 with no cached body) is a clean miss, never an error.
            IndexFetch::NotFound | IndexFetch::NotModified => Ok(None),
        }
    }

    async fn physical_reference(&self, identifier: &oci::Identifier) -> Result<Option<oci::Identifier>> {
        if identifier.registry() != self.namespace {
            return Ok(None);
        }
        // Dereference the root's `repository` pointer, carrying the logical
        // digest onto the physical location — transport-only (C2).
        self.physical_identifier(identifier).await
    }

    fn is_authoritative_for(&self, identifier: &oci::Identifier) -> bool {
        // This source is the authoritative resolver for every reference in the
        // namespace it serves, so a refusal here stops the chain (F3).
        identifier.registry() == self.namespace
    }

    fn source_kind(&self) -> super::local_index::SourceKind {
        super::local_index::SourceKind::Published
    }

    fn box_clone(&self) -> Box<dyn index_impl::IndexImpl> {
        Box::new(self.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::super::index_impl::IndexImpl;
    use super::*;
    use crate::oci::Algorithm;
    use crate::oci::client::test_transport::{StubTransport, StubTransportData};

    const BASE: &str = "https://index.test";
    const NAMESPACE: &str = "ocx.sh";
    const REPO: &str = "kitware/cmake";

    // ── HTTP boundary stub (mirrors the StubTransport pattern) ───────────────

    /// url → (body bytes, optional ETag). A present entry is a `200`.
    type StubResponses = Arc<Mutex<HashMap<String, (Vec<u8>, Option<String>)>>>;
    /// Recorded `(url, if_none_match)` requests, for assertions.
    type StubRequests = Arc<Mutex<Vec<(String, Option<String>)>>>;

    #[derive(Clone, Default)]
    struct StubIndexTransport {
        responses: StubResponses,
        requests: StubRequests,
        /// URLs that return a transport error (simulate a dead endpoint).
        failures: Arc<Mutex<std::collections::HashSet<String>>>,
    }

    impl StubIndexTransport {
        fn new() -> Self {
            Self::default()
        }

        fn insert(&self, url: &str, bytes: &[u8]) {
            self.responses
                .lock()
                .unwrap()
                .insert(url.to_string(), (bytes.to_vec(), None));
        }

        fn insert_with_etag(&self, url: &str, bytes: &[u8], etag: &str) {
            self.responses
                .lock()
                .unwrap()
                .insert(url.to_string(), (bytes.to_vec(), Some(etag.to_string())));
        }

        fn fail(&self, url: &str) {
            self.failures.lock().unwrap().insert(url.to_string());
        }

        fn request_urls(&self) -> Vec<String> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|(url, _)| url.clone())
                .collect()
        }

        fn request_count(&self, url: &str) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(requested, _)| requested == url)
                .count()
        }
    }

    #[async_trait]
    impl IndexTransport for StubIndexTransport {
        async fn get(&self, url: &str, if_none_match: Option<&str>) -> Result<IndexFetch> {
            self.requests
                .lock()
                .unwrap()
                .push((url.to_string(), if_none_match.map(str::to_owned)));
            if self.failures.lock().unwrap().contains(url) {
                return Err(super::super::error::Error::IndexHttpFailed {
                    url: url.to_string(),
                    source: "simulated transport failure".into(),
                }
                .into());
            }
            let responses = self.responses.lock().unwrap();
            match responses.get(url) {
                Some((bytes, etag)) => {
                    if let (Some(requested), Some(stored)) = (if_none_match, etag.as_deref())
                        && requested == stored
                    {
                        return Ok(IndexFetch::NotModified);
                    }
                    Ok(IndexFetch::Found {
                        bytes: bytes.clone(),
                        etag: etag.clone(),
                    })
                }
                None => Ok(IndexFetch::NotFound),
            }
        }

        fn box_clone(&self) -> Box<dyn IndexTransport> {
            Box::new(self.clone())
        }
    }

    // ── construction helpers ─────────────────────────────────────────────────

    fn stub_client() -> oci::Client {
        // The physical OCI client is never reached on the tag-resolution paths
        // these tests exercise; an empty stub satisfies construction.
        oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())))
    }

    fn make_source(transport: StubIndexTransport, allow_yanked: bool) -> OcxIndex {
        OcxIndex::new(OcxIndexConfig {
            transport: Box::new(transport),
            base_url: BASE.to_string(),
            namespace: NAMESPACE.to_string(),
            client: stub_client(),
            allow_yanked,
        })
    }

    fn config_url() -> String {
        format!("{BASE}/config.json")
    }
    fn root_url() -> String {
        format!("{BASE}/p/{REPO}.json")
    }
    fn obs_url(digest: &oci::Digest) -> String {
        format!(
            "{BASE}/p/{REPO}/o/{}/{}.json",
            digest.algorithm().prefix(),
            digest.hex()
        )
    }
    fn catalog_url() -> String {
        format!("{BASE}/c/index.json")
    }
    fn tagged_id() -> oci::Identifier {
        oci::Identifier::new_registry(REPO, NAMESPACE).clone_with_tag("3.28")
    }

    /// A two-platform observation (glibc + musl leaves) as verbatim wire bytes.
    fn glibc_musl_observation() -> &'static [u8] {
        concat!(
            r#"{"platforms":[{"platform":{"architecture":"amd64","os":"linux","os.features":["libc.glibc"]},"#,
            r#""digest":"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"#,
            r#"{"platform":{"architecture":"amd64","os":"linux","os.features":["libc.musl"]},"#,
            r#""digest":"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}]}"#
        )
        .as_bytes()
    }

    /// Seeds config + root (tag `3.28` → obs) + observation, returning the obs
    /// digest and its verbatim bytes. `yanked` toggles the per-tag marker.
    fn seed_package(transport: &StubIndexTransport, yanked: bool) -> oci::Digest {
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let obs_bytes = glibc_musl_observation();
        let obs_digest = Algorithm::Sha256.hash(obs_bytes);
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{}","yanked":{}}}}}}}"#,
            obs_digest, yanked
        );
        transport.insert(&root_url(), root.as_bytes());
        transport.insert(&obs_url(&obs_digest), obs_bytes);
        obs_digest
    }

    // ── redact_url (CWE-532) ──────────────────────────────────────────────────

    #[test]
    fn redact_url_strips_userinfo_with_password() {
        assert_eq!(redact_url("https://user:pass@host/x"), "https://***@host/x");
    }

    #[test]
    fn redact_url_leaves_credential_free_url_untouched() {
        assert_eq!(redact_url("https://host/x"), "https://host/x");
    }

    #[test]
    fn redact_url_leaves_non_url_string_untouched() {
        assert_eq!(redact_url("not a url"), "not a url");
    }

    #[test]
    fn redact_url_strips_userinfo_without_password() {
        assert_eq!(redact_url("https://user@host"), "https://***@host");
    }

    // ── serde roundtrips of the ● wire shapes ────────────────────────────────

    #[test]
    fn wire_shapes_deserialize_from_frozen_fixtures() {
        let config: IndexFormatConfig = serde_json::from_slice(br#"{"format_version":1}"#).unwrap();
        assert_eq!(config.format_version, 1);

        let root: IndexRoot = serde_json::from_slice(
            format!(
                r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","status":"deprecated","deprecated_message":"use 4.x","tags":{{"3.28":{{"content":"sha256:{}","observed":"2026-07-18T09:00:00Z","yanked":true}}}}}}"#,
                "a".repeat(64)
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(root.repository, "oci://ghcr.io/ocx-contrib/cmake");
        assert_eq!(root.status.as_deref(), Some("deprecated"));
        assert_eq!(root.deprecated_message.as_deref(), Some("use 4.x"));
        let tag = root.tags.get("3.28").expect("tag present");
        assert_eq!(tag.content, oci::Digest::Sha256("a".repeat(64)));
        assert_eq!(tag.yanked, Some(true));

        let observation: Observation = serde_json::from_slice(glibc_musl_observation()).unwrap();
        assert_eq!(observation.platforms.len(), 2);
        assert_eq!(
            observation.platforms[0].platform.os_features.as_deref(),
            Some(["libc.glibc".to_string()].as_slice())
        );

        let catalog: CatalogIndex =
            serde_json::from_slice(br#"{"kitware/cmake":"sha256:root1","other/tool":"sha256:root2"}"#).unwrap();
        assert_eq!(catalog.get("kitware/cmake").map(String::as_str), Some("sha256:root1"));
    }

    // ── select_best over obs platforms ───────────────────────────────────────

    #[tokio::test]
    async fn resolve_tag_synthesizes_index_and_select_picks_host_platform() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let index = super::super::Index::from_impl(make_source(transport, false));

        let glibc_host: oci::Platform = "linux/amd64+libc.glibc".parse().unwrap();
        let result = index
            .select(&tagged_id(), &glibc_host, IndexOperation::Resolve)
            .await
            .unwrap();

        match result {
            super::super::SelectResult::Found(id) => assert_eq!(
                id.digest().map(|d| d.to_string()),
                Some(format!("sha256:{}", "a".repeat(64))),
                "glibc host must select the libc.glibc obs leaf"
            ),
            super::super::SelectResult::Ambiguous(_) => panic!("expected Found(glibc leaf), got Ambiguous"),
            super::super::SelectResult::NotFound => panic!("expected Found(glibc leaf), got NotFound"),
            super::super::SelectResult::FeatureMismatch { .. } => {
                panic!("expected Found(glibc leaf), got FeatureMismatch")
            }
        }
    }

    #[tokio::test]
    async fn fetch_manifest_returns_synthetic_index_with_obs_digest() {
        let transport = StubIndexTransport::new();
        let obs_digest = seed_package(&transport, false);
        let source = make_source(transport, false);

        let (digest, manifest) = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("tag resolves");
        assert_eq!(
            digest, obs_digest,
            "the resolved digest is the observation digest (the CAS root)"
        );
        match manifest {
            oci::Manifest::ImageIndex(index) => assert_eq!(index.manifests.len(), 2),
            other => panic!("expected a synthesized image index, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fetch_manifest_surfaces_malformed_index_document_for_bad_tag_content_digest() {
        // `RootTag::content`'s `oci::Digest` deserialize is exact-wire
        // (`adr_index_indirection.md` amendment 2026-07-19) — a malformed
        // digest value fails the WHOLE root-document parse, surfaced through
        // this remote-fetch path as `MalformedIndexDocument`, never a
        // narrower per-tag error.
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let root = r#"{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{"3.28":{"content":"not-a-digest"}}}"#;
        transport.insert(&root_url(), root.as_bytes());

        let source = make_source(transport, false);
        let error = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("a malformed tag content digest must fail the whole root document parse");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::MalformedIndexDocument { .. })
            ),
            "expected MalformedIndexDocument, got {error:?}"
        );
    }

    // ── observation verify (trust anchor) ────────────────────────────────────

    #[tokio::test]
    async fn observation_digest_mismatch_is_hard_error() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let obs_bytes = glibc_musl_observation();
        let honest_digest = Algorithm::Sha256.hash(obs_bytes);
        // Root points at the honest digest, but the served obs URL holds
        // tampered bytes — the verify must catch it.
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{honest_digest}"}}}}}}"#,
        );
        transport.insert(&root_url(), root.as_bytes());
        transport.insert(&obs_url(&honest_digest), br#"{"platforms":[]}"#);

        let source = make_source(transport, false);
        let error = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("tampered obs bytes must not load");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::ObservationDigestMismatch { .. })
            ),
            "expected ObservationDigestMismatch, got {error:?}"
        );
    }

    // ── config.json fail-closed / inert ──────────────────────────────────────

    #[tokio::test]
    async fn unsupported_format_version_fails_closed() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":2}"#);
        transport.insert(&root_url(), br#"{"repository":"oci://ghcr.io/x/y","tags":{}}"#);

        let source = make_source(transport, false);
        let error = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("unknown format_version must fail closed");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::UnsupportedIndexFormat { version: 2 })
            ),
            "expected UnsupportedIndexFormat{{2}}, got {error:?}"
        );
    }

    #[tokio::test]
    async fn absent_config_leaves_source_inert() {
        // No config.json, no root registered → every fetch is a clean miss,
        // never an error (a base URL that serves no config is simply not an
        // OCX index yet).
        let source = make_source(StubIndexTransport::new(), false);
        let result = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_none(), "an inert source must miss cleanly, not error");
    }

    #[tokio::test]
    async fn absent_config_refuses_valid_root_and_never_fetches_it() {
        // Fail-closed (F1): a base that serves a fully valid root + observation
        // but NO config.json is not a version-pinned OCX index. The root must
        // never be consumed — an absent config.json must not degrade to a clean
        // pass that lets a valid-looking root resolve against an unversioned
        // endpoint. The root document must not even be fetched: the config check
        // short-circuits before the root GET.
        let transport = StubIndexTransport::new();
        // Deliberately DO NOT insert config.json — serve an otherwise-valid tree.
        let obs_bytes = glibc_musl_observation();
        let obs_digest = Algorithm::Sha256.hash(obs_bytes);
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{obs_digest}"}}}}}}"#,
        );
        transport.insert(&root_url(), root.as_bytes());
        transport.insert(&obs_url(&obs_digest), obs_bytes);
        let source = make_source(transport.clone(), false);

        let result = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "a valid root must not resolve when config.json is absent (fail-closed, F1)"
        );
        assert!(
            !transport.request_urls().contains(&root_url()),
            "the root document must never be fetched when config.json is absent"
        );
    }

    // ── status surfacing (F3): yank refusal + digest-pin passthrough ─────────

    #[tokio::test]
    async fn yanked_tag_is_refused_without_optin() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, true);
        let source = make_source(transport, false);

        let error = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("a yanked tag resolve must be refused without opt-in");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::YankedRefused { .. })
            ),
            "expected YankedRefused, got {error:?}"
        );
    }

    #[tokio::test]
    async fn yanked_tag_allowed_with_optin() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, true);
        let source = make_source(transport, true);

        let result = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_some(), "the yanked opt-in must let the tag resolve");
    }

    #[tokio::test]
    async fn digest_pinned_resolve_bypasses_yank_check() {
        // A digest-addressed identifier names an immutable manifest; the yank
        // refusal (a tag-path concern) must not apply. `fetch_manifest_digest`
        // returns the pinned digest without touching the root at all.
        let source = make_source(StubIndexTransport::new(), false);
        let pinned = oci::Digest::Sha256("c".repeat(64));
        let id = oci::Identifier::new_registry(REPO, NAMESPACE).clone_with_digest(pinned.clone());

        let resolved = source
            .fetch_manifest_digest(&id, IndexOperation::Resolve)
            .await
            .unwrap();
        assert_eq!(
            resolved,
            Some(pinned),
            "a digest-pinned resolve returns its own digest, no yank check"
        );
    }

    /// Builds an [`IndexRoot`] carrying tag `3.28` plus the given human-lane
    /// fields, for direct [`surface_root_status`] branch coverage. `content`
    /// is a fixed valid obs digest — the status lane never inspects it.
    fn root_with_status(
        status: Option<&str>,
        deprecated_message: Option<&str>,
        superseded_by: Option<&str>,
    ) -> IndexRoot {
        let mut fields = format!(
            r#""repository":"oci://ghcr.io/x/y","tags":{{"3.28":{{"content":"sha256:{}"}}}}"#,
            "a".repeat(64)
        );
        if let Some(status) = status {
            fields.push_str(&format!(r#","status":"{status}""#));
        }
        if let Some(message) = deprecated_message {
            fields.push_str(&format!(r#","deprecated_message":"{message}""#));
        }
        if let Some(successor) = superseded_by {
            fields.push_str(&format!(r#","superseded_by":"{successor}""#));
        }
        serde_json::from_str(&format!("{{{fields}}}")).expect("valid root document")
    }

    /// The deprecated-with-message branch of [`surface_root_status`] warns but
    /// does NOT refuse — a deprecation is advisory (only a yank without opt-in
    /// refuses). Complements the yank-refusal coverage above; the deprecated
    /// branch had none.
    #[test]
    fn surface_root_status_deprecated_with_message_warns_but_does_not_refuse() {
        let root = root_with_status(Some("deprecated"), Some("use 4.x"), None);
        let tag = root.tags.get("3.28").expect("tag present");
        // allow_yanked=false: proves the non-refusal is intrinsic to the
        // deprecation branch, not an opt-in effect.
        assert!(
            surface_root_status(&tagged_id(), &root, tag, false).is_ok(),
            "a deprecated (with message) root must warn but resolve, never refuse"
        );
    }

    /// The `superseded_by` branch of [`surface_root_status`] warns (advisory,
    /// never auto-follows the successor — the C-46 identity binding) and does
    /// NOT refuse.
    #[test]
    fn surface_root_status_superseded_by_warns_but_does_not_refuse() {
        let root = root_with_status(None, None, Some("x/y:4.0"));
        let tag = root.tags.get("3.28").expect("tag present");
        assert!(
            surface_root_status(&tagged_id(), &root, tag, false).is_ok(),
            "a superseded_by root must warn (advisory) but resolve, never refuse"
        );
    }

    // ── [registries."<ns>"] index base-URL minting (F5c) ──────────────────────

    fn no_mirrors() -> BTreeMap<String, crate::config::mirror::ParsedMirror> {
        BTreeMap::new()
    }

    #[test]
    fn resolve_base_url_defaults_and_honors_registries_index() {
        let empty = crate::config::Config::default();
        assert_eq!(
            OcxIndex::resolve_base_url(&empty, "ocx.sh", &no_mirrors(), &[]).unwrap(),
            DEFAULT_INDEX_BASE_URL,
            "no [registries.\"ocx.sh\"] index field must yield the default base URL"
        );

        let config: crate::config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"https://artifactory.corp/ocx-index/\"").unwrap();
        assert_eq!(
            OcxIndex::resolve_base_url(&config, "ocx.sh", &no_mirrors(), &[]).unwrap(),
            "https://artifactory.corp/ocx-index",
            "[registries.\"<ns>\"] index must replace the base URL (trailing slash trimmed)"
        );
        assert_eq!(
            OcxIndex::resolve_base_url(&config, "other.sh", &no_mirrors(), &[]).unwrap(),
            DEFAULT_INDEX_BASE_URL,
            "an unlisted namespace falls back to the default"
        );
    }

    #[test]
    fn resolve_base_url_gates_plain_http_target() {
        let config: crate::config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"http://mirror.corp/ocx-index\"").unwrap();

        // http base without the host in the insecure list → hard config error.
        let error = OcxIndex::resolve_base_url(&config, "ocx.sh", &no_mirrors(), &[])
            .expect_err("an http index base must be refused without an insecure-host listing");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::PlainHttpIndexNotAllowed { ref host, .. })
                    if host == "mirror.corp"
            ),
            "expected PlainHttpIndexNotAllowed naming the host, got {error:?}"
        );

        // Same base allowed once the host is listed.
        let insecure = vec!["mirror.corp".to_string()];
        assert_eq!(
            OcxIndex::resolve_base_url(&config, "ocx.sh", &no_mirrors(), &insecure).unwrap(),
            "http://mirror.corp/ocx-index",
            "an http base is allowed when its host is in OCX_INSECURE_REGISTRIES"
        );

        // The default https base URL is never gated.
        assert_eq!(
            OcxIndex::resolve_base_url(&crate::config::Config::default(), "ocx.sh", &no_mirrors(), &[]).unwrap(),
            DEFAULT_INDEX_BASE_URL,
            "https must pass the gate untouched"
        );
    }

    #[test]
    fn resolve_base_url_gates_mixed_case_http_scheme() {
        // CWE-319 regression: a mixed-case `HTTP://` index base must hit the
        // plain-HTTP gate exactly like lowercase `http://` — the scheme is
        // normalized to lowercase in `parse_url`, so the gate's `== "http"`
        // comparison cannot be bypassed by casing.
        let config: crate::config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"HTTP://mirror.corp/ocx-index\"").unwrap();

        let error = OcxIndex::resolve_base_url(&config, "ocx.sh", &no_mirrors(), &[])
            .expect_err("a mixed-case HTTP:// index base must be refused without an insecure-host listing");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::PlainHttpIndexNotAllowed { ref host, .. })
                    if host == "mirror.corp"
            ),
            "expected PlainHttpIndexNotAllowed for a mixed-case HTTP:// scheme, got {error:?}"
        );
    }

    #[test]
    fn resolve_base_url_applies_mirrors_index_role_override() {
        // The default `index.ocx.sh` host has a `[mirrors."index.ocx.sh"]
        // index` role override — the mirror wins over the un-mirrored default,
        // replace semantics (no fallback to the un-mirrored host).
        let mut mirrors_index = BTreeMap::new();
        mirrors_index.insert(
            "index.ocx.sh".to_string(),
            crate::config::mirror::parse_url("https://artifactory.corp/ocx-index").unwrap(),
        );

        assert_eq!(
            OcxIndex::resolve_base_url(&crate::config::Config::default(), "ocx.sh", &mirrors_index, &[]).unwrap(),
            "https://artifactory.corp/ocx-index",
            "a mirrors index-role override for the base's traffic host must replace the base URL"
        );

        // An override keyed by a DIFFERENT host than the base's own traffic
        // host must not apply — role override is host-keyed, not blanket.
        let mut unrelated_mirror = BTreeMap::new();
        unrelated_mirror.insert(
            "some-other-host.example".to_string(),
            crate::config::mirror::parse_url("https://artifactory.corp/ocx-index").unwrap(),
        );
        assert_eq!(
            OcxIndex::resolve_base_url(&crate::config::Config::default(), "ocx.sh", &unrelated_mirror, &[]).unwrap(),
            DEFAULT_INDEX_BASE_URL,
            "a mirror keyed by an unrelated host must not affect this base URL"
        );
    }

    // ── catalog sync (F2): conditional GET + digest diff ─────────────────────

    #[tokio::test]
    async fn sync_catalog_diff_returns_only_moved_packages() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert_with_etag(
            &catalog_url(),
            br#"{"kitware/cmake":"sha256:new","stable/tool":"sha256:same","fresh/pkg":"sha256:brand"}"#,
            "etag-v2",
        );
        let source = make_source(transport, false);

        let mut previous = CatalogIndex::new();
        previous.insert("kitware/cmake".to_string(), "sha256:old".to_string());
        previous.insert("stable/tool".to_string(), "sha256:same".to_string());

        let outcome = source.sync_catalog(&previous, Some("etag-v1")).await.unwrap();
        assert!(!outcome.unchanged);
        assert_eq!(outcome.etag.as_deref(), Some("etag-v2"));
        assert_eq!(
            outcome.moved,
            vec!["kitware/cmake".to_string()],
            "only the previously-known root whose digest moved (kitware/cmake) is a move; stable/tool is \
             unchanged and fresh/pkg is new-to-local — a listing row, never a re-snapshot (F2)"
        );
        assert!(
            outcome.catalog.contains_key("fresh/pkg"),
            "a new-to-local package is still recorded in the fetched catalog as a listing row"
        );
    }

    #[tokio::test]
    async fn sync_catalog_not_modified_is_unchanged() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert_with_etag(&catalog_url(), br#"{"kitware/cmake":"sha256:x"}"#, "etag-1");
        let source = make_source(transport, false);

        let mut previous = CatalogIndex::new();
        previous.insert("kitware/cmake".to_string(), "sha256:x".to_string());

        // The conditional GET matches the stored ETag → 304.
        let outcome = source.sync_catalog(&previous, Some("etag-1")).await.unwrap();
        assert!(outcome.unchanged, "a matching ETag must short-circuit to unchanged");
        assert!(outcome.moved.is_empty());
        assert_eq!(outcome.catalog, previous, "the previous catalog is carried forward");
    }

    // ── physical reference parsing (C3, one-way door) ────────────────────────

    #[test]
    fn parse_physical_repository_accepts_oci_scheme() {
        let (registry, repository) = parse_physical_repository("oci://ghcr.io/ocx-contrib/cmake").unwrap();
        assert_eq!(registry, "ghcr.io");
        assert_eq!(repository, "ocx-contrib/cmake");
    }

    #[test]
    fn parse_physical_repository_rejects_missing_scheme_and_empty_parts() {
        for bad in [
            "ghcr.io/x/y",
            "https://ghcr.io/x",
            "oci://ghcr.io",
            "oci:///x",
            "oci://ghcr.io/",
        ] {
            assert!(
                matches!(
                    parse_physical_repository(bad),
                    Err(crate::Error::OciIndex(
                        super::super::error::Error::MalformedPhysicalRef { .. }
                    ))
                ),
                "'{bad}' must be a MalformedPhysicalRef"
            );
        }
    }

    #[test]
    fn parse_physical_repository_rejects_grammar_violating_refs() {
        // The Identifier round-trip rejects anything the registry grammar would
        // not mint: whitespace / control chars in the repository, a smuggled tag
        // (`repo:x`) or digest (`repo@sha256:…`), an uppercase segment, and a
        // stray-colon (port) abuse in the repository path.
        for bad in [
            "oci://ghcr.io/foo bar",             // space in repository
            "oci://ghcr.io/foo\tbar",            // control char in repository
            "oci://ghcr.io/foo:3.28",            // smuggled tag
            "oci://ghcr.io/foo@sha256:deadbeef", // smuggled digest
            "oci://ghcr.io/Foo",                 // uppercase segment
            "oci://ghcr.io:5000/foo:1.0",        // colon/port abuse in the path
            "oci://ghcr.io/foo@bar",             // embedded @ (bad digest)
        ] {
            assert!(
                matches!(
                    parse_physical_repository(bad),
                    Err(crate::Error::OciIndex(
                        super::super::error::Error::MalformedPhysicalRef { .. }
                    ))
                ),
                "'{bad}' must be a MalformedPhysicalRef"
            );
        }
    }

    #[test]
    fn parse_physical_repository_accepts_host_with_port() {
        // A legitimate `host:port` registry (e.g. a private mirror) round-trips:
        // the port colon lives in the host segment, never mistaken for a tag.
        let (registry, repository) = parse_physical_repository("oci://localhost:5000/ocx-contrib/cmake").unwrap();
        assert_eq!(registry, "localhost:5000");
        assert_eq!(repository, "ocx-contrib/cmake");
    }

    #[tokio::test]
    async fn physical_identifier_dereferences_root_pointer() {
        let transport = StubIndexTransport::new();
        let obs_digest = seed_package(&transport, false);
        let source = make_source(transport, false);

        // A resolved leaf: logical id carrying the physical manifest digest.
        let leaf = oci::Digest::Sha256("a".repeat(64));
        let logical = oci::Identifier::new_registry(REPO, NAMESPACE).clone_with_digest(leaf.clone());
        let physical = source
            .physical_identifier(&logical)
            .await
            .unwrap()
            .expect("root resolves");
        assert_eq!(physical.registry(), "ghcr.io");
        assert_eq!(physical.repository(), "ocx-contrib/cmake");
        assert_eq!(
            physical.digest(),
            Some(leaf),
            "the leaf digest is carried onto the physical location"
        );
        let _ = obs_digest; // silence unused in this path
    }

    // ── catalog sync orchestration: diff → re-snapshot → persist on disk ─────

    #[tokio::test]
    async fn local_index_sync_catalog_persists_and_snapshots_moved_package() {
        // A moved package whose observation declares no platforms — so the
        // re-snapshot writes the observation object without a physical leaf
        // fetch (no OCI client needed).
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let obs_bytes = br#"{"platforms":[]}"#;
        let obs_digest = Algorithm::Sha256.hash(obs_bytes);
        let root = format!(r#"{{"repository":"oci://ghcr.io/x/y","tags":{{"1.0":{{"content":"{obs_digest}"}}}}}}"#,);
        transport.insert(&format!("{BASE}/p/ns/pkg.json"), root.as_bytes());
        // The remote catalog entry IS sha256 of the root bytes (F1) — so the
        // second, unchanged sync sees no diff. A mock literal that disagreed with
        // the served root would make every sync re-diff the root-derived local
        // entry against the stale claim and re-snapshot forever.
        let catalog = format!(
            r#"{{"ns/pkg":"{}"}}"#,
            crate::file_structure::IndexStore::root_catalog_entry(root.as_bytes())
        );
        transport.insert(&catalog_url(), catalog.as_bytes());
        transport.insert(
            &format!(
                "{BASE}/p/ns/pkg/o/{}/{}.json",
                obs_digest.algorithm().prefix(),
                obs_digest.hex()
            ),
            obs_bytes,
        );
        let source = make_source(transport, false);

        let dir = tempfile::tempdir().unwrap();
        let snapshot = crate::file_structure::IndexStore::new(dir.path().join("index"));
        let local = super::super::LocalIndex::new(super::super::LocalConfig {
            index_store: snapshot.clone(),
        });

        // Materialize `ns/pkg` with a STALE digest, so the remote catalog's newer
        // digest is a MOVE of an existing local root — the only shape re-snapshotted
        // under the corrected F2 listing-row contract (a brand-new package is a
        // listing row, materialized only when first updated).
        seed_stale_root(&snapshot, "ns/pkg").await;

        let outcome = local.sync_catalog(&source).await.unwrap();
        assert_eq!(
            outcome.moved,
            vec!["ns/pkg".to_string()],
            "an already-materialized package whose digest moved must re-snapshot"
        );

        // The per-source catalog is persisted at <home>/<source>/c/index.json —
        // the diff basis + offline source (A2/F2). A MOVED package's entry is set
        // by the refresh path (sha256 of the actually-served root bytes), NOT the
        // mock catalog literal — the reconcile-commit skips moved packages so it
        // never clobbers the root-derived entry back to the pre-fetch claim.
        let persisted = snapshot
            .read_source_catalog(NAMESPACE)
            .await
            .unwrap()
            .expect("per-source catalog persisted");
        assert_eq!(
            persisted.get("ns/pkg"),
            Some(&crate::file_structure::IndexStore::root_catalog_entry(root.as_bytes())),
            "the moved package's entry must be sha256 of the served root bytes, not the mock catalog literal"
        );

        // The moved package is re-snapshotted through the published grow path: its
        // verbatim root document plus the referenced observation dispatch object
        // land in the wire grammar, so a subsequent offline resolve walks the copy.
        assert!(
            snapshot.root_document_path(NAMESPACE, "ns/pkg").exists(),
            "the re-snapshot must write the verbatim root document for the moved package"
        );
        assert!(
            snapshot.dispatch_object_path(NAMESPACE, "ns/pkg", &obs_digest).exists(),
            "the re-snapshot must persist the observation as a dispatch object"
        );

        // A second sync with the catalog unchanged re-snapshots nothing.
        let again = local.sync_catalog(&source).await.unwrap();
        assert!(again.moved.is_empty(), "an unchanged catalog must re-snapshot nothing");
    }

    // ── catalog-entry precedence: moved = root-derived, unmoved = fetched ─────

    #[tokio::test]
    async fn sync_catalog_moved_entry_is_root_derived_unmoved_adopts_fetched() {
        // The remote catalog CLAIMS a digest for `moved/pkg` that does NOT match
        // its actually-served root bytes (CDN skew). The moved entry must come
        // from the refresh path (sha256 of the persisted root), never this claim.
        const WRONG_CLAIM: &str = "sha256:deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let transport = StubIndexTransport::new();
        transport.insert(
            &catalog_url(),
            format!(r#"{{"moved/pkg":"{WRONG_CLAIM}","unmoved/pkg":"sha256:oldunmoved"}}"#).as_bytes(),
        );
        seed_empty_obs(&transport, "moved/pkg", "1.0");
        let source = make_source(transport, false);

        let dir = tempfile::tempdir().unwrap();
        let snapshot = crate::file_structure::IndexStore::new(dir.path().join("index"));
        let local = super::super::LocalIndex::new(super::super::LocalConfig {
            index_store: snapshot.clone(),
        });

        // Materialize `moved/pkg` with a stale digest so the remote's WRONG_CLAIM
        // is a MOVE of an existing local root (the only shape re-snapshotted, F2).
        seed_stale_root(&snapshot, "moved/pkg").await;
        // Pre-seed `unmoved/pkg` as a listing row (catalog entry, no local root)
        // whose fetched digest equals the previous → UNMOVED, adopts fetched.
        let mut seed = snapshot.begin_catalog_transaction(NAMESPACE).await.unwrap();
        seed.catalog()
            .insert("unmoved/pkg".to_string(), "sha256:oldunmoved".to_string());
        seed.commit(None).await.unwrap();

        let outcome = local.sync_catalog(&source).await.unwrap();
        assert_eq!(
            outcome.moved,
            vec!["moved/pkg".to_string()],
            "only the materialized package whose digest moved re-snapshots; the equal-digest listing row did not"
        );

        let persisted = snapshot.read_source_catalog(NAMESPACE).await.unwrap().unwrap();

        // Moved: the entry is the root-derived digest set by the refresh path,
        // never the (wrong) fetched claim — the reconcile-commit skipped it.
        let root_bytes = std::fs::read(snapshot.root_document_path(NAMESPACE, "moved/pkg")).unwrap();
        assert_eq!(
            persisted.get("moved/pkg"),
            Some(&crate::file_structure::IndexStore::root_catalog_entry(&root_bytes)),
            "a moved package's entry must be the root-derived digest from the refresh path"
        );
        assert_ne!(
            persisted.get("moved/pkg").map(String::as_str),
            Some(WRONG_CLAIM),
            "the fetched catalog claim must never clobber the root-derived moved entry"
        );

        // Unmoved: the fetched value is merged into the catalog.
        assert_eq!(
            persisted.get("unmoved/pkg").map(String::as_str),
            Some("sha256:oldunmoved"),
            "an unmoved (listing-row) package adopts its fetched catalog value"
        );
    }

    // ── namespace isolation ──────────────────────────────────────────────────

    #[tokio::test]
    async fn identifier_in_other_registry_is_not_this_sources_concern() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let source = make_source(transport.clone(), false);

        let foreign = oci::Identifier::new_registry("cmake", "ghcr.io").clone_with_tag("3.28");
        assert!(
            source
                .fetch_manifest(&foreign, IndexOperation::Resolve)
                .await
                .unwrap()
                .is_none()
        );
        assert!(source.list_tags(&foreign).await.unwrap().is_none());
        assert!(
            source.list_repositories("ghcr.io").await.unwrap().is_empty(),
            "a foreign registry must not trigger a catalog fetch"
        );
        assert!(
            !transport.request_urls().iter().any(|url| url.contains("ghcr.io")),
            "a foreign identifier must not reach the index transport at all"
        );
    }

    // ── TLS: bundled roots (BLOCK) ────────────────────────────────────────────

    #[test]
    fn index_http_client_seeds_bundled_roots() {
        // The full Mozilla set is present and every root converts to a reqwest
        // Certificate — the non-empty root set is what keeps reqwest off the
        // empty-store platform verifier (the `No CA certificates were loaded`
        // panic on minimal containers). Mirrors the OCI builder's root test.
        let total = webpki_root_certs::TLS_SERVER_ROOT_CERTS.len();
        assert!(total > 100, "expected the full Mozilla root set, got {total}");
        let converted = webpki_root_certs::TLS_SERVER_ROOT_CERTS
            .iter()
            .filter(|root| reqwest::Certificate::from_der(root.as_ref()).is_ok())
            .count();
        assert_eq!(
            converted, total,
            "every bundled root must convert to a reqwest Certificate"
        );
        // Construction (which runs the seeding) must not panic.
        let _client = build_index_http_client();
    }

    // ── chain ordering: index BEFORE registry (HIGH, Codex R3) ────────────────

    /// Seed config + root (`tag` → an empty-platform observation) + observation
    /// at `repo`, returning the observation digest. Empty platforms keep the
    /// persist recursion off the physical (OCI client) path.
    fn seed_empty_obs(transport: &StubIndexTransport, repo: &str, tag: &str) -> oci::Digest {
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let obs_bytes = br#"{"platforms":[]}"#;
        let obs_digest = Algorithm::Sha256.hash(obs_bytes);
        let root = format!(r#"{{"repository":"oci://ghcr.io/x/y","tags":{{"{tag}":{{"content":"{obs_digest}"}}}}}}"#,);
        transport.insert(&format!("{BASE}/p/{repo}.json"), root.as_bytes());
        transport.insert(
            &format!(
                "{BASE}/p/{repo}/o/{}/{}.json",
                obs_digest.algorithm().prefix(),
                obs_digest.hex()
            ),
            obs_bytes,
        );
        obs_digest
    }

    /// Pre-seed `repo` as a MATERIALIZED local root carrying a stale digest, so a
    /// differing remote catalog entry counts as a MOVE of an existing local root.
    /// Under the corrected F2 listing-row contract, `sync_catalog` re-snapshots
    /// ONLY already-materialized packages whose digest changed — a package with
    /// no local root is a listing row, updated in the catalog without a fetch — so
    /// a test that wants to observe a re-snapshot must first materialize the
    /// package. The stale root's own bytes are overwritten by the fetched root on
    /// re-snapshot; only its existence and its (differing) catalog entry matter.
    async fn seed_stale_root(snapshot: &crate::file_structure::IndexStore, repo: &str) {
        let stale = br#"{"repository":"oci://ghcr.io/stale/root","tags":{}}"#;
        let mut transaction = snapshot.begin_catalog_transaction(NAMESPACE).await.unwrap();
        transaction.write_root(repo, stale, |_| Ok(())).await.unwrap();
        transaction.commit(None).await.unwrap();
    }

    fn registry_manifest() -> (Vec<u8>, oci::Digest) {
        let manifest = oci::Manifest::Image(oci::ImageManifest::default());
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let digest = Algorithm::Sha256.hash(&bytes);
        (bytes, digest)
    }

    /// A registry-style source that answers ANY tag with a fixed manifest, and
    /// counts calls — stands in for a registry that also serves `ocx.sh/...`.
    #[derive(Clone)]
    struct RegistryStub {
        calls: Arc<Mutex<usize>>,
    }

    impl RegistryStub {
        fn new() -> Self {
            Self {
                calls: Arc::new(Mutex::new(0)),
            }
        }
        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl IndexImpl for RegistryStub {
        async fn list_repositories(&self, _: &str) -> Result<Vec<String>> {
            *self.calls.lock().unwrap() += 1;
            Ok(Vec::new())
        }
        async fn list_tags(&self, _: &oci::Identifier) -> Result<Option<Vec<String>>> {
            *self.calls.lock().unwrap() += 1;
            Ok(Some(vec!["1.0".to_string()]))
        }
        async fn fetch_manifest(
            &self,
            id: &oci::Identifier,
            _: IndexOperation,
        ) -> Result<Option<(oci::Digest, oci::Manifest)>> {
            Ok(self
                .fetch_manifest_raw_bytes(id)
                .await?
                .map(|(_, digest, m)| (digest, m)))
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
        ) -> Result<Option<(Vec<u8>, oci::Digest, oci::Manifest)>> {
            *self.calls.lock().unwrap() += 1;
            let (bytes, digest) = registry_manifest();
            Ok(Some((
                bytes,
                digest,
                oci::Manifest::Image(oci::ImageManifest::default()),
            )))
        }
        fn box_clone(&self) -> Box<dyn IndexImpl> {
            Box::new(self.clone())
        }
    }

    fn local_index(dir: &tempfile::TempDir) -> super::super::LocalIndex {
        super::super::LocalIndex::new(super::super::LocalConfig {
            index_store: crate::file_structure::IndexStore::new(dir.path().join("index")),
        })
    }

    #[tokio::test]
    async fn chained_index_first_resolves_ocx_sh_through_index() {
        let transport = StubIndexTransport::new();
        let obs_digest = seed_empty_obs(&transport, "ns/pkg", "1.0");
        let source = make_source(transport, false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        // Index registered FIRST, registry second — the production order.
        let chained = super::super::Index::from_chained(
            local_index(&dir),
            vec![
                super::super::Index::from_source(source),
                super::super::Index::from_impl(registry.clone()),
            ],
            super::super::ChainMode::Default,
        );

        let id = oci::Identifier::new_registry("ns/pkg", NAMESPACE).clone_with_tag("1.0");
        let (digest, _) = chained
            .fetch_manifest(&id, IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("the ocx.sh package resolves");
        assert_eq!(
            digest, obs_digest,
            "an ocx.sh package must resolve through the verified index, not the registry"
        );
        assert_eq!(
            registry.calls(),
            0,
            "the registry must not be consulted once the index answers (index-first)"
        );
    }

    // ── GAP 1: physical transport reference (C2) ─────────────────────────────

    #[tokio::test]
    async fn physical_reference_dereferences_root_for_own_namespace_only() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let source = make_source(transport, false);

        // Own-namespace leaf → the physical location the root's `repository`
        // points at, with the leaf digest carried over (transport-only, C2).
        let leaf = oci::Digest::Sha256("a".repeat(64));
        let logical = oci::Identifier::new_registry(REPO, NAMESPACE).clone_with_digest(leaf.clone());
        let physical = source
            .physical_reference(&logical)
            .await
            .unwrap()
            .expect("an own-namespace reference maps to a physical location");
        assert_eq!(physical.registry(), "ghcr.io");
        assert_eq!(physical.repository(), "ocx-contrib/cmake");
        assert_eq!(physical.digest(), Some(leaf));
        assert!(source.is_authoritative_for(&logical), "the source owns its namespace");

        // A foreign namespace is neither rewritten nor owned.
        let foreign =
            oci::Identifier::new_registry("x/y", "ghcr.io").clone_with_digest(oci::Digest::Sha256("b".repeat(64)));
        assert!(source.physical_reference(&foreign).await.unwrap().is_none());
        assert!(!source.is_authoritative_for(&foreign));
    }

    // ── GAP 2: authoritative refusal stops the chain (Codex R4) ───────────────

    #[tokio::test]
    async fn chained_index_stops_at_authoritative_yank_refusal() {
        // A yanked package (no opt-in) + a registry that WOULD answer the same
        // ocx.sh name. Index-first + authoritative-stop means the refusal wins.
        let transport = StubIndexTransport::new();
        seed_package(&transport, true);
        let source = make_source(transport, false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = super::super::Index::from_chained(
            local_index(&dir),
            vec![
                super::super::Index::from_source(source),
                super::super::Index::from_impl(registry.clone()),
            ],
            super::super::ChainMode::Default,
        );

        let result = chained.fetch_manifest(&tagged_id(), IndexOperation::Resolve).await;
        assert!(
            result.is_err(),
            "an authoritative yank refusal must stop the chain, never resolve via the registry"
        );
        assert!(
            result.unwrap_err().to_string().contains("yanked"),
            "the yank refusal must be the surfaced error"
        );
        assert_eq!(
            registry.calls(),
            0,
            "the registry must never be consulted after an authoritative refusal"
        );
    }

    #[tokio::test]
    async fn chained_index_stops_at_authoritative_config_transport_failure() {
        // A dead index (config.json transport failure) is now a hard error —
        // the old per-source soft-miss/fallthrough cache is deleted (config-
        // driven construction means a configured index host is expected to
        // answer). An authoritative source's hard error must stop the chain,
        // exactly like the yank-refusal case, never fall through to the
        // registry.
        let transport = StubIndexTransport::new();
        transport.fail(&config_url());
        let source = make_source(transport, false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = super::super::Index::from_chained(
            local_index(&dir),
            vec![
                super::super::Index::from_source(source),
                super::super::Index::from_impl(registry.clone()),
            ],
            super::super::ChainMode::Default,
        );

        let result = chained.fetch_manifest(&tagged_id(), IndexOperation::Resolve).await;
        assert!(
            result.is_err(),
            "a dead index's transport failure must be a hard error, not a soft miss"
        );
        assert_eq!(
            registry.calls(),
            0,
            "the registry must never be consulted after an authoritative hard error"
        );
    }

    #[tokio::test]
    async fn chained_index_first_leaves_foreign_registry_to_the_registry_source() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = make_source(transport.clone(), false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = super::super::Index::from_chained(
            local_index(&dir),
            vec![
                super::super::Index::from_source(source),
                super::super::Index::from_impl(registry.clone()),
            ],
            super::super::ChainMode::Default,
        );

        // A foreign registry: the index returns None (namespace isolation), so
        // the registry source resolves it — index-first must not break this.
        let foreign = oci::Identifier::new_registry("x/y", "ghcr.io").clone_with_tag("1.0");
        let (digest, _) = chained
            .fetch_manifest(&foreign, IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("a foreign package resolves via the registry");
        assert_eq!(
            digest,
            registry_manifest().1,
            "a foreign package must resolve via the registry"
        );
        assert!(
            registry.calls() > 0,
            "the registry must be consulted for a foreign namespace"
        );
        assert!(
            !transport.request_urls().iter().any(|url| url.contains("ghcr.io")),
            "the index transport must never fetch anything for a foreign registry"
        );
    }

    // ── config.json check: no probing, no soft-miss ───────────────────────────

    #[tokio::test]
    async fn config_transport_failure_is_a_hard_error_every_call() {
        // The config endpoint FAILS at the transport layer. This is a hard
        // error on every call — a configured index host is expected to
        // answer, so there is no soft "maybe not an index yet" outcome to
        // absorb the failure into, and no cached verdict to short-circuit a
        // retry.
        let transport = StubIndexTransport::new();
        transport.fail(&config_url());
        let source = make_source(transport.clone(), false);

        let first = oci::Identifier::new_registry("a/one", NAMESPACE).clone_with_tag("1.0");
        let second = oci::Identifier::new_registry("b/two", NAMESPACE).clone_with_tag("1.0");
        assert!(
            source.fetch_manifest(&first, IndexOperation::Resolve).await.is_err(),
            "a dead index must be a hard error, never a silent soft miss"
        );
        assert!(
            source.fetch_manifest(&second, IndexOperation::Resolve).await.is_err(),
            "a second resolve against the same dead index must also hard-error"
        );

        assert_eq!(
            transport.request_count(&config_url()),
            2,
            "an unconfirmed config check is never cached — every call re-attempts"
        );
    }

    #[tokio::test]
    async fn unsupported_format_version_is_never_cached_and_rechecked_every_call() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":2}"#);
        let source = make_source(transport.clone(), false);

        let id = tagged_id();
        assert!(
            source.fetch_manifest(&id, IndexOperation::Resolve).await.is_err(),
            "an unknown format_version must fail closed (F1), not soften to a miss"
        );
        // Second call still errors, and re-fetches config.json — an unsupported
        // version is never cached as a steady state (only a confirmed-supported
        // version is), so a fixed deploy is picked up without restarting.
        assert!(source.fetch_manifest(&id, IndexOperation::Resolve).await.is_err());
        assert_eq!(
            transport.request_count(&config_url()),
            2,
            "an unsupported format_version verdict is never cached — every call re-checks"
        );
    }

    #[tokio::test]
    async fn config_format_version_check_runs_once_on_success() {
        // A confirmed-supported `config.json` IS cached (F1 "read once") via
        // a plain cached bool on the shared cache — a repeat resolve for a
        // different package skips the re-fetch.
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let second_obs = glibc_musl_observation();
        let second_digest = Algorithm::Sha256.hash(second_obs);
        let second_root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/other","tags":{{"1.0":{{"content":"{second_digest}"}}}}}}"#,
        );
        transport.insert(&format!("{BASE}/p/other/pkg.json"), second_root.as_bytes());
        transport.insert(
            &format!(
                "{BASE}/p/other/pkg/o/{}/{}.json",
                second_digest.algorithm().prefix(),
                second_digest.hex()
            ),
            second_obs,
        );
        let source = make_source(transport.clone(), false);

        let second_id = oci::Identifier::new_registry("other/pkg", NAMESPACE).clone_with_tag("1.0");
        assert!(
            source
                .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            source
                .fetch_manifest(&second_id, IndexOperation::Resolve)
                .await
                .unwrap()
                .is_some()
        );

        assert_eq!(
            transport.request_count(&config_url()),
            1,
            "a confirmed-supported format_version is cached — no re-fetch across packages"
        );
    }

    // ── ETag: LocalIndex catalog sync sends If-None-Match, handles 304 (item 5) ─

    #[tokio::test]
    async fn local_index_sync_catalog_persists_etag_and_honors_304() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert_with_etag(&catalog_url(), br#"{"ns/pkg":"sha256:root"}"#, "etag-1");
        // Serve the moved package so the first sync can re-snapshot it.
        seed_empty_obs(&transport, "ns/pkg", "1.0");
        let source = make_source(transport.clone(), false);

        let dir = tempfile::tempdir().unwrap();
        let local = local_index(&dir);
        let snapshot = crate::file_structure::IndexStore::new(dir.path().join("index"));
        // Materialize `ns/pkg` (stale digest) so the remote's newer digest is a
        // MOVE of an existing local root — the F2 shape that re-snapshots.
        seed_stale_root(&snapshot, "ns/pkg").await;

        // First sync: full GET, persists catalog + ETag, re-snapshots the package.
        let first = local.sync_catalog(&source).await.unwrap();
        assert_eq!(first.moved, vec!["ns/pkg".to_string()]);
        assert_eq!(
            snapshot.read_source_catalog_etag(NAMESPACE).await.unwrap().as_deref(),
            Some("etag-1"),
            "the per-source ETag must be persisted for the next conditional GET"
        );

        // Second sync: the persisted ETag is sent; the stub returns 304 → no
        // re-snapshot, catalog carried forward unchanged.
        let second = local.sync_catalog(&source).await.unwrap();
        assert!(second.unchanged, "a matching ETag must yield a 304 unchanged sync");
        assert!(second.moved.is_empty(), "a 304 sync must re-snapshot nothing");
        assert!(
            transport
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(url, inm)| url == &catalog_url() && inm.as_deref() == Some("etag-1")),
            "the second catalog sync must send If-None-Match with the persisted ETag"
        );
    }

    /// A `W/"..."`-prefixed weak ETag round-trips `sync_catalog` opaquely: OCX
    /// never parses or normalizes the validator (strong vs weak comparison is
    /// an HTTP semantics distinction OCX does not implement), it is stored and
    /// echoed back verbatim in the next `If-None-Match`.
    #[tokio::test]
    async fn sync_catalog_round_trips_weak_etag_opaquely() {
        const WEAK_ETAG: &str = "W/\"abc123\"";

        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert_with_etag(&catalog_url(), br#"{"ns/pkg":"sha256:root"}"#, WEAK_ETAG);
        seed_empty_obs(&transport, "ns/pkg", "1.0");
        let source = make_source(transport.clone(), false);

        let dir = tempfile::tempdir().unwrap();
        let local = local_index(&dir);
        let snapshot = crate::file_structure::IndexStore::new(dir.path().join("index"));
        // Materialize `ns/pkg` (stale digest) so the remote's newer digest is a
        // MOVE of an existing local root — the F2 shape that re-snapshots.
        seed_stale_root(&snapshot, "ns/pkg").await;

        let first = local.sync_catalog(&source).await.unwrap();
        assert_eq!(first.moved, vec!["ns/pkg".to_string()]);
        assert_eq!(
            snapshot.read_source_catalog_etag(NAMESPACE).await.unwrap().as_deref(),
            Some(WEAK_ETAG),
            "a weak validator must be persisted verbatim, `W/` prefix and quotes intact"
        );

        // Second sync: the persisted weak ETag is echoed back exactly; the
        // stub matches it byte-for-byte and answers 304.
        let second = local.sync_catalog(&source).await.unwrap();
        assert!(second.unchanged, "a matching weak ETag must yield a 304 unchanged sync");
        assert!(
            transport
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(url, inm)| url == &catalog_url() && inm.as_deref() == Some(WEAK_ETAG)),
            "the second catalog sync must send If-None-Match with the weak ETag, unmodified"
        );
    }

    // ── fetch_root_document: verbatim published-root fetch (A2/F1, C1 stub) ───
    //
    // Specification tests for the `OcxIndex::fetch_root_document` override
    // (currently `unimplemented!()`): a published source serves the verbatim
    // `p/<ns>/<pkg>.json` bytes paired with the parsed root so
    // `LocalIndex::persist_published_root` can grow the local copy byte-for-byte
    // (copy-a-mirror). These are EXPECTED TO PANIC on the stub until C1 lands.

    #[tokio::test]
    async fn fetch_root_document_returns_verbatim_bytes_and_parsed_root() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        // Deliberately non-canonical whitespace so a re-serialization would
        // change the bytes: fetch_root_document must return them verbatim, since
        // the catalog entry is sha256 of these exact bytes (F1).
        let root_bytes = br#"{  "repository" : "oci://ghcr.io/ocx-contrib/cmake" ,  "tags" : { }  }"#.to_vec();
        transport.insert(&root_url(), &root_bytes);
        let source = make_source(transport.clone(), false);

        let (bytes, root) = source
            .fetch_root_document(&tagged_id())
            .await
            .unwrap()
            .expect("a published source serves the verbatim root document");
        assert_eq!(
            bytes, root_bytes,
            "the root bytes must be returned verbatim, never re-serialized (F1 catalog-entry integrity)"
        );
        assert_eq!(root.repository, "oci://ghcr.io/ocx-contrib/cmake");
        assert!(
            transport.request_urls().contains(&root_url()),
            "fetch_root_document must GET p/<ns>/<pkg>.json"
        );
    }

    #[tokio::test]
    async fn fetch_root_document_returns_none_when_root_absent() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        // No root registered — a 404 must be a clean miss, never an error.
        let source = make_source(transport, false);
        assert!(
            source.fetch_root_document(&tagged_id()).await.unwrap().is_none(),
            "an absent root document (404) must resolve to Ok(None)"
        );
    }

    #[tokio::test]
    async fn fetch_root_document_returns_none_for_foreign_namespace() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = make_source(transport, false);
        let foreign = oci::Identifier::new_registry(REPO, "other.io").clone_with_tag("3.28");
        assert!(
            source.fetch_root_document(&foreign).await.unwrap().is_none(),
            "a foreign-namespace identifier is not this source's concern"
        );
    }
}
