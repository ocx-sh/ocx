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
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::RwLock;

use super::wire::{CatalogDocument, CatalogIndex, IndexFormatConfig, IndexRoot, RootTag, gate_format_version};
use super::{IndexOperation, error, index_impl};
use crate::oci::transport_policy::{self, Attempt, RetryBudget, RetryPolicy, TransportHardening};
use crate::utility::fs::path::{FileReference, Spelling};
use crate::utility::singleflight::{self, Acquisition};
use crate::{Result, log, oci, oci::client::ReadAddressing};

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

/// Per-frame idle bound for an index document fetch (CWE-400), replacing the
/// hard 60 s total-request deadline this path used to carry
/// (`adr_index_sync_performance.md` D-011).
///
/// The old deadline bought slowloris protection at the price of the throttled
/// link: a root document arriving honestly over 90 s through a corporate proxy
/// failed at 60 s with nothing wrong. An idle bound gives both — a connection
/// that goes genuinely quiet for 30 s still fires, an honest slow body never
/// does, however long it runs.
const INDEX_IDLE_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// Per-attempt outer cap for an index document fetch, the backstop
/// [`INDEX_IDLE_BOUND`] alone cannot provide.
///
/// A peer dribbling one byte every 29 s never trips the idle bound and never
/// reaches [`MAX_INDEX_DOCUMENT_BYTES`] in any human timeframe; multiplied by
/// the sync fan-out, the run simply does not terminate.
///
/// # Why this is a memory bound, not only a liveness one
///
/// [`MAX_INDEX_DOCUMENT_BYTES`] is 32 MiB of *per-request* allocation,
/// accumulated into an in-memory `Vec` by the body loop below. The sync's
/// ceiling is `INDEX_REFRESH_CONCURRENCY` (8) x `TAG_REFRESH_CONCURRENCY` (64)
/// = 512 in-flight requests, so the resident high-water mark is 16 GiB. The
/// byte cap bounds that peak; only a deadline bounds the peak's *duration*, and
/// duration is what turns a peak into exhaustion. Today's 60 s total deadline
/// is what stops a hostile peer holding all 512 allocations at once, so
/// dropping it with nothing in its place would be a regression rather than a
/// relaxation. The retry ladder compounds it on the traffic axis: a peer
/// serving `MAX_INDEX_DOCUMENT_BYTES - 1` and then resetting is a retryable
/// transport error, so cumulative transfer per logical fetch is up to 3x the
/// cap — bounded by the attempt count, which is why that bound is not optional
/// either.
///
/// Generous because it is a backstop and not an SLA: it must not fire on any
/// honest transfer.
const INDEX_OUTER_CAP: std::time::Duration = std::time::Duration::from_secs(300);

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
/// # Retry and budget
///
/// Every clone shares one [`RetryBudget`]: `IndexTransport` requires
/// `box_clone` and the sync fan-out builds one index — and so one cloned
/// transport — per package, so counters held as plain fields would meter
/// per-package, which is exactly the uncapped per-request policy the budget
/// replaces (D-010 rule 2).
#[derive(Clone)]
pub struct ReqwestIndexTransport {
    client: reqwest::Client,
    policy: RetryPolicy,
    budget: RetryBudget,
}

/// Builds the index HTTP client with the bundled Mozilla CA roots, under
/// `hardening`'s three timeout bounds.
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
fn build_index_http_client(hardening: &TransportHardening) -> reqwest::Client {
    // Transport hardening applied to every build this function can return, so
    // no unhardened client can escape it: bounded connect, a per-frame idle
    // bound and a per-attempt outer cap (CWE-400, all three composed — see
    // `TransportHardening`), and no redirect following (CWE-918 / CWE-319) —
    // a static-file index needs no redirects, and a 3xx must not relocate the
    // fetch to http:// or an internal host AFTER the plain-HTTP gate in
    // `resolve_base_url` already ran.
    let harden = |builder: reqwest::ClientBuilder| {
        builder
            .connect_timeout(hardening.connect_timeout)
            .read_timeout(hardening.idle_bound)
            .timeout(hardening.outer_cap)
            .redirect(reqwest::redirect::Policy::none())
    };
    let builder = crate::utility::tls::seed_embedded_roots(harden(reqwest::Client::builder()));
    builder.build().unwrap_or_else(|error| {
        // The bundled-roots build cannot hit the empty-store panic (roots are
        // non-empty). A different init failure is not expected; fall back so
        // construction stays infallible, logging so it is not silent — the
        // fallback keeps the same timeout + no-redirect hardening.
        //
        // There is deliberately no third arm. It used to be a bare
        // `reqwest::Client::new()`, which carries reqwest's defaults — no
        // timeouts, redirects *followed* up to 10 hops — i.e. remote-controlled
        // egress that could relocate the fetch to http:// after the plain-HTTP
        // gate already ran. It also bought nothing: `Client::new()` is itself
        // `builder().build().expect(..)`, so "this build fails but that one
        // succeeds" is unreachable, and the arm only traded a panic for an
        // unhardened client (D-011b).
        log::warn!("index HTTP client build with bundled roots failed ({error}); using hardened reqwest defaults");
        harden(reqwest::Client::builder())
            .build()
            .expect("a client with no custom roots and only timeout and redirect settings always builds")
    })
}

impl ReqwestIndexTransport {
    pub fn new() -> Self {
        Self::with_hardening(
            &TransportHardening {
                connect_timeout: INDEX_CONNECT_TIMEOUT,
                idle_bound: INDEX_IDLE_BOUND,
                outer_cap: INDEX_OUTER_CAP,
            },
            RetryPolicy::default(),
        )
    }

    /// Construction with the bounds injected — the seam D-011 keeps so a
    /// fixture can assert the same semantics in milliseconds rather than
    /// waiting out the shipped minutes against a real socket.
    fn with_hardening(hardening: &TransportHardening, policy: RetryPolicy) -> Self {
        Self {
            client: build_index_http_client(hardening),
            policy,
            budget: RetryBudget::new(),
        }
    }

    /// One attempt at `url`: everything from dispatching the request to the
    /// last body byte, classified for the ladder above it.
    ///
    /// Retryable outcomes carry the terminal value the caller gets if no
    /// further attempt is admitted, so giving up costs nothing extra and never
    /// changes the error the caller sees.
    async fn attempt(client: &reqwest::Client, url: &str) -> Attempt<Result<IndexFetch>> {
        let mut response = match client.get(url).send().await {
            Ok(response) => response,
            Err(source) => return Self::transport_failure(url, None, source),
        };

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Attempt::Done(Ok(IndexFetch::NotFound));
        }
        // Everything else — including a `304` answering this unconditional
        // `GET` (RFC 9110 §15.4.5, a misbehaving edge) — is an error. Only a
        // confirmed `404` above may read as absence: that `None` is what
        // [`OcxIndex::jurisdiction`] settles an `Outside` verdict off, and the
        // verdict is memoized, so one bad response would decide a name for the
        // rest of the process.
        if !status.is_success() {
            // Parsed before the error is built: ACR counts `Retry-After` down
            // across polls, so every attempt reads its own header and none
            // caches the first value seen.
            let retry_after = transport_policy::honours_retry_after(status.as_u16())
                .then(|| {
                    response
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| transport_policy::parse_retry_after(value, std::time::SystemTime::now()))
                })
                .flatten();
            let failure = super::error::Error::IndexHttpFailed {
                url: redact_url(url),
                status: Some(status.as_u16()),
                source: format!("unexpected status {status}").into(),
            };
            // The classifier reads the typed field, not the formatted message
            // (D-010a/C-024) — which is what makes the field load-bearing
            // rather than decoration.
            let retryable = matches!(
                &failure,
                super::error::Error::IndexHttpFailed { status: Some(code), .. }
                    if transport_policy::is_retryable_status(*code)
            );
            return if retryable {
                Attempt::Retry {
                    retry_after,
                    terminal: Err(failure.into()),
                }
            } else {
                Attempt::Done(Err(failure.into()))
            };
        }

        // Reject a declared oversize body before reading a single byte (CWE-400).
        // Not retryable: the server has already stated the size, and asking
        // again gets the same answer.
        if let Some(declared) = response.content_length()
            && declared > MAX_INDEX_DOCUMENT_BYTES as u64
        {
            return Attempt::Done(Err(super::error::Error::IndexHttpFailed {
                url: redact_url(url),
                status: Some(status.as_u16()),
                source: format!(
                    "response body {declared} bytes exceeds the {MAX_INDEX_DOCUMENT_BYTES}-byte index-document cap"
                )
                .into(),
            }
            .into()));
        }

        // Stream the body under a hard cap (CWE-400): a server that omits or lies
        // about Content-Length (chunked transfer, or a hostile endpoint) still
        // cannot stream more than the cap into memory — the running total is
        // checked before each chunk is appended. The cap, not the timeout, is
        // what bounds memory here, and relaxing the deadline does not relax it.
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                // A mid-body failure re-issues the whole `GET`, which is safe
                // because every request on this path is idempotent (D-010c).
                Err(source) => return Self::transport_failure(url, Some(status.as_u16()), source),
                Ok(None) => break,
                Ok(Some(chunk)) => {
                    if body.len() + chunk.len() > MAX_INDEX_DOCUMENT_BYTES {
                        return Attempt::Done(Err(super::error::Error::IndexHttpFailed {
                            url: redact_url(url),
                            status: Some(status.as_u16()),
                            source: format!(
                                "response body exceeds the {MAX_INDEX_DOCUMENT_BYTES}-byte index-document cap"
                            )
                            .into(),
                        }
                        .into()));
                    }
                    body.extend_from_slice(&chunk);
                }
            }
        }
        Attempt::Done(Ok(IndexFetch::Found { bytes: body }))
    }

    /// Wraps a `reqwest` failure as [`Error::IndexHttpFailed`], retryable only
    /// for the transient transport class (connect, timeout, reset/close).
    fn transport_failure(url: &str, status: Option<u16>, source: reqwest::Error) -> Attempt<Result<IndexFetch>> {
        let retryable = transport_policy::is_retryable_transport_error(&source);
        let failure: Result<IndexFetch> = Err(super::error::Error::IndexHttpFailed {
            url: redact_url(url),
            status,
            source: Box::new(source),
        }
        .into());
        if retryable {
            Attempt::Retry {
                retry_after: None,
                terminal: failure,
            }
        } else {
            Attempt::Done(failure)
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
    /// Fetches `url`, retrying the transient classes under
    /// [`RetryPolicy`] and the run-global [`RetryBudget`] (D-010).
    ///
    /// The ladder wraps the whole attempt — dispatch through last body byte —
    /// because a `GET` on this path is idempotent, so re-issuing one is always
    /// safe. It is deliberately not wrapped in a wall-clock cap of its own:
    /// retry *volume* is what the budget bounds, and per-attempt duration is
    /// what `TransportHardening::outer_cap` bounds.
    async fn get(&self, url: &str) -> Result<IndexFetch> {
        let client = &self.client;
        let policy = &self.policy;
        transport_policy::run(policy, &self.budget, move |attempt| async move {
            if attempt > 0 {
                // `debug!`, never `warn!`: a retried transient is a common
                // benign state, and an operator-facing warning per retry across
                // a 512-wide fan-out is noise, not signal (S-003). Redacted
                // because an index base URL may embed `user:password@`
                // (CWE-532) — same reason every error below this line is.
                log::debug!(
                    "retrying index request to {} (attempt {} of {})",
                    redact_url(url),
                    attempt + 1,
                    policy.attempts
                );
            }
            Self::attempt(client, url).await
        })
        .await
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

/// The tail of a `file:` base written with a single slash (`file:/srv/x`),
/// which carries no `://` for [`scheme_of`] to find.
///
/// Matched case-insensitively, because [`scheme_of`] lowercases its own token
/// and a base is not more or less valid for being shouted. `file://…` is
/// deliberately not matched: that spelling has a scheme and belongs to
/// [`resolve_file_base`].
fn file_colon_tail(url: &str) -> Option<&str> {
    let (prefix, tail) = url.split_at_checked("file:".len())?;
    (prefix.eq_ignore_ascii_case("file:") && !tail.starts_with("//")).then_some(tail)
}

/// The [`Error::InvalidIndexUrl`](error::Error::InvalidIndexUrl) `origin` for a
/// single-slash `file:` base.
///
/// The correction rides `origin` because that field is what an operator reads
/// to learn which setting to change, and it is the only part of the message
/// this module composes.
///
/// A tail that is not absolute gets the shape rather than a literal: turning
/// `file:srv/x` into `file://srv/x` would name a non-empty authority, a
/// spelling [`resolve_file_base`] refuses in turn, and inventing a leading
/// slash would name a directory the operator never wrote.
fn file_colon_origin(tail: &str) -> String {
    let from = error::INDEX_URL_FROM_REGISTRIES;
    if tail.starts_with('/') {
        format!("{from}; a file base needs two more slashes, as \"file://{tail}\"")
    } else {
        format!("{from}; a file base is written \"file:///<absolute path>\"")
    }
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
    // `FileReference::absolute` is both the empty-authority check and the
    // absolute-path check: a payload that does not lead with `/` has an
    // authority before its first slash, and one that trims away to nothing
    // (`file:///`, `file://`) names no directory.
    //
    // `path.len() == 3` is the whole of `/C:` — a drive with nothing under it.
    // Refused on every platform, so a base is valid or not independently of
    // where it is read; `/C:` is not a directory anyone means on Unix either.
    let path = FileReference::parse(base)
        .absolute()
        .filter(|path| !(has_drive_prefix(path) && path.len() == 3));
    let Some(path) = path else {
        return Err(invalid_index_url(
            namespace,
            base,
            error::INDEX_URL_FROM_REGISTRIES.to_string(),
            None,
        ));
    };
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
    /// entry is what keeps a name the index does not hold from re-asking the
    /// wire once per chain consult: it costs exactly one 404 per process, not
    /// one per source loop.
    roots: BTreeMap<String, Option<Arc<IndexRoot>>>,
    /// Set once `config.json` has been fetched and its `format_version`
    /// confirmed supported this invocation, so a repeat call skips the fetch
    /// (F1 "read once"). Never set on a served-but-unsupported version (a
    /// re-checked hard error, not a remembered steady state) NOR on an absent
    /// `config.json` (assumed v1 and re-derived every call, so a tree that
    /// later publishes one is picked up without restarting). Config-driven
    /// construction means there is no probe outcome to soften a transport
    /// failure into — that always propagates.
    config: Option<Arc<IndexFormatConfig>>,
}

/// Max keys a source's singleflight group admits.
///
/// **Sized for the run, not copied.** `chained_index.rs`'s `1024` was chosen
/// for a group whose keys are the identifiers of one refresh; these groups live
/// for the process and are shared across every `box_clone`, so one key accrues
/// per repository an `ocx index sync` touches. At `1024` a sync against a
/// registry holding more packages than that answers `CapacityExceeded` →
/// `TempFail(75)` — an exit code advertising "retry" for a condition no retry
/// inside the process can clear — on **successes** alone.
///
/// This is a memory backstop, never a throughput limit: the entries front
/// [`SourceCacheInner::roots`], which is itself unbounded, so a run that could
/// reach this ceiling has already stored strictly more in the map beside it.
const SOURCE_SINGLEFLIGHT_MAX_KEYS: usize = 1 << 20;

/// How long a coalesced caller blocks for the leader's fetch.
///
/// Derived from the transport underneath rather than copied: one index fetch is
/// up to [`RetryPolicy::attempts`] attempts, each bounded by
/// [`INDEX_OUTER_CAP`], plus a bounded backoff between them. A waiter that gave
/// up before the leader could finish would turn one slow-but-honest link into a
/// spurious `TempFail(75)` for every caller but one — the opposite of what the
/// coalescing is for. Four attempt-caps covers the three attempts and the
/// backoff between them.
const SOURCE_SINGLEFLIGHT_TIMEOUT: Duration = Duration::from_secs(INDEX_OUTER_CAP.as_secs() * 4);

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
    /// The `OCX_INSECURE_REGISTRIES` authorities this source may dial over
    /// plain HTTP — the same value its `client` carries as
    /// `plain_http_registries`. Read only to pick the dial scheme the SSRF
    /// floor's proxy question depends on
    /// ([`index_impl::IndexImpl::insecure_hosts`]).
    insecure_hosts: Vec<String>,
    /// Proxy-route rules for this source's own SSRF pre-flight
    /// ([`Self::physical_identifier`]) — see [`OcxIndexConfig::proxy_rules`].
    proxy_rules: Arc<oci::ssrf::ProxyRules>,
    cache: Arc<RwLock<SourceCacheInner>>,
    /// Coalesces the concurrent cold misses on `config.json`. Only a **served**
    /// document is broadcast as this group's answer — see
    /// [`Self::check_format_version`] for why an assumed v1 must not be.
    config_group: singleflight::Group<(), Option<Arc<IndexFormatConfig>>>,
    /// Coalesces the concurrent cold misses on `p/<repo>.json`, keyed exactly
    /// like [`SourceCacheInner::roots`].
    ///
    /// Shared across `box_clone` like `cache`, deliberately — and this is the
    /// half of `chained_index.rs`'s precedent that does **not** carry over
    /// there. That group's key does not encode write policy, so its
    /// `read_only()` / `remote_view()` build a *fresh* group rather than
    /// coalescing a read-only resolve onto a persisting leader. `OcxIndex`
    /// carries no write policy at all — nothing here writes locally — so
    /// sharing is safe and is what makes the coalescing reach the fan-out.
    root_group: singleflight::Group<String, Option<Arc<IndexRoot>>>,
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
    /// The authorities dialled over plain HTTP (`OCX_INSECURE_REGISTRIES` plus
    /// `[registries."<host>"] insecure`) — the same list `client` was built
    /// with. Empty = every dial is HTTPS.
    pub insecure_hosts: Vec<String>,
    /// Whether a physical dial is proxied, which decides whether the SSRF
    /// pre-flight resolves at all (ocx#407). Production passes
    /// [`oci::ssrf::proxy_rules`]; tests pass explicit rules so the verdict
    /// never depends on the developer's own environment.
    pub proxy_rules: Arc<oci::ssrf::ProxyRules>,
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
            insecure_hosts: config.insecure_hosts,
            proxy_rules: config.proxy_rules,
            cache: Arc::new(RwLock::new(SourceCacheInner::default())),
            // One key (`()`), so one slot is the whole capacity.
            config_group: singleflight::Group::new(1, SOURCE_SINGLEFLIGHT_TIMEOUT),
            root_group: singleflight::Group::new(SOURCE_SINGLEFLIGHT_MAX_KEYS, SOURCE_SINGLEFLIGHT_TIMEOUT),
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

    /// The authorities this source dials over plain HTTP, or empty when none
    /// were configured — see [`OcxIndexConfig::insecure_hosts`]. Decides which
    /// proxy setting applies to a physical dial, and nothing else.
    pub fn insecure_hosts(&self) -> &[String] {
        &self.insecure_hosts
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
    /// | `http` | [`ReqwestIndexTransport`], only when the final host is in `insecure_hosts` (the union of `[registries."<host>"] insecure` and `OCX_INSECURE_REGISTRIES`) — the root document is the index path's trust anchor, so a plaintext index is an on-path takeover (CWE-319), gated exactly like the registry role |
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

        // Check 1a, ahead of check 1 because check 1 cannot see it: `file:/srv/x`
        // holds no `://`, so `scheme_of` reads it as schemeless and the `None`
        // arm below waves it through as an https default. `parse_url` then
        // splits it into host `file:` and path `srv/x`, and the invocation dies
        // much later as a DNS lookup for a host named `file` instead of here as
        // a config error naming the spelling the operator meant (#382).
        if let Some(tail) = file_colon_tail(base) {
            return Err(invalid_index_url(namespace, base, file_colon_origin(tail), None));
        }

        // Check 1, on the CONFIGURED base and before `parse_url`: a `file` base
        // is diverted here because it must never be host-keyed — it has no host
        // to key a `[mirrors]` override by, and `parse_url` reads its empty
        // authority as `MissingHost`.
        //
        // Routed on the shared `Spelling`, and on `FileUrl` **only**: this door
        // refuses the bare spelling that the `key` door accepts, because a
        // schemeless `index = "index.corp.example"` already means
        // `https://index.corp.example`. Reading it as a path would silently
        // reroute an operator's index to the filesystem.
        if FileReference::parse(base).spelling() == Spelling::FileUrl {
            return resolve_file_base(namespace, base);
        }
        match scheme_of(base).as_deref() {
            None | Some("http") | Some("https") => {}
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
            "http" if crate::allows_plain_http(insecure_hosts, &target.host) => {}
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
    /// on a served-but-unknown version; the transport error otherwise. Both
    /// arrive inside a transparent
    /// [`Error::SourceFetchFailed`](super::error::Error::SourceFetchFailed)
    /// when this call led the coalesced fetch — same message, same exit code,
    /// one `source()` hop further down.
    async fn check_format_version(&self) -> Result<Arc<IndexFormatConfig>> {
        if let Some(config) = &self.cache.read().await.config {
            return Ok(config.clone());
        }
        // Coalesce the cold misses: this is a read-check-then-fetch, so under
        // the sync fan-out every task read-checks together, all miss and all
        // fetch the same document.
        let handle = match self
            .config_group
            .try_acquire(())
            .await
            .map_err(error::Error::SingleflightFailed)?
        {
            Acquisition::Leader(handle) => handle,
            Acquisition::Resolved(Some(config)) => return Ok(config),
            // The leader found no `config.json` and assumed v1. That answer is
            // deliberately never memoized (C-005), and a group entry retains
            // for the process — so this arm **bypasses the group** and asks the
            // wire again, which is how a tree that later publishes one is
            // picked up without a restart. Eviction-on-failure cannot cover
            // this: the assumed value is an `Ok`.
            Acquisition::Resolved(None) => return Ok(or_assumed_v1(self.fetch_format_config().await?)),
        };
        match self.fetch_format_config().await {
            Ok(served) => {
                // Only a served document is broadcast, on the same terms it is
                // memoized: `None` tells a waiter to derive its own assumed v1.
                handle.complete(served.clone());
                Ok(or_assumed_v1(served))
            }
            Err(error) => Err(error::broadcast_failure(handle, error)),
        }
    }

    /// One `GET config.json`, version-gated, memoizing **only** a document that
    /// was actually served.
    ///
    /// `Ok(None)` is the absent-`config.json` case, which the caller resolves
    /// to [`IndexFormatConfig::assumed_v1`] without memoizing it anywhere
    /// (C-005). Splitting that distinction out of the return type is what lets
    /// the coalescing group in [`Self::check_format_version`] retain the served
    /// case and only the served case.
    async fn fetch_format_config(&self) -> Result<Option<Arc<IndexFormatConfig>>> {
        let url = format!("{}/config.json", self.base_url);
        match self.transport.get(&url).await? {
            IndexFetch::Found { bytes } => {
                let config: IndexFormatConfig = parse_document(&bytes, &url)?;
                gate_format_version(config.format_version)?;
                let config = Arc::new(config);
                self.cache.write().await.config = Some(config.clone());
                Ok(Some(config))
            }
            IndexFetch::NotFound => {
                // The substitution happens before the gate, which is why the
                // gate takes a version and never a "was it there?" flag (C-004).
                gate_format_version(IndexFormatConfig::assumed_v1().format_version)?;
                Ok(None)
            }
        }
    }

    // ── root (F1 volatile) ──────────────────────────────────────────────────

    /// Fetches (and caches) the root for `repository`. `Ok(None)` on a 404
    /// miss — memoized like a hit, so a repeat ask costs nothing.
    ///
    /// # Errors
    ///
    /// [`Error::IndexHttpFailed`](super::error::Error::IndexHttpFailed) for any
    /// non-404 failure the transport surfaces — inside a transparent
    /// [`Error::SourceFetchFailed`](super::error::Error::SourceFetchFailed)
    /// when this call led the coalesced fetch. Only a *confirmed* 404 reads as a
    /// miss: this `None` is what [`Self::jurisdiction`] settles an `Outside`
    /// verdict off, and it is memoized, so no other status may fold into it. A
    /// failure memoizes nothing, and the singleflight entry is evicted on the
    /// next read, so a repeat ask re-requests.
    async fn resolve_root(&self, repository: &str) -> Result<Option<Arc<IndexRoot>>> {
        // The version gate runs before any root is consumed (F1). Absence is
        // v1, not a refusal (C-005) — an unsupported served version still is.
        self.check_format_version().await?;
        if let Some(root) = self.cache.read().await.roots.get(repository) {
            return Ok(root.clone());
        }
        // Coalesce the cold misses: the per-tag fan-out asks for one
        // repository's root once per tag, and all of them read-check together.
        let handle = match self
            .root_group
            .try_acquire(repository.to_string())
            .await
            .map_err(error::Error::SingleflightFailed)?
        {
            Acquisition::Leader(handle) => handle,
            // Both a hit and a confirmed miss are answers here, unlike the
            // assumed v1 above: `resolve_root` memoizes each on the same terms.
            Acquisition::Resolved(root) => return Ok(root),
        };
        match self.fetch_root(repository).await {
            Ok(root) => {
                self.memoize_root(repository, root.clone()).await;
                handle.complete(root.clone());
                Ok(root)
            }
            Err(error) => Err(error::broadcast_failure(handle, error)),
        }
    }

    /// One `GET p/<repo>.json`. `Ok(None)` on a confirmed 404, which is a
    /// result and not a failure — nothing else may fold into it.
    async fn fetch_root(&self, repository: &str) -> Result<Option<Arc<IndexRoot>>> {
        let url = format!("{}/p/{}.json", self.base_url, repository);
        match self.transport.get(&url).await? {
            IndexFetch::Found { bytes } => {
                let parsed: IndexRoot = parse_document(&bytes, &url)?;
                Ok(Some(Arc::new(parsed)))
            }
            IndexFetch::NotFound => Ok(None),
        }
    }

    /// Memoizes a root lookup under the key [`Self::resolve_root`] reads.
    ///
    /// The key is the bare repository — **no registry component** — so only a
    /// call that actually issued `GET p/<repo>.json` against *this* source may
    /// memoize (D-004a). A path that answered without contacting anything must
    /// memoize nothing: caching a foreign identifier's `None` here would settle
    /// [`Self::jurisdiction`] as `Outside` for the served registry's
    /// identically-named repository, and that package would silently stop
    /// resolving through the index for the rest of the process.
    async fn memoize_root(&self, repository: &str, root: Option<Arc<IndexRoot>>) {
        self.cache.write().await.roots.insert(repository.to_string(), root);
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
        // index data, so validate it BEFORE the first physical registry request
        // (`self.client.*` in every caller). `trusted_hosts` is the explicit
        // per-namespace escape hatch. `self.client` additionally pins the
        // validated address at connect time via its `GuardedResolver`, closing
        // the resolve -> connect rebinding window. The resolved addresses are
        // discarded here — the pin, not this pre-flight, drives the connection.
        //
        // Route-aware (ocx#407): when a configured proxy intercepts the dial the
        // process never resolves `registry` — the name is text in the proxy's
        // `CONNECT` line — so the floor judges a forbidden IP literal textually
        // and performs no lookup. `insecure_hosts` picks the scheme, because
        // which proxy setting applies (`HTTP_PROXY` vs `HTTPS_PROXY`) depends on
        // it.
        let (host, port) = oci::ssrf::split_host_port(&registry);
        oci::ssrf::guard_destination(
            oci::ssrf::DialScheme::for_registry(self.insecure_hosts(), &registry),
            host,
            port,
            &self.trusted_hosts,
            &self.proxy_rules,
        )
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

/// Resolves the absent-`config.json` case to [`IndexFormatConfig::assumed_v1`].
///
/// Kept as a substitution over `Option` rather than folded into the fetch so
/// the one caller that must **not** memoize the result — every caller, per
/// C-005 — cannot accidentally hand it to a memo or a coalescing group.
fn or_assumed_v1(served: Option<Arc<IndexFormatConfig>>) -> Arc<IndexFormatConfig> {
    served.unwrap_or_else(|| Arc::new(IndexFormatConfig::assumed_v1()))
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
            return Ok(Some(
                self.client
                    .fetch_manifest_addressed(&physical, ReadAddressing::Mirrored)
                    .await?,
            ));
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
            return Ok(self
                .client
                .fetch_manifest_raw_bytes_addressed(&physical, ReadAddressing::Mirrored)
                .await?);
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
        // Memoizes NOTHING: no request was issued, and the memo key carries no
        // registry (D-004a, and `memoize_root`'s doc comment).
        if !self.serves_registry(identifier.registry()) {
            return Ok(None);
        }
        // The version gate runs before any root is consumed (F1).
        self.check_format_version().await?;
        // A memoized **miss** is the whole answer, so it short-circuits: a flat
        // name costs one 404 per process however many times it is asked for. A
        // memoized hit cannot, because the caller needs the verbatim bytes and
        // the memo holds only the parse.
        if let Some(None) = self.cache.read().await.roots.get(identifier.repository()) {
            return Ok(None);
        }
        let url = format!("{}/p/{}.json", self.base_url, identifier.repository());
        // Both arms below issued the request, so both memoize — the miss on
        // exactly the same terms as the hit, because a confirmed 404 is what
        // `jurisdiction` settles an `Outside` verdict off. Without this the
        // per-tag fan-out that follows re-fetches this same root once per tag
        // (D-004): a sequencing bug, not a race, which is why coalescing alone
        // would not fix it — the leader finishes without ever registering.
        match self.transport.get(&url).await? {
            IndexFetch::Found { bytes } => {
                let root: IndexRoot = parse_document(&bytes, &url)?;
                self.memoize_root(identifier.repository(), Some(Arc::new(root.clone())))
                    .await;
                Ok(Some((bytes, root)))
            }
            // A 404 is a clean miss, never an error.
            IndexFetch::NotFound => {
                self.memoize_root(identifier.repository(), None).await;
                Ok(None)
            }
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

    fn trusted_hosts(&self) -> &[String] {
        OcxIndex::trusted_hosts(self)
    }

    fn insecure_hosts(&self) -> &[String] {
        OcxIndex::insecure_hosts(self)
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

    // Peels the wrapper a coalesced fetch's leader returns. Imported, never
    // re-implemented: these assertions and `ChainedIndex::is_source_outage`
    // must agree on what a leader's error *is*, and the two drifting apart is
    // exactly how the wrapper reached production recognised here and
    // unrecognised by the guard.
    use super::super::error::coalesced_cause;
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

    /// How long a held response is withheld, in **virtual** time.
    ///
    /// Only ever elapsed under `tokio::time::pause()`, where the clock advances
    /// solely once every task is parked — so the hold is released exactly when
    /// every concurrent caller has arrived, which is the deterministic form of
    /// the "hold the response until all N callers are here" fixture C-007 and
    /// C-008 require. Without the hold a coalescing assertion passes on serial
    /// execution and proves nothing: both `check_format_version` and
    /// `resolve_root` are read-check-then-fetch, so whether N tasks each fetch
    /// is otherwise a scheduling accident.
    const HELD_RESPONSE: std::time::Duration = std::time::Duration::from_secs(1);

    #[derive(Clone, Default)]
    struct StubIndexTransport {
        responses: StubResponses,
        requests: StubRequests,
        /// URLs that return a transport error (simulate a dead endpoint).
        failures: Arc<Mutex<std::collections::HashSet<String>>>,
        /// URLs whose response is withheld for [`HELD_RESPONSE`].
        held: Arc<Mutex<std::collections::HashSet<String>>>,
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

        fn hold(&self, url: &str) {
            self.held.lock().unwrap().insert(url.to_string());
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
            // Read the flag out before awaiting — the guard must not span it.
            let held = self.held.lock().unwrap().contains(url);
            if held {
                tokio::time::sleep(HELD_RESPONSE).await;
            }
            if self.failures.lock().unwrap().contains(url) {
                return Err(super::super::error::Error::IndexHttpFailed {
                    url: url.to_string(),
                    status: None,
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
            insecure_hosts: Vec::new(),
            proxy_rules: oci::ssrf::ProxyRules::direct(),
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
                coalesced_cause(&error),
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
                coalesced_cause(&error),
                crate::Error::OciIndex(super::super::error::Error::UnsupportedIndexFormat { version: 2 })
            ),
            "expected UnsupportedIndexFormat{{2}}, got {error:?}"
        );
        // The coalescing wrapper is transparent, so the message a caller reads
        // is the one a direct, uncoalesced fetch would have produced.
        assert_eq!(
            error.to_string(),
            "index format_version 2 is not supported",
            "coalescing must not prefix the message a direct fetch produced"
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
            "an http base is allowed when its host is in the resolved plain-HTTP set"
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

    /// The index gate is fed by the SHARED predicate, so a `[registries.<host>]
    /// insecure = true` entry licenses a plain-HTTP index base with the
    /// environment empty. Every other test of this gate hands it a literal
    /// vector, which is equally satisfied by an env-only build of the set.
    ///
    /// The env is explicitly `&[]` in both halves — an inherited
    /// `OCX_INSECURE_REGISTRIES` would make the green indistinguishable from
    /// the config entry doing nothing.
    #[test]
    fn resolve_base_url_accepts_an_http_base_licensed_by_the_config_half_alone() {
        let config: crate::config::Config = toml::from_str(
            "[registries.\"ocx.sh\"]\nindex = \"http://index.corp:8080/ocx-index\"\n\
             [registries.\"index.corp:8080\"]\ninsecure = true\n",
        )
        .unwrap();

        let licensed = crate::insecure_hosts(&config, &[]);
        assert_eq!(
            licensed,
            vec!["index.corp:8080".to_string()],
            "precondition: the allowance comes from the config, not the environment"
        );

        assert_eq!(
            OcxIndex::resolve_base_url(&config, "ocx.sh", &no_mirrors(), &licensed)
                .expect("a config-licensed plain-HTTP index base must resolve")
                .url,
            "http://index.corp:8080/ocx-index",
        );
        assert!(
            OcxIndex::resolve_base_url(&config, "ocx.sh", &no_mirrors(), &[]).is_err(),
            "and it must still be refused when nothing licenses it"
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

    /// S-022 — the negative half of the shared `FileReference` grammar (#379).
    ///
    /// This door takes the `file://` spelling and **refuses the bare one**,
    /// unlike `signers[].key`, which takes both: a schemeless `index` value is
    /// already a host over https. Routing it to the filesystem would silently
    /// point an operator's index at a local directory — at best a 404 much
    /// later, at worst a tree someone else can write.
    #[test]
    fn resolve_base_url_refuses_the_bare_file_reference_spelling() {
        for (base, expected) in [
            ("index.corp.example", "https://index.corp.example"),
            ("srv/ocx-index", "https://srv/ocx-index"),
        ] {
            let resolved = OcxIndex::resolve_base_url(&index_config(base), "ocx.sh", &no_mirrors(), &[])
                .expect("a value with no `file://` names a host, not a path");
            assert_eq!(
                resolved.url, expected,
                "`{base}` must resolve over https; a file base would read `file://…`"
            );
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
    fn resolve_base_url_refuses_a_single_slash_file_base() {
        // C-003/S-002 (#382): `file:/srv/x` holds no `://`, so before check 1a
        // it read as schemeless, defaulted to https, and `parse_url` split it
        // into host `file:` + path `srv/x` — an `IndexBase` pointed at a host
        // named `file`, failing much later as a DNS lookup.
        let error = OcxIndex::resolve_base_url(&index_config("file:/srv/x"), "ocx.sh", &no_mirrors(), &[])
            .expect_err("a file base written with one slash must be refused");
        let rendered = error.to_string();
        assert!(
            rendered.contains("file:///srv/x"),
            "the refusal must name the corrected spelling: {rendered}"
        );
        assert_eq!(
            crate::cli::ClassifyExitCode::classify(&error),
            Some(crate::cli::ExitCode::ConfigError),
            "a refused index base is a config error (78)"
        );

        // `FILE:` is the same mistake shouted, and `file:` alone names no
        // directory at all. Neither may reach the https default.
        for base in ["FILE:/srv/x", "file:", "file:srv/x"] {
            let error = OcxIndex::resolve_base_url(&index_config(base), "ocx.sh", &no_mirrors(), &[])
                .expect_err("a single-colon file base must be refused whatever follows it");
            assert!(
                error.to_string().contains("file://"),
                "the refusal must name the spelling that works: {error}"
            );
        }

        // A relative tail gets the shape, not a literal: `file://srv/x` names a
        // non-empty authority, which `resolve_file_base` refuses in turn.
        let error = OcxIndex::resolve_base_url(&index_config("file:srv/x"), "ocx.sh", &no_mirrors(), &[])
            .expect_err("a relative file tail is still refused");
        assert!(
            !error.to_string().contains("file://srv/x"),
            "a suggested spelling that is itself refused is worse than none: {error}"
        );
    }

    #[test]
    fn a_schemeless_base_still_resolves_as_https() {
        // S-022, the negative check 1a must not break: a schemeless base is an
        // https host, and stays one. `file` appearing anywhere but the scheme
        // position is ordinary text.
        for (base, expected) in [
            ("index.corp.example", "https://index.corp.example"),
            ("profile.corp.example/ocx", "https://profile.corp.example/ocx"),
        ] {
            let resolved = OcxIndex::resolve_base_url(&index_config(base), "ocx.sh", &no_mirrors(), &[])
                .expect("a schemeless base defaults to https");
            assert_eq!(resolved.url, expected);
        }
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
        // Construction (which runs the seeding) must not panic — including the
        // `expect` that is now the last resort (D-011b).
        let _client = ReqwestIndexTransport::new();
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

    // ── root cache + coalescing (C-006, C-007, C-008) ────────────────────────
    //
    // `fetch_root_document` populates the memo `resolve_root` reads, and both
    // it and `check_format_version` coalesce their concurrent cold misses.
    // Every assertion below is on the REQUEST COUNT, never the return value: a
    // poisoned memo and a genuine miss both answer `Ok(None)`, so only the
    // count discriminates.

    /// C-006 — a published refresh of one package issues exactly **one** root
    /// GET. `fetch_root_document` fetches the root; the per-tag fan-out that
    /// follows it must find that root already memoized.
    ///
    /// *Red-reachability:* without the memoizing insert the count is
    /// `1 + min(T, 64)`. The assertion is `== 1`, never `<= 64`.
    #[tokio::test]
    async fn a_published_refresh_of_one_package_issues_one_root_get() {
        let transport = StubIndexTransport::new();
        let digest = seed_package(&transport, false);
        // A second tag on a second content digest, so T = 2 with T distinct
        // dispatch objects and neither tag can be answered from the other's.
        // Byte-distinct from the first (one more byte of trailing JSON
        // whitespace), so it hashes to a different dispatch digest and neither
        // tag can be answered from the other's object.
        let second = [glibc_musl_index(), b"\n"].concat();
        let second_digest = Algorithm::Sha256.hash(&second);
        let root = format!(
            r#"{{"repository":"oci://ghcr.io/ocx-contrib/cmake","tags":{{"3.28":{{"content":"{digest}"}},"3.29":{{"content":"{second_digest}"}}}}}}"#
        );
        transport.insert(&root_url(), root.as_bytes());
        transport.insert(&dispatch_url(&second_digest), &second);
        let source = make_source(transport.clone(), false);

        source
            .fetch_root_document(&oci::Identifier::new_registry(REPO, NAMESPACE))
            .await
            .unwrap()
            .expect("the published root is served");
        for tag in ["3.28", "3.29"] {
            source
                .fetch_manifest(
                    &oci::Identifier::new_registry(REPO, NAMESPACE).clone_with_tag(tag),
                    IndexOperation::Resolve,
                )
                .await
                .unwrap()
                .expect("each tag resolves through the memoized root");
        }

        assert_eq!(
            transport.request_count(&root_url()),
            1,
            "the root fetch must populate the memo the per-tag fan-out reads"
        );
    }

    /// C-006 edge case (a) — a root that 404s costs exactly one request, and
    /// the miss is memoized: a second `fetch_root_document` for the same
    /// repository issues nothing.
    #[tokio::test]
    async fn a_confirmed_root_miss_is_memoized_and_never_re_requested() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = make_source(transport.clone(), false);
        let identifier = oci::Identifier::new_registry(REPO, NAMESPACE);

        assert!(source.fetch_root_document(&identifier).await.unwrap().is_none());
        assert!(source.fetch_root_document(&identifier).await.unwrap().is_none());
        assert!(source.resolve_root(REPO).await.unwrap().is_none());

        assert_eq!(
            transport.request_count(&root_url()),
            1,
            "a confirmed 404 is a result and is memoized like a hit"
        );
    }

    /// C-006 edge case (a2) — a **foreign-registry** identifier memoizes
    /// nothing.
    ///
    /// The memo key is the bare repository with no registry component, so a
    /// tail-position insert would poison `ns/pkg` with `None` for the served
    /// registry, `jurisdiction` would settle `Outside`, and the package would
    /// silently stop resolving through the index for the rest of the process
    /// (D-004a). *Red-reachability:* both a poisoned entry and a genuine miss
    /// return `Ok(None)`, so the assertion is on the request count.
    #[tokio::test]
    async fn a_foreign_registry_root_fetch_memoizes_nothing() {
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        let source = make_source(transport.clone(), false);

        let foreign = oci::Identifier::new_registry(REPO, "other.io");
        assert!(
            source.fetch_root_document(&foreign).await.unwrap().is_none(),
            "a foreign-namespace identifier is not this source's concern"
        );
        assert_eq!(
            transport.request_count(&root_url()),
            0,
            "the foreign early return issues no request"
        );

        // The count, not the return value, is the discriminator: a poisoned
        // entry and a genuine miss both answer `Ok(None)`.
        let resolved = source.resolve_root(REPO).await.unwrap();
        assert_eq!(
            transport.request_count(&root_url()),
            1,
            "the served registry's resolve must still issue its own GET, not read a poisoned memo"
        );
        assert!(
            resolved.is_some(),
            "the served registry's identically-named repository still resolves"
        );
    }

    /// C-006 edge case (b), first half — a **non-404** root failure memoizes
    /// nothing, so a repeat ask re-requests. Needs the singleflight primitive's
    /// eviction-on-read: without it the group answers the leader's error
    /// forever and the repeat ask issues nothing.
    #[tokio::test]
    async fn a_failed_root_fetch_memoizes_nothing_and_is_re_requested() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.fail(&root_url());
        let source = make_source(transport.clone(), false);

        source
            .resolve_root(REPO)
            .await
            .expect_err("a non-404 failure propagates");
        source.resolve_root(REPO).await.expect_err("and propagates again");

        assert_eq!(
            transport.request_count(&root_url()),
            2,
            "a transport failure is not a result: nothing is memoized and the repeat ask re-requests"
        );
    }

    /// C-006 edge case (b), second half — the discriminating companion. A
    /// `404` on the same code path memoizes, so the repeat ask issues nothing.
    /// Without both halves the test cannot tell the two cache policies apart.
    #[tokio::test]
    async fn a_404_root_fetch_memoizes_and_is_not_re_requested() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        let source = make_source(transport.clone(), false);

        assert!(source.resolve_root(REPO).await.unwrap().is_none());
        assert!(source.resolve_root(REPO).await.unwrap().is_none());

        assert_eq!(
            transport.request_count(&root_url()),
            1,
            "only a confirmed 404 folds into the memoized miss"
        );
    }

    /// C-006 edge case (b), the eviction half at the source level — a failed
    /// root fetch that later succeeds must be picked up, not answered from a
    /// poisoned singleflight entry.
    #[tokio::test]
    async fn a_root_that_recovers_after_a_failure_resolves() {
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.fail(&root_url());
        let source = make_source(transport.clone(), false);
        source.resolve_root(REPO).await.expect_err("the first ask fails");

        transport.failures.lock().unwrap().remove(&root_url());
        transport.insert(&root_url(), br#"{"repository":"oci://ghcr.io/x/y","tags":{}}"#);
        assert!(
            source.resolve_root(REPO).await.unwrap().is_some(),
            "a transient outage must not poison the name for the life of the process"
        );
    }

    /// D-005b(2) — the group is sized for the run, not copied.
    ///
    /// These groups live for the process, so one key accrues per repository an
    /// `ocx index sync` touches. `chained_index.rs`'s `SINGLEFLIGHT_MAX_KEYS =
    /// 1024` was chosen for a per-refresh group; copied here it would answer
    /// `CapacityExceeded` → `TempFail(75)` — an exit code promising a retry
    /// nothing inside the process can make succeed — on **successes** alone.
    ///
    /// *Red-reachability:* set `SOURCE_SINGLEFLIGHT_MAX_KEYS` to `1024`.
    #[tokio::test]
    async fn a_registry_larger_than_the_chained_index_key_budget_still_resolves() {
        const PACKAGES: usize = 1500;
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        for package in 0..PACKAGES {
            transport.insert(
                &format!("{BASE}/p/ns/pkg{package}.json"),
                br#"{"repository":"oci://ghcr.io/x/y","tags":{}}"#,
            );
        }
        let source = make_source(transport, false);

        for package in 0..PACKAGES {
            source
                .resolve_root(&format!("ns/pkg{package}"))
                .await
                .unwrap_or_else(|error| panic!("package {package} must resolve, got: {error}"))
                .expect("the stub serves every root");
        }
    }

    /// C-007 — `check_format_version` fetches a **served** `config.json` at
    /// most once per process, per source, under a fan-out.
    ///
    /// "Served" is the whole scope of the claim, not a hedge: an **absent**
    /// `config.json` deliberately bypasses the group (the
    /// `Acquisition::Resolved(None)` arm) so a tree that later publishes one is
    /// picked up without a restart, which means later callers each re-fetch it.
    /// That is the baseline behaviour — it re-derived on every call before any
    /// coalescing existed — and its own guards are
    /// [`an_absent_config_json_is_never_memoized_and_a_later_one_is_picked_up`]
    /// and [`concurrent_absent_config_json_reads_stay_unmemoized`].
    ///
    /// The stub **holds** the response, and virtual time only advances once
    /// every task is parked, so the leader cannot answer before all N callers
    /// have arrived. *Red-reachability:* without the hold this passes today —
    /// the function is a read-check-then-fetch, so serial execution gives a
    /// green that proves nothing.
    #[tokio::test(start_paused = true)]
    async fn concurrent_config_json_reads_produce_one_get() {
        const CALLERS: usize = 8;
        let transport = StubIndexTransport::new();
        transport.insert(&config_url(), br#"{"format_version":1}"#);
        transport.hold(&config_url());
        let source = make_source(transport.clone(), false);

        let results =
            futures::future::join_all((0..CALLERS).map(|_| async { source.check_format_version().await })).await;
        for result in results {
            result.expect("every caller gets the served document");
        }

        assert_eq!(
            transport.request_count(&config_url()),
            1,
            "a served config.json is fetched once per source, however wide the fan-out"
        );
    }

    /// C-007 edge case, load-bearing and inverting the headline — an **absent**
    /// `config.json` resolves to `assumed_v1()` and is deliberately **not**
    /// memoized, so a tree that later publishes one is picked up without
    /// restarting the process (snapshot-spec C-005). A coalescing group that
    /// retained the assumed value would break that silently, and
    /// eviction-on-failure cannot catch it: the assumed value is an `Ok`.
    #[tokio::test]
    async fn an_absent_config_json_is_never_memoized_and_a_later_one_is_picked_up() {
        let transport = StubIndexTransport::new();
        let source = make_source(transport.clone(), false);

        assert_eq!(
            source.check_format_version().await.unwrap().name_segments,
            None,
            "an absent config.json is assumed v1"
        );
        assert_eq!(transport.request_count(&config_url()), 1, "the first ask fetches");
        source.check_format_version().await.unwrap();
        assert_eq!(
            transport.request_count(&config_url()),
            2,
            "an assumed v1 is re-derived every call, never memoized and never retained by the group"
        );

        transport.insert(&config_url(), br#"{"format_version":1,"name_segments":2}"#);
        assert_eq!(
            source
                .check_format_version()
                .await
                .unwrap()
                .name_segments
                .map(std::num::NonZeroU32::get),
            Some(2),
            "a tree that later publishes a config.json is picked up without a restart"
        );
    }

    /// C-007's absent-`config.json` case under the same held-response fan-out:
    /// the coalescing must not turn "never memoized" into "memoized once".
    #[tokio::test(start_paused = true)]
    async fn concurrent_absent_config_json_reads_stay_unmemoized() {
        const CALLERS: usize = 8;
        let transport = StubIndexTransport::new();
        transport.hold(&config_url());
        let source = make_source(transport.clone(), false);

        futures::future::join_all((0..CALLERS).map(|_| async { source.check_format_version().await }))
            .await
            .into_iter()
            .for_each(|result| {
                result.expect("every caller assumes v1");
            });

        transport.insert(&config_url(), br#"{"format_version":1,"name_segments":2}"#);
        assert_eq!(
            source
                .check_format_version()
                .await
                .unwrap()
                .name_segments
                .map(std::num::NonZeroU32::get),
            Some(2),
            "coalescing an absent config.json must not memoize the assumed v1"
        );
    }

    /// C-008 — concurrent `resolve_root` for one repository produces one GET.
    /// Same held-response shape as C-007, width 8, one repository.
    #[tokio::test(start_paused = true)]
    async fn concurrent_resolve_root_for_one_repository_produces_one_get() {
        const CALLERS: usize = 8;
        let transport = StubIndexTransport::new();
        seed_package(&transport, false);
        transport.hold(&root_url());
        let source = make_source(transport.clone(), false);

        let results = futures::future::join_all((0..CALLERS).map(|_| async { source.resolve_root(REPO).await })).await;
        for result in results {
            assert!(result.unwrap().is_some(), "every caller gets the same root");
        }

        assert_eq!(
            transport.request_count(&root_url()),
            1,
            "one repository's concurrent root reads coalesce onto one leader"
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
    async fn a_failed_root_fetch_is_a_protocol_failure_not_an_absence() {
        // A root fetch that FAILS is not an absence. Folding a failure into the
        // 404 miss would memoize "this index does not hold the package" for the
        // rest of the process off one bad response — and with the fall-through
        // gone that memo is now a terminal refusal, not a reroute.
        let transport = StubIndexTransport::new();
        seed_with_config(&transport, br#"{"format_version":1}"#);
        transport.fail(&flat_root_url());
        let source = make_source(transport, false);

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
}

// ── Retry ladder and timeout inversion, at the wire ──────────────────────────

/// `C-016`, `C-017`'s wiring, `C-019`, `C-021` and `C-028` against a real
/// socket through the production [`ReqwestIndexTransport`].
///
/// These are the half the virtual-clock tests in
/// [`crate::oci::transport_policy`] cannot cover: that `get` *reads* the
/// header, *classifies* the status and *composes* the three timeout bounds.
/// The clock is real here on purpose — a paused clock auto-advances whenever
/// the runtime is idle waiting on a socket, which fires the very timeouts
/// [`TransportHardening`] exists to bound. The bounds are injected instead, so
/// the same semantics cost milliseconds rather than the shipped minutes.
///
/// Pattern lifted from `oci/client/builder.rs`'s `push_wire_tests` /
/// `read_timeout_tests` — the only place in the tree counting real wire
/// requests — rather than invented here.
#[cfg(test)]
mod transport_wire_tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    const BODY: &str = r#"{"formatVersion":1}"#;

    /// One scripted response. The stub answers request *n* with `script[n]`,
    /// repeating the last entry once the script runs out.
    #[derive(Clone)]
    enum Reply {
        /// A complete response: status line, extra headers, whole body at once.
        Status {
            code: u16,
            headers: Vec<(&'static str, String)>,
            body: &'static str,
        },
        /// `Content-Length: body.len()`, then one byte every `interval` until
        /// the body is delivered. Honest but slow — never idle longer than
        /// `interval`, so an idle bound above `interval` must not fire.
        Dribble { interval: Duration, body: &'static str },
        /// A `Content-Length` far beyond what is ever sent, then one byte every
        /// `interval`, forever. Trips no idle bound above `interval` and never
        /// approaches the byte cap in any human timeframe — so only an outer
        /// cap can end it.
        DribbleForever { interval: Duration },
        /// Headers, then silence with the socket held open. Silent-but-open is
        /// the case an idle bound exists for at all: a close yields EOF, which
        /// every layer above already handles.
        Stall,
        /// A `200` declaring `declared` bytes and sending none — a body the
        /// client must refuse on the declaration alone.
        ForgedLength { declared: u64 },
        /// A `200`, headers, a partial body, then the connection closed with
        /// the promised `Content-Length` unfulfilled — the shape a
        /// TLS-inspecting proxy produces. The only reply here whose failure
        /// lands in the body loop rather than in `send()`.
        AbortMidBody { sent: &'static str },
    }

    fn ok() -> Reply {
        Reply::Status {
            code: 200,
            headers: Vec::new(),
            body: BODY,
        }
    }

    fn status(code: u16) -> Reply {
        Reply::Status {
            code,
            headers: Vec::new(),
            body: "",
        }
    }

    fn status_with_retry_after(code: u16, retry_after: &str) -> Reply {
        Reply::Status {
            code,
            headers: vec![("Retry-After", retry_after.to_string())],
            body: "",
        }
    }

    fn reason(code: u16) -> &'static str {
        match code {
            200 => "OK",
            403 => "Forbidden",
            404 => "Not Found",
            429 => "Too Many Requests",
            503 => "Service Unavailable",
            _ => "Unknown",
        }
    }

    /// A minimal HTTP/1.1 index endpoint that counts every request it serves.
    ///
    /// Answers with `Connection: close` so one connection is exactly one
    /// request — otherwise keep-alive reuse would decouple the connection count
    /// from the request count the retry contracts assert on.
    struct StubIndexEndpoint {
        address: String,
        served: Arc<AtomicUsize>,
        /// Every request target the endpoint saw. A request *count* cannot show
        /// that a redirect went unfollowed — only the absence of the redirect's
        /// own target from this list can.
        targets: Arc<Mutex<Vec<String>>>,
    }

    impl StubIndexEndpoint {
        async fn start(script: Vec<Reply>) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let served = Arc::new(AtomicUsize::new(0));

            let counter = Arc::clone(&served);
            let targets = Arc::new(Mutex::new(Vec::new()));
            let log = Arc::clone(&targets);
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let script = script.clone();
                    let counter = Arc::clone(&counter);
                    let log = Arc::clone(&log);
                    tokio::spawn(async move {
                        let index = counter.fetch_add(1, Ordering::SeqCst);
                        let reply = script[index.min(script.len() - 1)].clone();
                        serve(socket, reply, log).await;
                    });
                }
            });

            Self {
                address,
                served,
                targets,
            }
        }

        fn url(&self) -> String {
            format!("http://{}/p/ocx.sh/tool.json", self.address)
        }

        fn served(&self) -> usize {
            self.served.load(Ordering::SeqCst)
        }

        fn targets(&self) -> Vec<String> {
            self.targets.lock().unwrap().clone()
        }
    }

    async fn serve(socket: tokio::net::TcpStream, reply: Reply, targets: Arc<Mutex<Vec<String>>>) {
        let (read_half, mut write_half) = socket.into_split();
        let mut reader = tokio::io::BufReader::new(read_half);

        let mut request_line = String::new();
        if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
            return;
        }
        if let Some(target) = request_line.split_whitespace().nth(1) {
            targets.lock().unwrap().push(target.to_string());
        }
        loop {
            let mut header = String::new();
            match reader.read_line(&mut header).await {
                Ok(0) | Err(_) => return,
                Ok(_) if header.trim_end().is_empty() => break,
                Ok(_) => {}
            }
        }

        match reply {
            Reply::Status { code, headers, body } => {
                let mut response = format!(
                    "HTTP/1.1 {code} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    reason(code),
                    body.len()
                );
                for (name, value) in headers {
                    response.push_str(&format!("{name}: {value}\r\n"));
                }
                response.push_str("\r\n");
                response.push_str(body);
                let _ = write_half.write_all(response.as_bytes()).await;
                let _ = write_half.flush().await;
            }
            Reply::Dribble { interval, body } => {
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if write_half.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for byte in body.as_bytes() {
                    tokio::time::sleep(interval).await;
                    if write_half.write_all(&[*byte]).await.is_err() || write_half.flush().await.is_err() {
                        return;
                    }
                }
            }
            Reply::DribbleForever { interval } => {
                let head = "HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\n";
                if write_half.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                loop {
                    tokio::time::sleep(interval).await;
                    if write_half.write_all(b".").await.is_err() || write_half.flush().await.is_err() {
                        return;
                    }
                }
            }
            Reply::Stall => {
                let head = "HTTP/1.1 200 OK\r\nContent-Length: 1048576\r\nConnection: close\r\n\r\nx";
                let _ = write_half.write_all(head.as_bytes()).await;
                let _ = write_half.flush().await;
                std::future::pending::<()>().await;
            }
            Reply::ForgedLength { declared } => {
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n");
                let _ = write_half.write_all(head.as_bytes()).await;
                let _ = write_half.flush().await;
            }
            Reply::AbortMidBody { sent } => {
                // Promise the whole document, deliver a prefix, then drop —
                // the client's body read ends short of the declared length.
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    BODY.len()
                );
                let _ = write_half.write_all(head.as_bytes()).await;
                let _ = write_half.write_all(sent.as_bytes()).await;
                let _ = write_half.flush().await;
                drop(write_half);
            }
        }
    }

    /// Bounds generous enough that only the retry ladder is under test.
    fn quick_bounds() -> TransportHardening {
        TransportHardening {
            connect_timeout: Duration::from_secs(5),
            idle_bound: Duration::from_secs(5),
            outer_cap: Duration::from_secs(10),
        }
    }

    /// A ladder whose backoff is negligible, so a request-count assertion does
    /// not also pay for the shipped 250 ms base.
    fn quick_ladder() -> RetryPolicy {
        RetryPolicy {
            base: Duration::from_millis(1),
            cap: Duration::from_millis(1),
            ..RetryPolicy::default()
        }
    }

    // ── C-016 — a retryable status is retried; a terminal one is not ─────────

    #[tokio::test]
    async fn a_503_then_200_yields_the_body_in_two_requests() {
        let endpoint = StubIndexEndpoint::start(vec![status(503), ok()]).await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());

        match transport.get(&endpoint.url()).await {
            Ok(IndexFetch::Found { bytes }) => assert_eq!(bytes, BODY.as_bytes()),
            other => panic!("a transient 503 must be retried into a body, got {other:?}"),
        }
        assert_eq!(endpoint.served(), 2, "one initial attempt plus exactly one retry");
    }

    #[tokio::test]
    async fn a_404_is_a_confirmed_absence_and_is_never_re_asked() {
        let endpoint = StubIndexEndpoint::start(vec![status(404)]).await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());

        assert!(matches!(transport.get(&endpoint.url()).await, Ok(IndexFetch::NotFound)));
        assert_eq!(
            endpoint.served(),
            1,
            "a 404 is a confirmed absence; re-asking cannot change it and must not be attempted"
        );
    }

    #[tokio::test]
    async fn a_403_fails_after_one_request() {
        let endpoint = StubIndexEndpoint::start(vec![status(403)]).await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());

        assert!(transport.get(&endpoint.url()).await.is_err());
        assert_eq!(endpoint.served(), 1, "a 403 will not change on re-ask");
    }

    /// **A `3xx` is neither retried nor followed.**
    ///
    /// `Policy::none()` stops *reqwest* following it, but the ladder puts a
    /// classifier and a live 3xx response in the same function, where the
    /// obvious next step is to re-issue against `Location`. That would be
    /// manual redirect-following on a client carrying no `GuardedResolver`,
    /// after `resolve_base_url`'s plain-HTTP gate has already run: a
    /// remote-controlled `Location` is arbitrary egress (CWE-918) and an
    /// `http://` one is a silent scheme downgrade (CWE-319).
    ///
    /// Both halves are asserted. The error and the request count alone would
    /// pass against a follower whose second hop happened to fail — only the
    /// redirect target's absence from the endpoint's request log shows it was
    /// never asked for.
    #[tokio::test]
    async fn a_redirect_is_neither_retried_nor_followed() {
        const TRAP: &str = "/p/ocx.sh/elsewhere.json";
        let endpoint = StubIndexEndpoint::start(vec![Reply::Status {
            code: 302,
            headers: vec![("Location", TRAP.to_string())],
            body: "",
        }])
        .await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());

        let outcome = transport.get(&endpoint.url()).await;

        // The security assertion goes first: it is the one a follower violates
        // even when its second hop happens to fail, which would leave the error
        // and the count both looking correct.
        let targets = endpoint.targets();
        assert!(
            !targets.iter().any(|target| target.contains("elsewhere")),
            "the `Location` target was requested — that is manual redirect-following past the SSRF and \
             plain-HTTP gates (CWE-918 / CWE-319): {targets:?}"
        );
        assert!(
            outcome.is_err(),
            "an unfollowed redirect is a failure with the status intact, never a silent success"
        );
        assert_eq!(endpoint.served(), 1, "retrying a redirect re-fetches the same redirect");
    }

    /// **The ladder covers the body stream, not just `send()`.**
    ///
    /// This is the discriminator for the failure the whole work package exists
    /// for: a TLS-inspecting proxy that accepts, answers `200` with headers,
    /// then resets mid-body. A ladder wrapped around `send()` alone — the
    /// literal reading of "where the response and its status are in hand" —
    /// retries it zero times, and passes every other contract here, because
    /// every one of them fails before the first body byte.
    #[tokio::test]
    async fn a_body_that_aborts_mid_stream_is_retried_from_the_start() {
        let endpoint = StubIndexEndpoint::start(vec![Reply::AbortMidBody { sent: "{\"format" }, ok()]).await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());

        match transport.get(&endpoint.url()).await {
            Ok(IndexFetch::Found { bytes }) => assert_eq!(
                bytes,
                BODY.as_bytes(),
                "the retry must deliver the whole document, not the aborted prefix"
            ),
            other => panic!("a reset mid-body is transient and must be retried, got {other:?}"),
        }
        assert_eq!(
            endpoint.served(),
            2,
            "the whole GET is re-issued — safe because every request on this path is idempotent"
        );
    }

    /// C-024: the status rides the variant structurally, and the exit code is
    /// unchanged at 69 — both halves, because the second is the non-regression
    /// promise the plan makes about the CLI surface.
    #[tokio::test]
    async fn a_failing_status_lands_on_the_variant_and_still_exits_69() {
        use crate::cli::ClassifyExitCode as _;

        let endpoint = StubIndexEndpoint::start(vec![status(503)]).await;
        let transport = ReqwestIndexTransport::with_hardening(
            &quick_bounds(),
            RetryPolicy {
                attempts: 1,
                ..quick_ladder()
            },
        );

        let error = transport.get(&endpoint.url()).await.expect_err("503 is a failure");
        let crate::Error::OciIndex(index_error) = &error else {
            panic!("expected an index error, got {error:?}");
        };
        match index_error {
            super::super::error::Error::IndexHttpFailed { status, .. } => assert_eq!(
                *status,
                Some(503),
                "the retry classifier reads this field; a formatted message is unreadable to it"
            ),
            other => panic!("expected IndexHttpFailed, got {other:?}"),
        }
        assert_eq!(
            index_error.classify(),
            Some(crate::cli::ExitCode::Unavailable),
            "the status field is for the retry classifier, not for reclassifying the exit code — \
             exit codes are the CLI surface other tools branch on"
        );
    }

    // ── C-017 — the header is actually read off the wire ─────────────────────

    /// The virtual-clock tests prove the ladder sleeps a stated interval; this
    /// proves `get` parses one off a real response and hands it over.
    #[tokio::test]
    async fn a_retry_after_on_the_wire_is_waited_out() {
        let endpoint = StubIndexEndpoint::start(vec![status_with_retry_after(429, "1"), ok()]).await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());

        let start = std::time::Instant::now();
        assert!(matches!(
            transport.get(&endpoint.url()).await,
            Ok(IndexFetch::Found { .. })
        ));
        assert!(
            start.elapsed() >= Duration::from_secs(1),
            "the 1 ms ladder backoff would finish instantly; only the header's second explains the wait, got {:?}",
            start.elapsed()
        );
        assert_eq!(endpoint.served(), 2);
    }

    /// C-017 edge case (a), at the wire. Without the clamp this test does not
    /// fail — it hangs for a day, which is the exposure (CWE-400).
    #[tokio::test]
    async fn a_retry_after_above_the_clamp_fails_fast_instead_of_freezing_the_run() {
        let endpoint = StubIndexEndpoint::start(vec![status_with_retry_after(503, "86400"), ok()]).await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());

        let start = std::time::Instant::now();
        assert!(transport.get(&endpoint.url()).await.is_err());
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "above the clamp means stop retrying now, never sleep the stated day"
        );
        assert_eq!(endpoint.served(), 1, "the ladder stopped rather than waiting");
    }

    // ── C-019 — the budget is shared across every clone of the transport ─────

    /// The discriminator: `index_common.rs` builds one index per package inside
    /// the fan-out, so each package holds a *cloned* transport. A budget on a
    /// plain field would be per-package, and a single-package test would pass
    /// either way — so this fans out across several cloned transports at once
    /// and measures the wire.
    #[tokio::test]
    async fn the_retry_budget_is_run_global_across_cloned_transports() {
        const CLONES: usize = 3;
        const REQUESTS_PER_CLONE: usize = 10;

        let endpoint = StubIndexEndpoint::start(vec![status(503)]).await;
        let transport = ReqwestIndexTransport::with_hardening(&quick_bounds(), quick_ladder());
        let url = endpoint.url();

        let mut fan_out = tokio::task::JoinSet::new();
        for _ in 0..CLONES {
            // `box_clone` is the exact call the fan-out makes.
            let cloned = IndexTransport::box_clone(&transport);
            let url = url.clone();
            fan_out.spawn(async move {
                for _ in 0..REQUESTS_PER_CLONE {
                    let _ = cloned.get(&url).await;
                }
            });
        }
        while let Some(joined) = fan_out.join_next().await {
            joined.expect("no fan-out task panics");
        }

        let issued = CLONES * REQUESTS_PER_CLONE;
        let total = endpoint.served();
        let retries = total - issued;
        let allowed = std::cmp::max(10, total / 10);
        assert!(
            retries <= allowed,
            "{retries} retries over {total} requests exceeds the run budget of {allowed}; a per-clone \
             budget would admit {CLONES} floors instead of one"
        );
        assert!(
            retries > 0,
            "non-vacuity: the ladder must have retried at least once, or the bound proves nothing"
        );
    }

    // ── C-021 / C-028 — the three bounds compose ─────────────────────────────

    /// S-005: an honest slow body is no longer aborted. The transfer runs far
    /// past the idle bound without ever idling that long — exactly the case the
    /// old hard total deadline killed.
    #[tokio::test]
    async fn an_honest_slow_body_completes_however_long_it_takes() {
        let idle_bound = Duration::from_millis(200);
        let endpoint = StubIndexEndpoint::start(vec![Reply::Dribble {
            interval: Duration::from_millis(50),
            body: BODY,
        }])
        .await;
        let transport = ReqwestIndexTransport::with_hardening(
            &TransportHardening {
                connect_timeout: Duration::from_secs(5),
                idle_bound,
                outer_cap: Duration::from_secs(30),
            },
            quick_ladder(),
        );

        let start = std::time::Instant::now();
        match transport.get(&endpoint.url()).await {
            Ok(IndexFetch::Found { bytes }) => assert_eq!(bytes, BODY.as_bytes()),
            other => panic!("a body that never stalls must arrive, got {other:?}"),
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed > idle_bound * 3,
            "non-vacuity: the transfer must outlast a hard total deadline of the idle bound's size to \
             prove the inversion, took only {elapsed:?}"
        );
        assert_eq!(endpoint.served(), 1, "no retry — nothing failed");
    }

    /// The other half of C-021: a connection that genuinely goes quiet still
    /// fires, at roughly the idle bound.
    #[tokio::test]
    async fn a_stalled_body_fails_at_the_idle_bound() {
        let idle_bound = Duration::from_millis(300);
        let endpoint = StubIndexEndpoint::start(vec![Reply::Stall]).await;
        let transport = ReqwestIndexTransport::with_hardening(
            &TransportHardening {
                connect_timeout: Duration::from_secs(5),
                idle_bound,
                outer_cap: Duration::from_secs(30),
            },
            RetryPolicy {
                attempts: 1,
                ..quick_ladder()
            },
        );

        let start = std::time::Instant::now();
        assert!(transport.get(&endpoint.url()).await.is_err(), "a silent peer must fail");
        let elapsed = start.elapsed();
        assert!(
            elapsed >= idle_bound && elapsed < idle_bound * 20,
            "the idle bound, not the outer cap, must end a stall; took {elapsed:?}"
        );
    }

    /// C-021's byte-cap edge: relaxing the deadline must not relax the cap.
    ///
    /// Both halves, because either alone is half a proof — a cap that refuses
    /// everything and a cap that refuses nothing are indistinguishable from one
    /// assertion. The oversize half declares a `Content-Length` past the cap and
    /// sends nothing, so it also pins the "refused before a single byte is read"
    /// property.
    #[tokio::test]
    async fn the_byte_cap_still_fires_and_still_lets_an_ordinary_body_through() {
        let endpoint = StubIndexEndpoint::start(vec![
            Reply::ForgedLength {
                declared: MAX_INDEX_DOCUMENT_BYTES as u64 + 1,
            },
            ok(),
        ])
        .await;
        let transport = ReqwestIndexTransport::with_hardening(
            &quick_bounds(),
            RetryPolicy {
                attempts: 1,
                ..quick_ladder()
            },
        );
        let url = endpoint.url();

        let refusal = transport
            .get(&url)
            .await
            .expect_err("a declared oversize body is refused");
        assert!(
            refusal.to_string().contains("index request to"),
            "the refusal is an IndexHttpFailed, got {refusal}"
        );
        assert_eq!(
            endpoint.served(),
            1,
            "the declaration is refused before the body is read, and an oversize body is not retryable"
        );

        match transport.get(&url).await {
            Ok(IndexFetch::Found { bytes }) => assert_eq!(bytes, BODY.as_bytes()),
            other => panic!("a body inside the cap must still be served, got {other:?}"),
        }
    }

    /// **C-028 — the contract that makes dropping the total deadline
    /// detectable at all.**
    ///
    /// The peer dribbles one byte every `idle_bound − ε`: it never trips the
    /// per-frame bound and never approaches the 32 MiB byte cap, so nothing but
    /// the outer cap can end it. C-021's two halves pass identically with or
    /// without an outer cap, which is why C-021 alone is not enough.
    ///
    /// *Red-reachability:* remove `.timeout(hardening.outer_cap)` from
    /// `build_index_http_client` and this test hangs past the cap.
    #[tokio::test]
    async fn a_dribbling_peer_is_ended_by_the_outer_cap() {
        let idle_bound = Duration::from_millis(500);
        let outer_cap = Duration::from_secs(2);
        let endpoint = StubIndexEndpoint::start(vec![Reply::DribbleForever {
            // Comfortably under the idle bound, so the idle bound never fires.
            interval: Duration::from_millis(200),
        }])
        .await;
        let transport = ReqwestIndexTransport::with_hardening(
            &TransportHardening {
                connect_timeout: Duration::from_secs(5),
                idle_bound,
                outer_cap,
            },
            RetryPolicy {
                attempts: 1,
                ..quick_ladder()
            },
        );

        let start = std::time::Instant::now();
        let outcome = tokio::time::timeout(outer_cap * 5, transport.get(&endpoint.url())).await;
        let elapsed = start.elapsed();
        let outcome = outcome.expect("without an outer cap a dribbling peer never terminates — this is the red");
        assert!(
            outcome.is_err(),
            "a peer that never finishes must fail, got {outcome:?}"
        );
        assert!(
            elapsed >= outer_cap && elapsed < outer_cap * 3,
            "the failure must land at roughly the outer cap, not the idle bound; took {elapsed:?}"
        );
    }
}

// ── Diagnostic-surface guards (C-026, C-031) ─────────────────────────────────

#[cfg(test)]
mod diagnostic_surface_tests {
    /// The module's own source, non-test half only, comments intact.
    ///
    /// Splitting on the first `#[cfg(test)]` is the
    /// `local_index.rs::the_per_tag_fan_out_is_sized_by_the_constant_at_every_site`
    /// preprocessing, copied rather than re-invented: a raw grep counts
    /// occurrences inside the test half and reports a break that is not there.
    fn production_source() -> &'static str {
        include_str!("ocx_index.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("the module has a non-test half")
    }

    /// Drops `//`-prefixed lines.
    ///
    /// Required before any denylist scan: a comment that quotes the form it
    /// forbids — the right thing for a comment to do — otherwise matches
    /// itself. Applied *after* slicing, never before, because the section
    /// dividers that delimit a region are themselves comments.
    fn strip_comments(source: &str) -> String {
        source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The transport region: `ReqwestIndexTransport`'s inherent methods and its
    /// `IndexTransport` impl — where the retry ladder lives and where a new
    /// diagnostic would land.
    fn transport_region() -> String {
        let source = production_source();
        let start = source
            .find("impl ReqwestIndexTransport {")
            .expect("the transport's inherent impl anchors the region");
        let end = source
            .find("// ── Physical reference parsing")
            .expect("the next section divider closes the region");
        assert!(end > start, "the region anchors must be in source order");
        strip_comments(&source[start..end])
    }

    /// The operator-facing `warn!` inventory this module is allowed to have —
    /// each entry the site's **whole** format string, compared by equality.
    ///
    /// Four are publisher-signal advisories from the yank/deprecation/supersede
    /// feature; the fifth reports a degraded HTTP-client build. None came from
    /// the retry work, and none may be removed to satisfy a count — they are
    /// another feature's operator surface.
    ///
    /// Whole strings, not fragments, because a fragment check on a fixed-width
    /// window around each site lets one site's window run into its neighbour's
    /// text: deleting the supersede advisory then still "matched", inside the
    /// deprecation site's window, and the guard stayed green.
    const ALLOWED_WARN_SITES: [&str; 5] = [
        "index HTTP client build with bundled roots failed ({error}); using hardened reqwest defaults",
        "'{identifier}' resolves to a yanked entry — a yank is a publisher signal, not a delete",
        "'{identifier}' is deprecated: {message}",
        "'{identifier}' is deprecated",
        "'{identifier}' is superseded by '{successor}' (advisory; not followed automatically)",
    ];

    /// The format string of one `log::` site, given the source text following
    /// the macro name.
    fn format_string(site: &str) -> &str {
        let open = site.find('"').expect("a log site opens a format string");
        let rest = &site[open + 1..];
        let close = rest.find('"').expect("the format string closes");
        &rest[..close]
    }

    /// **C-026 — the retry ladder adds no operator-facing line.**
    ///
    /// Per site, not a count. A count is the wrong instrument twice over: at
    /// zero the contract is red before any of this work exists (the five sites
    /// below predate it), and at five a green says only that the total did not
    /// move — a retry `warn!` added while an advisory was deleted passes.
    /// Matching each site against a known inventory says what the contract
    /// actually means: *this* line is one we already had.
    ///
    /// `index_common.rs::the_funnel_neutralizes_both_halves` is the precedent.
    ///
    /// *Red-reachability:* any new `log::warn!` or `log::info!` matches no
    /// entry and fails; deleting an advisory fails the inventory check.
    #[test]
    fn no_operator_facing_diagnostic_is_added_to_this_module() {
        let source = strip_comments(production_source());
        assert!(!source.is_empty(), "non-vacuity: the scanned window must not be empty");
        // Anchored on text that exists ONLY in the excluded half, which is the
        // one form of this check that can actually fail.
        //
        // Two earlier forms could not. `!source.contains("#[cfg(test)]")` is the
        // prefix before the first occurrence, so it holds in every state of the
        // file. Comparing lengths fails for the same reason one step removed:
        // the needle appears *in this file* (in the `split` call below and in
        // this very comment), so `split` always finds a separator and the prefix
        // is always strictly shorter — true whether or not the truncation landed
        // where it should. Both are self-matching detectors, and no choice of
        // needle repairs either: a `#[cfg(all(test))]` regate leaves the window
        // spanning both test modules while every assertion here stays green.
        //
        // A module name cannot self-match, because it is declared only in the
        // half this window must not reach.
        for excluded in ["mod tests {", "mod diagnostic_surface_tests {"] {
            assert!(
                !source.contains(excluded),
                "non-vacuity: the window reached `{excluded}`, so the split did not truncate the test \
                 half and a truncation bug fakes a low count"
            );
        }
        assert!(
            source.contains("impl IndexTransport for ReqwestIndexTransport"),
            "non-vacuity: the window must actually reach the transport, or it scans nothing"
        );
        // The other truncation direction, and the one the message above names:
        // a window that stops *early* drops production `log::warn!` sites and
        // fakes a low count, which the length check cannot see. This anchor is
        // the last item of the production half, so the window has to span all
        // of it.
        assert!(
            source.contains("impl index_impl::IndexImpl for OcxIndex"),
            "non-vacuity: the window must reach the production half's last item, or it scans only a prefix of it"
        );
        assert_eq!(
            source.matches("log::info!").count(),
            0,
            "a retried transient is a common benign state; `info!` per retry is noise across a 512-wide fan-out"
        );

        let sites: Vec<&str> = source.split("log::warn!").skip(1).map(format_string).collect();
        for site in &sites {
            assert!(
                ALLOWED_WARN_SITES.contains(site),
                "new operator-facing `warn!` in this module — S-003 says retries are `debug!` and S-007 \
                 says a self-heal is silent: \"{site}\""
            );
        }
        for known in ALLOWED_WARN_SITES {
            assert!(
                sites.contains(&known),
                "the `{known}` advisory is gone — it is publisher semantics, not retry noise, and this \
                 inventory must be edited deliberately, never emptied to make a guard pass"
            );
        }
    }

    /// **C-031 — every diagnostic in the retry region redacts the URL.**
    ///
    /// Checked **per site**, never as a count: a count budget is satisfied by
    /// one raw call paired with one redacted call elsewhere. Precedent:
    /// `index_common.rs::the_funnel_neutralizes_both_halves`.
    ///
    /// An index base URL may embed `user:password@` — that is what `redact_url`
    /// exists for (CWE-532), and the retry ladder's natural line ("retrying
    /// {url}, attempt 2/3") is a new emission site in exactly this region.
    #[test]
    fn every_diagnostic_and_error_in_the_retry_region_redacts_the_url() {
        let region = transport_region();
        assert!(
            region.contains("transport_policy::run("),
            "non-vacuity: the region must contain the retry ladder, or it watches the wrong code"
        );

        let mut log_sites = 0;
        for site in region.split("log::").skip(1) {
            let statement = site.split(");").next().expect("a macro invocation ends somewhere");
            assert!(
                statement.starts_with("debug!"),
                "only `debug!` belongs in the retry region (C-026); found `log::{}`",
                statement.lines().next().unwrap_or_default()
            );
            assert!(
                statement.contains("redact_url(url)"),
                "this diagnostic renders a URL raw (CWE-532): log::{statement}"
            );
            log_sites += 1;
        }
        assert!(
            log_sites >= 1,
            "non-vacuity: the retry ladder must emit at least one diagnostic, or this guard watches nothing"
        );

        // Tripwire for the likely accident, not the contract itself — the
        // contract is `a_redirect_is_neither_retried_nor_followed`. A ladder
        // that reads `Location` at all is re-issuing against it.
        for needle in ["LOCATION", "\"location\"", "Location\""] {
            assert!(
                !region.contains(needle),
                "`{needle}` in the retry region: following a redirect here bypasses `resolve_base_url`'s \
                 plain-HTTP gate on a client with no `GuardedResolver` (CWE-918 / CWE-319)"
            );
        }

        let mut error_sites = 0;
        for site in region.split("IndexHttpFailed {").skip(1) {
            // The classifier's `matches!` pattern binds fields rather than
            // constructing; only construction carries a `url:`.
            let Some(fields) = site.split('}').next() else {
                continue;
            };
            if !fields.contains("url:") {
                continue;
            }
            assert!(
                fields.contains("redact_url(url)"),
                "every failure raised here echoes the request URL and must redact it: {fields}"
            );
            error_sites += 1;
        }
        assert!(
            error_sites >= 4,
            "non-vacuity: the region raises several IndexHttpFailed variants; saw {error_sites}"
        );
    }

    /// The three bounds must all be applied, and applied from the injected
    /// hardening rather than from a re-introduced constant.
    ///
    /// *Red-reachability:* delete any one builder call and the matching
    /// assertion fails — including `.timeout(...)`, whose absence
    /// `a_dribbling_peer_is_ended_by_the_outer_cap` detects behaviourally.
    #[test]
    fn the_client_builder_applies_all_three_bounds_and_follows_no_redirect() {
        let source = strip_comments(production_source());
        for needle in [
            ".connect_timeout(hardening.connect_timeout)",
            ".read_timeout(hardening.idle_bound)",
            ".timeout(hardening.outer_cap)",
            ".redirect(reqwest::redirect::Policy::none())",
        ] {
            assert!(
                source.contains(needle),
                "`{needle}` is missing: the index client must fast-fail connects, detect stalls, cap one \
                 attempt, and never follow a redirect (CWE-918 / CWE-319)"
            );
        }
        assert!(
            !source.contains("reqwest::Client::new()"),
            "D-011b: a bare `Client::new()` carries reqwest's defaults — no timeouts, redirects followed \
             up to 10 hops — which is remote-controlled egress able to relocate the fetch to http:// \
             after the plain-HTTP gate already ran"
        );
    }
}
