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
//! The **root document** is the anchor: nothing pins it from above, so it is
//! trusted on the strength of the channel it arrived over — TLS for an
//! `https://` base (which is why [`Error::PlainHttpIndexNotAllowed`](super::error::Error::PlainHttpIndexNotAllowed)
//! refuses an ungated plaintext one), or the operator's own filesystem for a
//! `file://` shipped copy.
//!
//! Everything *below* the root is verified rather than trusted: the
//! dispatch-object verify (`sha256(bytes) == <hex>`) is the one place OCX
//! re-derives a digest it did not mint (F1), and a mismatch is a hard
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

use super::wire::{CatalogDocument, CatalogIndex, IndexFormatConfig, IndexRoot, RootTag, gate_format_version};
use super::{IndexOperation, error, index_impl};
use crate::{Result, log, oci};

// ── Frozen wire shapes (● contract) ──────────────────────────────────────────
//
// `IndexRoot` / `RootTag` / `CatalogDocument` / `CatalogIndex` and the shared
// `SUPPORTED_FORMAT_VERSION` pin live in `oci::index::wire`
// (`adr_index_indirection.md` §Data Model) — the frozen grammar shared
// verbatim by this remote client and the local store
// (`crate::file_structure::IndexStore`), imported above. What a tag points at
// is an `oci::ImageIndex`, whose shape is the OCI image spec's, not ours.
//
// `IndexFormatConfig` (`config.json`) joined them there: no longer this
// module's private struct but a shared one — this module reads it today, and
// the local store will read it (WP11) while the update path writes it (WP5)
// (`adr_servable_index_snapshot.md` C-001).

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
    /// `200 OK` — the response body.
    Found { bytes: Vec<u8> },
    /// `404 Not Found` — the object is absent (a normal miss, not an error).
    NotFound,
}

/// Transport for the static-file index endpoints.
///
/// Two production impls, one per scheme [`OcxIndex::resolve_base_url`] admits:
/// [`ReqwestIndexTransport`] over `https://` (or gated `http://`), and
/// [`FileIndexTransport`](super::FileIndexTransport) over a `file://` shipped
/// copy. It is also the seam that lets [`OcxIndex`] resolve without hitting the
/// network in tests (mock this the way
/// [`StubTransport`](super::super::client::test_transport::StubTransport) mocks
/// the OCI transport).
#[async_trait]
pub trait IndexTransport: Send + Sync {
    /// Fetch `url`. Unconditional — nothing here sends a validator, so an impl
    /// must never answer with a not-modified outcome; the only outcomes are
    /// the bytes, [`IndexFetch::NotFound`], or an error.
    async fn get(&self, url: &str) -> Result<IndexFetch>;

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
    async fn get(&self, url: &str) -> Result<IndexFetch> {
        let mut response =
            self.client
                .get(url)
                .send()
                .await
                .map_err(|source| super::error::Error::IndexHttpFailed {
                    url: redact_url(url),
                    source: Box::new(source),
                })?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(IndexFetch::NotFound);
        }
        // Everything else — including a `304` answering this unconditional
        // `GET` (RFC 9110 §15.4.5, a misbehaving edge) — is an error. Only a
        // confirmed `404` above may read as absence: that `None` is what
        // [`OcxIndex::jurisdiction`] settles an `Outside` verdict off, and the
        // verdict is memoized, so one bad response would decide a name for the
        // rest of the process.
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
        Ok(IndexFetch::Found { bytes: body })
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

// ── Source ───────────────────────────────────────────────────────────────────

/// Default base URL when no `[registries."<ns>"] index` field is configured.
pub const DEFAULT_INDEX_BASE_URL: &str = "https://index.ocx.sh";

/// A resolved index base: the URL every fetch is minted from, paired with the
/// transport that serves that scheme — one value, because
/// [`OcxIndex::resolve_base_url`] decides the scheme once and the caller never
/// re-derives it (`adr_servable_index_snapshot.md` C-018).
pub struct IndexBase {
    /// Trailing-slash-trimmed, as [`OcxIndex::new`] stores it.
    pub url: String,
    /// The transport for `url`'s scheme.
    pub transport: Box<dyn IndexTransport>,
}

impl std::fmt::Debug for IndexBase {
    // `Box<dyn IndexTransport>` cannot derive it, and the URL may carry
    // `user:password@` userinfo (CWE-532), so it goes through `redact_url`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndexBase")
            .field("url", &redact_url(&self.url))
            .finish_non_exhaustive()
    }
}

/// The lowercased scheme of `url`, or `None` when it carries none.
///
/// The same split [`crate::config::mirror::parse_url`] does, applied *before*
/// it so the closed scheme set can be checked on the configured base — a `file`
/// base has to be diverted before that parser reads its empty authority as
/// `MissingHost` (C-018 check 1).
fn scheme_of(url: &str) -> Option<String> {
    url.split_once("://").map(|(scheme, _)| scheme.to_ascii_lowercase())
}

fn invalid_index_url(
    namespace: &str,
    url: &str,
    origin: String,
    source: Option<crate::config::mirror::MirrorConfigError>,
) -> crate::Error {
    error::Error::InvalidIndexUrl {
        namespace: namespace.to_string(),
        // A configured base or a mirror value may embed `user:password@`
        // (CWE-532), and this error names the offending URL verbatim.
        url: redact_url(url),
        origin,
        source: source.map(Box::new),
    }
    .into()
}

/// Resolves a `file://` configured base into its [`IndexBase`] (C-018's `file`
/// row, C-019).
///
/// Requires an **empty authority** — `file://host/srv/x` and
/// `file://localhost/srv/x` are UNC/remote forms, not local trees — and an
/// **absolute path**. Two paths are refused for naming no directory: the
/// filesystem root (`file:///`), which survives the trailing-slash trim as the
/// empty string, and a bare Windows drive (`file:///C:/`), which survives it as
/// `/C:` and would otherwise reach [`file_root`] as the designator `C:` — a
/// path Win32 resolves against the **per-drive working directory**, silently
/// serving the whole index out of wherever `ocx` was launched.
fn resolve_file_base(namespace: &str, base: &str) -> Result<IndexBase> {
    let rest = base.split_once("://").map_or("", |(_, rest)| rest);
    // Splitting at the first `/` makes the authority check and the
    // absolute-path check the same test: whatever precedes it is the authority,
    // and a path that survives non-empty necessarily starts with `/`.
    let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let path = path.trim_end_matches('/');
    // `path.len() == 3` is the whole of `/C:` — a drive with nothing under it.
    // Refused on every platform, so a base is valid or not independently of
    // where it is read; `/C:` is not a directory anyone means on Unix either.
    let bare_drive = has_drive_prefix(path) && path.len() == 3;
    if !authority.is_empty() || path.is_empty() || bare_drive {
        return Err(invalid_index_url(
            namespace,
            base,
            error::INDEX_URL_FROM_REGISTRIES.to_string(),
            None,
        ));
    }
    let url = format!("file://{path}");
    Ok(IndexBase {
        transport: Box::new(super::FileIndexTransport::new(url.clone(), file_root(path))),
        url,
    })
}

/// The absolute filesystem path a `file://` URL's tail names.
///
/// On Windows the tail `/C:/srv/x` is not itself an absolute path — the
/// drive-letter form needs its leading separator stripped (C-018). Stripping it
/// unconditionally would turn a legitimate Unix root literally named `/C:/…`
/// into a **relative** path resolved against the process working directory, so
/// it is gated on the target OS.
fn file_root(path: &str) -> std::path::PathBuf {
    if cfg!(windows) && has_drive_prefix(path) {
        std::path::PathBuf::from(&path[1..])
    } else {
        std::path::PathBuf::from(path)
    }
}

/// Whether `path` is a `file://` tail of the Windows drive-letter form
/// (`/C:/…`). OS-independent so it stays testable off Windows.
fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':'
}

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
    /// (F1 "read once") and [`OcxIndex::jurisdiction`] reads the declared name
    /// grammar for free. Never set on a served-but-unsupported version (a
    /// re-checked hard error, not a remembered steady state) NOR on an absent
    /// `config.json` (assumed v1 and re-derived every call, so a tree that
    /// later publishes one is picked up without restarting). Config-driven
    /// construction means there is no probe outcome to soften a transport
    /// failure into — that always propagates.
    config: Option<Arc<IndexFormatConfig>>,
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
    /// means — decided on **evidence**, with the index's published declaration
    /// only ever interpreting a miss.
    ///
    /// | Case | Verdict |
    /// |---|---|
    /// | Foreign registry | [`Outside`](super::Jurisdiction::Outside), **no I/O** |
    /// | No declaration, or a declaration the name satisfies | [`Authoritative`](super::Jurisdiction::Authoritative), no root fetch |
    /// | Declared inexpressible, root **found** | [`Authoritative`](super::Jurisdiction::Authoritative) — the root is the index's opinion about this name; the declaration is overruled by it |
    /// | Declared inexpressible, root **absent** (404) | [`Outside`](super::Jurisdiction::Outside) — the flat-`ocx.sh/go-task` case, and the only thing the declaration decides |
    /// | Declared inexpressible, root fetch **failed** | [`Authoritative`](super::Jurisdiction::Authoritative), fail-closed |
    ///
    /// The declaration ([`name_segments`](IndexFormatConfig::name_segments),
    /// `2` on `index.ocx.sh`, restating its root schema's
    /// `^ocx\.sh/<ns>/<pkg>$`) is an unsigned, CDN-cacheable integer fetched
    /// over the same channel as the roots — so it must never be able to *stop
    /// the client asking*. A template bug, a stale edge, or a compromise scoped
    /// to that one file could otherwise narrow a namespace and skip the yank
    /// gate on a name the index does hold a (yanking) root for. Here it can
    /// only say what an unavoidable 404 means: fall through to plain OCI rather
    /// than stop the chain, which is what strands every flat package otherwise.
    ///
    /// **Fail-closed** end to end: an absent, malformed, unsupported or
    /// unreachable `config.json` — or a root fetch that errors — keeps the
    /// source authoritative, so an index outage can never silently downgrade a
    /// namespace to plain OCI.
    ///
    /// Infallible by construction, and not error-swallowing: nothing is cached
    /// on failure, so the `resolve_root` / `fetch_root_document` that
    /// immediately follows on the same source re-fetches and raises the real
    /// `UnsupportedIndexFormat` / transport error loud. This probe only defers.
    ///
    /// Cost: one `GET /config.json` that [`Self::resolve_root`] would fire one
    /// step later anyway, plus — for a name the declaration rejects — one root
    /// `GET` that 404s. Both memoized per source instance, so eight flat tools
    /// in a project cost eight 404s on a cold run and none thereafter.
    pub async fn jurisdiction(&self, identifier: &oci::Identifier) -> super::Jurisdiction {
        if !self.serves_registry(identifier.registry()) {
            return super::Jurisdiction::Outside;
        }
        let declared = match self.check_format_version().await {
            Ok(config) => config.name_segments,
            Err(error) => {
                log::debug!(
                    "Could not read '{}' config.json to check jurisdiction over '{identifier}' \
                     (staying authoritative; the resolve below re-raises it): {error}",
                    self.namespace
                );
                None
            }
        };
        let Some(segments) = declared else {
            return super::Jurisdiction::Authoritative;
        };
        if identifier.repository().split('/').count() == segments.get() as usize {
            return super::Jurisdiction::Authoritative;
        }

        // The declaration says this name is inexpressible. Ask anyway: only the
        // MEANING of the miss is delegated to it, never the decision to ask.
        match self.resolve_root(identifier.repository()).await {
            Ok(Some(_)) => {
                log::debug!(
                    "Index '{}' declares {segments}-segment names but does hold a root for '{identifier}' — \
                     the root decides.",
                    self.namespace
                );
                super::Jurisdiction::Authoritative
            }
            Ok(None) => {
                // `debug!`, not `warn!`: this is the ordinary steady state for
                // the shipped namespace — 41 of the 44 `ocx.sh` repositories are
                // flat names index.ocx.sh cannot express. It also fires per chain
                // consult, not per name (the `roots` memo suppresses the request,
                // not the log line, and `candidate_sources` re-enters this from
                // every routing decision), so at `warn!` a cold `ocx lock` over
                // one project's tools buries the log in tens of identical lines.
                log::debug!(
                    "Index '{}' declares {segments}-segment names and holds no root for '{identifier}' — \
                     resolving it through the registry instead.",
                    self.namespace
                );
                super::Jurisdiction::Outside
            }
            Err(error) => {
                log::debug!(
                    "Could not read '{}' root for '{identifier}' to settle jurisdiction \
                     (staying authoritative; the resolve below re-raises it): {error}",
                    self.namespace
                );
                super::Jurisdiction::Authoritative
            }
        }
    }

    /// This source's own SSRF escape hatch (`[registries."<ns>"].trusted_hosts`,
    /// X2) — read-only accessor over already-public construction input
    /// ([`OcxIndexConfig::trusted_hosts`]), so callers can confirm a built
    /// source carries exactly its own namespace's set and never another's.
    pub fn trusted_hosts(&self) -> &[String] {
        &self.trusted_hosts
    }

    /// Resolves the static-file base for `namespace` — the URL and the
    /// transport that serves it, as one [`IndexBase`]: the
    /// `[registries."<ns>"] index` base (already merged through the managed
    /// tier) if present, else [`DEFAULT_INDEX_BASE_URL`] — then applies the
    /// `[mirrors."<host>"] index` role override for the base's own traffic
    /// host, if one is declared (`mirrors_index`, replace semantics, no
    /// fallback). Minted **once** here, the single place base URLs come
    /// from (`adr_index_indirection.md` F5c).
    ///
    /// The scheme set is closed and checked twice — on the configured base,
    /// and again on the post-override target, since a `[mirrors]` entry (table
    /// or `OCX_MIRRORS`) replaces the scheme
    /// (`adr_servable_index_snapshot.md` C-018/C-019/C-020):
    ///
    /// | Scheme | Target |
    /// |---|---|
    /// | absent / `https` | [`ReqwestIndexTransport`] |
    /// | `http` | [`ReqwestIndexTransport`], only when the final host is in `insecure_hosts` (`OCX_INSECURE_REGISTRIES`) — the root document is the index path's trust anchor, so a plaintext index is an on-path takeover (CWE-319), gated exactly like the registry role |
    /// | `file` | [`FileIndexTransport`](super::FileIndexTransport), as a **configured base only** — empty authority, absolute path, and no `[mirrors]` override (which is host-keyed, and a `file` base has no host) |
    /// | anything else | refused |
    ///
    /// # Errors
    ///
    /// [`Error::PlainHttpIndexNotAllowed`](super::error::Error::PlainHttpIndexNotAllowed)
    /// for an ungated `http://` target; [`Error::InvalidIndexUrl`](super::error::Error::InvalidIndexUrl)
    /// for an unparseable `[registries."<ns>"] index` base, a scheme outside the
    /// set above, or a `file://` base with a non-empty authority or a relative
    /// path.
    pub fn resolve_base_url(
        config: &crate::config::Config,
        namespace: &str,
        mirrors_index: &BTreeMap<String, crate::config::mirror::ParsedMirror>,
        insecure_hosts: &[String],
    ) -> Result<IndexBase> {
        let base = config
            .registries
            .as_ref()
            .and_then(|table| table.get(namespace))
            .and_then(|entry| entry.index.as_deref())
            .filter(|url| !url.is_empty())
            .unwrap_or(DEFAULT_INDEX_BASE_URL);

        // Check 1, on the CONFIGURED base and before `parse_url`: a `file` base
        // is diverted here because it must never be host-keyed — it has no host
        // to key a `[mirrors]` override by, and `parse_url` reads its empty
        // authority as `MissingHost`.
        match scheme_of(base).as_deref() {
            None | Some("http") | Some("https") => {}
            Some("file") => return resolve_file_base(namespace, base),
            Some(_) => {
                return Err(invalid_index_url(
                    namespace,
                    base,
                    error::INDEX_URL_FROM_REGISTRIES.to_string(),
                    None,
                ));
            }
        }

        // Reuse the mirror URL parser (scheme/host split, https default) so the
        // plain-HTTP gate matches the registry role byte for byte.
        let parsed = crate::config::mirror::parse_url(base).map_err(|source| {
            invalid_index_url(
                namespace,
                base,
                error::INDEX_URL_FROM_REGISTRIES.to_string(),
                Some(source),
            )
        })?;

        // Index-role mirror override, keyed by the base's own traffic host —
        // replace semantics, no fallback. The key is kept because it is what
        // names the offending `[mirrors]` entry if check 2 refuses below.
        let upstream = parsed.host.clone();
        let overridden = mirrors_index.get(&upstream);
        let target = overridden.cloned().unwrap_or(parsed);
        let path = if target.path_prefix.is_empty() {
            String::new()
        } else {
            format!("/{}", target.path_prefix)
        };
        let url = format!("{}://{}{}", target.protocol, target.host, path);

        // Check 2, on the POST-override target. Not redundant with check 1: the
        // override replaces the scheme, so a `[mirrors]` entry — including one
        // injected through `OCX_MIRRORS` — bypasses a base-only check (C-020).
        match target.protocol.as_str() {
            "https" => {}
            "http" if insecure_hosts.iter().any(|host| host == &target.host) => {}
            "http" => {
                return Err(super::error::Error::PlainHttpIndexNotAllowed {
                    namespace: namespace.to_string(),
                    host: target.host,
                }
                .into());
            }
            _ => {
                // Check 1 admitted only http/https past its own branch, so a
                // scheme reaching here can only have come from the override.
                let origin = if overridden.is_some() {
                    error::index_url_from_mirrors(&upstream)
                } else {
                    error::INDEX_URL_FROM_REGISTRIES.to_string()
                };
                return Err(invalid_index_url(namespace, &url, origin, None));
            }
        }

        Ok(IndexBase {
            url,
            transport: Box::new(ReqwestIndexTransport::new()),
        })
    }

    // ── config.json (F1) ─────────────────────────────────────────────────────

    /// Resolves this source's `config.json` and version-gates it, fetching it
    /// once per source instance on success (F1 "read once") and skipping the
    /// fetch on every later call. Config-driven construction
    /// (`[registries."<ns>"].index` presence) already decided this host serves
    /// an ocx-index, so there is nothing left to *probe* for — this only
    /// guards the wire-format version and carries the declared name grammar
    /// ([`IndexFormatConfig::name_segments`]) on the same fetch.
    ///
    /// An **absent** `config.json` (404) resolves to
    /// [`IndexFormatConfig::assumed_v1`] and is deliberately **not** memoized,
    /// so a tree that later publishes one is picked up without restarting the
    /// process (`adr_servable_index_snapshot.md` C-005). A
    /// served-but-unsupported `format_version` is a hard, fail-closed error
    /// (F1), likewise never cached as a steady state. A transport failure
    /// reaching `config.json` propagates as a hard error on every call — there
    /// is no soft "maybe not an index yet" state to absorb it.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedIndexFormat`](super::error::Error::UnsupportedIndexFormat)
    /// on a served-but-unknown version; the transport error otherwise.
    async fn check_format_version(&self) -> Result<Arc<IndexFormatConfig>> {
        if let Some(config) = &self.cache.read().await.config {
            return Ok(config.clone());
        }
        let url = format!("{}/config.json", self.base_url);
        let fetched = self.transport.get(&url).await?;
        let config = match &fetched {
            IndexFetch::Found { bytes } => parse_document(bytes, &url)?,
            IndexFetch::NotFound => IndexFormatConfig::assumed_v1(),
        };
        gate_format_version(config.format_version)?;
        let config = Arc::new(config);
        // Only a document that was actually served is memoized. An assumed v1
        // is re-derived every call, so a tree that later publishes a
        // `config.json` is picked up without restarting the process (C-005).
        if matches!(fetched, IndexFetch::Found { .. }) {
            self.cache.write().await.config = Some(config.clone());
        }
        Ok(config)
    }

    // ── root (F1 volatile) ──────────────────────────────────────────────────

    /// Fetches (and caches) the root for `repository`. `Ok(None)` on a 404
    /// miss — memoized like a hit, so a repeat ask costs nothing.
    ///
    /// # Errors
    ///
    /// [`Error::IndexHttpFailed`](super::error::Error::IndexHttpFailed) for any
    /// non-404 failure the transport surfaces. Only a *confirmed* 404 reads as a
    /// miss: this `None` is what [`Self::jurisdiction`] settles an `Outside`
    /// verdict off, and it is memoized, so no other status may fold into it.
    async fn resolve_root(&self, repository: &str) -> Result<Option<Arc<IndexRoot>>> {
        // The version gate runs before any root is consumed (F1). Absence is
        // v1, not a refusal (C-005) — an unsupported served version still is.
        self.check_format_version().await?;
        if let Some(root) = self.cache.read().await.roots.get(repository) {
            return Ok(root.clone());
        }
        let url = format!("{}/p/{}.json", self.base_url, repository);
        let root = match self.transport.get(&url).await? {
            IndexFetch::Found { bytes } => {
                let parsed: IndexRoot = parse_document(&bytes, &url)?;
                Some(Arc::new(parsed))
            }
            IndexFetch::NotFound => None,
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
        let bytes = match self.transport.get(&url).await? {
            IndexFetch::Found { bytes } => bytes,
            IndexFetch::NotFound => return Ok(None),
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

    /// Fetches this source's `c/index.json` — the site's own listing, read live.
    ///
    /// Nothing is persisted from it: the local `c/index.json` is authored from
    /// the roots this machine snapshotted, never mirrored from here. This is
    /// what answers `ocx index catalog --remote`, and it is the only reason the
    /// remote catalog is fetched at all.
    ///
    /// A `404` yields an empty catalog rather than an error — not a failure,
    /// just an index with nothing to list. That tolerance is right for a
    /// *listing* command and wrong for an enumeration that then acts on the
    /// result; see [`Self::fetch_catalog_strict`].
    pub async fn fetch_catalog(&self) -> Result<CatalogIndex> {
        Ok(self.fetch_catalog_document().await?.unwrap_or_else(CatalogIndex::new))
    }

    /// [`Self::fetch_catalog`], but an **absent** catalog document is an error
    /// rather than an empty one.
    ///
    /// For a caller that enumerates a source in order to act on the result,
    /// "this source serves no catalog" and "this source serves a catalog
    /// listing nothing" are different facts and only the second is a clean
    /// enumeration. `index sync` collapsed them and so exited
    /// 0 having refreshed nothing and printed nothing — C-013's
    /// authoritative-stop rule requires the stop.
    ///
    /// A served catalog with zero packages still returns `Ok` and still exits 0
    /// (C-027's Exit row): the source answered.
    pub async fn fetch_catalog_strict(&self) -> Result<CatalogIndex> {
        let url = format!("{}/c/index.json", self.base_url);
        self.fetch_catalog_document().await?.ok_or_else(|| {
            super::error::Error::CatalogDocumentAbsent {
                index_source: self.namespace.clone(),
                url,
            }
            .into()
        })
    }

    /// The catalog document, or `None` when the source serves none. The two
    /// public wrappers differ only in what they make of that `None`.
    async fn fetch_catalog_document(&self) -> Result<Option<CatalogIndex>> {
        // The version gate runs before the listing every other read fans out
        // from (F1).
        self.check_format_version().await?;
        let url = format!("{}/c/index.json", self.base_url);
        Ok(match self.transport.get(&url).await? {
            IndexFetch::NotFound => None,
            IndexFetch::Found { bytes } => Some(parse_document::<CatalogDocument>(&bytes, &url)?.into_packages()?),
        })
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
        let mut repositories: Vec<String> = self.fetch_catalog().await?.into_keys().collect();
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
        // The version gate runs before any root is consumed (F1).
        self.check_format_version().await?;
        let url = format!("{}/p/{}.json", self.base_url, identifier.repository());
        match self.transport.get(&url).await? {
            IndexFetch::Found { bytes } => {
                let root: IndexRoot = parse_document(&bytes, &url)?;
                Ok(Some((bytes, root)))
            }
            // A 404 is a clean miss, never an error.
            IndexFetch::NotFound => Ok(None),
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

    async fn jurisdiction(&self, identifier: &oci::Identifier) -> super::Jurisdiction {
        // Forwards to the inherent method (same shape as `namespace()`) so the
        // one caller that holds a concrete `OcxIndex` — `ocx index update`'s
        // source routing — reaches it without the private trait.
        OcxIndex::jurisdiction(self, identifier).await
    }

    fn serves_registry(&self, registry: &str) -> bool {
        OcxIndex::serves_registry(self, registry)
    }

    fn trusted_hosts(&self) -> &[String] {
        OcxIndex::trusted_hosts(self)
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

    /// url → body bytes. A present entry is a `200`, an absent one a `404`.
    type StubResponses = Arc<Mutex<HashMap<String, Vec<u8>>>>;
    /// Recorded request URLs, for assertions.
    type StubRequests = Arc<Mutex<Vec<String>>>;

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
            self.responses.lock().unwrap().insert(url.to_string(), bytes.to_vec());
        }

        fn fail(&self, url: &str) {
            self.failures.lock().unwrap().insert(url.to_string());
        }

        fn request_urls(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }

        fn request_count(&self, url: &str) -> usize {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|requested| *requested == url)
                .count()
        }
    }

    #[async_trait]
    impl IndexTransport for StubIndexTransport {
        async fn get(&self, url: &str) -> Result<IndexFetch> {
            self.requests.lock().unwrap().push(url.to_string());
            if self.failures.lock().unwrap().contains(url) {
                return Err(super::super::error::Error::IndexHttpFailed {
                    url: url.to_string(),
                    source: "simulated transport failure".into(),
                }
                .into());
            }
            let responses = self.responses.lock().unwrap();
            match responses.get(url) {
                Some(bytes) => Ok(IndexFetch::Found { bytes: bytes.clone() }),
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

    // ── config.json: absent is v1, unknown is fatal (C-004/C-005) ────────────

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
        assert_eq!(
            crate::cli::ClassifyExitCode::classify(&error),
            Some(crate::cli::ExitCode::DataError),
            "a served-but-unknown format_version is a data error (65)"
        );
    }

    #[tokio::test]
    async fn absent_config_and_absent_root_is_a_clean_miss() {
        // No config.json and no root registered. The miss now comes from the
        // ABSENT ROOT — an absent config is version 1 (C-005), so the root GET
        // is issued rather than short-circuited.
        let transport = StubIndexTransport::new();
        let source = make_source(transport.clone(), false);
        let result = source
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .unwrap();
        assert!(result.is_none(), "an empty base must miss cleanly, not error");
        assert!(
            transport.request_urls().contains(&root_url()),
            "the root must be asked for — the absent config no longer short-circuits it"
        );
    }

    #[tokio::test]
    async fn absent_config_is_version_one_and_the_root_resolves() {
        // C-005, the inverse of the behaviour this file shipped: a tree with a
        // valid root + dispatch object but NO config.json is a v1 index, and it
        // resolves. This is the defect the whole change exists to fix — such a
        // tree is exactly what `ocx index update` produces.
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
            result.is_some(),
            "a valid root under a config-less tree must resolve — absent means v1 (C-005)"
        );
        assert!(
            transport.request_urls().contains(&root_url()),
            "the root document must be fetched"
        );
    }

    #[tokio::test]
    async fn an_assumed_v1_is_never_memoized() {
        // C-005: only a SERVED config.json is memoized. An assumed v1 is
        // re-derived every call, so a tree that publishes one later is picked
        // up without restarting the process.
        let transport = StubIndexTransport::new();
        let source = make_source(transport.clone(), false);

        assert!(source.fetch_catalog().await.unwrap().is_empty());
        assert!(source.fetch_catalog().await.unwrap().is_empty());

        assert_eq!(
            transport.request_count(&config_url()),
            2,
            "an assumed v1 must not be cached — every call re-asks for config.json"
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
            OcxIndex::resolve_base_url(&empty, "ocx.sh", &no_mirrors(), &[])
                .unwrap()
                .url,
            DEFAULT_INDEX_BASE_URL,
            "no [registries.\"ocx.sh\"] index field must yield the default base URL"
        );

        let config: crate::config::Config =
            toml::from_str("[registries.\"ocx.sh\"]\nindex = \"https://artifactory.corp/ocx-index/\"").unwrap();
        assert_eq!(
            OcxIndex::resolve_base_url(&config, "ocx.sh", &no_mirrors(), &[])
                .unwrap()
                .url,
            "https://artifactory.corp/ocx-index",
            "[registries.\"<ns>\"] index must replace the base URL (trailing slash trimmed)"
        );
        assert_eq!(
            OcxIndex::resolve_base_url(&config, "other.sh", &no_mirrors(), &[])
                .unwrap()
                .url,
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
            OcxIndex::resolve_base_url(&config, "ocx.sh", &no_mirrors(), &insecure)
                .unwrap()
                .url,
            "http://mirror.corp/ocx-index",
            "an http base is allowed when its host is in OCX_INSECURE_REGISTRIES"
        );

        // The default https base URL is never gated.
        assert_eq!(
            OcxIndex::resolve_base_url(&crate::config::Config::default(), "ocx.sh", &no_mirrors(), &[])
                .unwrap()
                .url,
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
            OcxIndex::resolve_base_url(&crate::config::Config::default(), "ocx.sh", &mirrors_index, &[])
                .unwrap()
                .url,
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
            OcxIndex::resolve_base_url(&crate::config::Config::default(), "ocx.sh", &unrelated_mirror, &[])
                .unwrap()
                .url,
            DEFAULT_INDEX_BASE_URL,
            "a mirror keyed by an unrelated host must not affect this base URL"
        );
    }

    // ── closed scheme set (C-018/C-019/C-020) ────────────────────────────────

    fn index_config(url: &str) -> crate::config::Config {
        toml::from_str(&format!("[registries.\"ocx.sh\"]\nindex = \"{url}\"")).unwrap()
    }

    /// The `InvalidIndexUrl` a refused scheme must raise, with its exit code.
    fn expect_invalid_index_url(error: &crate::Error, expected_origin: &str) {
        let crate::Error::OciIndex(super::super::error::Error::InvalidIndexUrl { origin, .. }) = error else {
            panic!("expected InvalidIndexUrl, got {error:?}");
        };
        assert_eq!(*origin, expected_origin, "the diagnostic must name the setting to fix");
        assert_eq!(
            crate::cli::ClassifyExitCode::classify(error),
            Some(crate::cli::ExitCode::ConfigError),
            "a refused index scheme is a config error (78)"
        );
    }

    #[test]
    fn a_refused_index_url_is_redacted() {
        // CWE-532: the refusal names the offending URL, and a configured base
        // may embed credentials.
        let error = OcxIndex::resolve_base_url(
            &index_config("ftp://alice:hunter2@mirror.corp/ocx-index"),
            "ocx.sh",
            &no_mirrors(),
            &[],
        )
        .expect_err("ftp is outside the closed scheme set");
        let rendered = error.to_string();
        assert!(!rendered.contains("hunter2"), "userinfo must not reach the message");
        assert!(
            rendered.contains("mirror.corp"),
            "the host must survive — the operator has to recognise the entry"
        );
    }

    #[test]
    fn resolve_base_url_refuses_a_scheme_outside_the_closed_set() {
        // C-018 last row, at check 1. Today such a base flows through
        // `parse_url` and fails later as a transport error — wrong class,
        // wrong moment.
        for base in ["ftp://mirror.corp/ocx-index", "gopher://mirror.corp"] {
            let error = OcxIndex::resolve_base_url(&index_config(base), "ocx.sh", &no_mirrors(), &[])
                .expect_err("a scheme outside the closed set must be refused");
            expect_invalid_index_url(&error, super::super::error::INDEX_URL_FROM_REGISTRIES);
        }
    }

    #[test]
    fn resolve_base_url_accepts_a_file_base() {
        // C-018 `file` row: empty authority + absolute path, yielding a
        // `file://<abs>` base. The scheme of the returned URL is what proves
        // the `FileIndexTransport` branch was taken — the two are one decision.
        let base = OcxIndex::resolve_base_url(&index_config("file:///srv/ocx-index"), "ocx.sh", &no_mirrors(), &[])
            .expect("a file:// base with an empty authority and an absolute path is permitted");
        assert_eq!(base.url, "file:///srv/ocx-index");

        let trimmed = OcxIndex::resolve_base_url(&index_config("file:///srv/ocx-index/"), "ocx.sh", &no_mirrors(), &[])
            .expect("a trailing slash is trimmed, matching every other base");
        assert_eq!(trimmed.url, "file:///srv/ocx-index");

        // A drive with a directory under it stays valid — the bare-drive
        // refusal must not swallow the Windows form C-018's table admits.
        let drive = OcxIndex::resolve_base_url(&index_config("file:///C:/srv/x"), "ocx.sh", &no_mirrors(), &[])
            .expect("a drive-qualified path is a valid file base");
        assert_eq!(drive.url, "file:///C:/srv/x");
    }

    #[test]
    fn resolve_base_url_refuses_a_file_base_with_an_authority() {
        // C-019: a non-empty authority is a UNC/remote form, never a local
        // tree. `localhost` is the interesting one — `parse_url` accepts it
        // (WP4's regression guard pins that), so this gate is the only refusal.
        // `file://srv` has no `/` at all, so `srv` is the authority.
        for base in ["file://localhost/srv/x", "file://host.example/srv/x", "file://srv"] {
            let error = OcxIndex::resolve_base_url(&index_config(base), "ocx.sh", &no_mirrors(), &[])
                .expect_err("a file:// base needs an empty authority");
            expect_invalid_index_url(&error, super::super::error::INDEX_URL_FROM_REGISTRIES);
        }
    }

    #[test]
    fn resolve_base_url_refuses_a_file_base_naming_no_directory() {
        // C-018's absolute-path half, which is a separate row from C-019: these
        // all have an EMPTY authority and are refused for naming no directory.
        // `file:///C:/` is the dangerous one — see the assertion below.
        for base in ["file://", "file:///", "file:///C:/", "file:///c:"] {
            let error = OcxIndex::resolve_base_url(&index_config(base), "ocx.sh", &no_mirrors(), &[])
                .expect_err("a file:// base must name a directory");
            expect_invalid_index_url(&error, super::super::error::INDEX_URL_FROM_REGISTRIES);
        }
    }

    #[test]
    #[cfg(windows)]
    fn a_bare_windows_drive_is_not_an_absolute_path() {
        // The rule the test above rests on, and the reason a bare drive cannot
        // be waved through as "it starts with a drive letter, so it is
        // absolute": `C:` carries a `Prefix(Disk)` component with no `RootDir`,
        // so Win32 resolves it against the per-drive working directory. An
        // index base that resolved there would serve every document — including
        // the root, the trust anchor of the resolve — out of whatever directory
        // `ocx` happened to be launched from.
        assert!(!std::path::Path::new("C:").is_absolute());
        assert!(std::path::Path::new("C:/").is_absolute());
    }

    #[test]
    fn mirrors_may_not_route_index_traffic_to_file() {
        // C-020 through C-018 check 2: the override replaces the scheme AFTER
        // check 1 ran, so a base-only check is bypassed by any `[mirrors]`
        // entry — including one injected through `OCX_MIRRORS`, which resolves
        // into this same map. The diagnostic must name `[mirrors]`, not the
        // configured base: that is where the operator's mistake is.
        let mut mirrors_index = BTreeMap::new();
        mirrors_index.insert(
            "up.example".to_string(),
            crate::config::mirror::parse_url("file://localhost/srv/x").unwrap(),
        );

        let error = OcxIndex::resolve_base_url(&index_config("https://up.example"), "ocx.sh", &mirrors_index, &[])
            .expect_err("a [mirrors] index-role override may not route index traffic to file://");
        expect_invalid_index_url(&error, &super::super::error::index_url_from_mirrors("up.example"));
    }

    #[test]
    fn a_file_base_ignores_mirrors_entirely() {
        // C-018 `file` row: the override is host-keyed and a `file` base has no
        // host, so check 1 diverts before the map is ever consulted. Keyed by
        // the literal authority a naive parse would produce, to pin that.
        let mut mirrors_index = BTreeMap::new();
        mirrors_index.insert(
            String::new(),
            crate::config::mirror::parse_url("https://artifactory.corp").unwrap(),
        );

        let base = OcxIndex::resolve_base_url(&index_config("file:///srv/x"), "ocx.sh", &mirrors_index, &[])
            .expect("a file base resolves without consulting the host-keyed mirror map");
        assert_eq!(base.url, "file:///srv/x");
    }

    #[test]
    fn a_file_url_tail_is_an_absolute_path() {
        // C-018's Windows row. `/C:/srv/x` is not an absolute path — the
        // drive-letter form needs its leading separator stripped — while
        // `/srv/x` already is one and must survive untouched.
        //
        // This pins the STRIPPING rule only. It is deliberately blind to
        // whether the result is absolute: `file_root("/C:")` returns `C:`, and
        // the assertions below would accept that. The absoluteness rule is
        // `resolve_file_base`'s, pinned by
        // `resolve_base_url_refuses_a_file_base_naming_no_directory`.
        assert!(has_drive_prefix("/C:/srv/x"), "the Windows drive-letter tail");
        assert!(has_drive_prefix("/c:/srv/x"), "the letter case is irrelevant");
        assert!(
            !has_drive_prefix("/srv/x"),
            "a Unix absolute path is not drive-prefixed"
        );
        assert!(!has_drive_prefix("/CC:/srv/x"), "only a single-letter drive qualifies");

        assert_eq!(
            file_root("/srv/x"),
            std::path::PathBuf::from("/srv/x"),
            "a Unix absolute path is used verbatim on every platform"
        );
        let drive = file_root("/C:/srv/x");
        assert_eq!(
            drive,
            std::path::PathBuf::from(if cfg!(windows) { "C:/srv/x" } else { "/C:/srv/x" }),
            "the leading separator is stripped only where the drive form is meaningful"
        );
    }

    // ── catalog sync (F2): digest diff ───────────────────────────────────────

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
            source.jurisdiction(&logical).await,
            super::super::Jurisdiction::Authoritative,
            "the source owns its namespace"
        );

        // A foreign namespace is neither rewritten nor owned.
        let foreign =
            oci::Identifier::new_registry("x/y", "ghcr.io").clone_with_digest(oci::Digest::Sha256("b".repeat(64)));
        assert!(source.physical_reference(&foreign).await.unwrap().is_none());
        assert_eq!(source.jurisdiction(&foreign).await, super::super::Jurisdiction::Outside);
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

    // ── fetch_root_document: verbatim published-root fetch (A2/F1) ───────────
    //
    // A published source serves the verbatim `p/<ns>/<pkg>.json` bytes paired
    // with the parsed root so `LocalIndex::persist_published_root` can grow the
    // local copy byte-for-byte (copy-a-mirror).

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

    // ── jurisdiction: the INDEX declares what it can express, the client asks ──
    //
    // `name_segments` in `config.json` is the index operator's own published
    // statement about its name grammar. A name it declares inexpressible is
    // outside that source's jurisdiction: never asked, its silence decides
    // nothing, and the chain falls through to the registry. Every other outcome
    // — absent, malformed, unsupported, unreachable config — stays
    // AUTHORITATIVE, so an index outage can never silently downgrade a
    // namespace to plain OCI.

    const FLAT_REPO: &str = "go-task";

    fn flat_id() -> oci::Identifier {
        oci::Identifier::new_registry(FLAT_REPO, NAMESPACE).clone_with_tag("3")
    }

    fn flat_root_url() -> String {
        format!("{BASE}/p/{FLAT_REPO}.json")
    }

    /// Seeds `config.json` with the given body plus a resolvable root for the
    /// namespaced name. The FLAT name is deliberately left un-served: the
    /// declaration only decides what its 404 means, so a seeded flat root would
    /// make every "declined" test authoritative by evidence.
    fn seed_with_config(transport: &StubIndexTransport, config_body: &[u8]) {
        seed_package(transport, false);
        transport.insert(&config_url(), config_body); // seed_package writes its own
    }

    #[tokio::test]
    async fn jurisdiction_is_outside_for_a_foreign_registry_and_issues_no_request() {
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1,"name_segments":2}"#);
        let source = make_source(transport.clone(), false);

        let foreign = oci::Identifier::new_registry(REPO, "ghcr.io").clone_with_tag("3.28");
        assert_eq!(source.jurisdiction(&foreign).await, super::super::Jurisdiction::Outside);
        assert_eq!(
            transport.request_urls(),
            Vec::<String>::new(),
            "a foreign registry is decided with no I/O — not even config.json"
        );
    }

    #[tokio::test]
    async fn jurisdiction_is_outside_for_a_name_the_declared_grammar_cannot_express() {
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1,"name_segments":2}"#);
        let source = make_source(transport.clone(), false);

        assert_eq!(
            source.jurisdiction(&flat_id()).await,
            super::super::Jurisdiction::Outside,
            "the index declares 2-segment names AND holds no root for `ocx.sh/go-task`"
        );
        // The measured cost of the verdict, asserted rather than assumed: the
        // config.json the resolve would fetch anyway, plus ONE root request that
        // 404s. The declaration is what turns that 404 from a terminal stop into
        // a fall-through — it never decides whether to ask.
        assert_eq!(
            transport.request_urls(),
            vec![config_url(), flat_root_url()],
            "one memoized 404 per declined name, nothing more"
        );
        source.jurisdiction(&flat_id()).await;
        assert_eq!(
            transport.request_count(&flat_root_url()),
            1,
            "the miss is memoized — a repeat consult costs no request"
        );
    }

    #[tokio::test]
    async fn only_a_confirmed_404_settles_a_declined_name_as_outside() {
        // A root fetch that FAILS is not an absence. Folding any non-404 answer
        // into the 404 miss would let one bad response hand a declined name to
        // plain OCI — and memoize that verdict for the process. Fail-closed
        // instead: `resolve_root` errors, and an errored root keeps the source
        // authoritative. (`IndexFetch` carries exactly `Found` and `NotFound`,
        // so every other status — a `304` from a misbehaving edge included —
        // reaches here as an error by construction.)
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1,"name_segments":2}"#);
        transport.fail(&flat_root_url());
        let source = make_source(transport, false);

        assert_eq!(
            source.jurisdiction(&flat_id()).await,
            super::super::Jurisdiction::Authoritative,
            "only a confirmed 404 may hand a declined name to the registry"
        );
        let error = source
            .resolve_root(FLAT_REPO)
            .await
            .expect_err("a failed root fetch is a protocol failure, not a miss");
        assert!(
            error.to_string().contains(&flat_root_url()),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn a_wrong_declaration_cannot_bypass_the_yank_gate() {
        // The bypass this design closes. `name_segments` rides an unsigned,
        // CDN-cacheable JSON file on the same channel as the roots: a template
        // bug, a stale edge, or a compromise scoped to that one file can declare
        // a WRONG count. If the declaration decided whether to ask, the client
        // would stop asking about a name the index does hold a yanking root for,
        // and the yanked build would install through the plain-OCI catch-all.
        // The root exists, so the root decides.
        let transport = StubIndexTransport::new();
        seed_package(&transport, true);
        transport.insert(&config_url(), br#"{"format_version":1,"name_segments":3}"#);
        let source = make_source(transport, false);
        let registry = RegistryStub::new();

        assert_eq!(
            source.jurisdiction(&tagged_id()).await,
            super::super::Jurisdiction::Authoritative,
            "`kitware/cmake` has 2 segments, not the declared 3 — but the index holds its root"
        );

        let dir = tempfile::tempdir().unwrap();
        let chained = chain_with(&dir, source, registry.clone());
        let error = chained
            .fetch_manifest(&tagged_id(), IndexOperation::Resolve)
            .await
            .expect_err("a yanked tag must be refused however the config mis-declares the grammar");
        assert!(error.to_string().contains("yanked"), "unexpected error: {error}");
        assert_eq!(registry.calls(), 0, "the registry must never shadow the yank refusal");
    }

    #[tokio::test]
    async fn jurisdiction_is_authoritative_for_a_name_matching_the_declared_grammar() {
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1,"name_segments":2}"#);
        let source = make_source(transport, false);

        assert_eq!(
            source.jurisdiction(&tagged_id()).await,
            super::super::Jurisdiction::Authoritative,
            "`ocx.sh/kitware/cmake` is exactly what the index declared it serves"
        );
    }

    #[tokio::test]
    async fn jurisdiction_is_authoritative_when_the_config_declares_no_grammar() {
        // R1's private index: it declares nothing, so the client never narrows
        // it. Today's behaviour, verbatim — including for a flat name.
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1}"#);
        let source = make_source(transport, false);

        assert_eq!(
            source.jurisdiction(&flat_id()).await,
            super::super::Jurisdiction::Authoritative
        );
    }

    #[tokio::test]
    async fn jurisdiction_is_authoritative_when_config_json_is_absent() {
        let transport = StubIndexTransport::new();
        let source = make_source(transport, false);
        assert_eq!(
            source.jurisdiction(&flat_id()).await,
            super::super::Jurisdiction::Authoritative,
            "a 404 config.json is not a declaration that the name is inexpressible"
        );
    }

    #[tokio::test]
    async fn jurisdiction_is_authoritative_when_the_config_fetch_fails() {
        // Fail CLOSED: an index that cannot be asked what it serves must not be
        // assumed to serve nothing, or an outage silently downgrades the whole
        // namespace to plain OCI.
        let transport = StubIndexTransport::new();
        transport.fail(&config_url());
        let source = make_source(transport, false);
        assert_eq!(
            source.jurisdiction(&flat_id()).await,
            super::super::Jurisdiction::Authoritative
        );
        // Not swallowed: nothing was cached, so the resolve that follows on the
        // same source re-fetches and raises the real transport error.
        assert!(
            source.fetch_root_document(&flat_id()).await.is_err(),
            "the deferred error must surface loud on the very next read"
        );
    }

    #[tokio::test]
    async fn jurisdiction_is_authoritative_when_the_config_is_malformed() {
        for body in [
            &b"not json at all"[..],
            // `name_segments: 0` is rejected by NonZeroU32 — same malformed path,
            // no hand-written validator.
            &br#"{"format_version":1,"name_segments":0}"#[..],
        ] {
            let transport = StubIndexTransport::new();
            transport.insert(&config_url(), body);
            let source = make_source(transport, false);
            assert_eq!(
                source.jurisdiction(&flat_id()).await,
                super::super::Jurisdiction::Authoritative,
                "a malformed config declares nothing: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[tokio::test]
    async fn jurisdiction_reuses_the_memoized_config() {
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1,"name_segments":2}"#);
        let source = make_source(transport.clone(), false);

        source.jurisdiction(&tagged_id()).await;
        source.jurisdiction(&tagged_id()).await;
        source.resolve_root(REPO).await.unwrap();
        assert_eq!(
            transport.request_count(&config_url()),
            1,
            "the probe rides the same read-once config.json fetch the resolve makes"
        );
    }

    // ── chain routing: fall through for a declined name, fail closed otherwise ─

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

    #[tokio::test]
    async fn a_declared_out_of_jurisdiction_name_falls_through_to_the_registry() {
        // The measured bug: with an index configured for ocx.sh, `ocx.sh/go-task`
        // resolved ONLY when no index was configured.
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1,"name_segments":2}"#);
        let source = make_source(transport.clone(), false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = chain_with(&dir, source, registry.clone());

        let (digest, _) = chained
            .fetch_manifest(&flat_id(), IndexOperation::Resolve)
            .await
            .unwrap()
            .expect("a declined name resolves through the plain-OCI registry");
        assert_eq!(digest, registry_manifest().1);
        assert_eq!(
            transport.request_count(&flat_root_url()),
            1,
            "the declined name costs exactly one memoized 404, then falls through: {:?}",
            transport.request_urls()
        );
    }

    #[tokio::test]
    async fn an_expressible_name_keeps_the_terminal_stop_on_a_clean_miss() {
        // Fail-closed survives for every name the index CAN express: an absent
        // root is terminal, never a fall-through the registry could shadow.
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1,"name_segments":2}"#);
        let source = make_source(transport, false);
        let registry = RegistryStub::new();

        let dir = tempfile::tempdir().unwrap();
        let chained = chain_with(&dir, source, registry.clone());

        let absent = oci::Identifier::new_registry("ns/absent", NAMESPACE).clone_with_tag("1.0");
        assert!(
            chained
                .fetch_manifest(&absent, IndexOperation::Resolve)
                .await
                .unwrap()
                .is_none(),
            "an authoritative source's clean miss is terminal"
        );
        assert_eq!(registry.calls(), 0, "the registry must never be consulted");
    }

    #[tokio::test]
    async fn an_expressible_name_keeps_the_yank_gate() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, true);
        transport.insert(&config_url(), br#"{"format_version":1,"name_segments":2}"#);
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
    async fn an_undeclared_index_still_stops_the_chain_for_a_flat_name() {
        // R1's exact vulnerability. A PRIVATE index that declares no
        // `name_segments` keeps full authority over every name in its
        // namespace — including a flat one — so its yank refusal is never
        // bypassed by the plain-OCI catch-all.
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
            .expect_err("an undeclared index owns every name in its namespace");
        assert!(error.to_string().contains("yanked"), "unexpected error: {error}");
        assert_eq!(
            registry.calls(),
            0,
            "the yanked build must never be resolvable through the registry"
        );
    }

    #[tokio::test]
    async fn physical_reference_and_fetch_blob_skip_an_out_of_jurisdiction_source() {
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1,"name_segments":2}"#);
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
            "both paths skip the declined source off ONE memoized jurisdiction probe: {:?}",
            transport.request_urls()
        );
    }
}
