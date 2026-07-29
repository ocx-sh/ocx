// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `index.ocx.sh`-style static-file index source (`adr_index_indirection.md`
//! Decision F).
//!
//! A pointer index, **not** a registry: no `/v2`, no blobs. An index is a
//! catalog of OCI artifacts and defines no object shapes of its own, so a
//! logical reference resolves through two verified HTTP hops — a root document
//! that locks a floating tag, and the OCI image index that tag resolved to,
//! served back byte-for-byte as the registry produced it
//! (`adr_oci_index_only_dispatch.md` D1):
//!
//! ```text
//! logical id (ocx.sh/<ns>/<pkg>[:tag])
//!   → GET /p/<ns>/<pkg>.json                root  → tags[tag].content = image-index digest
//!   → GET /p/<ns>/<pkg>/o/sha256/<hex>.json index → VERIFY sha256(bytes)==hex
//!                                                 → manifests[] = OCI descriptors
//!   → select_best(host, platforms)          → leaf platform-manifest digest
//!   → root.repository (oci://…)             → physical fetch through the mirror seam
//! ```
//!
//! Only the ● frozen wire shapes in the ADR's Data Model are contract; the
//! image index's `manifests[].digest` leaves are already the doctrine-correct
//! platform-manifest digests, each OCI-CAS-verified when its manifest is
//! fetched from the physical registry (Decision D).
//!
//! ## Why the index and not the manifest is snapshotted
//!
//! A manifest is immutable by digest, and keeping it reachable is the registry
//! operator's and the publisher's concern. An **image index** has neither
//! property: adding a platform mints a new index, the tag moves to it, and the
//! previous index becomes unreferenced in the ordinary course of correct
//! publishing. So the thing that can disappear is the thing that is copied.
//!
//! ## Trust anchor
//!
//! The dispatch-object verify (`sha256(bytes) == <hex>`) is the one place OCX
//! re-derives a digest it did not mint, so it is the trust boundary of the
//! whole index path (F1). A mismatch is a hard
//! [`DataError`](crate::cli::ExitCode::DataError), never a silent load. The
//! bytes are publisher-controlled, so `annotations` and `artifactType` ride
//! through stored but never rendered.
//!
//! ## Snapshot integration
//!
//! This source is a live [`IndexImpl`](super::index_impl::IndexImpl) chain source
//! (like [`OciIndex`](super::OciIndex)). Its
//! [`fetch_manifest_raw_bytes`](OcxIndex::fetch_manifest_raw_bytes) returns the
//! verbatim image-index bytes (which hash to its digest, A3-valid) paired with
//! the parsed index, so [`LocalIndex::persist_dispatch`](super::LocalIndex)
//! writes them under that digest as the dispatch object — the physical
//! platform-manifest leaf it names is fetched on demand, never copied into the
//! local index (A3/B2). The read-back (`decode_index_manifest` in
//! `local_index`) is the same single OCI parse, so a hosted index subtree
//! copy-pasted into a machine's local index just works.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::RwLock;

use super::wire::{CatalogDocument, CatalogIndex, IndexRoot, RootTag, SUPPORTED_FORMAT_VERSION};
use super::{IndexOperation, index_impl};
use crate::{Result, log, oci};

// ── Frozen wire shapes (● contract) ──────────────────────────────────────────
//
// `IndexRoot` / `RootTag` / `CatalogDocument` / `CatalogIndex` and the shared
// `SUPPORTED_FORMAT_VERSION` pin live in `oci::index::wire`
// (`adr_index_indirection.md` §Data Model) — the frozen grammar shared
// verbatim by this remote client and the local store
// (`crate::file_structure::IndexStore`), imported above. What a tag points at
// is an `oci::ImageIndex`, whose shape is the OCI image spec's, not ours.

/// `config.json` — the version pin (● `{"format_version": 1}`).
///
/// Read once per source; an unknown `format_version` is a hard error
/// (fail-closed, F1). Forward-compatible: unknown sibling fields are ignored,
/// so a newer deploy never bricks an older binary reading the same file.
#[derive(Debug, Clone, Deserialize)]
pub struct IndexFormatConfig {
    pub format_version: u64,
}

use crate::oci::client::MAX_INDEX_DOCUMENT_BYTES;

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
/// same set from the `oci-client` fork's `ClientConfig::default()` (a
/// different client type, so it cannot share a builder with this one); the
/// root-seeding loop itself is shared with `forge::github`'s bare-`reqwest`
/// client via [`crate::utility::tls::seed_embedded_roots`].
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
    let builder = crate::utility::tls::seed_embedded_roots(harden(reqwest::Client::builder()));
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
    // above would otherwise wave through. Host *allowlisting* (which hosts may
    // appear in roots at all) stays index-side governance (X4); the private-IP /
    // SSRF floor is enforced at deref time by [`OcxIndex::physical_identifier`]
    // via [`oci::ssrf::resolve_and_validate`] (ocx#218).
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
/// dispatch → physical hops all need the same root). This is the same "shared
/// cache across clones" model [`OciIndex`](super::OciIndex) uses — it is not
/// the committed local index.
#[derive(Default)]
struct SourceCacheInner {
    /// repository → root document, `None` for a confirmed 404. The negative
    /// entry is what keeps [`OcxIndex::jurisdiction`]'s miss probe from
    /// re-asking the wire once per chain consult: a flat name costs exactly one
    /// 404 per process, not one per source loop.
    roots: BTreeMap<String, Option<Arc<IndexRoot>>>,
    /// Set once `config.json` has been fetched and its `format_version`
    /// confirmed supported this invocation, so a repeat call skips the fetch
    /// (F1 "read once"). Never set on a served-but-unsupported version (a
    /// re-checked hard error, not a remembered steady state) NOR on an absent
    /// `config.json` (a re-checked inert state — a fixed deploy that later
    /// serves `config.json` is picked up without restarting). Config-driven
    /// construction means there is no probe outcome to soften a transport
    /// failure into — that always propagates.
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
    /// Built with an SSRF [`GuardedResolver`](oci::ssrf::GuardedResolver) so the
    /// connect pins the address `physical_identifier` validated (resolve ->
    /// validate -> pin).
    client: oci::Client,
    /// When false, a tag resolving to a yanked entry is refused (F3).
    allow_yanked: bool,
    /// SSRF escape hatch for this namespace: hosts / CIDRs whose resolved
    /// addresses skip the default-on private/loopback/link-local/metadata
    /// refusal (`[registries."<ns>"].trusted_hosts`, X2).
    trusted_hosts: Vec<String>,
    cache: Arc<RwLock<SourceCacheInner>>,
}

/// Construction inputs for [`OcxIndex::new`].
pub struct OcxIndexConfig {
    pub transport: Box<dyn IndexTransport>,
    pub base_url: String,
    pub namespace: String,
    pub client: oci::Client,
    pub allow_yanked: bool,
    /// SSRF escape hatch for the physical hosts this source dereferences
    /// (`[registries."<ns>"].trusted_hosts`, X2). Empty = guard every host.
    pub trusted_hosts: Vec<String>,
}

impl OcxIndex {
    pub fn new(config: OcxIndexConfig) -> Self {
        Self {
            transport: config.transport,
            base_url: config.base_url.trim_end_matches('/').to_string(),
            namespace: config.namespace,
            client: config.client,
            allow_yanked: config.allow_yanked,
            trusted_hosts: config.trusted_hosts,
            cache: Arc::new(RwLock::new(SourceCacheInner::default())),
        }
    }

    /// The logical registry this source serves (e.g. `"ocx.sh"`).
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Whether `registry` is the one this source serves — a cheap, no-I/O
    /// ownership test, and the single predicate behind every read-path guard
    /// below.
    ///
    /// Distinct from [`Self::jurisdiction`]: ownership is per-**registry** and
    /// decides local-subtree layout (the `c/index.json` catalog vs `p/`
    /// enumeration), which is per-source and never per-name. Jurisdiction is
    /// per-**name** and needs the published `config.json`.
    pub fn serves_registry(&self, registry: &str) -> bool {
        registry == self.namespace
    }

    /// Whether this source will answer for `identifier`, and what its silence
    /// means — a configured index is **authoritative for its whole registry**
    /// (ocx#251).
    ///
    /// | Case | Verdict |
    /// |---|---|
    /// | Foreign registry | [`Outside`](super::Jurisdiction::Outside) |
    /// | This source's own registry | [`Authoritative`](super::Jurisdiction::Authoritative) |
    ///
    /// [`Outside`](super::Jurisdiction::Outside) now means only "another
    /// registry entirely". A name this index holds no root for is a **hard
    /// miss**, never a silent hand-off to the plain OCI registry the index
    /// points at — that hand-off is what let a flat `ocx.sh/<tool>` name resolve
    /// past the index, and with it past the yank and deprecation gate.
    ///
    /// Decided with **no I/O** — hence synchronous, where this used to be
    /// `async`: the verdict is `identifier.registry()` against this source's
    /// namespace and nothing else. There was a published
    /// declaration (`config.json`'s `name_segments`) whose only job was to
    /// interpret a missing root as "fall through"; with the fall-through gone it
    /// bought a config fetch and a 404 per declined name and decided nothing, so
    /// the client no longer reads it.
    ///
    /// Fail-closed is preserved where it matters: an absent, malformed,
    /// unsupported or unreachable `config.json` cannot change this verdict, and
    /// the [`resolve_root`](Self::resolve_root) that follows raises the real
    /// `UnsupportedIndexFormat` / transport error — an index outage stays a loud
    /// error and never degrades into "this package does not exist".
    pub fn jurisdiction(&self, identifier: &oci::Identifier) -> super::Jurisdiction {
        if self.serves_registry(identifier.registry()) {
            super::Jurisdiction::Authoritative
        } else {
            super::Jurisdiction::Outside
        }
    }

    /// This source's own SSRF escape hatch (`[registries."<ns>"].trusted_hosts`,
    /// X2) — read-only accessor over already-public construction input
    /// ([`OcxIndexConfig::trusted_hosts`]), so callers can confirm a built
    /// source carries exactly its own namespace's set and never another's.
    pub fn trusted_hosts(&self) -> &[String] {
        &self.trusted_hosts
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
    /// miss — memoized like a hit, so a repeat ask costs nothing.
    ///
    /// # Errors
    ///
    /// [`Error::IndexHttpFailed`](super::error::Error::IndexHttpFailed) when the
    /// endpoint answers `304` to this unconditional `GET` — see the arm below
    /// for why that must not read as a miss.
    async fn resolve_root(&self, repository: &str) -> Result<Option<Arc<IndexRoot>>> {
        // An absent config.json makes the base a non-index — no root resolves
        // (fail-closed, F1), never a pass that consumes a valid-looking root.
        // Deliberately NOT memoized: the config itself is re-checked every call.
        if let FormatVersionState::NotAnIndex = self.check_format_version().await? {
            return Ok(None);
        }
        if let Some(root) = self.cache.read().await.roots.get(repository) {
            return Ok(root.clone());
        }
        let url = format!("{}/p/{}.json", self.base_url, repository);
        let root = match self.transport.get(&url, None).await? {
            IndexFetch::Found { bytes, .. } => {
                let parsed: IndexRoot = parse_document(&bytes, &url)?;
                Some(Arc::new(parsed))
            }
            IndexFetch::NotFound => None,
            // The request carried no `If-None-Match`, so a `304` answers a
            // question nobody asked (RFC 9110 §15.4.5) — a misbehaving CDN, not
            // a confirmed absence. It must NOT fold into the 404 miss: this
            // `None` is what [`Self::jurisdiction`] reads an `Outside` verdict
            // off, and the whole design rests on only a *confirmed* 404 being
            // able to hand a name to plain OCI. The miss is memoized too, so one
            // bad response would decide the name for the rest of the process.
            IndexFetch::NotModified => {
                return Err(super::error::Error::IndexHttpFailed {
                    url: redact_url(&url),
                    source: "304 Not Modified answered an unconditional GET".into(),
                }
                .into());
            }
        };
        self.cache
            .write()
            .await
            .roots
            .insert(repository.to_string(), root.clone());
        Ok(root)
    }

    // ── dispatch object (F1 immutable, VERIFIED) ─────────────────────────────

    /// Fetches and verifies the dispatch object for `(repository, digest)` —
    /// the OCI image index the tag resolved to — returning its verbatim bytes
    /// alongside the parsed index.
    ///
    /// Verifies `sha256(bytes) == digest` before parsing — the index path's
    /// trust anchor (F1). The bytes travel with the parsed form because they,
    /// not a re-serialisation of it, are what the local copy stores: an
    /// unmodelled key a newer writer added must survive the round trip
    /// (`adr_oci_index_only_dispatch.md` A4). `Ok(None)` on a 404 miss.
    ///
    /// # Errors
    ///
    /// [`Error::DispatchObjectDigestMismatch`](super::error::Error::DispatchObjectDigestMismatch)
    /// when the served bytes do not hash to the digest the root claimed;
    /// [`Error::MalformedIndexDocument`](super::error::Error::MalformedIndexDocument)
    /// when they are not an OCI image index.
    async fn resolve_index_object(
        &self,
        repository: &str,
        digest: &oci::Digest,
    ) -> Result<Option<(Vec<u8>, oci::ImageIndex)>> {
        let url = format!(
            "{}/p/{}/o/{}/{}.json",
            self.base_url,
            repository,
            digest.algorithm().prefix(),
            digest.hex()
        );
        let bytes = match self.transport.get(&url, None).await? {
            IndexFetch::Found { bytes, .. } => bytes,
            IndexFetch::NotFound | IndexFetch::NotModified => return Ok(None),
        };

        // Trust boundary: re-derive the digest OCX did not mint and compare.
        let computed = digest.algorithm().hash(&bytes);
        if &computed != digest {
            return Err(super::error::Error::DispatchObjectDigestMismatch {
                claimed: digest.clone(),
                computed,
            }
            .into());
        }

        // Admission is on document KIND and image-spec semantics: this must be
        // an image index, and a valid one — deserialisation proves shape only
        // (`schemaVersion` is an unconstrained `u8`). It deliberately does NOT
        // inspect `artifactType` — nothing in ocx reads an image index's
        // artifact type, and gating on it would refuse or warn about documents
        // that are structurally exactly what we asked for.
        let index: oci::ImageIndex = parse_document(&bytes, &url)?;
        crate::oci::manifest::validate_image_index(&index).map_err(super::error::Error::from)?;
        Ok(Some((bytes, index)))
    }

    /// Surfaces the human-governed status lane (F3) for a live tag resolve —
    /// delegates to the shared [`surface_root_status`] with this source's
    /// `allow_yanked` opt-in. Called on the tag path only; a digest-pinned
    /// resolve skips it (immutability).
    fn surface_status(&self, identifier: &oci::Identifier, root: &IndexRoot, tag: &RootTag) -> Result<()> {
        surface_root_status(identifier, root, tag, self.allow_yanked)
    }

    /// Resolves a tag-addressed identifier to its dispatch object: root → tag →
    /// status surfacing → content digest → fetch + verify.
    ///
    /// `Ok(None)` when the package or tag is absent. Errors on a yanked refusal
    /// or a dispatch-object digest mismatch.
    async fn resolve_tag(&self, identifier: &oci::Identifier) -> Result<Option<(oci::Digest, oci::ImageIndex)>> {
        let Some(root) = self.resolve_root(identifier.repository()).await? else {
            return Ok(None);
        };
        let tag = identifier.tag_or_latest();
        let Some(tag_entry) = root.tags.get(tag) else {
            return Ok(None);
        };
        self.surface_status(identifier, &root, tag_entry)?;

        let content = tag_entry.content.clone();
        let Some((_, index)) = self.resolve_index_object(identifier.repository(), &content).await? else {
            return Ok(None);
        };
        Ok(Some((content, index)))
    }

    /// Builds the physical [`oci::Identifier`] for `identifier` by dereferencing
    /// the root's `repository` pointer. The logical tag/digest are copied onto
    /// the physical location; the physical value is transport-only routing (C2).
    async fn physical_identifier(&self, identifier: &oci::Identifier) -> Result<Option<oci::Identifier>> {
        let Some(root) = self.resolve_root(identifier.repository()).await? else {
            return Ok(None);
        };
        let (registry, repository) = parse_physical_repository(&root.repository)?;
        // SSRF floor (X1-X3, ocx#218): `registry` is a host from remote-controlled
        // index data, so resolve + validate it against the private/loopback/
        // link-local/metadata ranges BEFORE the first physical registry request
        // (`self.client.*` in every caller). `trusted_hosts` is the explicit
        // per-namespace escape hatch. `self.client` additionally pins the
        // validated address at connect time via its `GuardedResolver`, closing
        // the resolve -> connect rebinding window. The resolved addresses are
        // discarded here — the pin, not this pre-flight, drives the connection.
        let (host, port) = oci::ssrf::split_host_port(&registry);
        oci::ssrf::resolve_and_validate(host, port, &self.trusted_hosts)
            .await
            .map_err(super::error::Error::from)?;
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
                let document: CatalogDocument = parse_document(&bytes, &url)?;
                let fetched = document.into_packages()?;
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
    let yanked = tag.yanked.is_some() || root.status.as_deref() == Some("yanked");
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
        if !self.serves_registry(identifier.registry()) {
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
        if !self.serves_registry(identifier.registry()) {
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

        // Tag-addressed: resolve the image index the tag was locked to and hand
        // it straight to `fetch_candidates` / `select_best`.
        let Some((content, index)) = self.resolve_tag(identifier).await? else {
            return Ok(None);
        };
        Ok(Some((content, oci::Manifest::ImageIndex(index))))
    }

    async fn fetch_manifest_digest(
        &self,
        identifier: &oci::Identifier,
        _op: IndexOperation,
    ) -> Result<Option<oci::Digest>> {
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }
        // A digest-addressed identifier already names its manifest digest.
        if let Some(digest) = identifier.digest() {
            return Ok(Some(digest));
        }
        // Tag-addressed: the dispatch-object digest is what the tag points at.
        Ok(self.resolve_tag(identifier).await?.map(|(digest, _)| digest))
    }

    async fn fetch_blob(&self, blob_ref: &oci::PinnedIdentifier) -> Result<Option<Vec<u8>>> {
        if !self.serves_registry(blob_ref.as_identifier().registry()) {
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
        if !self.serves_registry(identifier.registry()) {
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

        // Tag: the verbatim image-index bytes (which hash to the dispatch-object
        // digest — A3-valid) paired with the parsed index. The persist layer
        // writes the BYTES, not a re-serialisation of the parse, so a key this
        // client does not model survives into the local copy.
        let Some(root) = self.resolve_root(identifier.repository()).await? else {
            return Ok(None);
        };
        let tag = identifier.tag_or_latest();
        let Some(tag_entry) = root.tags.get(tag) else {
            return Ok(None);
        };
        self.surface_status(identifier, &root, tag_entry)?;
        let content = tag_entry.content.clone();
        let Some((bytes, index)) = self.resolve_index_object(identifier.repository(), &content).await? else {
            return Ok(None);
        };
        Ok(Some((bytes, content, oci::Manifest::ImageIndex(index))))
    }

    async fn fetch_root_document(&self, identifier: &oci::Identifier) -> Result<Option<(Vec<u8>, IndexRoot)>> {
        // A published source serves the verbatim `p/<ns>/<pkg>.json` bytes paired
        // with the parsed root, so `LocalIndex::persist_published_root` grows the
        // local copy byte-for-byte (copy-a-mirror, A2). The bytes are returned
        // verbatim (never re-serialized) so they hash to the catalog entry (F1).
        if !self.serves_registry(identifier.registry()) {
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
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }
        // Dereference the root's `repository` pointer, carrying the logical
        // digest onto the physical location — transport-only (C2).
        self.physical_identifier(identifier).await
    }

    fn jurisdiction(&self, identifier: &oci::Identifier) -> super::Jurisdiction {
        // Forwards to the inherent method (same shape as `namespace()`) so the
        // one caller that holds a concrete `OcxIndex` — `ocx index update`'s
        // source routing — reaches it without the private trait.
        OcxIndex::jurisdiction(self, identifier)
    }

    fn serves_registry(&self, registry: &str) -> bool {
        OcxIndex::serves_registry(self, registry)
    }

    fn index_base_url(&self) -> Option<&str> {
        Some(&self.base_url)
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
    use super::super::wire::YankMarker;
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
        /// URLs that answer `304` to EVERY request, conditional or not —
        /// a misbehaving CDN edge.
        not_modified: Arc<Mutex<std::collections::HashSet<String>>>,
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

        fn always_not_modified(&self, url: &str) {
            self.not_modified.lock().unwrap().insert(url.to_string());
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
            if self.not_modified.lock().unwrap().contains(url) {
                return Ok(IndexFetch::NotModified);
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
        make_source_with(transport, allow_yanked, stub_client(), Vec::new())
    }

    /// Like [`make_source`] but with an explicit physical-fetch client and
    /// `trusted_hosts` — used by the SSRF read-path tests.
    fn make_source_with(
        transport: StubIndexTransport,
        allow_yanked: bool,
        client: oci::Client,
        trusted_hosts: Vec<String>,
    ) -> OcxIndex {
        OcxIndex::new(OcxIndexConfig {
            transport: Box::new(transport),
            base_url: BASE.to_string(),
            namespace: NAMESPACE.to_string(),
            client,
            allow_yanked,
            trusted_hosts,
        })
    }

    fn config_url() -> String {
        format!("{BASE}/config.json")
    }
    fn root_url() -> String {
        format!("{BASE}/p/{REPO}.json")
    }
    fn dispatch_url(digest: &oci::Digest) -> String {
        format!(
            "{BASE}/p/{REPO}/o/{}/{}.json",
            digest.algorithm().prefix(),
            digest.hex()
        )
    }
    fn catalog_url() -> String {
        format!("{BASE}/c/index.json")
    }

    /// A served `c/index.json` body: `packages_json` (the bare `<ns>/<pkg>` →
    /// digest object) inside the format-version envelope the site serves.
    /// Stubs must speak the real wire — a bare map here would let the client
    /// drift back off the served shape unnoticed.
    fn catalog_body(packages_json: &str) -> Vec<u8> {
        format!(r#"{{"format_version":1,"packages":{packages_json}}}"#).into_bytes()
    }
    fn tagged_id() -> oci::Identifier {
        oci::Identifier::new_registry(REPO, NAMESPACE).clone_with_tag("3.28")
    }

    /// A two-platform OCI image index (glibc + musl leaves) as the verbatim
    /// bytes a registry served.
    ///
    /// **Deliberately NOT the canonical serde encoding of what it parses to.**
    /// It is pretty-printed and carries a `subject` field `oci::ImageIndex` does
    /// not model, so a re-serialising implementation cannot reproduce these
    /// bytes — which is the only way the verbatim-storage and digest-stability
    /// assertions downstream can fail when the property is broken.
    fn glibc_musl_index() -> &'static [u8] {
        concat!(
            "{\n",
            "  \"schemaVersion\": 2,\n",
            "  \"mediaType\": \"application/vnd.oci.image.index.v1+json\",\n",
            "  \"artifactType\": \"application/vnd.sh.ocx.package.v1\",\n",
            "  \"subject\": { \"mediaType\": \"application/vnd.oci.image.manifest.v1+json\", ",
            "\"digest\": \"sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\", \"size\": 7 },\n",
            "  \"manifests\": [\n",
            "    { \"mediaType\": \"application/vnd.oci.image.manifest.v1+json\", ",
            "\"digest\": \"sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", \"size\": 11, ",
            "\"platform\": { \"architecture\": \"amd64\", \"os\": \"linux\", \"os.features\": [\"libc.glibc\"] } },\n",
            "    { \"mediaType\": \"application/vnd.oci.image.manifest.v1+json\", ",
            "\"digest\": \"sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\", \"size\": 12, ",
            "\"platform\": { \"architecture\": \"amd64\", \"os\": \"linux\", \"os.features\": [\"libc.musl\"] } }\n",
            "  ]\n",
            "}\n"
        )
        .as_bytes()
    }

    /// Seeds config + root (tag `3.28` → dispatch object) + that image index,
    /// returning its digest. `yanked` toggles the per-tag marker
    /// (wire object `{"reason": "...", "at": "..."}`, omitted when `false`).
    fn seed_package(transport: &StubIndexTransport, yanked: bool) -> oci::Digest {
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let dispatch_bytes = glibc_musl_index();
        let dispatch_digest = Algorithm::Sha256.hash(dispatch_bytes);
        let yanked_field = if yanked {
            r#","yanked":{"reason":"critical security issue","at":"2026-02-01T00:00:00Z"}"#
        } else {
            ""
        };
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{dispatch_digest}"{yanked_field}}}}}}}"#
        );
        transport.insert(&root_url(), root.as_bytes());
        transport.insert(&dispatch_url(&dispatch_digest), dispatch_bytes);
        dispatch_digest
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
                r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","status":"deprecated","deprecated_message":"use 4.x","tags":{{"3.28":{{"content":"sha256:{}","observed":"2026-07-18T09:00:00Z","yanked":{{"reason":"critical security issue","at":"2026-02-01T00:00:00Z"}}}}}}}}"#,
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
        assert_eq!(
            tag.yanked,
            Some(YankMarker {
                reason: "critical security issue".to_string(),
                at: "2026-02-01T00:00:00Z".to_string(),
            })
        );

        let dispatch: oci::ImageIndex = serde_json::from_slice(glibc_musl_index()).unwrap();
        assert_eq!(dispatch.manifests.len(), 2);
        assert_eq!(
            dispatch.manifests[0]
                .platform
                .as_ref()
                .and_then(|platform| platform.os_features.as_deref()),
            Some(["libc.glibc".to_string()].as_slice())
        );
        assert_eq!(
            dispatch.artifact_type.as_deref(),
            Some("application/vnd.sh.ocx.package.v1"),
            "artifactType is stored and never rendered — but it must survive the parse"
        );

        let catalog: CatalogIndex = serde_json::from_slice::<CatalogDocument>(&catalog_body(
            r#"{"kitware/cmake":"sha256:root1","other/tool":"sha256:root2"}"#,
        ))
        .unwrap()
        .into_packages()
        .unwrap();
        assert_eq!(catalog.get("kitware/cmake").map(String::as_str), Some("sha256:root1"));
    }

    // ── select_best over dispatch platforms ──────────────────────────────────

    #[tokio::test]
    async fn resolve_tag_parses_index_and_select_picks_host_platform() {
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
                "glibc host must select the libc.glibc dispatch leaf"
            ),
            super::super::SelectResult::Ambiguous(_) => panic!("expected Found(glibc leaf), got Ambiguous"),
            super::super::SelectResult::NotFound => panic!("expected Found(glibc leaf), got NotFound"),
            super::super::SelectResult::FeatureMismatch { .. } => {
                panic!("expected Found(glibc leaf), got FeatureMismatch")
            }
        }
    }

    #[tokio::test]
    async fn fetch_manifest_returns_parsed_index_with_dispatch_digest() {
        let transport = StubIndexTransport::new();
        let dispatch_digest = seed_package(&transport, false);
        let source = make_source(transport, false);

        let (digest, manifest) = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("tag resolves");
        assert_eq!(
            digest, dispatch_digest,
            "the resolved digest is the dispatch-object digest (the CAS root)"
        );
        match manifest {
            oci::Manifest::ImageIndex(index) => assert_eq!(index.manifests.len(), 2),
            other => panic!("expected a parsed image index, got {other:?}"),
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

    // ── SSRF read-path guard (X1-X3, ocx#218) ────────────────────────────────

    /// Seeds config.json + a root whose `repository` points at
    /// `physical_repository` (e.g. `oci://127.0.0.1/x`), so a physical deref runs
    /// through the SSRF guard in `physical_identifier`.
    fn seed_root_pointing_at(transport: &StubIndexTransport, physical_repository: &str) {
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let root = format!(
            r#"{{"repository":"{physical_repository}","tags":{{"3.28":{{"content":"sha256:{}"}}}}}}"#,
            "a".repeat(64)
        );
        transport.insert(&root_url(), root.as_bytes());
    }

    /// A digest-addressed identifier in this source's namespace/repo — routes
    /// straight through `physical_identifier` to the physical fetch.
    fn digest_id() -> oci::Identifier {
        oci::Identifier::new_registry(REPO, NAMESPACE).clone_with_digest(oci::Digest::Sha256("b".repeat(64)))
    }

    /// X3 ordering + #218 regression: a root whose physical host resolves to a
    /// forbidden range is refused during `physical_identifier`, BEFORE the OCI
    /// client is ever touched. The recording transport proves no physical
    /// request was made.
    #[tokio::test]
    async fn ssrf_guard_refuses_forbidden_physical_host_before_any_transport_call() {
        let transport = StubIndexTransport::new();
        seed_root_pointing_at(&transport, "oci://127.0.0.1/x");

        let recorder = StubTransportData::new();
        let client = oci::Client::with_transport(Box::new(StubTransport::new(recorder.clone())));
        let source = make_source_with(transport, false, client, Vec::new());

        let error = source
            .fetch_manifest(&digest_id(), IndexOperation::Resolve)
            .await
            .expect_err("a forbidden physical host must be refused");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::Ssrf(
                    crate::oci::ssrf::SsrfError::ForbiddenTarget { .. }
                ))
            ),
            "expected an SSRF ForbiddenTarget refusal, got {error:?}"
        );
        assert!(
            recorder.read().calls.is_empty(),
            "the SSRF guard must fire before any physical registry request; recorded: {:?}",
            recorder.read().calls
        );
    }

    /// The cloud-metadata endpoint (169.254.169.254) is refused by default, and
    /// reachable only when the operator lists it in `trusted_hosts`. When trusted,
    /// the guard passes and the failure comes from the physical fetch itself
    /// (no seeded manifest), proving the request was let through, not refused.
    #[tokio::test]
    async fn ssrf_guard_allows_metadata_host_only_when_trusted() {
        let transport = StubIndexTransport::new();
        seed_root_pointing_at(&transport, "oci://169.254.169.254/x");
        let client = oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let refused = make_source_with(transport, false, client, Vec::new());
        assert!(
            matches!(
                refused.fetch_manifest(&digest_id(), IndexOperation::Resolve).await,
                Err(crate::Error::OciIndex(super::super::error::Error::Ssrf(_)))
            ),
            "the metadata endpoint must be refused by default"
        );

        let transport = StubIndexTransport::new();
        seed_root_pointing_at(&transport, "oci://169.254.169.254/x");
        let client = oci::Client::with_transport(Box::new(StubTransport::new(StubTransportData::new())));
        let trusted = make_source_with(transport, false, client, vec!["169.254.169.254".to_string()]);
        let error = trusted
            .fetch_manifest(&digest_id(), IndexOperation::Resolve)
            .await
            .expect_err("no manifest is seeded, so the physical fetch itself fails");
        assert!(
            !matches!(error, crate::Error::OciIndex(super::super::error::Error::Ssrf(_))),
            "a trusted host must pass the SSRF guard (failure must come from the fetch, not the guard); got {error:?}"
        );
    }

    // ── dispatch-object verify (trust anchor) ────────────────────────────────

    /// The format's only client-side trust anchor: `o/` now holds
    /// publisher-controlled bytes, and the recompute is the one place OCX
    /// re-derives a digest it did not mint.
    ///
    /// The tampered payload is a **structurally valid image index** — a
    /// substitution an attacker would actually attempt, and one that parses
    /// cleanly. Only the digest comparison can refuse it, so an implementation
    /// that dropped the recompute could not pass by failing the parse instead.
    #[tokio::test]
    async fn dispatch_object_digest_mismatch_is_a_hard_error() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let honest_digest = Algorithm::Sha256.hash(glibc_musl_index());
        // Root points at the honest digest, but the served object URL holds a
        // DIFFERENT, perfectly well-formed image index — the verify must catch
        // it before anything is loaded.
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{honest_digest}"}}}}}}"#,
        );
        transport.insert(&root_url(), root.as_bytes());
        let substituted =
            br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        assert!(
            serde_json::from_slice::<oci::ImageIndex>(substituted).is_ok(),
            "the substituted payload must parse, or the digest check is not what refuses it"
        );
        transport.insert(&dispatch_url(&honest_digest), substituted);

        let source = make_source(transport, false);
        let error = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("substituted dispatch-object bytes must not load");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::DispatchObjectDigestMismatch { .. })
            ),
            "expected DispatchObjectDigestMismatch, got {error:?}"
        );
    }

    /// A dispatch object whose bytes hash correctly but are not an image index
    /// is refused as a malformed document — the admission gate is on document
    /// KIND, and on nothing else. It does **not** inspect `artifactType`:
    /// nothing in ocx reads an image index's artifact type, and gating on it
    /// would refuse documents that are structurally exactly what was asked for.
    #[tokio::test]
    async fn resolve_index_object_rejects_a_non_index_body() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let body = br#"{"platforms":[{"platform":{"architecture":"amd64","os":"linux"},"digest":"sha256:aa"}]}"#;
        let digest = Algorithm::Sha256.hash(body);
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{digest}"}}}}}}"#,
        );
        transport.insert(&root_url(), root.as_bytes());
        transport.insert(&dispatch_url(&digest), body);

        let error = make_source(transport, false)
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("a body that is not an image index must not resolve");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::MalformedIndexDocument { .. })
            ),
            "expected MalformedIndexDocument, got {error:?}"
        );
    }

    /// A dispatch object that hashes correctly and IS an image index, but
    /// declares `schemaVersion: 1`, is refused.
    ///
    /// This is the fail-open case the digest anchor cannot catch: the bytes are
    /// exactly what the root pointed at, so the publisher — not an attacker —
    /// put them there. Without the semantic check the document parses, the
    /// selection comes back empty, and the client reports an ordinary "no
    /// matching platform" instead of "this index is malformed".
    ///
    /// The fixture is a byte literal for a reason: `schemaVersion: 1` cannot be
    /// produced by serialising an `oci::ImageIndex`, so a fixture built by the
    /// code under test could never contradict it.
    #[tokio::test]
    async fn resolve_index_object_refuses_a_wrong_schema_version() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let body = br#"{"schemaVersion":1,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        assert!(
            serde_json::from_slice::<oci::ImageIndex>(body).is_ok(),
            "the payload must parse, or the semantic check is not what refuses it"
        );
        let digest = Algorithm::Sha256.hash(body);
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{digest}"}}}}}}"#,
        );
        transport.insert(&root_url(), root.as_bytes());
        transport.insert(&dispatch_url(&digest), body);

        let error = make_source(transport, false)
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("an invalid image index must not resolve");
        assert!(
            matches!(
                error,
                crate::Error::OciIndex(super::super::error::Error::InvalidImageIndex(_))
            ),
            "expected InvalidImageIndex, got {error:?}"
        );
        assert_eq!(
            crate::cli::ClassifyExitCode::classify(&error),
            Some(crate::cli::ExitCode::DataError),
            "malformed public-index data is a data error"
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
        // Fail-closed (F1): a base that serves a fully valid root + dispatch object
        // but NO config.json is not a version-pinned OCX index. The root must
        // never be consumed — an absent config.json must not degrade to a clean
        // pass that lets a valid-looking root resolve against an unversioned
        // endpoint. The root document must not even be fetched: the config check
        // short-circuits before the root GET.
        let transport = StubIndexTransport::new();
        // Deliberately DO NOT insert config.json — serve an otherwise-valid tree.
        let dispatch_bytes = glibc_musl_index();
        let dispatch_digest = Algorithm::Sha256.hash(dispatch_bytes);
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{dispatch_digest}"}}}}}}"#,
        );
        transport.insert(&root_url(), root.as_bytes());
        transport.insert(&dispatch_url(&dispatch_digest), dispatch_bytes);
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
            &catalog_body(r#"{"kitware/cmake":"sha256:new","stable/tool":"sha256:same","fresh/pkg":"sha256:brand"}"#),
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
        transport.insert_with_etag(
            &catalog_url(),
            &catalog_body(r#"{"kitware/cmake":"sha256:x"}"#),
            "etag-1",
        );
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
        let dispatch_digest = seed_package(&transport, false);
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
        let _ = dispatch_digest; // silence unused in this path
    }

    // ── catalog sync orchestration: diff → re-snapshot → persist on disk ─────

    #[tokio::test]
    async fn local_index_sync_catalog_persists_and_snapshots_moved_package() {
        // A moved package whose image index declares no platforms — so the
        // re-snapshot writes the dispatch object without a physical leaf
        // fetch (no OCI client needed).
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let dispatch_bytes =
            br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        let dispatch_digest = Algorithm::Sha256.hash(dispatch_bytes);
        let root =
            format!(r#"{{"repository":"oci://ghcr.io/x/y","tags":{{"1.0":{{"content":"{dispatch_digest}"}}}}}}"#,);
        transport.insert(&format!("{BASE}/p/ns/pkg.json"), root.as_bytes());
        // The remote catalog entry IS sha256 of the root bytes (F1) — so the
        // second, unchanged sync sees no diff. A mock literal that disagreed with
        // the served root would make every sync re-diff the root-derived local
        // entry against the stale claim and re-snapshot forever.
        let catalog = catalog_body(&format!(
            r#"{{"ns/pkg":"{}"}}"#,
            crate::file_structure::IndexStore::root_catalog_entry(root.as_bytes())
        ));
        transport.insert(&catalog_url(), &catalog);
        transport.insert(
            &format!(
                "{BASE}/p/ns/pkg/o/{}/{}.json",
                dispatch_digest.algorithm().prefix(),
                dispatch_digest.hex()
            ),
            dispatch_bytes,
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
        // verbatim root document plus the referenced dispatch object
        // land in the wire grammar, so a subsequent offline resolve walks the copy.
        assert!(
            snapshot.root_document_path(NAMESPACE, "ns/pkg").exists(),
            "the re-snapshot must write the verbatim root document for the moved package"
        );
        assert!(
            snapshot
                .dispatch_object_path(NAMESPACE, "ns/pkg", &dispatch_digest)
                .exists(),
            "the re-snapshot must persist the image index as a dispatch object"
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
            &catalog_body(&format!(
                r#"{{"moved/pkg":"{WRONG_CLAIM}","unmoved/pkg":"sha256:oldunmoved"}}"#
            )),
        );
        seed_empty_index(&transport, "moved/pkg", "1.0");
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

    /// Seed config + root (`tag` → an empty image index) + that index at
    /// `repo`, returning its digest. An empty `manifests[]` keeps the persist
    /// recursion off the physical (OCI client) path.
    fn seed_empty_index(transport: &StubIndexTransport, repo: &str, tag: &str) -> oci::Digest {
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let dispatch_bytes =
            br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#;
        let dispatch_digest = Algorithm::Sha256.hash(dispatch_bytes);
        let root =
            format!(r#"{{"repository":"oci://ghcr.io/x/y","tags":{{"{tag}":{{"content":"{dispatch_digest}"}}}}}}"#,);
        transport.insert(&format!("{BASE}/p/{repo}.json"), root.as_bytes());
        transport.insert(
            &format!(
                "{BASE}/p/{repo}/o/{}/{}.json",
                dispatch_digest.algorithm().prefix(),
                dispatch_digest.hex()
            ),
            dispatch_bytes,
        );
        dispatch_digest
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
        let dispatch_digest = seed_empty_index(&transport, "ns/pkg", "1.0");
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
            digest, dispatch_digest,
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
        assert_eq!(
            source.jurisdiction(&logical),
            super::super::Jurisdiction::Authoritative,
            "the source owns its namespace"
        );

        // A foreign namespace is neither rewritten nor owned.
        let foreign =
            oci::Identifier::new_registry("x/y", "ghcr.io").clone_with_digest(oci::Digest::Sha256("b".repeat(64)));
        assert!(source.physical_reference(&foreign).await.unwrap().is_none());
        assert_eq!(source.jurisdiction(&foreign), super::super::Jurisdiction::Outside);
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
        let second_obs = glibc_musl_index();
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
        transport.insert_with_etag(&catalog_url(), &catalog_body(r#"{"ns/pkg":"sha256:root"}"#), "etag-1");
        // Serve the moved package so the first sync can re-snapshot it.
        seed_empty_index(&transport, "ns/pkg", "1.0");
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
        transport.insert_with_etag(&catalog_url(), &catalog_body(r#"{"ns/pkg":"sha256:root"}"#), WEAK_ETAG);
        seed_empty_index(&transport, "ns/pkg", "1.0");
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

    // ── jurisdiction: a configured index owns its WHOLE registry (ocx#251) ────
    //
    // The verdict is `identifier.registry()` and nothing else. There is no
    // per-name decline left: a name this index holds no root for is a hard miss,
    // never a hand-off to the plain OCI registry underneath. The index's own
    // published `name_segments` declaration existed only to interpret such a
    // miss as a fall-through and is gone with it — the client no longer reads
    // it, and no config state (absent, malformed, unsupported, unreachable) can
    // move the verdict.

    const FLAT_REPO: &str = "go-task";

    fn flat_id() -> oci::Identifier {
        oci::Identifier::new_registry(FLAT_REPO, NAMESPACE).clone_with_tag("3")
    }

    fn flat_root_url() -> String {
        format!("{BASE}/p/{FLAT_REPO}.json")
    }

    /// Seeds `config.json` plus a resolvable root for the namespaced name. The
    /// FLAT name is deliberately left un-served — it is the name every
    /// terminal-miss test below is about.
    fn seed_with_config(transport: &StubIndexTransport, config_body: &[u8]) {
        seed_package(transport, false);
        transport.insert(&config_url(), config_body); // seed_package writes its own
    }

    #[tokio::test]
    async fn jurisdiction_is_outside_for_a_foreign_registry_and_issues_no_request() {
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1}"#);
        let source = make_source(transport.clone(), false);

        let foreign = oci::Identifier::new_registry(REPO, "ghcr.io").clone_with_tag("3.28");
        assert_eq!(source.jurisdiction(&foreign), super::super::Jurisdiction::Outside);
        assert_eq!(
            transport.request_urls(),
            Vec::<String>::new(),
            "a foreign registry is decided with no I/O — not even config.json"
        );
    }

    #[tokio::test]
    async fn jurisdiction_is_authoritative_for_every_name_in_its_own_registry_with_no_io() {
        // The inversion ocx#251 is: a flat `ocx.sh/go-task` the index holds no
        // root for used to be OUTSIDE this source. Both shapes are now
        // authoritative, and — the second half of the claim — the verdict costs
        // no request at all: neither the config.json that carried the old
        // declaration nor the root probe whose 404 the declaration interpreted.
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1}"#);
        let source = make_source(transport.clone(), false);

        for identifier in [flat_id(), tagged_id()] {
            assert_eq!(
                source.jurisdiction(&identifier),
                super::super::Jurisdiction::Authoritative,
                "'{identifier}' is in this source's registry, so this source owns it"
            );
        }
        assert_eq!(
            transport.request_urls(),
            Vec::<String>::new(),
            "the verdict is decided with no I/O on either shape"
        );
    }

    #[tokio::test]
    async fn resolve_root_refuses_a_304_answering_an_unconditional_get() {
        // The root request carries no `If-None-Match`, so a `304` is a
        // misbehaving edge (RFC 9110 §15.4.5), not an absence. Reading it as a
        // miss would memoize "this index does not hold the package" for the rest
        // of the process off one bad response.
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1}"#);
        transport.always_not_modified(&flat_root_url());
        let source = make_source(transport, false);

        let error = source
            .resolve_root(FLAT_REPO)
            .await
            .expect_err("a 304 answering an unconditional GET is a protocol violation, not a miss");
        assert!(
            error.to_string().contains(&flat_root_url()),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn no_config_state_can_move_the_jurisdiction_verdict() {
        // Fail CLOSED, and now by construction: an index that cannot be asked
        // what it serves — absent, malformed, unsupported, or unreachable
        // config.json — must not be assumed to serve nothing, or an outage
        // silently downgrades the whole namespace to plain OCI. The verdict no
        // longer reads the config at all, so there is no state to get this
        // wrong.
        let cases: Vec<(&str, Option<&[u8]>)> = vec![
            ("absent", None),
            ("malformed", Some(&b"not json at all"[..])),
            ("unsupported version", Some(&br#"{"format_version":9999}"#[..])),
        ];
        for (label, body) in cases {
            let transport = StubIndexTransport::new();
            if let Some(body) = body {
                transport.insert(&config_url(), body);
            }
            let source = make_source(transport, false);
            assert_eq!(
                source.jurisdiction(&flat_id()),
                super::super::Jurisdiction::Authoritative,
                "config.json {label} must not narrow the namespace"
            );
        }

        // Unreachable is the one that matters most, so it also carries the
        // second half of the contract: the failure is deferred, never swallowed
        // — the very next read on the same source raises the real error.
        let transport = StubIndexTransport::new();
        transport.fail(&config_url());
        let source = make_source(transport, false);
        assert_eq!(
            source.jurisdiction(&flat_id()),
            super::super::Jurisdiction::Authoritative,
            "config.json unreachable must not narrow the namespace"
        );
        assert!(
            source.fetch_root_document(&flat_id()).await.is_err(),
            "the deferred error must surface loud on the very next read"
        );
    }

    // ── chain routing: an authoritative miss is terminal AND self-naming ──────

    /// The chain the production wiring builds: index source first, plain-OCI
    /// registry catch-all second.
    fn chain_with(dir: &tempfile::TempDir, source: OcxIndex, registry: RegistryStub) -> super::super::Index {
        super::super::Index::from_chained(
            local_index(dir),
            vec![
                super::super::Index::from_source(source),
                super::super::Index::from_impl(registry),
            ],
            super::super::ChainMode::Default,
        )
    }

    /// The `NotInIndex` refusal, asserted on the shape a user actually reads —
    /// the rendered `Display` chain, not the variant name. The message IS the
    /// deliverable of ocx#251: someone hitting it must learn, from this string
    /// alone, that the name is absent from a specific index, which index that
    /// was, and both ways out.
    fn assert_names_the_index(error: &crate::Error, identifier: &oci::Identifier) {
        let text = format!("{error:#}");
        for expected in [
            &identifier.to_string(),
            "is not in the index at",
            BASE,
            "authoritative for every name in registry",
            NAMESPACE,
            "ocx package announce",
            "index = \"\"",
        ] {
            assert!(
                text.contains(expected),
                "the refusal must carry {expected:?} — a user gets no other diagnosis: {text}"
            );
        }
    }

    #[tokio::test]
    async fn a_flat_name_the_index_does_not_hold_is_a_terminal_miss_naming_the_index() {
        // The inversion (ocx#251). This resolved off the plain-OCI registry
        // before — past the index, and so past its yank and deprecation gate —
        // because the index declared it could not express a one-segment name.
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = make_source(transport.clone(), false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = chain_with(&dir, source, registry.clone());

        let error = chained
            .fetch_manifest(&flat_id(), IndexOperation::Resolve)
            .await
            .expect_err("a name the authoritative index does not hold must not resolve at all");
        assert_names_the_index(&error, &flat_id());
        assert_eq!(
            registry.calls(),
            0,
            "the registry must never answer for a name the index owns: {:?}",
            transport.request_urls()
        );
    }

    #[tokio::test]
    async fn a_namespaced_name_the_index_does_not_hold_is_the_same_terminal_miss() {
        // The terminal stop already held for a namespaced name, but it produced
        // a bare `Ok(None)` -> "package not found". Same verdict, same exit
        // class; what ocx#251 adds is that it now says which index answered.
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = make_source(transport, false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = chain_with(&dir, source, registry.clone());

        let absent = oci::Identifier::new_registry("ns/absent", NAMESPACE).clone_with_tag("1.0");
        let error = chained
            .fetch_manifest(&absent, IndexOperation::Resolve)
            .await
            .expect_err("an authoritative source's clean miss is terminal");
        assert_names_the_index(&error, &absent);
        assert_eq!(registry.calls(), 0, "the registry must never be consulted");
    }

    #[tokio::test]
    async fn an_index_outage_is_never_reported_as_a_missing_package() {
        // The arm-merging hazard, and the one regression this whole change can
        // introduce. "The index says no" and "the index could not be read" reach
        // the chain one match arm apart; collapsing them would turn every index
        // outage into a confident `NotInIndex` telling the user to go announce a
        // package that is already there.
        //
        // Both halves are asserted: the resolve fails (never a silent
        // fall-through to the registry), and it fails as the TRANSPORT error.
        for failing in [config_url(), flat_root_url()] {
            let transport = StubIndexTransport::new();
            transport.insert(&config_url(), br#"{"format_version":1}"#);
            transport.fail(&failing);
            let source = make_source(transport, false);
            let registry = RegistryStub::new();

            let dir = tempfile::tempdir().unwrap();
            let chained = chain_with(&dir, source, registry.clone());

            let error = chained
                .fetch_manifest(&flat_id(), IndexOperation::Resolve)
                .await
                .expect_err("an unreachable index must fail loud, never resolve past itself");
            let text = format!("{error:#}");
            assert!(
                text.contains(&failing),
                "the refusal must name the endpoint that failed ({failing}): {text}"
            );
            assert!(
                !text.contains("is not in the index at"),
                "an outage must never be reported as an absent package: {text}"
            );
            assert_eq!(registry.calls(), 0, "the registry must never shadow an index outage");
        }
    }

    #[tokio::test]
    async fn an_expressible_name_keeps_the_yank_gate() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, true);
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = make_source(transport, false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = chain_with(&dir, source, registry.clone());

        let error = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("a yanked tag must be refused, not silently served by the registry");
        assert!(error.to_string().contains("yanked"), "unexpected error: {error}");
        assert_eq!(registry.calls(), 0, "the registry must never be consulted");
    }

    #[tokio::test]
    async fn a_flat_name_the_index_does_hold_still_resolves_and_keeps_its_yank_gate() {
        // The other half of "authoritative for the whole registry": a flat name
        // is not rejected for its shape — an index that holds a root for it
        // resolves it, and its yank refusal is never bypassed by the plain-OCI
        // catch-all.
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let dispatch_bytes = glibc_musl_index();
        let dispatch_digest = Algorithm::Sha256.hash(dispatch_bytes);
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/x/y","tags":{{"3":{{"content":"{dispatch_digest}","yanked":{{"reason":"bad build","at":"2026-02-01T00:00:00Z"}}}}}}}}"#
        );
        transport.insert(&flat_root_url(), root.as_bytes());
        let source = make_source(transport, false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = chain_with(&dir, source, registry.clone());

        let error = chained
            .fetch_manifest(&flat_id(), IndexOperation::Resolve)
            .await
            .expect_err("an index owns every name in its namespace, flat included");
        assert!(error.to_string().contains("yanked"), "unexpected error: {error}");
        assert_eq!(
            registry.calls(),
            0,
            "the yanked build must never be resolvable through the registry"
        );
    }

    #[tokio::test]
    async fn physical_reference_and_fetch_blob_consult_the_authoritative_source_for_a_flat_name() {
        // These two paths used to skip the source entirely for a flat name, off
        // the memoized `Outside` verdict. They now ask it — the source owns the
        // name — so the root IS fetched and the miss is the source's own answer.
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1}"#);
        let source = make_source(transport.clone(), false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = chain_with(&dir, source, registry);

        assert!(chained.physical_reference(&flat_id()).await.unwrap().is_none());
        let pinned = oci::PinnedIdentifier::try_from(
            oci::Identifier::new_registry(FLAT_REPO, NAMESPACE).clone_with_digest(oci::Digest::Sha256("c".repeat(64))),
        )
        .unwrap();
        let _ = chained.fetch_blob(&pinned).await;
        assert_eq!(
            transport.request_count(&flat_root_url()),
            1,
            "both paths reach the source, and its 404 is memoized across them: {:?}",
            transport.request_urls()
        );
    }

    // ── sync_catalog: an unfetchable catalog row is skipped, not fatal ─────────

    /// The catalog entry `source` currently holds for `repository`, or `None`.
    async fn catalog_entry(local: &super::super::LocalIndex, repository: &str) -> Option<String> {
        local
            .index_store()
            .read_source_catalog(NAMESPACE)
            .await
            .unwrap()
            .and_then(|catalog| catalog.get(repository).cloned())
    }

    #[tokio::test]
    async fn sync_catalog_neither_advances_nor_retires_a_row_whose_root_is_unfetchable() {
        // A catalog row whose `p/<key>.json` 404s. The key HAS a local root on
        // disk, so step 3 re-snapshots it, `refresh_tags` fails, and without the
        // non-fatal handling its `?` would abort before the catalog + ETag
        // commit — the ETag would never advance and every later
        // `ocx index update` would repeat it identically, swallowed as a warn.
        //
        // The row is skipped, but its FETCHED value must not be adopted either:
        // the root that value names is unfetchable, so committing it would leave
        // the on-disk root and the catalog straddled.
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert_with_etag(
            &catalog_url(),
            &catalog_body(r#"{"go-task":"sha256:moved"}"#),
            "etag-unfetchable",
        );
        let source = make_source(transport.clone(), false);

        let dir = tempfile::tempdir().unwrap();
        let local = local_index(&dir);
        seed_stale_root(local.index_store(), FLAT_REPO).await;
        let before = catalog_entry(&local, FLAT_REPO).await;

        let outcome = local
            .sync_catalog(&source)
            .await
            .expect("one unfetchable row in the remote catalog must not fail the whole sync");
        assert_eq!(outcome.moved, vec![FLAT_REPO.to_string()]);
        assert!(
            transport.request_count(&flat_root_url()) > 0,
            "the row is re-snapshotted against the source that published it: {:?}",
            transport.request_urls()
        );
        assert_eq!(
            catalog_entry(&local, FLAT_REPO).await,
            before,
            "the entry must still describe the root actually on disk"
        );
        assert_eq!(
            local.index_store().read_source_catalog_etag(NAMESPACE).await.unwrap(),
            None,
            "committing the ETag would answer 304 forever and retire the retry"
        );
    }

    #[tokio::test]
    async fn sync_catalog_lands_the_healthy_rows_and_retries_the_failed_one_next_run() {
        // Publish skew, no grammar violation anywhere: both keys are expressible
        // and materialized, but `ns/skewed`'s root 404s (rolled back, or the
        // catalog regenerated ahead of the roots). `refresh_tags` errors.
        //
        // Three properties, and the third is the point. The failure must not
        // veto the source-wide commit (its `?` used to, stranding every other
        // moved package on every later `ocx index update`). It must not advance
        // the failed row either — the on-disk root is still the old one, so the
        // fetched entry would straddle them, `read_root` would self-heal the
        // catalog back and serve the STALE root past the yank gate. And with the
        // entry kept, the ETag must be held back, or the next sync answers 304,
        // diffs nothing, and the row is never retried at all.
        let transport = StubIndexTransport::new();
        seed_empty_index(&transport, "ns/pkg", "1.0");
        transport.insert_with_etag(
            &catalog_url(),
            &catalog_body(r#"{"ns/pkg":"sha256:moved","ns/skewed":"sha256:moved"}"#),
            "etag-skew",
        );
        let source = make_source(transport.clone(), false);
        let skewed_root_url = format!("{BASE}/p/ns/skewed.json");

        let dir = tempfile::tempdir().unwrap();
        let local = local_index(&dir);
        seed_stale_root(local.index_store(), "ns/pkg").await;
        seed_stale_root(local.index_store(), "ns/skewed").await;
        let skewed_before = catalog_entry(&local, "ns/skewed").await;
        let healthy_before = catalog_entry(&local, "ns/pkg").await;

        local
            .sync_catalog(&source)
            .await
            .expect("one package's re-snapshot failure must not fail the source-wide sync");

        assert!(
            transport.request_urls().contains(&format!("{BASE}/p/ns/pkg.json")),
            "the healthy moved key is still re-snapshotted: {:?}",
            transport.request_urls()
        );
        let healthy_after = catalog_entry(&local, "ns/pkg").await;
        assert!(
            healthy_after.is_some() && healthy_after != healthy_before,
            "the healthy row must land its re-snapshotted entry"
        );
        assert_eq!(
            catalog_entry(&local, "ns/skewed").await,
            skewed_before,
            "the failed row must keep the entry matching the root actually on disk"
        );
        assert_eq!(
            local.index_store().read_source_catalog_etag(NAMESPACE).await.unwrap(),
            None,
            "the ETag must be withheld while a moved row is stale"
        );
        let attempts = transport.request_count(&skewed_root_url);
        assert!(
            attempts > 0,
            "the first run must have tried: {:?}",
            transport.request_urls()
        );

        // The whole point: the next update re-diffs the failed key and tries
        // again. With the fetched entry adopted and the ETag committed, the
        // second sync would get a `304`, diff nothing, and never touch it.
        local
            .sync_catalog(&source)
            .await
            .expect("the retry must be an ordinary sync, not a failure");
        assert!(
            transport.request_count(&skewed_root_url) > attempts,
            "the failed key must be re-diffed and retried: {:?}",
            transport.request_urls()
        );
    }

    #[tokio::test]
    async fn sync_catalog_still_refreshes_a_healthy_moved_key() {
        let transport = StubIndexTransport::new();
        seed_empty_index(&transport, "ns/pkg", "1.0");
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.insert_with_etag(&catalog_url(), &catalog_body(r#"{"ns/pkg":"sha256:moved"}"#), "etag-ok");
        let source = make_source(transport.clone(), false);

        let dir = tempfile::tempdir().unwrap();
        let local = local_index(&dir);
        seed_stale_root(local.index_store(), "ns/pkg").await;

        local.sync_catalog(&source).await.unwrap();
        assert!(
            transport.request_urls().contains(&format!("{BASE}/p/ns/pkg.json")),
            "an expressible moved key is still re-snapshotted: {:?}",
            transport.request_urls()
        );
    }
}
